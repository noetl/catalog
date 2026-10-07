//! The store: four `L0Engine`s and the folds over them.

use crate::datasets::{
    AttributeDataset, AttributeOp, AttributeOpKind, EntityDataset, EntityOp, EntityOpKind,
    RelationDataset, RelationOp, RelationOpKind, TypeDataset, TypeOp,
};
use crate::fold_latest_by;
use catalog_model::{Attribute, Entity, Relation, ResourceType};
use ehdb_l0::{Dataset, FlushPolicy, L0Config, L0Engine, LocalFsSubstrate};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How long an unsealed part may sit before the sealer must take it.
///
/// ⚠⚠ `L0Config::seal_max_age` defaults to `None`, and `ehdb-l0`'s own comment says
/// what that means: *"today's behavior leaves the durability window **unbounded in
/// time**: a shard that appends a few records and goes quiet never seals, so those
/// records never reach the substrate."* EHDB's durability spec requires 5 s.
///
/// **The catalog is the worst case for this, not a mild one.** It holds ~1,600
/// records and registration is a human-paced act, so the shard is almost always
/// quiet — which is exactly the condition under which the window is unbounded. A
/// registered playbook could sit unsealed indefinitely and be lost on pod
/// replacement.
pub const SEAL_MAX_AGE: Duration = Duration::from_secs(5);

/// Where the catalog's datasets live, and the knobs that are not safe to default.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub root: PathBuf,
    /// ⚠ One. `L0Engine` takes `&mut self` for appends and has **no built-in mutual
    /// exclusion** — "the caller is the shard owner". The only election/fencing code
    /// in EHDB is the shadow-mode, non-authoritative pair in `ehdb-reference`, which
    /// this crate deliberately does not depend on. Multi-writer is out of scope and
    /// would need work EHDB has not finished.
    pub shard_count: u32,

    /// Records per sealed part. `None` keeps EHDB's default of 1024.
    ///
    /// Exposed because it sets when a **merge** becomes eligible, and that interacts
    /// with [`CatalogStore::tick`]: EHDB's `MergePolicy::d1` has `trigger_run_len: 4`
    /// and only counts parts that are already durable, so a partition needs roughly
    /// `4 × seal_max_records` ops before any merge is planned. At the default that is
    /// ~**4,096**, which a test cannot reach quickly — and an untested merge driver is
    /// one nobody can show works.
    pub seal_max_records: Option<u64>,
}

impl StoreConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            shard_count: 1,
            seal_max_records: None,
        }
    }

    /// Override the records-per-part threshold. See [`StoreConfig::seal_max_records`].
    pub fn with_seal_max_records(mut self, n: u64) -> Self {
        self.seal_max_records = Some(n);
        self
    }

    fn l0(&self, dataset: &str) -> L0Config {
        let cfg = L0Config::for_dataset(dataset, self.root.join(dataset))
            .with_shard_count(self.shard_count)
            // ⚠ Set explicitly. See SEAL_MAX_AGE — and note this flag is
            // NECESSARY BUT NOT SUFFICIENT: an idle shard takes no appends, so
            // something must call `seal_aged_parts` on a timer. `CatalogStore::tick`
            // is that something, and a guard asserts it exists.
            .with_seal_max_age(Some(SEAL_MAX_AGE))
            // ⚠ `EveryAppend`, not `CallerDriven`. `CallerDriven` is only for a
            // writer that owns its commit points (`FeedWriter` does; a bare
            // `L0Engine` caller does not). Catalog writes are rare and each one
            // matters, so the fsync cost is irrelevant and the failure mode of being
            // wrong is losing a registration.
            .with_flush(FlushPolicy::EveryAppend);
        // `manifest_retain` is deliberately left at its default of 32. `0` means
        // unbounded, which on prod produced 6,770 snapshots and 19.4 GB behind
        // 71.8 MB of data and stopped every append.
        match self.seal_max_records {
            Some(n) => cfg.with_seal_max_records(n),
            None => cfg,
        }
    }
}

/// What one [`CatalogStore::register_from_source`] recorded.
///
/// The counts are returned separately rather than summed, because they answer different
/// questions and a single total would hide a zero. A subscription with 2 edges and 0
/// attributes and one with 0 edges and 2 attributes are very different situations, and
/// "2" would describe both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registered {
    /// The entity op's sequence number.
    pub op_seq: u64,
    /// Relations appended to `c2`.
    pub relations: usize,
    /// Attributes appended to `c3`.
    pub attributes: usize,
}

