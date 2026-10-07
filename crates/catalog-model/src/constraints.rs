//! **Attribute constraints, scoped to fields noetl actually has.**
//!
//! # The rule this follows
//!
//! A constraint here exists only where **noetl itself already enforces it**. The catalog
//! must not accept a value the platform rejects, and equally must not invent a rule the
//! platform does not have — a speculative constraint is worse than none, because it
//! refuses real data and gets disabled.
//!
//! Every value set below is copied from the authority, cited:
//!
//! | attribute | constraint | authority |
//! | :-- | :-- | :-- |
//! | `spec.source` | `pubsub` · `nats` · `kafka` · `webhook` | `server/src/services/catalog.rs` `SUBSCRIPTION_SOURCES` |
//! | `spec.mode` | `pull` · `push` | same file, `validate_subscription_spec` |
//! | `spec.activation` | `continuous` · `scheduled` | same file, `SUBSCRIPTION_ACTIVATIONS` |
//! | `uses_tool.<kind>` | one of **25** `ToolKind` variants | `server/orchestrate-core/src/playbook.rs` |
//!
//! ⚠ `uses_credential.<alias>` is deliberately **unconstrained**. An alias names a keychain
//! entry, the set is deployment-specific and not knowable from this repo, and guessing it
//! would reject a perfectly good credential. Absence of a constraint here is a decision,
//! not an omission.
//!
//! # ⚠⚠ The asymmetry: enforced on an explicit write, never on extraction
//!
//! `set_attribute` — where a caller *asserts* a fact — validates. Extraction during
//! `register_from_source` does **not**, and that is deliberate and already documented on
//! `register_from_source`: *"Extraction is additive information about a registration, never
//! a gate on it"*, because `noetl/server` accepts documents this extractor reads
//! imperfectly, and refusing to register on an extraction result would make the catalog
//! reject things the platform accepts.
//!
//! So a malformed document still registers, with whatever facts were legible. A caller
//! writing `spec.source = "rabbitmq"` by hand is refused, because that is a claim the
//! platform would reject.

use crate::AttributeValue;

/// Sources a subscription may declare. From `SUBSCRIPTION_SOURCES`.
pub const SUBSCRIPTION_SOURCES: [&str; 4] = ["pubsub", "nats", "kafka", "webhook"];

/// Subscription delivery modes. From `validate_subscription_spec`.
pub const SUBSCRIPTION_MODES: [&str; 2] = ["pull", "push"];

/// Pull-subscription activation modes. From `SUBSCRIPTION_ACTIVATIONS`.
pub const SUBSCRIPTION_ACTIVATIONS: [&str; 2] = ["continuous", "scheduled"];

/// The 25 `ToolKind` variants, in their serde `snake_case` spelling — the form that
/// appears in a playbook's `tool.kind`.
///
/// ⚠ Not a resource type. `agent` and `mcp` are valid *resource types* and **rejected**
/// tool kinds (noetl/ai-meta#447), so this list is deliberately not the resource-type list.
pub const TOOL_KINDS: [&str; 25] = [
    "http",
    "postgres",
    "duckdb",
    "ducklake",
    "python",
    "workbook",
    "playbook",
    "playbooks",
    "secrets",
    "iterator",
    "container",
    "script",
    "snowflake",
    "transfer",
    "snowflake_transfer",
    "gcs",
    "gateway",
    "nats",
    "shell",
    "artifact",
    "noop",
    "task_sequence",
    "rhai",
    "subscription",
    "wasm",
];

/// Why an attribute write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintViolation {
    pub attribute: String,
    pub reason: String,
}

impl std::fmt::Display for ConstraintViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "attribute {:?}: {}", self.attribute, self.reason)
    }
}

/// Validate one attribute against the constraints noetl actually has.
///
/// `Ok(())` for any attribute with no known constraint — the default is permissive,
/// because this catalog is generic and most attributes are free-form facts.
pub fn validate_attribute(name: &str, value: &AttributeValue) -> Result<(), ConstraintViolation> {
    let fail = |reason: String| {
        Err(ConstraintViolation {
            attribute: name.to_string(),
            reason,
        })
    };

    // --- enumerated string fields -------------------------------------------
    let enumerated: Option<(&[&str], &str)> = match name {
        "spec.source" => Some((&SUBSCRIPTION_SOURCES, "SUBSCRIPTION_SOURCES")),
        "spec.mode" => Some((&SUBSCRIPTION_MODES, "the mode set")),
        "spec.activation" => Some((&SUBSCRIPTION_ACTIVATIONS, "SUBSCRIPTION_ACTIVATIONS")),
        _ => None,
    };
    if let Some((allowed, source)) = enumerated {
        let AttributeValue::Text(s) = value else {
            return fail(format!(
                "must be Text (one of {allowed:?}, per {source}), got {value:?}"
            ));
        };
        if !allowed.contains(&s.as_str()) {
            return fail(format!(
                "{s:?} is not one of {allowed:?} — noetl/server rejects it at registration \
                 ({source}), so the catalog must not record it as a fact"
            ));
        }
        return Ok(());
    }

    // --- uses_tool.<kind> ---------------------------------------------------
    if let Some(kind) = name.strip_prefix("uses_tool.") {
        if !TOOL_KINDS.contains(&kind) {
            return fail(format!(
                "{kind:?} is not one of noetl's {} tool kinds — registering a playbook with \
                 it is rejected by validate_tool_kinds (noetl/ai-meta#256), so recording it \
                 as a used tool would assert something that cannot run",
                TOOL_KINDS.len()
            ));
        }
        // The value is membership; anything else is a modelling mistake.
        if !matches!(value, AttributeValue::Flag(_)) {
            return fail(format!("must be a Flag (membership), got {value:?}"));
        }
        return Ok(());
    }

    // ⚠ uses_credential.<alias> is intentionally NOT validated — see the module note.
    Ok(())
}

