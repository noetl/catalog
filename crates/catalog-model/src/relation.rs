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
// ⚠ NOT `Copy`: `References` carries a `ForeignKey` with owned columns. Dropping
// `Copy` is the honest consequence of making the FK first-class — a kind is no longer
// a bare tag. Callers clone it; the type is small and relations are not a hot loop.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// **A foreign key.** The reference a relational schema declares between two
    /// rows, carrying the semantics the constraint actually has.
    ///
    /// # Why this is a distinct kind rather than a reuse of `Requires`
    ///
    /// Measured: a probe expressing an adiona slice had to abuse `Requires` for every
    /// FK, and `relations_to("category/10")` then returned the self-referencing
    /// hierarchy parent **and** the `trip_category` M:N join as
    /// `[("category/11", "Requires"), ("trip/100", "Requires")]` — two semantically
    /// different references, indistinguishable in the answer. A reader asking "what
    /// references this row" got a list it could not interpret.
    ///
    /// # Why the payload, and not just a bare `References`
    ///
    /// The three fields are the ones a reader needs and cannot recover: whether the
    /// reference may be absent, which direction the multiplicity runs, and what
    /// happens to the referent on delete. `ON DELETE NO ACTION` and `ON DELETE
    /// CASCADE` are different claims about whether the target can be removed at all,
    /// which is exactly the question `relations_to` exists to answer.
    References(ForeignKey),
}

/// The semantics of one foreign key, as its DDL declared them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignKey {
    /// Whether the referencing column admits NULL — i.e. whether the reference is
    /// optional. `categories.master_category_id` is nullable (a root category has no
    /// parent); `categories.category_type_id` is `NOT NULL`.
    pub nullable: bool,
    /// Which way the multiplicity runs.
    pub cardinality: Cardinality,
    /// What the DDL says happens on delete of the referent.
    pub on_delete: ReferentialAction,
    /// The referencing column(s), in DDL order. A composite FK has more than one.
    pub columns: Vec<String>,
    /// The constraint name, when the DDL named it. adiona names most of its
    /// (`r_category_type_category_type_id`), which is the only handle an operator has
    /// when the database complains.
    pub constraint_name: Option<String>,
}

impl ForeignKey {
    /// The common case: a `NOT NULL` many-to-one with `ON DELETE NO ACTION`, which is
    /// what adiona declares almost everywhere.
    pub fn many_to_one(column: impl Into<String>) -> Self {
        Self {
            nullable: false,
            cardinality: Cardinality::ManyToOne,
            on_delete: ReferentialAction::NoAction,
            columns: vec![column.into()],
            constraint_name: None,
        }
    }

    /// Mark this reference optional (the column admits NULL).
    pub fn optional(mut self) -> Self {
        self.nullable = true;
        self
    }

    /// Record the DDL's constraint name.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.constraint_name = Some(name.into());
        self
    }

    /// Set the on-delete action.
    pub fn on_delete(mut self, action: ReferentialAction) -> Self {
        self.on_delete = action;
        self
    }
}

/// Which way a reference's multiplicity runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    OneToOne,
    /// The usual FK: many referencing rows, one referent.
    ManyToOne,
    OneToMany,
    /// Each side of a join table's pair of FKs, taken together.
    ManyToMany,
}

/// What a DDL says happens to a referencing row when its referent is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferentialAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl RelationKind {
    /// Every variant, with a representative payload for `References`.
    ///
    /// ⚠ This is for exhaustive iteration, so the `References` payload here is
    /// arbitrary. Anything that compares *kinds* must use [`Self::discriminant`], not
    /// equality against a member of this array.
    pub fn all() -> Vec<RelationKind> {
        vec![
            Self::Invokes,
            Self::DerivesFrom,
            Self::Supersedes,
            Self::Requires,
            Self::Annotates,
            Self::References(ForeignKey::many_to_one("id")),
        ]
    }

    /// The **kind label**, independent of any payload.
    ///
    /// ⚠⚠ This exists because the edge identity must not include FK metadata. The two
    /// places that key an edge by its kind used `format!("{:?}", kind)`, which for a
    /// data-carrying variant renders the whole payload — so the *same* foreign key
    /// re-asserted with `nullable` corrected would become a **different edge**, and a
    /// retraction naming it would not match the row it meant to remove. The edge is
    /// identified by (target, version, kind); the payload is a property *of* that edge.
    pub fn discriminant(&self) -> &'static str {
        match self {
            Self::Invokes => "invokes",
            Self::DerivesFrom => "derives_from",
            Self::Supersedes => "supersedes",
            Self::Requires => "requires",
            Self::Annotates => "annotates",
            Self::References(_) => "references",
        }
    }

    /// The foreign-key payload, when this is a `References`.
    pub fn foreign_key(&self) -> Option<&ForeignKey> {
        match self {
            Self::References(fk) => Some(fk),
            _ => None,
        }
    }
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
            RelationKind::all().len(),
            6,
            "all() must list every variant; a missing one would make this loop \
             silently cover less than it claims"
        );
        for k in RelationKind::all() {
            let r = rel(k.clone(), Provenance::Declared);
            let back: Relation =
                serde_json::from_str(&serde_json::to_string(&r).expect("ser")).expect("de");
            assert_eq!(back.kind, k, "{k:?} must survive a round trip");
        }
    }

    #[test]
    fn all_is_exhaustive_against_the_match() {
        // A total match: adding a variant without adding it to ALL fails to compile
        // here, rather than silently shrinking every guard that iterates ALL.
        for k in RelationKind::all() {
            let covered = match k {
                RelationKind::Invokes => true,
                RelationKind::DerivesFrom => true,
                RelationKind::Supersedes => true,
                RelationKind::Requires => true,
                RelationKind::Annotates => true,
                RelationKind::References(_) => true,
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
