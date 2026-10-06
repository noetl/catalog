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
}

impl StoreConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            shard_count: 1,
        }
    }

    fn l0(&self, dataset: &str) -> L0Config {
        L0Config::for_dataset(dataset, self.root.join(dataset))
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
            .with_flush(FlushPolicy::EveryAppend)
        // `manifest_retain` is deliberately left at its default of 32. `0` means
        // unbounded, which on prod produced 6,770 snapshots and 19.4 GB behind
        // 71.8 MB of data and stopped every append.
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

/// The catalog's four logs and the folds over them.
pub struct CatalogStore {
    entities: L0Engine<EntityDataset>,
    attributes: L0Engine<AttributeDataset>,
    relations: L0Engine<RelationDataset>,
    types: L0Engine<TypeDataset>,
    next_seq: u64,
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
            next_seq: 1,
        })
    }

    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    // --- writes -------------------------------------------------------------

    pub fn register(&mut self, entity: Entity) -> Result<u64> {
        let op = EntityOp {
            op_seq: self.seq(),
            path: entity.path.clone(),
            version: entity.version,
            op: EntityOpKind::Registered(Box::new(entity)),
        };
        self.entities.append_record(op)
    }

    pub fn archive(&mut self, path: &str, version: u32, at: i64) -> Result<u64> {
        let op = EntityOp {
            op_seq: self.seq(),
            path: path.to_string(),
            version,
            op: EntityOpKind::Archived { at },
        };
        self.entities.append_record(op)
    }

    pub fn restore(&mut self, path: &str, version: u32) -> Result<u64> {
        let op = EntityOp {
            op_seq: self.seq(),
            path: path.to_string(),
            version,
            op: EntityOpKind::Restored,
        };
        self.entities.append_record(op)
    }

    pub fn set_attribute(&mut self, path: &str, attr: Attribute) -> Result<u64> {
        let op = AttributeOp {
            op_seq: self.seq(),
            path: path.to_string(),
            op: AttributeOpKind::Set(Box::new(attr)),
        };
        self.attributes.append_record(op)
    }

    pub fn unset_attribute(&mut self, path: &str, name: &str) -> Result<u64> {
        let op = AttributeOp {
            op_seq: self.seq(),
            path: path.to_string(),
            op: AttributeOpKind::Unset {
                name: name.to_string(),
            },
        };
        self.attributes.append_record(op)
    }

    pub fn assert_relation(&mut self, rel: Relation) -> Result<u64> {
        let op = RelationOp {
            op_seq: self.seq(),
            from_path: rel.from_entity.path.clone(),
            op: RelationOpKind::Asserted(Box::new(rel)),
        };
        self.relations.append_record(op)
    }

    pub fn declare_type(&mut self, t: ResourceType) -> Result<u64> {
        let op = TypeOp {
            op_seq: self.seq(),
            name: t.name.clone(),
            declared: t,
        };
        self.types.append_record(op)
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
        let folded = fold_latest_by(ops, |o| o.version);
        let mut out = Vec::new();
        for op in folded.into_values() {
            match op.op {
                EntityOpKind::Registered(e) => out.push(*e),
                // An archive/restore with no preceding register cannot be
                // reconstructed into an Entity, and inventing one would fabricate a
                // record. Skipped, deliberately.
                EntityOpKind::Archived { .. } | EntityOpKind::Restored => {}
            }
        }
        Ok(out)
    }

    /// The newest non-archived version at `path`.
    pub fn latest(&self, path: &str) -> Result<Option<Entity>> {
        let ops = self.entities.read_index_after(path, 0)?;
        // Replay per version so archive/restore apply to the version they name.
        let mut per_version: BTreeMap<u32, (Option<Entity>, Option<i64>)> = BTreeMap::new();
        for op in ops {
            let slot = per_version.entry(op.version).or_insert((None, None));
            match op.op {
                EntityOpKind::Registered(e) => slot.0 = Some(*e),
                EntityOpKind::Archived { at } => slot.1 = Some(at),
                EntityOpKind::Restored => slot.1 = None,
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
        let folded = fold_latest_by(ops, |o| o.op.name().to_string());
        Ok(folded
            .into_iter()
            .filter_map(|(name, op)| match op.op {
                AttributeOpKind::Set(a) => Some((name, *a)),
                AttributeOpKind::Unset { .. } => None,
            })
            .collect())
    }

    /// Every live outgoing edge from `from_path`.
    pub fn relations_from(&self, from_path: &str) -> Result<Vec<Relation>> {
        let ops = self.relations.read_index_after(from_path, 0)?;
        let folded = fold_latest_by(ops, |o| o.edge_key());
        Ok(folded
            .into_values()
            .filter_map(|op| match op.op {
                RelationOpKind::Asserted(r) => Some(*r),
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
    /// Returns the number of parts sealed across all four datasets.
    pub fn tick(&mut self) -> Result<usize> {
        let mut n = 0;
        n += self.entities.seal_aged_parts()?;
        n += self.attributes.seal_aged_parts()?;
        n += self.relations.seal_aged_parts()?;
        n += self.types.seal_aged_parts()?;
        Ok(n)
    }
}