/// Every constraint this module knows, for an API that wants to publish them.
///
/// ⚠ Published deliberately: a caller refused by a constraint it cannot see has no way to
/// comply. The same reason the server quotes its valid set in a rejection.
pub fn described_constraints() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("spec.source", SUBSCRIPTION_SOURCES.to_vec()),
        ("spec.mode", SUBSCRIPTION_MODES.to_vec()),
        ("spec.activation", SUBSCRIPTION_ACTIVATIONS.to_vec()),
        ("uses_tool.<kind>", TOOL_KINDS.to_vec()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_subscription_sets_are_enforced() {
        for s in SUBSCRIPTION_SOURCES {
            assert!(validate_attribute("spec.source", &AttributeValue::Text(s.into())).is_ok());
        }
        let e = validate_attribute("spec.source", &AttributeValue::Text("rabbitmq".into()))
            .expect_err("rabbitmq is not a noetl subscription source");
        println!("refused: {e}");
        assert!(e.to_string().contains("rabbitmq") && e.to_string().contains("pubsub"));
        // ⚠ The error must offer the valid set, or the caller cannot comply.
    }

    #[test]
    fn all_25_tool_kinds_are_accepted_and_a_rejected_one_is_refused() {
        for k in TOOL_KINDS {
            let n = format!("uses_tool.{k}");
            assert!(
                validate_attribute(&n, &AttributeValue::Flag(true)).is_ok(),
                "{k} is a real tool kind and must be accepted"
            );
        }
        assert_eq!(TOOL_KINDS.len(), 25, "noetl has 25 tool kinds");

        // The four noetl/ai-meta#256 found live and rejected at registration.
        //
        // ⚠ My first draft of this loop used `unwrap_or_else(|e| panic!(...))`, which runs
        // its closure on Err — so it panicked exactly when the rejection WORKED. The code
        // was right and the test was inverted.
        for bad in ["agent", "mcp", "provider", "result_fetch"] {
            let n = format!("uses_tool.{bad}");
            let e = validate_attribute(&n, &AttributeValue::Flag(true)).expect_err(&format!(
                "{bad} is a rejected tool kind and must be refused"
            ));
            assert!(
                e.to_string().contains(bad) && e.to_string().contains("tool kinds"),
                "the rejection must quote the offending kind: {e}"
            );
        }
    }

    /// ⚠ `agent` and `mcp` are REJECTED tool kinds and VALID resource types. This asserts
    /// the constraint does not confuse the two namespaces (noetl/ai-meta#447).
    #[test]
    fn a_rejected_tool_kind_that_is_a_valid_resource_type_is_still_refused_as_a_tool() {
        for n in ["agent", "mcp"] {
            assert!(crate::is_known_noetl_type(n), "{n} IS a resource type");
            let e = validate_attribute(&format!("uses_tool.{n}"), &AttributeValue::Flag(true))
                .expect_err("but it is NOT a tool kind");
            println!("{n}: {e}");
            assert!(e.to_string().contains("tool kinds"));
        }
    }

    /// The default is permissive: a generic catalog mostly carries free-form facts.
    #[test]
    fn an_attribute_with_no_known_constraint_is_accepted() {
        for (n, v) in [
            ("labels.team", AttributeValue::Text("platform".into())),
            ("anything.at.all", AttributeValue::Integer(7)),
            // ⚠ Deliberately unconstrained — the alias set is deployment-specific.
            ("uses_credential.whatever_alias", AttributeValue::Flag(true)),
        ] {
            assert!(validate_attribute(n, &v).is_ok(), "{n} must be accepted");
        }
    }

    #[test]
    fn a_wrong_type_on_a_constrained_field_is_refused() {
        let e = validate_attribute("spec.mode", &AttributeValue::Flag(true))
            .expect_err("mode is an enumerated string");
        assert!(e.to_string().contains("must be Text"));
        let e = validate_attribute("uses_tool.postgres", &AttributeValue::Text("yes".into()))
            .expect_err("uses_tool is membership");
        assert!(e.to_string().contains("must be a Flag"));
    }

    /// The published set must match what is enforced, or a caller reads a list that does
    /// not govern anything.
    #[test]
    fn the_published_constraints_match_what_is_enforced() {
        let d = described_constraints();
        assert_eq!(d.len(), 4);
        for (name, allowed) in d {
            if name == "uses_tool.<kind>" {
                assert_eq!(allowed.len(), 25);
                continue;
            }
            // every published value must actually pass
            for v in allowed {
                assert!(
                    validate_attribute(name, &AttributeValue::Text(v.into())).is_ok(),
                    "{name} publishes {v:?} but rejects it"
                );
            }
        }
    }
}