/// What one [`CatalogStore::tick`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticked {
    /// Parts sealed because they exceeded [`SEAL_MAX_AGE`].
    pub sealed: usize,
    /// Merges performed. Zero is the normal reading until a partition has roughly
    /// `4 × seal_max_records` ops; see [`CatalogStore::tick`].
    pub merged: usize,
    /// Part files and substrate objects the manifest no longer references, deleted.
    ///
    /// ⚠ This is reported separately from `merged` on purpose. A merge that cuts the
    /// part count while raising bytes on disk is a merge that costs storage instead of
    /// saving it, and a single combined number cannot show that. It is exactly what was
    /// measured before reclaim was wired in: 25 parts → 4, and disk **+42%**.
    pub reclaimed: usize,
}

/// Sealed-part counts per dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartCounts {
    pub entities: usize,
    pub attributes: usize,
    pub relations: usize,
    pub types: usize,
}

impl PartCounts {
    pub fn total(&self) -> usize {
        self.entities + self.attributes + self.relations + self.types
    }
}

/// The catalog's four logs and the folds over them.
pub struct CatalogStore {
    entities: L0Engine<EntityDataset>,
    attributes: L0Engine<AttributeDataset>,
    relations: L0Engine<RelationDataset>,
    types: L0Engine<TypeDataset>,
    shard_count: u32,
}

type Result<T> = std::result::Result<T, ehdb_core::EhdbError>;

impl CatalogStore {
    /// Open all four datasets under one root.
    pub fn open(cfg: &StoreConfig) -> Result<Self> {
        let sub = |name: &str| -> Result<Arc<dyn ehdb_l0::DurableSubstrate>> {
            Ok(Arc::new(LocalFsSubstrate::new(
                cfg.root.join(name).join("substrate"),
            )?))
        };
        Ok(Self {
            entities: L0Engine::open(cfg.l0(EntityDataset::NAME), sub(EntityDataset::NAME)?)?,
            attributes: L0Engine::open(
                cfg.l0(AttributeDataset::NAME),
                sub(AttributeDataset::NAME)?,
            )?,
            relations: L0Engine::open(cfg.l0(RelationDataset::NAME), sub(RelationDataset::NAME)?)?,
            types: L0Engine::open(cfg.l0(TypeDataset::NAME), sub(TypeDataset::NAME)?)?,
            shard_count: cfg.shard_count,
        })
    }

    // --- writes -------------------------------------------------------------

    pub fn register(&mut self, entity: Entity) -> Result<u64> {
        EntityOp::assert_forward_path_is_not_a_synthetic_key(&entity.path)
            .map_err(ehdb_core::EhdbError::InvalidIdentifier)?;
        let type_name = entity.resource_type.clone();
        let path_for_index = entity.path.clone();
        let op = EntityOp {
            // Assigned by the engine in `append_writer_assigned`; see
            // `Dataset::assign_sort_key`. A placeholder here, never the real key.
            op_seq: 0,
            path: entity.path.clone(),
            version: entity.version,
            op: EntityOpKind::Registered(Box::new(entity)),
        };
        // Forward first, for the same reason as the c3 reverse row: a crash between
        // the two leaves the forward answer complete and the listing short by one.
        let forward_seq = self.entities.append_writer_assigned(op)?;
        self.write_type_index(&path_for_index, &type_name, true)?;
        Ok(forward_seq)
    }

    /// Append one type-index row. Shared by register/archive/restore so the three
    /// cannot drift in how they build the key.
    fn write_type_index(&mut self, path: &str, resource_type: &str, live: bool) -> Result<u64> {
        let op = EntityOp {
            op_seq: 0,
            path: path.to_string(),
            // ⚠ Zero, never a real version. A type-index row is about the PATH, not
            // about one version of it: a listing must not show a path twice because
            // it has three versions. The forward folds filter these rows out, so the
            // 0 can never become a phantom version.
            version: 0,
            op: EntityOpKind::TypeIndex {
                index: crate::datasets::type_key(resource_type),
                path: path.to_string(),
                live,
            },
        };
        self.entities.append_writer_assigned(op)
    }

