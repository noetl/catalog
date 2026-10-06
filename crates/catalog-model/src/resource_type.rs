//! Resource types — the type registry, as data rather than schema.
//!
//! Today's equivalent is the `noetl.resource` table, whose `name` column is the
//! foreign-key target of `noetl.catalog.kind`. Adding a resource type there means
//! editing a DDL seed or the server's startup code; here it is one appended
//! record. That difference is the whole point of the model, and
//! `design/catalog-model.md` §10 states it as an acceptance criterion.

use serde::{Deserialize, Serialize};

/// A kind of catalogued resource.
///
/// The six names permitted by today's FK are `playbook`, `subscription`,
/// `credential`, `mcp`, `agent` and `memory`. Only `playbook` and `subscription`
/// are registered in volume; `mcp` has two rows on prod. Nothing here hard-codes
/// that list — it is data, and a seventh name costs one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceType {
    /// The identity. Lowercase by construction — see [`ResourceType::new`].
    pub name: String,

    /// Whether `/api/execute` can take a resource of this type.
    ///
    /// Today this lives in `noetl.resource.meta.executable`. `playbook`,
    /// `subscription` and `agent` are executable; `credential`, `mcp` and
    /// `memory` are not.
    pub executable: bool,

    /// Whether resources of this type belong in the catalog at all.
    ///
    /// `credential` is the case that makes this field necessary rather than
    /// implied: it is seeded as a resource type, but the CLI *deliberately*
    /// diverts credentials to `POST /api/credentials` and `noetl.credential`.
    /// A type can therefore be known and not catalogued, and conflating the two
    /// would either lose the type or wrongly invite credentials into the catalog.
    pub catalogued: bool,

    /// Optional declared shape for this type's attributes.
    ///
    /// `None` means unconstrained, which is what every type is today. This exists
    /// so a type can later declare its own attribute contract without that
    /// contract living in code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute_schema: Option<serde_json::Value>,

    /// A more general type this one specializes.
    ///
    /// From the reference model's self-referencing taxonomy
    /// (`master_category_type_id`). It lets `agent` be a specialization of
    /// `playbook` — sharing its attributes and executability — without a new
    /// table and without duplicating the parent's declarations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supertype: Option<String>,
}

impl ResourceType {
    /// A resource type, with the name normalized to lowercase.
    ///
    /// ⚠ The normalization is not cosmetic. `noetl.catalog.kind` holds **both**
    /// `'Playbook'` (875 rows) and `'playbook'` (650 rows) on prod, because
    /// registration began lowercasing in 2026-06 and never backfilled. A
    /// case-sensitive comparison against that column returns a *partial* set
    /// rather than an empty one, so it reads as a working query. Normalizing at
    /// construction makes the mixed state unrepresentable here.
    pub fn new(name: impl AsRef<str>, executable: bool, catalogued: bool) -> Self {
        Self {
            name: name.as_ref().to_lowercase(),
            executable,
            catalogued,
            attribute_schema: None,
            supertype: None,
        }
    }

    /// Declare this type a specialization of `supertype`.
    pub fn specializing(mut self, supertype: impl AsRef<str>) -> Self {
        self.supertype = Some(supertype.as_ref().to_lowercase());
        self
    }

    /// Attach a declared attribute shape.
    pub fn with_attribute_schema(mut self, schema: serde_json::Value) -> Self {
        self.attribute_schema = Some(schema);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_lowercased_at_construction() {
        // The exact pair that is mixed on prod.
        assert_eq!(ResourceType::new("Playbook", true, true).name, "playbook");
        assert_eq!(ResourceType::new("playbook", true, true).name, "playbook");
        // And a shoutier spelling, so the test is about case folding rather than
        // about one known string.
        assert_eq!(
            ResourceType::new("SUBSCRIPTION", true, true).name,
            "subscription"
        );
    }

    #[test]
    fn two_spellings_of_one_type_are_the_same_value() {
        // The property that matters: the mixed-case state that exists on prod
        // cannot be represented as two distinct types here.
        assert_eq!(
            ResourceType::new("Playbook", true, true),
            ResourceType::new("playbook", true, true),
            "a type must not differ from itself by capitalisation — that is \
             exactly the prod state this model exists to make impossible"
        );
    }

    #[test]
    fn a_supertype_is_lowercased_too() {
        let agent = ResourceType::new("agent", true, true).specializing("Playbook");
        assert_eq!(agent.supertype.as_deref(), Some("playbook"));
    }

    #[test]
    fn catalogued_is_independent_of_executable() {
        // `credential` is the case that forces these to be separate fields: it is
        // a known resource type that is deliberately NOT catalogued.
        let cred = ResourceType::new("credential", false, false);
        assert!(!cred.executable);
        assert!(!cred.catalogued);

        // and `memory` is catalogued but not executable, so the two fields are
        // not merely inverses of each other.
        let mem = ResourceType::new("memory", false, true);
        assert!(!mem.executable);
        assert!(mem.catalogued);
    }

    #[test]
    fn absent_optionals_are_omitted_from_the_wire_form() {
        let t = ResourceType::new("playbook", true, true);
        let json = serde_json::to_string(&t).expect("serialize");
        assert!(
            !json.contains("attribute_schema") && !json.contains("supertype"),
            "absent optionals must be skipped, not emitted as null: {json}"
        );
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        // `deny_unknown_fields` is deliberate on this type — unlike EHDB's
        // `EventRecord`, which omits it so a rollback can read newer writes.
        // These records are read only by this crate today. ⚠ If that stops being
        // true, revisit before the second reader ships, not after.
        let err = serde_json::from_str::<ResourceType>(
            r#"{"name":"playbook","executable":true,"catalogued":true,"surprise":1}"#,
        );
        assert!(err.is_err(), "an unknown field must be rejected");
    }

    #[test]
    fn a_round_trip_preserves_every_field() {
        let t = ResourceType::new("agent", true, true)
            .specializing("playbook")
            .with_attribute_schema(serde_json::json!({"required": ["owner"]}));
        let back: ResourceType =
            serde_json::from_str(&serde_json::to_string(&t).expect("ser")).expect("de");
        assert_eq!(t, back);
    }
}
