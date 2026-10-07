//! **noetl's internal object types, as its own schema declares them.**
//!
//! # Discovered, not invented
//!
//! The authority is `noetl.resource` in `repos/server/db/ddl/postgres/schema_ddl.sql`,
//! which seeds five rows and is the FK target of `noetl.catalog.kind`:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS noetl.resource (name VARCHAR PRIMARY KEY, meta JSONB);
//! INSERT INTO noetl.resource (name, meta) VALUES
//!   ('playbook',   '{"executable":true, "catalog":true}'),
//!   ('credential', '{"executable":false,"catalog":true}'),
//!   ('mcp',        '{"executable":false,"catalog":true}'),
//!   ('agent',      '{"executable":true, "catalog":true}'),
//!   ('memory',     '{"executable":false,"catalog":true}');
//! ...
//! kind VARCHAR NOT NULL REFERENCES noetl.resource(name)
//! ```
//!
//! The `executable` / `catalog` flags map onto [`ResourceType`]'s `executable` /
//! `catalogued` exactly, which is why those two fields exist.
//!
//! # ⚠ `subscription` is the sixth, and its status is inconsistent upstream
//!
//! `noetl/server` validates `kind: Subscription` as "a first-class catalog type"
//! (`services/catalog.rs`, `validate_subscription_spec`), and the travel repo registers
//! such documents — but **`subscription` is NOT seeded in `noetl.resource`**, so it has
//! no row for `noetl.catalog.kind` to reference. It is included here because the running
//! platform treats it as a type; the upstream gap is reported rather than papered over.
//!
//! # ⚠ A resource type is NOT a tool kind
//!
//! Conflating them is easy and wrong. `noetl_orchestrate_core::playbook::ToolKind` has
//! **25** variants (`Http`, `Postgres`, `Python`, `Playbook`, `Wasm`, …) and governs
//! `tool.kind` *inside a workflow step*. `agent`, `mcp`, `provider` and `result_fetch`
//! are **rejected** as tool kinds (noetl/ai-meta#256) while `agent` and `mcp` are
//! perfectly good *resource types*. This module is about resource types only.

use crate::ResourceType;

/// One of noetl's internal object types, with the flags its own schema declares.
struct Seed {
    name: &'static str,
    executable: bool,
    catalogued: bool,
    description: &'static str,
}

/// The five types `noetl.resource` seeds, plus `subscription`.
const SEEDS: [Seed; 6] = [
    Seed {
        name: "playbook",
        executable: true,
        catalogued: true,
        description: "Executable NoETL workflow definition",
    },
    Seed {
        name: "credential",
        executable: false,
        catalogued: true,
        description: "Credential or secret reference metadata",
    },
    Seed {
        name: "mcp",
        executable: false,
        catalogued: true,
        description: "Model Context Protocol server/tool provider",
    },
    Seed {
        name: "agent",
        executable: true,
        catalogued: true,
        description: "Agent-as-playbook or agent capability resource",
    },
    Seed {
        name: "memory",
        executable: false,
        catalogued: true,
        description: "AI memory, knowledge, or coordination artifact",
    },
    Seed {
        // Not in noetl.resource — see the module note.
        name: "subscription",
        executable: true,
        catalogued: true,
        description: "Continuous message-source subscription (first-class in the server, \
                      absent from noetl.resource)",
    },
];

/// noetl's internal object types, as [`ResourceType`] values.
///
/// ⚠ This is a **convenience seed, not a gate.** The catalog accepts any resource type;
/// nothing here restricts what can be catalogued. Its purpose is that the six types
/// noetl already has arrive with the right `executable` / `catalogued` flags instead of
/// a guess. A seventh type needs no change to this list to be catalogued — it only
/// misses the curated flags, which is why `catalog-ingest` reports a type it has never
/// seen rather than rejecting it.
pub fn noetl_resource_types() -> Vec<ResourceType> {
    SEEDS
        .iter()
        .map(|s| ResourceType::new(s.name, s.executable, s.catalogued))
        .collect()
}

/// The declared description for one of noetl's types, when it is one of them.
pub fn noetl_type_description(name: &str) -> Option<&'static str> {
    let want = name.trim().to_lowercase();
    SEEDS.iter().find(|s| s.name == want).map(|s| s.description)
}

/// Whether `name` is one of noetl's own internal object types.
///
/// ⚠ Used for **reporting**, never for admission. A catalog that only accepts a
/// hard-coded list is not a generic catalog, which is the whole point of this work.
pub fn is_known_noetl_type(name: &str) -> bool {
    let want = name.trim().to_lowercase();
    SEEDS.iter().any(|s| s.name == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_six_discovered_types_carry_their_declared_flags() {
        let types = noetl_resource_types();
        println!("noetl internal object types: {}", types.len());
        assert_eq!(
            types.len(),
            6,
            "the discovered set is five seeded + subscription"
        );

        let by: std::collections::BTreeMap<&str, &ResourceType> =
            types.iter().map(|t| (t.name.as_str(), t)).collect();

        // ⚠ Set equality on the NAMES, not a count — a count of 6 could be the wrong 6.
        let mut names: Vec<&str> = by.keys().copied().collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "agent",
                "credential",
                "mcp",
                "memory",
                "playbook",
                "subscription"
            ]
        );

        // The flags come from noetl.resource's `meta`, not from a guess.
        assert!(by["playbook"].executable, "playbook is executable:true");
        assert!(by["agent"].executable, "agent is executable:true");
        assert!(
            !by["credential"].executable,
            "credential is executable:false"
        );
        assert!(!by["mcp"].executable, "mcp is executable:false");
        assert!(!by["memory"].executable, "memory is executable:false");
        for t in &types {
            assert!(t.catalogued, "every seeded type has catalog:true");
        }
    }

    /// Names are lowercase, because `noetl.catalog` holds BOTH spellings — prod has
    /// 875 `'Playbook'` and 650 `'playbook'` rows for one type, which is what made
    /// noetl/server#429 return 650 of 1,525 playbooks.
    #[test]
    fn names_are_lowercase_and_lookup_is_case_insensitive() {
        for t in noetl_resource_types() {
            assert_eq!(t.name, t.name.to_lowercase());
        }
        for spelling in ["playbook", "Playbook", "PLAYBOOK", "  PlayBook  "] {
            assert!(
                is_known_noetl_type(spelling),
                "{spelling:?} must be recognised"
            );
        }
    }

    /// ⚠ The known-set is for reporting, not admission. This asserts the intent: an
    /// unknown name is simply not *known*, and nothing here can reject it.
    #[test]
    fn an_unknown_type_is_merely_unknown_not_forbidden() {
        assert!(!is_known_noetl_type("dashboard"));
        assert!(noetl_type_description("dashboard").is_none());
        // And the function surface offers no rejection at all — there is no
        // `is_allowed`, deliberately.
    }

    /// A resource type is not a tool kind. `agent` and `mcp` are valid resource types
    /// and REJECTED tool kinds (noetl/ai-meta#256); conflating them would make the
    /// catalog refuse two of noetl's own object types.
    #[test]
    fn resource_types_are_not_tool_kinds() {
        for rejected_tool_kind in ["agent", "mcp"] {
            assert!(
                is_known_noetl_type(rejected_tool_kind),
                "{rejected_tool_kind} is a resource type even though it is not a tool kind"
            );
        }
    }
}