    /// **List every live resource of one type.**
    ///
    /// The query that would otherwise force a fifth dataset, and the one that makes
    /// AC3's claim — "adding a resource type costs no schema" — checkable rather than
    /// asserted. Before this, `catalog list` printed that it could not do it.
    ///
    /// ⚠ Folded per PATH, not with `.last()`. Many paths share one type key (~1,600
    /// under `playbook` at expected size), so `.last()` would return one of them,
    /// successfully. The sibling c3 index measured exactly that: 1 of 49.
    pub fn resources_of_type(&self, resource_type: &str) -> Result<Vec<String>> {
        let key = crate::datasets::type_key(resource_type);
        let ops = self.entities.read_index_after(&key, 0)?;
        let folded = fold_latest_by(ops, |o| match &o.op {
            EntityOpKind::TypeIndex { path, .. } => path.clone(),
            _ => o.path.clone(),
        });
        let mut out: Vec<String> = folded
            .into_values()
            .filter_map(|op| match op.op {
                EntityOpKind::TypeIndex {
                    path, live: true, ..
                } => Some(path),
                _ => None,
            })
            .collect();
        out.sort();
        Ok(out)
    }

    pub fn archive(&mut self, path: &str, version: u32, at: i64) -> Result<u64> {
        let op = EntityOp {
            // Assigned by the engine in `append_writer_assigned`; see
            // `Dataset::assign_sort_key`. A placeholder here, never the real key.
            op_seq: 0,
            path: path.to_string(),
            version,
            op: EntityOpKind::Archived { at },
        };
        let forward_seq = self.entities.append_writer_assigned(op)?;
        self.refresh_type_index(path)?;
        Ok(forward_seq)
    }

    pub fn restore(&mut self, path: &str, version: u32) -> Result<u64> {
        let op = EntityOp {
            // Assigned by the engine in `append_writer_assigned`; see
            // `Dataset::assign_sort_key`. A placeholder here, never the real key.
            op_seq: 0,
            path: path.to_string(),
            version,
            op: EntityOpKind::Restored,
        };
        let forward_seq = self.entities.append_writer_assigned(op)?;
        self.refresh_type_index(path)?;
        Ok(forward_seq)
    }

    /// Recompute a path's presence in the type listing after an archive or restore.
    ///
    /// ⚠⚠ **Archiving a version is not archiving the path.** A path carries many
    /// versions; archiving v1 while v2 and v3 are live must leave the path in the
    /// listing. So the tombstone is derived from `latest(path)` — "is any version
    /// still live" — rather than from the op that just happened. Writing
    /// `live: false` on every archive would silently drop paths that are still
    /// perfectly serveable, and the listing would under-report.
    ///
    /// The type name comes from `latest()` when a live version exists, and otherwise
    /// from the newest *registered* version, because a tombstone still has to be
    /// filed under the right key to shadow the live row it supersedes. If no
    /// registration exists at all there is nothing to index and this is a no-op.
    fn refresh_type_index(&mut self, path: &str) -> Result<()> {
        let is_live = self.latest(path)?.is_some();

        // ⚠ The type name must come from the raw ops, NOT from `versions()`.
        //
        // This is where the first draft was wrong, and the archive test caught it.
        // `versions()` folds latest-op-per-version and emits only `Registered`, so
        // once every version is archived it returns an EMPTY vec — the fallback found
        // no type name, returned early, and wrote **no tombstone at all**. The path
        // stayed listed forever, which is precisely the under-reporting this function
        // exists to prevent, arriving by the opposite route.
        //
        // So: scan for the most recent `Registered` op regardless of what later
        // archive/restore ops say about it. A path that was never registered has no
        // type and nothing to index.
        let ops = self.entities.read_index_after(path, 0)?;
        let type_name = ops.iter().rev().find_map(|o| match &o.op {
            EntityOpKind::Registered(e) => Some(e.resource_type.clone()),
            _ => None,
        });
        let Some(type_name) = type_name else {
            return Ok(());
        };

        self.write_type_index(path, &type_name, is_live)?;
        Ok(())
    }

