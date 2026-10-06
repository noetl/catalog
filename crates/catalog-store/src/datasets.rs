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
    fn partition(r: &EntityOp, shard_count: u32) -> u32 {
        shard_for_execution(&r.path, shard_count)
    }
    fn index_key(r: &EntityOp) -> &str {
        &r.path
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AttributeOpKind {
    Set(Box<Attribute>),
    Unset { name: String },
}

impl AttributeOpKind {
    /// The attribute name this op concerns — the fold's grouping key.
    pub fn name(&self) -> &str {
        match self {
            Self::Set(a) => &a.name,
            Self::Unset { name } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributeOp {
    pub op_seq: u64,
    /// Index + partition dimension.
    pub path: String,
    pub op: AttributeOpKind,
}

#[derive(Debug, Clone, Copy)]
pub struct AttributeDataset;

impl Dataset for AttributeDataset {
    type Record = AttributeOp;
    const NAME: &'static str = DATASET_C3_ATTRIBUTE;

    fn sort_key(r: &AttributeOp) -> u64 {
        r.op_seq
    }
    fn partition(r: &AttributeOp, shard_count: u32) -> u32 {
        shard_for_execution(&r.path, shard_count)
    }
    fn index_key(r: &AttributeOp) -> &str {
        &r.path
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
    Retracted { to: Box<EntityRef>, kind: String },
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
                format!("{:?}", r.kind),
            ),
            RelationOpKind::Retracted { to, kind } => (to.path.clone(), to.version, kind.clone()),
        }
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
    fn partition(r: &RelationOp, shard_count: u32) -> u32 {
        shard_for_execution(&r.from_path, shard_count)
    }
    fn index_key(r: &RelationOp) -> &str {
        &r.from_path
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
