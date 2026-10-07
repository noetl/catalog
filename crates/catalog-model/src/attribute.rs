//! Attributes — typed metadata on an entity.
//!
//! This is the reference model's entity/attribute/value core, collapsed. adiona
//! splits it across an `attributes` definition table plus a per-entity-type value
//! table (`item_attributes`, `trip_attributes`) plus a content table
//! (`item_attribute_content`). Here one shape covers all of it, keyed to the
//! polymorphic entity identity.
//!
//! The typed variants mirror adiona's own typed value columns, which its stored
//! procedures expose as `attribute_value` / `attribute_text` / `attribute_measure`
//! / `attribute_flag` / `attribute_timestamp`.

use crate::EntityRef;
use serde::{Deserialize, Serialize};

/// A typed attribute value.
///
/// ⚠ `Json` is an escape hatch and it will be abused. It exists so today's
/// `meta` / `payload` / `layout` JSONB columns can be carried across without loss.
/// The rule, enforced by [`AttributeValue::is_queryable`] and its guard: anything
/// filtered or compared gets a real variant; `Json` is for opaque bodies only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AttributeValue {
    Text(String),
    Integer(i64),
    Measure(f64),
    Flag(bool),
    /// Epoch micros, UTC.
    ///
    /// ⚠ Micros and explicitly UTC, because today's `noetl.catalog.created_at` is
    /// `TIMESTAMP` **without** time zone and every read has to say
    /// `AT TIME ZONE 'UTC'` to mean anything. A naive local timestamp is a value
    /// whose meaning depends on who reads it.
    Timestamp(i64),
    /// A typed pointer to another catalogued entity.
    ///
    /// Added beyond the reference model because a NoETL catalog's most useful
    /// attribute *is* a pointer to another catalogued thing.
    Ref(EntityRef),
    /// Opaque structured data. See the warning on the enum.
    Json(serde_json::Value),
}

impl AttributeValue {
    /// The variant name, for diagnostics and for the guard below.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Integer(_) => "integer",
            Self::Measure(_) => "measure",
            Self::Flag(_) => "flag",
            Self::Timestamp(_) => "timestamp",
            Self::Ref(_) => "ref",
            Self::Json(_) => "json",
        }
    }

    /// Whether this value may be filtered or compared on.
    ///
    /// `Json` is not queryable by design. A query over an opaque blob is how a
    /// schema-less column becomes a schema nobody declared — and it is unindexable,
    /// so the cost arrives later and as a performance mystery rather than as an
    /// error.
    pub fn is_queryable(&self) -> bool {
        !matches!(self, Self::Json(_))
    }
}

/// One typed attribute on one entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribute {
    /// The entity this describes.
    pub entity_id: i64,
    /// e.g. `labels.team`, `exposed_in_ui`.
    pub name: String,
    pub value: AttributeValue,
    /// **The language this value is written in**, as an ISO code — `None` for a value
    /// that has no language.
    ///
    /// # Why this is a real dimension and not part of the name
    ///
    /// **24 of adiona's 58 tables are `_translate` / `_content`** tables. Dropping
    /// localization would make adiona a false worked example — nearly half its schema
    /// would be inexpressible, and the reference model would not be the thing the
    /// catalog is a generalization of.
    ///
    /// The alternative, encoding the language into the attribute name
    /// (`category_name@de`), was measured to "work" and is wrong: the language becomes
    /// unqueryable, `attributes()` returns one entry per language as if they were
    /// different attributes, and nothing can ask "which languages is this translated
    /// into" or fall back to the default.
    ///
    /// `None` is **not** the same as `Some("en")`: a playbook's `uses_tool.postgres`
    /// has no language at all, while an English label is a translation that happens to
    /// be English. Conflating them would make every noetl attribute pretend to be
    /// English.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

impl Attribute {
    /// A **language-neutral** attribute. Every noetl attribute is this: a playbook's
    /// `uses_tool.postgres` is not written in a language.
    pub fn new(entity_id: i64, name: impl Into<String>, value: AttributeValue) -> Self {
        Self {
            entity_id,
            name: name.into(),
            value,
            lang: None,
        }
    }

    /// A **localized** attribute — one translation of a value.
    ///
    /// The language code is lowercased, because `lang_code` arrives as `en`/`EN`/`En`
    /// across adiona's columns and two spellings of one language would be two
    /// translations. Same reasoning as the kind-casing bug in noetl/server#429.
    pub fn localized(
        entity_id: i64,
        name: impl Into<String>,
        value: AttributeValue,
        lang: impl AsRef<str>,
    ) -> Self {
        Self {
            entity_id,
            name: name.into(),
            value,
            lang: Some(lang.as_ref().trim().to_lowercase()),
        }
    }