    /// Set an attribute, writing the forward row **and** its reverse-index row.
    ///
    /// Returns the **forward** op's sequence — that is the write a caller means when
    /// it asks "what sequence did this land at". The reverse row is an index
    /// artefact of the same logical write, not a second attribute.
    ///
    /// ⚠ The two appends are not atomic; `ehdb-l0` has no multi-append transaction.
    /// The ordering is deliberate: **forward first**. If the process dies between
    /// them the forward answer is complete and the reverse answer is missing one
    /// entry — recoverable by replaying the forward rows. The other order would
    /// leave the reverse index claiming a resource carries an attribute that the
    /// forward read says it does not, which is a contradiction between two stores
    /// and far worse to reason about.
    pub fn set_attribute(&mut self, path: &str, attr: Attribute) -> Result<u64> {
        AttributeOp::assert_forward_path_is_not_a_reverse_key(path)
            .map_err(ehdb_core::EhdbError::InvalidIdentifier)?;
        let name = attr.name.clone();
        let op = AttributeOp {
            // Assigned by the engine in `append_writer_assigned`; see
            // `Dataset::assign_sort_key`. A placeholder here, never the real key.
            op_seq: 0,
            path: path.to_string(),
            op: AttributeOpKind::Set(Box::new(attr)),
        };
        let forward_seq = self.attributes.append_writer_assigned(op)?;
        self.write_reverse(path, &name, true)?;
        Ok(forward_seq)
    }

    /// Unset an attribute, and **tombstone** its reverse row.
    ///
    /// ⚠ The tombstone is the whole reason the reverse row carries a `live` flag
    /// rather than being append-only-present. Without it, unsetting an attribute
    /// would leave the resource in `resources_with_attribute` forever: the reverse
    /// index would only grow, and "which resources use this credential" would
    /// accumulate resources that stopped using it. That is a wrong answer that
    /// never self-corrects.
    pub fn unset_attribute(&mut self, path: &str, name: &str) -> Result<u64> {
        AttributeOp::assert_forward_path_is_not_a_reverse_key(path)
            .map_err(ehdb_core::EhdbError::InvalidIdentifier)?;
        let op = AttributeOp {
            // Assigned by the engine in `append_writer_assigned`; see
            // `Dataset::assign_sort_key`. A placeholder here, never the real key.
            op_seq: 0,
            path: path.to_string(),
            op: AttributeOpKind::Unset {
                name: name.to_string(),
            },
        };
        let forward_seq = self.attributes.append_writer_assigned(op)?;
        self.write_reverse(path, name, false)?;
        Ok(forward_seq)
    }

    /// Append one reverse-index row. Shared by set and unset so the two paths
    /// cannot drift in how they build the key.
    fn write_reverse(&mut self, path: &str, name: &str, live: bool) -> Result<u64> {
        let op = AttributeOp {
            op_seq: 0,
            // For a reverse row `path` is the ANSWER, not the key.
            path: path.to_string(),
            op: AttributeOpKind::Reverse {
                index: crate::datasets::reverse_key(name),
                path: path.to_string(),
                live,
            },
        };
        self.attributes.append_writer_assigned(op)
    }

    /// Assert an edge, writing the forward row **and** its reverse-edge row.
    ///
    /// Forward first, for the same reason as the other two indexes: a crash between
    /// them leaves the forward answer complete and the caller list short by one.
    pub fn assert_relation(&mut self, rel: Relation) -> Result<u64> {
        let from = rel.from_entity.path.clone();
        let to = rel.to_entity.path.clone();
        // The kind LABEL — see the note in `RelationOp::edge_key`. The reverse index
        // must answer with a label a reader can interpret ("references"), not a
        // Debug-rendered struct.
        let kind = rel.kind.discriminant().to_string();
        let op = RelationOp {
            // Assigned by the engine in `append_writer_assigned`; see
            // `Dataset::assign_sort_key`. A placeholder here, never the real key.
            op_seq: 0,
            from_path: from.clone(),
            op: RelationOpKind::Asserted(Box::new(rel)),
        };
        let forward_seq = self.relations.append_writer_assigned(op)?;
        self.write_reverse_edge(&to, &from, &kind, true)?;
        Ok(forward_seq)
    }

