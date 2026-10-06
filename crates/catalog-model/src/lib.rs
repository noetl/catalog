//! Generalized entity/relation model for the NoETL catalog.
//!
//! # What this crate is, and is not
//!
//! **It is the model and API layer. It is not a datastore.** The NoETL catalog
//! is persisted through [EHDB](https://github.com/noetl/ehdb): a catalog write
//! becomes an EHDB event on the chain, and every catalog read is served from an
//! EHDB-derived projection. There is no second database, no embedded engine of
//! our own, and no external datastore — EHDB *is* the database, and this crate
//! inherits its invariants (single-root chain, append-only log,
//! self-sufficiency).
//!
//! That boundary is load-bearing rather than stylistic. A catalog that owned its
//! own storage would be a second source of truth for facts the event log already
//! holds, and the two would disagree — the failure mode NoETL has paid for
//! repeatedly (see `agents/rules/representation-drift.md` in `noetl/ai-meta`).
//!
//! # Design reference
//!
//! The design specification lives at [`design/catalog-model.md`][spec] in this
//! repository. Read it before adding a resource type: the point of the model is
//! that adding one is *data*, not a schema change, and that property is easy to
//! break by accident.
//!
//! [spec]: https://github.com/noetl/catalog/blob/main/design/catalog-model.md

#![forbid(unsafe_code)]

mod attribute;
mod entity;
mod relation;
mod resource_type;

pub use attribute::{Attribute, AttributeValue};
pub use entity::{Entity, EntityRef};
pub use relation::{Provenance, Relation, RelationKind};
pub use resource_type::ResourceType;

/// The crate's own version, surfaced so a running binary can report which
/// catalog model it was built against.
///
/// This exists because NoETL has twice diagnosed a problem against the wrong
/// build: a version a process reports about *itself* is evidence, whereas a
/// version read from a manifest or a note is a representation that drifts.
pub const MODEL_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_version_is_a_real_semver_triple() {
        let parts: Vec<&str> = MODEL_VERSION.split('.').collect();
        assert_eq!(
            parts.len(),
            3,
            "MODEL_VERSION must be a three-part semver, got {MODEL_VERSION:?}"
        );
        for p in &parts {
            assert!(
                !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()),
                "each MODEL_VERSION component must be numeric, got {MODEL_VERSION:?}"
            );
        }
    }
}