    /// The fold identity of this attribute: `(name, lang)`.
    ///
    /// ⚠⚠ **Not just `name`.** `category_name` in `en` and in `de` are two values of
    /// one attribute, and folding on the name alone makes the second overwrite the
    /// first — silent data loss, with a successful-looking write. That is the exact
    /// shape `fold_latest_by`'s doc warns about, one level deeper.
    pub fn fold_key(&self) -> (String, Option<String>) {
        (self.name.clone(), self.lang.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_round_trips_and_keeps_its_discriminant() {
        let cases = vec![
            AttributeValue::Text("alpha".into()),
            AttributeValue::Integer(-7),
            AttributeValue::Measure(1.5),
            AttributeValue::Flag(true),
            AttributeValue::Timestamp(1_760_000_000_000_000),
            AttributeValue::Ref(EntityRef::pinned("playbook", "a/b", 2)),
            AttributeValue::Json(serde_json::json!({"k": [1, 2]})),
        ];

        // ⚠ Assert the population before asserting about it: a loop over an empty
        // vec passes every assertion inside it.
        assert_eq!(
            cases.len(),
            7,
            "every AttributeValue variant must be covered; add the new one here"
        );

        for c in &cases {
            let back: AttributeValue =
                serde_json::from_str(&serde_json::to_string(c).expect("ser")).expect("de");
            assert_eq!(&back, c, "{} must round trip", c.type_name());
        }
    }

    #[test]
    fn the_variants_are_not_interchangeable_on_the_wire() {
        // An externally-tagged-by-`type` representation means a Text("1") and an
        // Integer(1) are distinct documents. Without that, a numeric-looking string
        // would silently change type across a round trip.
        let t = serde_json::to_string(&AttributeValue::Text("1".into())).expect("ser");
        let i = serde_json::to_string(&AttributeValue::Integer(1)).expect("ser");
        assert_ne!(
            t, i,
            "Text(\"1\") and Integer(1) must not share a wire form"
        );
        assert!(t.contains("\"text\""), "{t}");
        assert!(i.contains("\"integer\""), "{i}");
    }

    #[test]
    fn only_json_is_unqueryable() {
        // Stated as a total match so a new variant cannot be added without a
        // deliberate decision about whether it is filterable.
        let queryable = [
            AttributeValue::Text("a".into()),
            AttributeValue::Integer(1),
            AttributeValue::Measure(1.0),
            AttributeValue::Flag(false),
            AttributeValue::Timestamp(0),
            AttributeValue::Ref(EntityRef::latest("playbook", "a")),
        ];
        assert_eq!(
            queryable.len(),
            6,
            "six of the seven variants are queryable; update this if a variant is added"
        );
        for v in &queryable {
            assert!(v.is_queryable(), "{} must be queryable", v.type_name());
        }
        assert!(
            !AttributeValue::Json(serde_json::json!({})).is_queryable(),
            "Json must not be queryable — a filter over an opaque blob is a schema \
             nobody declared, and it is unindexable"
        );
    }

    #[test]
    fn a_ref_attribute_preserves_pinnedness() {
        let a = Attribute::new(
            1,
            "invokes",
            AttributeValue::Ref(EntityRef::latest("playbook", "child")),
        );
        let back: Attribute =
            serde_json::from_str(&serde_json::to_string(&a).expect("ser")).expect("de");
        match back.value {
            AttributeValue::Ref(r) => assert!(
                !r.is_pinned(),
                "an unpinned ref must not acquire a version through serialisation"
            ),
            other => panic!("expected Ref, got {}", other.type_name()),
        }
    }

    #[test]
    fn many_attributes_coexist_on_one_entity() {
        // The shape AC4 protects. The storage fold is where this can actually be
        // broken — a latest-wins fold copied from EHDB's `ProjectionStore` keeps one
        // attribute per index key and looks correct. This test pins the model-level
        // expectation that the fold must satisfy; the storage-level positive control
        // lands with P2.
        let attrs = [
            Attribute::new(1, "labels.team", AttributeValue::Text("muno".into())),
            Attribute::new(1, "labels.tier", AttributeValue::Text("prod".into())),
            Attribute::new(1, "exposed_in_ui", AttributeValue::Flag(true)),
        ];
        let names: std::collections::BTreeSet<&str> =
            attrs.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names.len(),
            3,
            "three distinct attribute names on one entity must stay three"
        );
        assert!(attrs.iter().all(|a| a.entity_id == 1));
    }

    #[test]
    fn an_unknown_field_on_an_attribute_is_rejected() {
        let r = serde_json::from_str::<Attribute>(
            r#"{"entity_id":1,"name":"x","value":{"type":"flag","value":true},"extra":1}"#,
        );
        assert!(r.is_err(), "an unknown field must be rejected");
    }
}