    /// Retract an edge, tombstoning its reverse row so the caller list shrinks.
    /// Retract an edge, tombstoning its reverse row so the caller list shrinks.
    ///
    /// ⚠⚠ `kind` is the **discriminant label** (`"references"`, `"invokes"`), not a
    /// `Debug` rendering. An unknown label is REFUSED rather than written, because a
    /// tombstone that matches no edge is a silent no-op: the caller believes the edge
    /// is gone, `relations_from` still returns it, and nothing errored. This was
    /// observed for real when the labels changed from `Debug` casing (`"Invokes"`) to
    /// the discriminant (`"invokes"`) — a retraction passing the old spelling appended
    /// a tombstone that matched nothing.
    pub fn retract_relation(
        &mut self,
        from_path: &str,
        to: catalog_model::EntityRef,
        kind: &str,
    ) -> Result<u64> {
        let known: Vec<&str> = catalog_model::RelationKind::all()
            .iter()
            .map(|k| k.discriminant())
            .collect();
        if !known.contains(&kind) {
            return Err(ehdb_core::EhdbError::InvalidIdentifier(format!(
                "unknown relation kind {kind:?}; a tombstone with an unrecognised kind \
                 would match no edge and silently no-op. Known: {known:?}"
            )));
        }
        let to_path = to.path.clone();
        let op = RelationOp {
            op_seq: 0,
            from_path: from_path.to_string(),
            op: RelationOpKind::Retracted {
                to: Box::new(to),
                kind: kind.to_string(),
            },
        };
        let forward_seq = self.relations.append_writer_assigned(op)?;
        self.write_reverse_edge(&to_path, from_path, kind, false)?;
        Ok(forward_seq)
    }

    fn write_reverse_edge(
        &mut self,
        to_path: &str,
        from_path: &str,
        kind: &str,
        live: bool,
    ) -> Result<u64> {
        let op = RelationOp {
            op_seq: 0,
            // For a reverse row `from_path` is the ANSWER; the key is the target.
            from_path: from_path.to_string(),
            op: RelationOpKind::ReverseEdge {
                index: crate::datasets::edge_to_key(to_path),
                from_path: from_path.to_string(),
                kind: kind.to_string(),
                live,
            },
        };
        self.relations.append_writer_assigned(op)
    }

    /// **Who calls this?** Every live source that points at `to_path`.
    ///
    /// The direction that matters when changing or retiring a resource:
    /// `relations_from` says what a playbook invokes, and this says who would break.
    ///
    /// ⚠ Folded per `(from_path, kind)`, **not** per `edge_key()`. Under one reverse
    /// key every caller shares the same `(to_path, to_version, kind)`, so folding by
    /// the edge identity would collapse all callers into one — the same partial-answer
    /// shape the sibling indexes measured at 1 of 49 and 1 of 53. Here it would read
    /// 1 of however many callers a shared dependency has, which for a central MCP
    /// playbook is the whole point of asking.
    ///
    /// Returns `(from_path, kind)` pairs, sorted.
    pub fn relations_to(&self, to_path: &str) -> Result<Vec<(String, String)>> {
        let key = crate::datasets::edge_to_key(to_path);
        let ops = self.relations.read_index_after(&key, 0)?;
        let folded = fold_latest_by(ops, |o| match &o.op {
            RelationOpKind::ReverseEdge {
                from_path, kind, ..
            } => (from_path.clone(), kind.clone()),
            _ => (o.from_path.clone(), String::new()),
        });
        let mut out: Vec<(String, String)> = folded
            .into_values()
            .filter_map(|op| match op.op {
                RelationOpKind::ReverseEdge {
                    from_path,
                    kind,
                    live: true,
                    ..
                } => Some((from_path, kind)),
                _ => None,
            })
            .collect();
        out.sort();
        Ok(out)
    }

    pub fn declare_type(&mut self, t: ResourceType) -> Result<u64> {
        let op = TypeOp {
            // Assigned by the engine in `append_writer_assigned`; see
            // `Dataset::assign_sort_key`. A placeholder here, never the real key.
            op_seq: 0,
            name: t.name.clone(),
            declared: t,
        };
        self.types.append_writer_assigned(op)
    }

