//! Entities — a catalogued resource at one version.
//!
//! The identity is `(resource_type, path, version)`. That triple is the
//! generalization the reference model does not have: adiona keys attributes and
//! relations to a *per-type* table (`item_attributes`, `trip_attributes`), so a new
//! entity type means new DDL. Here everything references one polymorphic identity,
//! so a new resource type is data.

use serde::{Deserialize, Serialize};

/// A pointer to a catalogued entity.
///
/// Used by relations and by `AttributeValue::Ref`. Carries the version, because a
/// reference to "whatever is latest" and a reference to a pinned version are
/// different claims and conflating them loses the distinction execution needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityRef {
    pub resource_type: String,
    pub path: String,
    /// `None` means "whichever version is latest at resolution time".
    ///
    /// ⚠ This is deliberately representable. A playbook step naming a child by
    /// path alone — which is what every `kind: playbook` step does today — is
    /// genuinely an unpinned reference, and recording it as pinned would be a
    /// fabrication. Execution pins it by writing a `catalog_snapshot` event; the
    /// catalog records what the source actually said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

impl EntityRef {
    /// An unpinned reference — "the latest at this path".
    pub fn latest(resource_type: impl AsRef<str>, path: impl AsRef<str>) -> Self {
        Self {
            resource_type: resource_type.as_ref().to_lowercase(),
            path: path.as_ref().to_string(),
            version: None,
        }
    }

    /// A reference pinned to one version.
    pub fn pinned(resource_type: impl AsRef<str>, path: impl AsRef<str>, version: u32) -> Self {
        Self {
            resource_type: resource_type.as_ref().to_lowercase(),
            path: path.as_ref().to_string(),
            version: Some(version),
        }
    }

    /// Whether this reference names one specific version.
    pub fn is_pinned(&self) -> bool {
        self.version.is_some()
    }
}

/// A catalogued resource at one version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    /// FK → [`crate::ResourceType::name`]. Lowercase.
    pub resource_type: String,

    /// The logical identity, e.g. `muno/playbooks/itinerary-planner`.
    ///
    /// Case is preserved: a path is a name chosen by its author, not an enum.
    pub path: String,

    /// Monotonic per path.
    ///
    /// ⚠ `u32`, not `i16`. Today's column is `SMALLSERIAL`, a ceiling of **32,767
    /// versions per path**, and `noetl catalog load` creates a new version of every
    /// entry on each run — so the ceiling is reachable by routine operation rather
    /// than by abuse. Widening costs nothing in an event-sourced store and cannot
    /// be done cheaply in the Postgres one.
    pub version: u32,

    /// Snowflake id, stable for this `(path, version)`.
    pub entity_id: i64,

    /// The raw source, e.g. the playbook YAML.
    ///
    /// ⚠ `Option`, and absent is the normal case in a listing. Measured on prod,
    /// `content` + `layout` were **97.4%** of a listing response, and once they
    /// were nulled `payload` became **98.1%**. A listing that carries bodies is
    /// not a listing. `None` here means "not fetched", never "empty" — which is
    /// why [`Entity::content_sha256`] is not optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Always present, even when [`Entity::content`] was projected away.
    ///
    /// This is the field that makes a body-less listing still verifiable: identity
    /// survives projection. Without it, "the content I have matches the catalog"
    /// could only be asked by refetching the body.
    pub content_sha256: String,

    /// Soft-delete marker, epoch micros UTC.
    ///
    /// An archived entity stops resolving **by path** but remains resolvable by
    /// explicit `entity_id`, so a historical version can be deliberately re-run.
    /// That is the semantics the current server already implements, kept here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
}

impl Entity {
    /// Whether this entity is soft-deleted.
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// A pinned reference to this exact entity.
    pub fn as_ref_pinned(&self) -> EntityRef {
        EntityRef::pinned(&self.resource_type, &self.path, self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(version: u32) -> Entity {
        Entity {
            resource_type: "playbook".into(),
            path: "muno/playbooks/itinerary-planner".into(),
            version,
            entity_id: 7,
            content: None,
            content_sha256: "a".repeat(64),
            archived_at: None,
        }
    }

    /// Acceptance criterion AC6 in `design/catalog-model.md`.
    #[test]
    fn a_version_above_the_smallint_ceiling_round_trips() {
        // 32,767 is i16::MAX — today's `SMALLSERIAL` ceiling. This test fails to
        // even compile against an `i16` field, which is the point.
        for v in [32_767u32, 32_768, 40_000, 100_000, u32::MAX] {
            let e = entity(v);
            let back: Entity =
                serde_json::from_str(&serde_json::to_string(&e).expect("ser")).expect("de");
            assert_eq!(back.version, v, "version {v} must survive a round trip");
        }
    }

    #[test]
    fn absent_content_is_distinguishable_from_empty_content() {
        // The distinction a listing depends on: "not fetched" is not "no body".
        let not_fetched = entity(1);
        let mut empty_body = entity(1);
        empty_body.content = Some(String::new());

        assert!(not_fetched.content.is_none());
        assert_eq!(empty_body.content.as_deref(), Some(""));
        assert_ne!(
            not_fetched, empty_body,
            "a projected-away body must not compare equal to a genuinely empty one"
        );
    }

    /// Acceptance criterion AC9.
    #[test]
    fn identity_survives_when_the_body_is_projected_away() {
        let e = entity(3);
        assert!(e.content.is_none(), "this fixture models a listing row");
        let json = serde_json::to_string(&e).expect("ser");
        assert!(
            !json.contains("\"content\""),
            "the body must be omitted: {json}"
        );
        for needle in ["path", "version", "content_sha256", "resource_type"] {
            assert!(
                json.contains(needle),
                "identity field {needle} must survive projection: {json}"
            );
        }
    }

    #[test]
    fn an_unpinned_reference_is_not_silently_pinned() {
        // A `kind: playbook` step naming only a path IS an unpinned reference.
        // Defaulting it to a version would fabricate a claim the source did not
        // make — the same shape as EHDB's `prev_event_id`, which deliberately has
        // no `Default` because a defaulted `None` would stamp a chain root.
        let r = EntityRef::latest("playbook", "a/b");
        assert!(!r.is_pinned());
        assert_eq!(r.version, None);

        let p = EntityRef::pinned("playbook", "a/b", 4);
        assert!(p.is_pinned());
        assert_ne!(
            r, p,
            "pinned and unpinned references must not compare equal"
        );
    }

    #[test]
    fn a_reference_lowercases_the_resource_type_but_preserves_the_path() {
        let r = EntityRef::latest("Playbook", "Muno/Playbooks/Itinerary-Planner");
        assert_eq!(r.resource_type, "playbook", "the type is an enum-like name");
        assert_eq!(
            r.path, "Muno/Playbooks/Itinerary-Planner",
            "the path is a name its author chose; folding its case would rename it"
        );
    }

    #[test]
    fn as_ref_pinned_names_this_exact_version() {
        let e = entity(12);
        assert_eq!(
            e.as_ref_pinned(),
            EntityRef::pinned("playbook", &e.path, 12)
        );
    }

    #[test]
    fn archival_is_observable() {
        let mut e = entity(1);
        assert!(!e.is_archived());
        e.archived_at = Some(1_760_000_000_000_000);
        assert!(e.is_archived());
    }
}
