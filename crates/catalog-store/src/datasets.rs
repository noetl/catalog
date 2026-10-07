//! The four EHDB datasets the catalog appends to.
//!
//! # Why a `c…` namespace and not a `D…` number
//!
//! `ehdb-l0` owns the `d1…d10` space and **all ten slots are taken** — `d7_catalog`
//! already exists there, keyed by `path`, mirroring today's flat `noetl.catalog`
//! row. `dataset.rs` in that crate states adding a dataset is "a deliberate
//! compiled-in change here, never a runtime operation", which governs *its* datasets.
//!
//! `Dataset` is a public trait and `L0Engine<D>` is generic over it, so a downstream
//! crate implements it without touching `ehdb-l0`. Taking `D11` would make this
//! repo's release cadence a constraint on ehdb's, and would falsify ehdb's own
//! `lib.rs` header, which enumerates the fixed set as D1–D10. So: `c1…c4`.
//!
//! # Why four logs and not one
//!
//! The partition key differs. Entity and attribute reads are by `path`; relation
//! reads are by edge endpoint. One log keyed by `path` would make "what references
//! X" a full scan.

use catalog_model::{Attribute, Entity, EntityRef, Relation, ResourceType};
use ehdb_l0::{shard_for_execution, Dataset};
use serde::{Deserialize, Serialize};

pub const DATASET_C1_ENTITY: &str = "c1_catalog_entity";
pub const DATASET_C2_RELATION: &str = "c2_catalog_relation";
pub const DATASET_C3_ATTRIBUTE: &str = "c3_catalog_attribute";
pub const DATASET_C4_TYPE: &str = "c4_catalog_type";

// ---------------------------------------------------------------------------
// c1 — entities
// ---------------------------------------------------------------------------

/// What happened to one catalogued entity.
///
/// The three variants are the event vocabulary the shadow catalog log in
/// `noetl/server` already writes — `catalog.registered` / `.archived` /
/// `.restored`. Inherited deliberately rather than reinvented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EntityOpKind {
    Registered(Box<Entity>),
    /// Epoch micros UTC.
    Archived {
        at: i64,
    },
    Restored,
    /// A **type-index** row: "a resource of the type named by `index` lives at
    /// `path`".
    ///
    /// Written alongside every register/archive/restore, so `list --type playbook`
    /// is one indexed read. Before this, listing was impossible: the CLI's `list`
    /// printed "a full path listing needs an index this store does not yet keep".
    ///
    /// ⚠ This is what makes the generalized model's central claim checkable. AC3
    /// says adding a resource type must not add a dataset; "list every resource of
    /// type X" is the query that would otherwise force one.
    ///
    /// ⚠⚠ Many paths share one type key — at the catalog's expected size, ~1,600
    /// under `playbook` alone. Same `.last()` hazard as the c3 reverse index, where
    /// it measured 1 of 49.
    TypeIndex {
        index: String,
        path: String,
        /// `false` is a tombstone, written on archive so an archived resource leaves
        /// the listing. Without it the listing would only grow.
        live: bool,
    },
}