    /// Register a playbook from its source, recording the references it declares.
    ///
    /// This is the point of P3: registration today extracts the parent's own metadata
    /// and **discards** the child paths its steps name, so a playbook can be
    /// registered referencing a child that does not exist and nothing says so until
    /// the step runs. Here the edges are recorded alongside the entity.
    ///
    /// Returns [`Registered`] — the entity's op sequence plus how many relations and
    /// attributes were recorded.
    ///
    /// # ⚠ The entity is registered even when extraction finds nothing
    ///
    /// And even when the source does not parse as a playbook. `noetl/server` accepts
    /// documents this extractor cannot find references in — a `kind: Subscription`
    /// entry has no `workflow:` at all — so refusing to register on an extraction
    /// failure would make this store reject things the platform accepts. Extraction is
    /// *additive information about* a registration, never a gate on it.
    ///
    /// The `Err` case is a storage failure, never an extraction failure.
    pub fn register_from_source(
        &mut self,
        entity: Entity,
        source: &str,
        extracted_at: i64,
    ) -> Result<Registered> {
        let parent = entity.as_ref_pinned();
        let path = entity.path.clone();
        let entity_id = entity.entity_id;

        // Extract BEFORE the append, so a malformed source cannot leave a
        // half-registered entity with no edges and no record of why.
        let found = catalog_extract::find_references(source).unwrap_or_default();
        let rels = catalog_extract::relations_from(&parent, &found, extracted_at);
        let attrs = catalog_extract::find_attributes(source, entity_id).unwrap_or_default();

        let op_seq = self.register(entity)?;
        let mut relations = 0;
        for r in rels {
            self.assert_relation(r)?;
            relations += 1;
        }
        let mut attributes = 0;
        for a in attrs {
            self.set_attribute(&path, a)?;
            attributes += 1;
        }
        Ok(Registered {
            op_seq,
            relations,
            attributes,
        })
    }

    // --- reads (folds) ------------------------------------------------------

    /// Every live version at `path`, newest version last.
    ///
    /// ⚠ Folded **per version**, not per path. One `path` carries many versions, so
    /// the index key does not identify the folded entity — see [`fold_latest_by`].
    pub fn versions(&self, path: &str) -> Result<Vec<Entity>> {
        let ops = self.entities.read_index_after(path, 0)?;
        // Type-index rows excluded from the fold INPUT; see the same note on
        // `attributes`. They carry `version: 0`, so admitting them would create a
        // phantom version 0 alongside the real ones.
        let ops: Vec<_> = ops.into_iter().filter(|o| !o.op.is_type_index()).collect();
        let folded = fold_latest_by(ops, |o| o.version);
        let mut out = Vec::new();
        for op in folded.into_values() {
            match op.op {
                EntityOpKind::Registered(e) => out.push(*e),
                // An archive/restore with no preceding register cannot be
                // reconstructed into an Entity, and inventing one would fabricate a
                // record. Skipped, deliberately.
                EntityOpKind::Archived { .. } | EntityOpKind::Restored => {}
                // Unreachable: filtered above.
                EntityOpKind::TypeIndex { .. } => {}
            }
        }
        Ok(out)
    }

    /// The newest non-archived version at `path`.
    pub fn latest(&self, path: &str) -> Result<Option<Entity>> {
        let ops = self.entities.read_index_after(path, 0)?;
        let ops: Vec<_> = ops.into_iter().filter(|o| !o.op.is_type_index()).collect();
        // Replay per version so archive/restore apply to the version they name.
        let mut per_version: BTreeMap<u32, (Option<Entity>, Option<i64>)> = BTreeMap::new();
        for op in ops {
            let slot = per_version.entry(op.version).or_insert((None, None));
            match op.op {
                EntityOpKind::Registered(e) => slot.0 = Some(*e),
                EntityOpKind::Archived { at } => slot.1 = Some(at),
                EntityOpKind::Restored => slot.1 = None,
                // Unreachable: filtered above.
                EntityOpKind::TypeIndex { .. } => {}
            }
        }
        Ok(per_version
            .into_iter()
            .rev()
            .find_map(|(_, (e, archived))| match (e, archived) {
                (Some(e), None) => Some(e),
                _ => None,
            }))
    }

    /// Every live attribute on `path`, keyed by name.
    ///
    /// ⚠ This is where a copied `ProjectionStore` fold loses data silently.
    pub fn attributes(&self, path: &str) -> Result<BTreeMap<String, Attribute>> {
        let ops = self.attributes.read_index_after(path, 0)?;

        // ⚠ Reverse rows are excluded from the fold INPUT, not from its output.
        //
        // They live under a disjoint key space (`\u{1}attr/…`) and
        // `read_index_after` matches the index key exactly, so none should arrive
        // here. But if one ever did, its `name()` would equal a real attribute name
        // and it would **shadow** that attribute in the fold — a wrong value, not a
        // missing one. Dropping them before the fold means the worst case is a
        // missing reverse row rather than a corrupted forward answer.
        let forward: Vec<_> = ops.into_iter().filter(|o| !o.op.is_reverse()).collect();

        let folded = fold_latest_by(forward, |o| o.op.name().to_string());
        Ok(folded
            .into_iter()
            .filter_map(|(name, op)| match op.op {
                AttributeOpKind::Set(a) => Some((name, *a)),
                AttributeOpKind::Unset { .. } => None,
                // Unreachable: filtered above. Not `unreachable!()` — a panic in a
                // read path is worse than returning the rest of a correct answer.
                AttributeOpKind::Reverse { .. } => None,
            })
            .collect())
    }

