//! EHDB-backed storage for the catalog model.
//!
//! The catalog is a model and an API; **EHDB is the database**. A write is an event
//! appended to an EHDB dataset, a read is a fold over that log. This crate is the
//! only place in the repo that knows EHDB exists — `catalog-model` stays pure, which
//! is what makes it testable without a store and is asserted by AC1.
//!
//! See [`design/catalog-model.md`][spec] for the whole design.
//!
//! [spec]: https://github.com/noetl/catalog/blob/main/design/catalog-model.md

#![forbid(unsafe_code)]

pub mod datasets;
pub mod metrics;
pub mod store;

pub use datasets::{
    AttributeDataset, AttributeOp, AttributeOpKind, EntityDataset, EntityOp, EntityOpKind,
    RelationDataset, RelationOp, RelationOpKind, TypeDataset, TypeOp, DATASET_C1_ENTITY,
    DATASET_C2_RELATION, DATASET_C3_ATTRIBUTE, DATASET_C4_TYPE,
};
pub use store::{CatalogStore, PartCounts, Registered, StoreConfig, Ticked};

use std::collections::BTreeMap;

/// Fold a run of ops into "the latest op per sub-key", in `op_seq` order.
///
/// # ⚠ Why this exists instead of `read_index_after(key, 0).last()`
///
/// That one-liner is the pattern every in-tree EHDB store uses — `ProjectionStore`
/// (D3), `RuntimeStore` (D8), `CatalogStore` (D7) — and copying it here would be
/// silently wrong for three of this crate's four datasets.
///
/// It is correct only when **the index key uniquely identifies the thing being
/// folded**. For D3 it does: one `execution_id`, one projection. For this crate it
/// does not:
///
/// | dataset | index key | what actually coexists under it |
/// | :-- | :-- | :-- |
/// | `c1_catalog_entity` | `path` | **every version** of that path |
/// | `c3_catalog_attribute` | `path` | **every attribute name** on it |
/// | `c2_catalog_relation` | `from_path` | **every outgoing edge** |
/// | `c4_catalog_type` | `name` | one type — `.last()` would be fine here |
///
/// So `.last()` would return one version, one attribute and one edge, and *return
/// them successfully*. There is no error to notice. That is the failure mode AC4
/// exists to catch, and it is why AC4 is written as a positive control rather than
/// as an assertion that the happy path works.
///
/// EHDB guarantees ops arrive in ascending `sort_key` within a partition (the single
/// writer provides this), so taking the last op per group in iteration order is the
/// latest op per group. The grouping is `BTreeMap` so the result is deterministic
/// rather than hash-ordered — a fold whose output order varies between runs is a
/// comparator's nightmare.
pub fn fold_latest_by<R, K, F>(ops: Vec<R>, key_of: F) -> BTreeMap<K, R>
where
    K: Ord,
    F: Fn(&R) -> K,
{
    let mut out: BTreeMap<K, R> = BTreeMap::new();
    for op in ops {
        let k = key_of(&op);
        out.insert(k, op);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Op {
        seq: u64,
        name: &'static str,
        val: u32,
    }

    #[test]
    fn the_latest_op_wins_within_a_group() {
        let ops = vec![
            Op {
                seq: 1,
                name: "a",
                val: 1,
            },
            Op {
                seq: 2,
                name: "a",
                val: 2,
            },
            Op {
                seq: 3,
                name: "b",
                val: 9,
            },
        ];
        let folded = fold_latest_by(ops, |o| o.name);
        assert_eq!(folded.len(), 2, "two distinct names must yield two entries");
        assert_eq!(folded["a"].val, 2, "the later op must win within a group");
        assert_eq!(folded["b"].val, 9);
    }

    /// The whole point, stated as a contrast against the idiom it replaces.
    #[test]
    fn grouping_keeps_what_a_bare_last_would_discard() {
        let ops = vec![
            Op {
                seq: 1,
                name: "labels.team",
                val: 1,
            },
            Op {
                seq: 2,
                name: "labels.tier",
                val: 2,
            },
            Op {
                seq: 3,
                name: "exposed_in_ui",
                val: 3,
            },
        ];

        // What `read_index_after(key, 0).last()` does.
        let naive = ops.last().cloned();
        assert!(naive.is_some());
        assert_eq!(
            naive.map(|_| 1usize),
            Some(1),
            "the naive idiom yields exactly ONE record — and it is a valid-looking \
             record, which is why this is a silent defect rather than an error"
        );

        // What this function does.
        let folded = fold_latest_by(ops, |o| o.name);
        assert_eq!(folded.len(), 3, "all three attributes must survive");
        assert!(
            folded.len() > 1,
            "the grouped fold must keep strictly more than the naive one; if it does \
             not, this test no longer demonstrates the hazard and should be rewritten"
        );
    }

    #[test]
    fn the_output_order_is_deterministic() {
        // Two identical runs must agree. A fold whose order varies between runs makes
        // a parity comparator report differences that are not differences.
        let mk = || {
            vec![
                Op {
                    seq: 1,
                    name: "z",
                    val: 1,
                },
                Op {
                    seq: 2,
                    name: "a",
                    val: 2,
                },
                Op {
                    seq: 3,
                    name: "m",
                    val: 3,
                },
            ]
        };
        let a: Vec<&str> = fold_latest_by(mk(), |o| o.name).keys().copied().collect();
        let b: Vec<&str> = fold_latest_by(mk(), |o| o.name).keys().copied().collect();
        assert_eq!(a, b);
        assert_eq!(
            a,
            vec!["a", "m", "z"],
            "BTreeMap order, not insertion order"
        );
    }
}