impl EntityOpKind {
    /// Whether this is a type-index row.
    pub fn is_type_index(&self) -> bool {
        matches!(self, Self::TypeIndex { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityOp {
    pub op_seq: u64,
    /// Index + partition dimension.
    pub path: String,
    /// ⚠ Load-bearing for the fold: one `path` carries **many versions**, so the
    /// index key does NOT uniquely identify the thing being folded. See
    /// [`crate::fold_latest_by`].
    pub version: u32,
    pub op: EntityOpKind,
}

#[derive(Debug, Clone, Copy)]
pub struct EntityDataset;

impl Dataset for EntityDataset {
    type Record = EntityOp;
    const NAME: &'static str = DATASET_C1_ENTITY;

    fn sort_key(r: &EntityOp) -> u64 {
        r.op_seq
    }

    /// Derived from [`Self::index_key`], never from `r.path` — see the same note on
    /// [`AttributeDataset::partition`]. A row partitioned by one value and indexed by
    /// another sends the reader to the wrong shard, which returns **nothing** rather
    /// than erroring.
    fn partition(r: &EntityOp, shard_count: u32) -> u32 {
        shard_for_execution(Self::index_key(r), shard_count)
    }

    fn index_key(r: &EntityOp) -> &str {
        match &r.op {
            EntityOpKind::TypeIndex { index, .. } => index.as_str(),
            _ => &r.path,
        }
    }

    fn read_partition(path: &str, shard_count: u32) -> u32 {
        shard_for_execution(path, shard_count)
    }

    /// ⚠ The ENGINE assigns the ordering key, not this crate.
    ///
    /// `ehdb-l0`'s `Dataset` contract requires records appended in **ascending
    /// `sort_key` order within a partition** — the sparse index, MinMax pruning and
    /// merge all rely on it. A hand-rolled counter cannot satisfy that across a
    /// process restart: it would begin again from its initial value and append
    /// *below* the existing maximum, which the engine accepts without erroring.
    ///
    /// The engine's `global_sequence` is recovered from the manifest on `open`
    /// (`manifest.max_sequence()`), so it is monotonic across reopens by
    /// construction. That is why every write here goes through
    /// `append_writer_assigned` rather than `append_record`.
    fn assign_sort_key(mut record: EntityOp, writer_seq: u64) -> EntityOp {
        record.op_seq = writer_seq;
        record
    }
}

// ---------------------------------------------------------------------------
// c3 — attributes
// ---------------------------------------------------------------------------

/// The prefix that marks a `c3` row as a **reverse-index** row rather than a
/// forward one.
///
/// `\u{1}` (SOH) is used deliberately: a reverse key must be impossible to collide
/// with a real `metadata.path`, because `read_index_after` matches the index key by
/// **exact string equality** (`ehdb-l0` `engine.rs:1489`). A path reading
/// `attr/uses_tool.postgres` would otherwise silently answer a reverse query. A
/// control character cannot appear in a YAML `metadata.path`, and
/// [`AttributeDataset::index_key`] asserts a forward path never starts with it, so
/// the two key spaces are disjoint by construction rather than by convention.
pub const REVERSE_KEY_PREFIX: &str = "\u{1}attr/";

/// The prefix marking a `c1` row as a **type-index** row rather than a forward one.
///
/// Same device as [`REVERSE_KEY_PREFIX`], a separate key space in the same dataset,
/// and for the same reason: `list every resource of type X` cannot be answered from
/// an index keyed by `path`. A distinct sentinel (`type/` vs `attr/`) keeps the two
/// reverse spaces apart even though they live in different datasets — one grep for
/// `\u{1}` finds every synthetic key in the crate.
pub const TYPE_KEY_PREFIX: &str = "\u{1}type/";

/// The prefix marking a `c2` row as a **reverse-edge** row rather than a forward one.
///
/// Third application of the same device. The forward index answers "what does X
/// call"; this answers **"what calls X"**, which is the direction that matters when
/// changing or retiring a resource. Measured on the real corpus:
/// `automation/agents/mcp/firestore` has **2 callers** and neither was reachable
/// without scanning every path in the catalog.
pub const EDGE_TO_KEY_PREFIX: &str = "\u{1}to/";

/// The reverse-edge key for a target path.
pub fn edge_to_key(to_path: &str) -> String {
    format!("{EDGE_TO_KEY_PREFIX}{to_path}")
}

/// The type-index key for a resource-type name. Lowercased, because
/// `resource_type()` folds on the lowercased name and noetl/server#429 was a real
/// prod bug caused by two spellings of one kind.
pub fn type_key(resource_type: &str) -> String {
    format!("{TYPE_KEY_PREFIX}{}", resource_type.to_lowercase())
}

/// The reverse index key for an attribute name.
pub fn reverse_key(attribute_name: &str) -> String {
    format!("{REVERSE_KEY_PREFIX}{attribute_name}")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AttributeOpKind {
    Set(Box<Attribute>),
    Unset {
        name: String,
    },
    /// A **reverse-index** row: "the attribute named by `index` is live on `path`".
    ///
    /// Written alongside every forward `Set`, so that "every resource carrying
    /// attribute X" is one indexed read instead of a scan over every path. The
    /// motivating query is `uses_credential.<alias>` — rotating a keychain alias
    /// means knowing which resources break, and the forward index (keyed by `path`)
    /// cannot answer it.
    ///
    /// ⚠ `index` is stored rather than computed because [`Dataset::index_key`]
    /// returns a borrowed `&str`; there is nowhere to borrow a freshly-formatted
    /// key from.
    ///
    /// ⚠⚠ MANY paths share one reverse key. That makes this the dataset's worst
    /// case for the `.last()` idiom: a naive reverse read returns **one** resource
    /// out of however many carry the attribute — a *partial* answer that looks like
    /// a successful one. Measured on the real corpus before the fix: **1 of 49**.
    /// See [`crate::fold_latest_by`].
    Reverse {
        index: String,
        path: String,
        /// `true` mirrors a `Set`, `false` mirrors an `Unset`. A tombstone is
        /// required: without it, unsetting an attribute would leave the resource
        /// in the reverse answer forever, and the reverse index would only ever
        /// grow.
        live: bool,
    },
}

impl AttributeOpKind {
    /// The attribute name this op concerns — the fold's grouping key.
    pub fn name(&self) -> &str {
        match self {
            Self::Set(a) => &a.name,
            Self::Unset { name } => name,
            // The reverse row's name lives in its key, after the sentinel.
            Self::Reverse { index, .. } => index
                .strip_prefix(REVERSE_KEY_PREFIX)
                .unwrap_or(index.as_str()),
        }
    }

    /// Whether this is a reverse-index row.
    pub fn is_reverse(&self) -> bool {
        matches!(self, Self::Reverse { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributeOp {
    pub op_seq: u64,
    /// The resource path. Also the index + partition dimension for a **forward**
    /// row; for a reverse row the key is in [`AttributeOpKind::Reverse::index`] and
    /// this field is the *answer* the reverse query returns.
    pub path: String,
    pub op: AttributeOpKind,
}

impl EntityOp {
    /// A forward row's path must not intrude on either synthetic key space.
    ///
    /// Checks BOTH sentinels, not just this dataset's: a single `\u{1}` prefix test
    /// would be laxer, and naming both makes the error say which space was hit.
    pub fn assert_forward_path_is_not_a_synthetic_key(path: &str) -> Result<(), String> {
        for (prefix, what) in [
            (TYPE_KEY_PREFIX, "type-index"),
            (REVERSE_KEY_PREFIX, "reverse-index"),
        ] {
            if path.starts_with(prefix) {
                return Err(format!(
                    "resource path {path:?} begins with the {what} sentinel; it would \
                     collide with a synthetic key space and silently answer index queries"
                ));
            }
        }
        Ok(())
    }
}

impl AttributeOp {
    /// A forward row's path must not intrude on the reverse key space.
    ///
    /// Returns an error rather than panicking, because the path comes from a
    /// document's `metadata.path` — untrusted input, not a programming mistake.
    pub fn assert_forward_path_is_not_a_reverse_key(path: &str) -> Result<(), String> {
        if path.starts_with(REVERSE_KEY_PREFIX) {
            return Err(format!(
                "resource path {path:?} begins with the reverse-index sentinel; it \
                 would collide with the reverse key space and silently answer \
                 reverse queries"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AttributeDataset;

impl Dataset for AttributeDataset {
    type Record = AttributeOp;
    const NAME: &'static str = DATASET_C3_ATTRIBUTE;

    fn sort_key(r: &AttributeOp) -> u64 {
        r.op_seq
    }

    /// ⚠ Derived from [`Self::index_key`] rather than from `r.path`, so the
    /// partition and the index key cannot disagree.
    ///
    /// They are two halves of one contract: a reader calls
    /// `read_partition(index_value)` to pick the shard and then matches
    /// `index_key(rec) == index_value` inside it. If a reverse row were partitioned
    /// by its `path` but indexed by its reverse key, the reader would look in the
    /// wrong shard and find **nothing** — a clean empty answer, with no error.
    /// Writing both from one expression removes that possibility.
    fn partition(r: &AttributeOp, shard_count: u32) -> u32 {
        shard_for_execution(Self::index_key(r), shard_count)
    }

    /// The forward key is the resource `path`; a reverse row carries its own key.
    ///
    /// ⚠ A forward path starting with [`REVERSE_KEY_PREFIX`] would land in the
    /// reverse key space and answer reverse queries. It cannot happen — the prefix
    /// is a control character — but "cannot happen" is how silent corruption gets
    /// in, so [`AttributeOp::assert_forward_path_is_not_a_reverse_key`] is called on
    /// every forward write.
    fn index_key(r: &AttributeOp) -> &str {
        match &r.op {
            AttributeOpKind::Reverse { index, .. } => index.as_str(),
            _ => &r.path,
        }
    }

    fn read_partition(path: &str, shard_count: u32) -> u32 {
        shard_for_execution(path, shard_count)
    }

    /// ⚠ The ENGINE assigns the ordering key, not this crate.
    ///
    /// `ehdb-l0`'s `Dataset` contract requires records appended in **ascending
    /// `sort_key` order within a partition** — the sparse index, MinMax pruning and
    /// merge all rely on it. A hand-rolled counter cannot satisfy that across a
    /// process restart: it would begin again from its initial value and append
    /// *below* the existing maximum, which the engine accepts without erroring.
    ///
    /// The engine's `global_sequence` is recovered from the manifest on `open`
    /// (`manifest.max_sequence()`), so it is monotonic across reopens by
    /// construction. That is why every write here goes through
    /// `append_writer_assigned` rather than `append_record`.
    fn assign_sort_key(mut record: AttributeOp, writer_seq: u64) -> AttributeOp {
        record.op_seq = writer_seq;
        record
    }
}

// ---------------------------------------------------------------------------
// c2 — relations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RelationOpKind {
    Asserted(Box<Relation>),
    Retracted {
        to: Box<EntityRef>,
        kind: String,
    },
    /// A **reverse-edge** row: "something at `from_path` points at the target named
    /// by `index`".
    ///
    /// ⚠ The edge identity here is `(from_path, kind)` — the *source* varies under one
    /// reverse key, where the forward fold varies the target. Folding this per
    /// `edge_key()` would be wrong: every caller of one target shares the same
    /// `(to_path, to_version, kind)`, so they would collapse to one.
    ReverseEdge {
        index: String,
        from_path: String,
        kind: String,
        /// `false` is a retraction tombstone, so a removed edge leaves the caller
        /// list instead of accumulating forever.
        live: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationOp {
    pub op_seq: u64,
    /// Index + partition dimension: the edge's **source** path.
    pub from_path: String,
    pub op: RelationOpKind,
}

impl RelationOp {
    /// The edge identity this op concerns — the fold's grouping key.
    ///
    /// `(to_path, to_version, kind)`. The target's pinnedness is part of the
    /// identity: an unpinned edge to `a/b` and a pinned edge to `a/b@3` are
    /// different claims, so retracting one must not retract the other.
    pub fn edge_key(&self) -> (String, Option<u32>, String) {
        match &self.op {
            RelationOpKind::Asserted(r) => (
                r.to_entity.path.clone(),
                r.to_entity.version,
                // ⚠⚠ The kind LABEL, never `format!("{:?}", kind)`. `References`
                // carries a `ForeignKey`, so the Debug rendering would put the
                // payload into the edge identity — and then one foreign key
                // re-asserted with corrected metadata becomes a SECOND edge, while a
                // retraction naming it matches nothing. Measured before this fix:
                // 1 FK -> 2 edges, and an unremovable stale edge.
                r.kind.discriminant().to_string(),
            ),
            RelationOpKind::Retracted { to, kind } => (to.path.clone(), to.version, kind.clone()),
            // A reverse row keyed by its own identity, so it can never collapse a
            // forward entry if the two key spaces ever met.
            RelationOpKind::ReverseEdge {
                from_path, kind, ..
            } => (from_path.clone(), None, kind.clone()),
        }
    }

    /// Whether this is a reverse-edge row.
    pub fn is_reverse_edge(&self) -> bool {
        matches!(self.op, RelationOpKind::ReverseEdge { .. })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RelationDataset;

impl Dataset for RelationDataset {
    type Record = RelationOp;
    const NAME: &'static str = DATASET_C2_RELATION;

    fn sort_key(r: &RelationOp) -> u64 {
        r.op_seq
    }
    /// Derived from [`Self::index_key`] — see the note on [`AttributeDataset::partition`].
    fn partition(r: &RelationOp, shard_count: u32) -> u32 {
        shard_for_execution(Self::index_key(r), shard_count)
    }

    fn index_key(r: &RelationOp) -> &str {
        match &r.op {
            RelationOpKind::ReverseEdge { index, .. } => index.as_str(),
            _ => &r.from_path,
        }
    }
    fn read_partition(from_path: &str, shard_count: u32) -> u32 {
        shard_for_execution(from_path, shard_count)
    }

    /// ⚠ The ENGINE assigns the ordering key, not this crate.
    ///
    /// `ehdb-l0`'s `Dataset` contract requires records appended in **ascending
    /// `sort_key` order within a partition** — the sparse index, MinMax pruning and
    /// merge all rely on it. A hand-rolled counter cannot satisfy that across a
    /// process restart: it would begin again from its initial value and append
    /// *below* the existing maximum, which the engine accepts without erroring.
    ///
    /// The engine's `global_sequence` is recovered from the manifest on `open`
    /// (`manifest.max_sequence()`), so it is monotonic across reopens by
    /// construction. That is why every write here goes through
    /// `append_writer_assigned` rather than `append_record`.
    fn assign_sort_key(mut record: RelationOp, writer_seq: u64) -> RelationOp {
        record.op_seq = writer_seq;
        record
    }
}

// ---------------------------------------------------------------------------
// c4 — resource types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeOp {
    pub op_seq: u64,
    /// Index + partition dimension. Lowercase, like the type's own name.
    pub name: String,
    pub declared: ResourceType,
}

#[derive(Debug, Clone, Copy)]
pub struct TypeDataset;

impl Dataset for TypeDataset {
    type Record = TypeOp;
    const NAME: &'static str = DATASET_C4_TYPE;

    fn sort_key(r: &TypeOp) -> u64 {
        r.op_seq
    }
    fn partition(r: &TypeOp, shard_count: u32) -> u32 {
        shard_for_execution(&r.name, shard_count)
    }
    fn index_key(r: &TypeOp) -> &str {
        &r.name
    }
    fn read_partition(name: &str, shard_count: u32) -> u32 {
        shard_for_execution(name, shard_count)
    }

    /// ⚠ The ENGINE assigns the ordering key, not this crate.
    ///
    /// `ehdb-l0`'s `Dataset` contract requires records appended in **ascending
    /// `sort_key` order within a partition** — the sparse index, MinMax pruning and
    /// merge all rely on it. A hand-rolled counter cannot satisfy that across a
    /// process restart: it would begin again from its initial value and append
    /// *below* the existing maximum, which the engine accepts without erroring.
    ///
    /// The engine's `global_sequence` is recovered from the manifest on `open`
    /// (`manifest.max_sequence()`), so it is monotonic across reopens by
    /// construction. That is why every write here goes through
    /// `append_writer_assigned` rather than `append_record`.
    fn assign_sort_key(mut record: TypeOp, writer_seq: u64) -> TypeOp {
        record.op_seq = writer_seq;
        record
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_dataset_names_are_distinct_and_outside_ehdbs_d_space() {
        let names = [
            EntityDataset::NAME,
            RelationDataset::NAME,
            AttributeDataset::NAME,
            TypeDataset::NAME,
        ];
        let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), 4, "dataset names must be distinct: {names:?}");
        for n in names {
            assert!(
                n.starts_with('c'),
                "{n} must live in the catalog's own `c…` namespace"
            );
            assert!(
                !n.starts_with('d'),
                "{n} would collide with ehdb-l0's D-number space, which is fully \
                 allocated (d1…d10, and d7_catalog already exists)"
            );
        }
    }

    #[test]
    fn an_edge_key_distinguishes_a_pinned_target_from_an_unpinned_one() {
        use catalog_model::{Provenance, RelationKind};
        let from = EntityRef::pinned("playbook", "parent", 1);
        let unpinned = RelationOp {
            op_seq: 1,
            from_path: "parent".into(),
            op: RelationOpKind::Asserted(Box::new(Relation::new(
                from.clone(),
                EntityRef::latest("playbook", "child"),
                RelationKind::Invokes,
                Provenance::Declared,
            ))),
        };
        let pinned = RelationOp {
            op_seq: 2,
            from_path: "parent".into(),
            op: RelationOpKind::Asserted(Box::new(Relation::new(
                from,
                EntityRef::pinned("playbook", "child", 3),
                RelationKind::Invokes,
                Provenance::Declared,
            ))),
        };
        assert_ne!(
            unpinned.edge_key(),
            pinned.edge_key(),
            "an unpinned edge and a pinned one are different claims; sharing a key \
             would let retracting one silently retract the other"
        );
    }
}
