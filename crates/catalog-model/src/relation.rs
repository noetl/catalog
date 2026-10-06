//! Relations — the edge between two catalogued entities.
//!
//! This is the capability the current catalog does not have at all. A playbook
//! names a child by path string inside a step's `tool:` block:
//!
//! ```yaml
//! tool:
//! - name: save_profile
//!   kind: playbook
//!   path: fixtures/playbooks/playbook_composition/user_profile_scorer
//! ```
//!
//! Registration never reads that `path`. It extracts the parent's own path, kind,
//! `workload:` and `workflow:` blocks and `metadata.labels`, walks the workflow tree
//! *only* to validate `tool.kind` strings, and discards the pairs. So a playbook can
//! be registered referencing a child that does not exist, and nothing says so until
//! the step runs.
//!
//! The only relation recorded anywhere today is execution-level —
//! `noetl.execution.parent_execution_id`, nullable and with no foreign key. It
//! answers "which runs were children of this run" but not "which playbooks reference
//! this playbook", which needs re-parsing every stored body.

use crate::EntityRef;
use serde::{Deserialize, Serialize};

/// What kind of edge this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// A step with `kind: playbook` names another resource by path.
    Invokes,
    /// Lineage. Generalizes `noetl.registry.lineage`, which exists today as an
    /// unvalidated, unindexed JSONB array with no reader that traverses it.
    DerivesFrom,
    /// Version N replaces version N-1 at the same path.
    Supersedes,
    /// A declared dependency — a credential alias, a required tool kind.
    Requires,
    /// A memory or documentation entity describing another entity.
    Annotates,
}

impl RelationKind {
    /// Every variant. For exhaustive iteration in guards.
    pub const ALL: [RelationKind; 5] = [
        Self::Invokes,
        Self::DerivesFrom,
        Self::Supersedes,
        Self::Requires,
        Self::Annotates,
    ];
}

/// How we know a relation exists.
///
/// ⚠ The three are different claims and merging them loses the distinction.
/// `Extracted` says *the source says so*; `Observed` says *it actually happened*.
/// Treating a declared edge as an observed one is the existence-vs-reachability
/// conflation that has produced a string of false-clean readings in this codebase:
/// a thing can be declared, registered and documented, and never run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Provenance {
    /// Stated in the resource's own source, as a first-class declaration.
    Declared,
    /// Parsed out of the stored body at registration. `at` is epoch micros UTC.
    Extracted { at: i64 },
    /// Inferred from a real execution.
    Observed { execution_id: i64 },
}

impl Provenance {
    /// Whether this edge is known to have actually been traversed.
    ///
    /// Only `Observed` qualifies. A declared or extracted edge is a claim about
    /// intent; it may name a resource that does not exist.
    pub fn is_evidence_of_execution(&self) -> bool {
        matches!(self, Self::Observed { .. })
    }
}

/// A directed edge between two catalogued entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub from_entity: EntityRef,
    pub to_entity: EntityRef,
    pub kind: RelationKind,
    pub discovered_by: Provenance,
}

impl Relation {
    pub fn new(
        from_entity: EntityRef,
        to_entity: EntityRef,
        kind: RelationKind,
        discovered_by: Provenance,
    ) -> Self {
        Self {
            from_entity,
            to_entity,
            kind,
            discovered_by,
        }
    }

    /// Whether the edge is self-referential.
    ///
    /// Not rejected at construction: `Supersedes` between two versions of one path
    /// is legitimately same-path, and a genuinely self-invoking playbook is a real
    /// (if alarming) thing a catalog should be able to record rather than refuse.
    /// Callers that care ask.
    pub fn is_self_edge(&self) -> bool {
        self.from_entity == self.to_entity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(kind: RelationKind, p: Provenance) -> Relation {
        Relation::new(
            EntityRef::pinned("playbook", "parent", 1),
            EntityRef::latest("playbook", "child"),
            kind,
            p,
        )
    }

    /// Acceptance criterion AC5 in `design/catalog-model.md`.
    #[test]
    fn extracted_and_observed_stay_distinguishable_after_a_round_trip() {
        let extracted = rel(RelationKind::Invokes, Provenance::Extracted { at: 1_700 });
        let observed = rel(
            RelationKind::Invokes,
            Provenance::Observed { execution_id: 42 },
        );

        assert_ne!(
            extracted, observed,
            "a declared edge and an executed one must not compare equal — that is \
             the existence-vs-reachability distinction the field exists to keep"
        );

        for r in [&extracted, &observed] {
            let back: Relation =
                serde_json::from_str(&serde_json::to_string(r).expect("ser")).expect("de");
            assert_eq!(&back, r);
        }

        assert!(!extracted.discovered_by.is_evidence_of_execution());
        assert!(observed.discovered_by.is_evidence_of_execution());
        assert!(!Provenance::Declared.is_evidence_of_execution());
    }

    #[test]
    fn observed_carries_the_execution_that_witnessed_it() {
        // Without the id, "observed" would be an unfalsifiable claim: nothing could
        // check it later or attribute it to a run.
        let r = rel(
            RelationKind::Invokes,
            Provenance::Observed {
                execution_id: 9_001,
            },
        );
        match r.discovered_by {
            Provenance::Observed { execution_id } => assert_eq!(execution_id, 9_001),
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn every_relation_kind_round_trips() {
        assert_eq!(
            RelationKind::ALL.len(),
            5,
            "ALL must list every variant; a missing one would make this loop \
             silently cover less than it claims"
        );
        for k in RelationKind::ALL {
            let r = rel(k, Provenance::Declared);
            let back: Relation =
                serde_json::from_str(&serde_json::to_string(&r).expect("ser")).expect("de");
            assert_eq!(back.kind, k, "{k:?} must survive a round trip");
        }
    }

    #[test]
    fn all_is_exhaustive_against_the_match() {
        // A total match: adding a variant without adding it to ALL fails to compile
        // here, rather than silently shrinking every guard that iterates ALL.
        for k in RelationKind::ALL {
            let covered = match k {
                RelationKind::Invokes => true,
                RelationKind::DerivesFrom => true,
                RelationKind::Supersedes => true,
                RelationKind::Requires => true,
                RelationKind::Annotates => true,
            };
            assert!(covered);
        }
    }

    #[test]
    fn an_unpinned_target_is_preserved_as_unpinned() {
        // The common real case: a `kind: playbook` step names a path and no version.
        let r = rel(RelationKind::Invokes, Provenance::Extracted { at: 1 });
        assert!(
            r.from_entity.is_pinned(),
            "the parent is a concrete version"
        );
        assert!(
            !r.to_entity.is_pinned(),
            "the child reference is genuinely unpinned and must stay so — pinning it \
             would record a claim the playbook did not make"
        );
    }

    #[test]
    fn a_self_edge_is_detectable_but_not_forbidden() {
        let same = EntityRef::pinned("playbook", "a", 1);
        let r = Relation::new(
            same.clone(),
            same,
            RelationKind::Invokes,
            Provenance::Declared,
        );
        assert!(r.is_self_edge());

        let other = rel(RelationKind::Invokes, Provenance::Declared);
        assert!(!other.is_self_edge());
    }

    #[test]
    fn an_unknown_field_on_a_relation_is_rejected() {
        let r = serde_json::from_str::<Relation>(
            r#"{"from_entity":{"resource_type":"playbook","path":"a"},
                "to_entity":{"resource_type":"playbook","path":"b"},
                "kind":"invokes","discovered_by":{"by":"declared"},"extra":1}"#,
        );
        assert!(r.is_err(), "an unknown field must be rejected");
    }
}