    /// **Reverse lookup: every live resource carrying the attribute `name`.**
    ///
    /// The query this exists for: `uses_credential.adiona_actor` — rotating a
    /// keychain alias means knowing which resources break. The forward index is
    /// keyed by `path`, so answering it without a reverse index means reading every
    /// path in the catalog.
    ///
    /// # ⚠ Why this is folded per PATH and not with `.last()`
    ///
    /// Every resource carrying the attribute shares **one** reverse key, so this is
    /// the dataset's worst case for the `.last()` idiom documented on
    /// [`crate::fold_latest_by`]. Measured on the real corpus before this was folded
    /// correctly: `.last()` returned **1 path out of 49** — and returned it
    /// *successfully*. A partial answer to "which resources use this credential" is
    /// more dangerous than an empty one, because an empty answer gets investigated.
    ///
    /// Returns paths sorted, so a caller diffing two runs sees a stable order.
    pub fn resources_with_attribute(&self, name: &str) -> Result<Vec<String>> {
        let key = crate::datasets::reverse_key(name);
        let ops = self.attributes.read_index_after(&key, 0)?;

        // Fold per PATH: the latest op for each path under this key decides whether
        // that path still carries the attribute.
        let folded = fold_latest_by(ops, |o| match &o.op {
            AttributeOpKind::Reverse { path, .. } => path.clone(),
            // A forward row cannot appear under a reverse key (disjoint key spaces,
            // exact-match read). Keyed by its own path so it cannot collapse other
            // entries if it somehow did.
            _ => o.path.clone(),
        });

        let mut out: Vec<String> = folded
            .into_values()
            .filter_map(|op| match op.op {
                AttributeOpKind::Reverse {
                    path, live: true, ..
                } => Some(path),
                _ => None,
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Every live outgoing edge from `from_path`.
    pub fn relations_from(&self, from_path: &str) -> Result<Vec<Relation>> {
        let ops = self.relations.read_index_after(from_path, 0)?;
        // Reverse rows excluded from the fold INPUT, as in the sibling indexes.
        let ops: Vec<_> = ops.into_iter().filter(|o| !o.is_reverse_edge()).collect();
        let folded = fold_latest_by(ops, |o| o.edge_key());
        Ok(folded
            .into_values()
            .filter_map(|op| match op.op {
                RelationOpKind::Asserted(r) => Some(*r),
                // Unreachable: filtered above.
                RelationOpKind::ReverseEdge { .. } => None,
                RelationOpKind::Retracted { .. } => None,
            })
            .collect())
    }

    /// The current declaration for one resource type.
    pub fn resource_type(&self, name: &str) -> Result<Option<ResourceType>> {
        let key = name.to_lowercase();
        let ops = self.types.read_index_after(&key, 0)?;
        Ok(fold_latest_by(ops, |o| o.name.clone())
            .into_iter()
            .next_back()
            .map(|(_, op)| op.declared))
    }

    // --- lifecycle ----------------------------------------------------------

    /// Seal any part older than [`SEAL_MAX_AGE`] on every dataset.
    ///
    /// ⚠⚠ **This must be called on a timer.** `seal_max_age` alone is inert on
    /// exactly the shard it protects: an idle shard takes no appends, so nothing
    /// triggers a size- or count-based seal, and the records never reach the
    /// substrate. Setting the config field and never calling this is the
    /// configured-but-unreachable shape that `representation-drift.md` is about.
    ///
    /// Also drives **merges**, which nothing else does either.
    ///
    /// ⚠ `run_pending_merges` is caller-owned exactly like `seal_aged_parts`: the only
    /// thing EHDB runs on its own is the background uploader. Measured here before
    /// adding it — 3,500 records produced 3 parts and `run_pending_merges()` performed
    /// **0**, because `MergePolicy::d1` needs a run of **4** consecutive *durable*
    /// small parts. So at the default `seal_max_records` of 1024 a partition needs
    /// ~**4,096** ops before a merge is even planned.
    ///
    /// That is why this was harmless and still worth fixing: the catalog's ~1,600
    /// entries will not reach it on the entity log, but the **attribute** log will —
    /// 1,600 resources carrying several labels each is thousands of ops in one
    /// partition. Past that point parts accumulate monotonically with nothing merging
    /// them, which is the shape that filled a prod PVC while the writer reported
    /// `Ready`.
    ///
    /// # And then reclaims what the merge superseded
    ///
    /// ⚠⚠ Driving merges without reclaiming is not a fix, it is a trade: measured on
    /// 200 writes at `seal_max_records = 8`, one `tick()` cut the manifest from 25 parts
    /// to **4** while part files on disk went 49 → 56 and bytes **94,784 → 189,568**.
    /// The merge doubled storage. `reclaim_orphans` is caller-owned like the other two,
    /// and `ehdb-l0` documents it as deleting precisely "the superseded source parts a
    /// merge (L0.3) leaves behind".
    ///
    /// It runs **after** the merges in the same tick so the manifest swap has already
    /// dropped the sources — reclaiming first would find nothing unreferenced and
    /// report a healthy `0`.
    ///
    /// # The two lifecycle calls this deliberately does NOT make
    ///
    /// * **`apply_retention`** drops whole parts below a sequence floor. A catalog is
    ///   folded latest-op-wins over *every* op in the log, so a resource whose only
    ///   `Registered` op fell below the floor would silently vanish from `latest()`
    ///   while its later attribute ops survived. Retention is correct for an event
    ///   stream and wrong for a state log. Never call it here.
    /// * **`flush_and_wait_uploads`** is unnecessary for a graceful close:
    ///   `L0Engine::drop` sets `upload_tx = None` and joins the uploader thread, whose
    ///   loop is `while let Ok(job) = rx.recv()` — an mpsc receiver drains everything
    ///   already queued before it sees the disconnect. Verified in `ehdb-l0`
    ///   `engine.rs:654` / `:1701`. It remains the right call before a cold-load
    ///   equality check, which this store does not perform.
    pub fn tick(&mut self) -> Result<Ticked> {
        let mut sealed = 0;
        sealed += self.entities.seal_aged_parts()?;
        sealed += self.attributes.seal_aged_parts()?;
        sealed += self.relations.seal_aged_parts()?;
        sealed += self.types.seal_aged_parts()?;

        let mut merged = 0;
        merged += self.entities.run_pending_merges()?;
        merged += self.attributes.run_pending_merges()?;
        merged += self.relations.run_pending_merges()?;
        merged += self.types.run_pending_merges()?;

        // After the merges, never before: the manifest swap inside `run_pending_merges`
        // is what makes the source parts unreferenced in the first place.
        let mut reclaimed = 0;
        reclaimed += self.entities.reclaim_orphans()?;
        reclaimed += self.attributes.reclaim_orphans()?;
        reclaimed += self.relations.reclaim_orphans()?;
        reclaimed += self.types.reclaim_orphans()?;

        // ⚠ Recorded here, not left to the caller. The counters existed as return
        // values and nothing recorded them — the reachability-vs-existence shape
        // again, one level up from the lifecycle calls this function drives.
        crate::metrics::record_tick(sealed, merged, reclaimed);

        Ok(Ticked {
            sealed,
            merged,
            reclaimed,
        })
    }

    /// Sealed parts per partition on each dataset, for observability and tests.
    ///
    /// Reported per dataset rather than summed: a total cannot say *which* log is
    /// accumulating, and the attribute log is the one expected to grow fastest.
    pub fn part_counts(&self) -> PartCounts {
        let n = self.shard_count;
        let count =
            |m: ehdb_l0::Manifest| -> usize { (0..n).map(|p| m.parts_in_partition(p).len()).sum() };
        PartCounts {
            entities: count(self.entities.manifest_snapshot()),
            attributes: count(self.attributes.manifest_snapshot()),
            relations: count(self.relations.manifest_snapshot()),
            types: count(self.types.manifest_snapshot()),
        }
    }
}
