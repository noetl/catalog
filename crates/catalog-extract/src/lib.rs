//! Extract catalog relations from a playbook source.
//!
//! # The gap this closes
//!
//! A playbook names a child by **path string** inside a step's `tool:` block:
//!
//! ```yaml
//! tool:
//! - name: save_profile
//!   kind: playbook
//!   path: fixtures/playbooks/playbook_composition/user_profile_scorer
//! ```
//!
//! Registration never reads that `path`. `noetl/server`'s `CatalogService::register`
//! extracts the parent's own path, kind, `workload:`, `workflow:` and
//! `metadata.labels`, walks the workflow tree **only** to validate `tool.kind`
//! strings, and discards the pairs. So a playbook can be registered referencing a
//! child that does not exist, and nothing says so until the step runs.
//!
//! This crate reads those references and turns them into
//! [`catalog_model::Relation`]s with [`Provenance::Extracted`], which is the honest
//! provenance: the source *says* the edge exists. Whether it was ever traversed is a
//! different claim, carried by `Observed`.
//!
//! # ⚠ `tool:` is written two ways and both must be read
//!
//! A mapping:
//!
//! ```yaml
//! tool:
//!   kind: playbook
//!   path: child
//! ```
//!
//! …and a sequence:
//!
//! ```yaml
//! tool:
//! - name: a
//!   kind: playbook
//!   path: child
//! ```
//!
//! Measured over the 166 `noetl/e2e` fixture files that carry a `tool:` key: **20**
//! use the sequence form only, **109** the mapping form only, and **37 use both in
//! one file**. A reader that handles one shape silently sees a fraction of the
//! references — which is exactly the defect
//! [noetl/ai-meta#432](https://github.com/noetl/ai-meta/issues/432) found in the
//! server's own `collect_tool_kinds`: it read the mapping form only and missed
//! **219 of 1,178** tool kinds across **59** files.

#![forbid(unsafe_code)]

use catalog_model::{EntityRef, Provenance, Relation, RelationKind};

/// A reference found in a playbook source, before it becomes a [`Relation`].
///
/// Kept as its own type so a caller can see *what was found and where* without the
/// extractor having to know the parent's version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundReference {
    /// The step the reference sits on, for diagnostics.
    pub step: String,
    /// The tool item's own `name`, when the sequence form supplies one.
    pub tool: Option<String>,
    /// The `kind` the tool declared — `playbook`, `playbooks`, …
    pub tool_kind: String,
    /// The referenced path, verbatim. Case is preserved: a path is a name.
    pub path: String,
}

/// What `kind` values name another catalogued resource by path.
///
/// `playbooks` (plural) is a real variant of `ToolKind` in `orchestrate-core`, though
/// **0 of the e2e fixtures use it**. Included because the enum has it and a reference
/// through it would otherwise be invisible — an unused variant is still reachable.
const REFERENCING_KINDS: [&str; 2] = ["playbook", "playbooks"];

/// Every reference to another catalogued resource in this source.
///
/// Returns `Err` only when the document does not parse. A document with no references
/// yields an empty vec, which is distinct from a parse failure — a caller that
/// conflated them would treat a malformed playbook as one with no dependencies.
pub fn find_references(source: &str) -> Result<Vec<FoundReference>, serde_yaml::Error> {
    let doc: serde_yaml::Value = serde_yaml::from_str(source)?;
    let mut out = Vec::new();
    if let Some(workflow) = doc.get(serde_yaml::Value::from("workflow")) {
        walk(workflow, None, &mut out);
    }
    Ok(out)
}

/// Turn found references into relations from a known parent.
pub fn relations_from(
    parent: &EntityRef,
    found: &[FoundReference],
    extracted_at: i64,
) -> Vec<Relation> {
    found
        .iter()
        .map(|f| {
            Relation::new(
                parent.clone(),
                // ⚠ UNPINNED, deliberately. The source names a path and no version,
                // so that is genuinely an unpinned reference. Stamping the parent's
                // version here, or "latest at extraction time", would record a claim
                // the playbook never made — the same reasoning that gives EHDB's
                // `prev_event_id` no `Default`.
                EntityRef::latest("playbook", &f.path),
                RelationKind::Invokes,
                Provenance::Extracted { at: extracted_at },
            )
        })
        .collect()
}

/// Recursive walk, shaped after `noetl/server`'s `collect_tool_kinds` so the two agree
/// about what counts as a step — but reading **both** `tool:` shapes.
///
/// Walks the whole tree rather than only its top level: nested constructs (`iterator`,
/// `task_sequence`) carry steps of their own, and a top-level-only scan would report a
/// playbook with no dependencies while an inner step invoked three.
fn walk(node: &serde_yaml::Value, step: Option<String>, out: &mut Vec<FoundReference>) {
    match node {
        serde_yaml::Value::Mapping(map) => {
            let here = map
                .get(serde_yaml::Value::from("step"))
                .or_else(|| map.get(serde_yaml::Value::from("name")))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or(step);

            if let Some(tool) = map.get(serde_yaml::Value::from("tool")) {
                match tool {
                    serde_yaml::Value::Sequence(items) => {
                        for item in items {
                            collect_one(item, here.as_deref(), out);
                        }
                    }
                    _ => collect_one(tool, here.as_deref(), out),
                }
            }

            for (_k, v) in map {
                walk(v, here.clone(), out);
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for v in seq {
                walk(v, step.clone(), out);
            }
        }
        _ => {}
    }
}

/// One tool entry — mapping form, or one item of the sequence form.
fn collect_one(tool: &serde_yaml::Value, step: Option<&str>, out: &mut Vec<FoundReference>) {
    let Some(kind) = tool
        .get(serde_yaml::Value::from("kind"))
        .and_then(|v| v.as_str())
    else {
        return;
    };
    // Lowercased because `kind` is written both ways in the corpus, exactly as
    // `noetl.catalog.kind` is — and a case-sensitive comparison there returned a
    // PARTIAL result that read as a working query (noetl/ai-meta#429).
    let kind_lower = kind.to_lowercase();
    if !REFERENCING_KINDS.contains(&kind_lower.as_str()) {
        return;
    }
    let Some(path) = tool
        .get(serde_yaml::Value::from("path"))
        .and_then(|v| v.as_str())
    else {
        // A `kind: playbook` tool with no `path` is malformed, but this crate reports
        // what is there rather than judging it. Silently skipped so extraction never
        // fails a registration that the server itself accepts.
        return;
    };
    out.push(FoundReference {
        step: step.unwrap_or("<unnamed>").to_string(),
        tool: tool
            .get(serde_yaml::Value::from("name"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        tool_kind: kind_lower,
        path: path.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mapping_form_reference_is_found() {
        let src =
            "workflow:\n  - step: s\n    tool:\n      kind: playbook\n      path: child/one\n";
        let got = find_references(src).expect("parse");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "child/one");
        assert_eq!(got[0].step, "s");
        assert_eq!(got[0].tool, None, "the mapping form carries no tool name");
    }

    /// ⚠ The shape the server's own walker could not see.
    #[test]
    fn a_sequence_form_reference_is_found() {
        let src = "workflow:\n  - step: s\n    tool:\n    - name: t1\n      kind: playbook\n      path: child/two\n";
        let got = find_references(src).expect("parse");
        assert_eq!(
            got.len(),
            1,
            "the sequence form must be read; this is the shape noetl/ai-meta#432 found \
             invisible to the server's walker: {got:?}"
        );
        assert_eq!(got[0].path, "child/two");
        assert_eq!(got[0].tool.as_deref(), Some("t1"));
    }

    /// 37 of the 166 fixtures mix both shapes in one file, so this is the common case.
    #[test]
    fn a_file_mixing_both_shapes_yields_every_reference() {
        let src = "workflow:\n\
                   \x20 - step: m\n\
                   \x20   tool:\n\
                   \x20     kind: playbook\n\
                   \x20     path: child/a\n\
                   \x20 - step: l\n\
                   \x20   tool:\n\
                   \x20   - name: t1\n\
                   \x20     kind: playbook\n\
                   \x20     path: child/b\n\
                   \x20   - name: t2\n\
                   \x20     kind: playbook\n\
                   \x20     path: child/c\n";
        let got = find_references(src).expect("parse");
        let paths: Vec<&str> = got.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            got.len(),
            3,
            "one mapping-form plus two sequence-form references = 3, got {paths:?}"
        );
        assert!(
            paths.contains(&"child/a") && paths.contains(&"child/b") && paths.contains(&"child/c")
        );
    }

    /// A nested construct must not hide behind a clean top level.
    #[test]
    fn a_reference_inside_a_nested_construct_is_found() {
        let src = "workflow:\n\
                   \x20 - step: outer\n\
                   \x20   tool:\n\
                   \x20     kind: iterator\n\
                   \x20     task:\n\
                   \x20       - step: inner\n\
                   \x20         tool:\n\
                   \x20           kind: playbook\n\
                   \x20           path: child/nested\n";
        let got = find_references(src).expect("parse");
        assert_eq!(got.len(), 1, "a nested reference must be found: {got:?}");
        assert_eq!(got[0].path, "child/nested");
        assert_eq!(
            got[0].step, "inner",
            "the INNER step must be named, not the outer"
        );
    }

    #[test]
    fn a_non_referencing_tool_kind_yields_nothing() {
        for kind in ["python", "http", "postgres", "noop"] {
            let src = format!("workflow:\n  - step: s\n    tool:\n      kind: {kind}\n      path: not/a/playbook\n");
            let got = find_references(&src).expect("parse");
            assert!(
                got.is_empty(),
                "kind {kind:?} does not reference a catalogued resource, even with a \
                 `path:` key — a `path` on an http tool is a URL path: {got:?}"
            );
        }
    }

    #[test]
    fn the_plural_playbooks_kind_is_also_recognised() {
        // 0 fixtures use it, but ToolKind has the variant, so a reference through it
        // would otherwise be invisible. An unused variant is still reachable.
        let src =
            "workflow:\n  - step: s\n    tool:\n      kind: playbooks\n      path: child/plural\n";
        assert_eq!(find_references(src).expect("parse").len(), 1);
    }

    #[test]
    fn a_capitalised_kind_is_recognised() {
        // `kind` is written both ways in the corpus, as it is in noetl.catalog.kind
        // where a case-sensitive comparison returned a PARTIAL result.
        for k in ["Playbook", "PLAYBOOK", "playbook"] {
            let src =
                format!("workflow:\n  - step: s\n    tool:\n      kind: {k}\n      path: c\n");
            assert_eq!(
                find_references(&src).expect("parse").len(),
                1,
                "kind {k:?} must be recognised"
            );
        }
    }

    #[test]
    fn a_malformed_reference_is_skipped_rather_than_failing() {
        // A `kind: playbook` with no `path`. The server accepts this document, so
        // extraction must not be the thing that rejects it.
        let src = "workflow:\n  - step: s\n    tool:\n      kind: playbook\n";
        assert!(find_references(src).expect("parse").is_empty());
    }

    #[test]
    fn an_unparseable_document_is_an_error_not_an_empty_result() {
        // The distinction matters: an empty result means "no dependencies", and a
        // malformed playbook must not be reported as having none.
        let got = find_references("workflow: [\n  - broken: {{{\n");
        assert!(got.is_err(), "a malformed document must surface as Err");
    }

    #[test]
    fn a_document_with_no_workflow_is_empty_and_not_an_error() {
        // `kind: Subscription` entries have a `spec:` and no `workflow:`.
        assert!(find_references("kind: Subscription\nspec: {}\n")
            .expect("parse")
            .is_empty());
    }

    #[test]
    fn relations_carry_extracted_provenance_and_an_unpinned_target() {
        let found = vec![FoundReference {
            step: "s".into(),
            tool: Some("t".into()),
            tool_kind: "playbook".into(),
            path: "child".into(),
        }];
        let parent = EntityRef::pinned("playbook", "parent", 7);
        let rels = relations_from(&parent, &found, 1_760_000_000_000_000);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].kind, RelationKind::Invokes);
        assert_eq!(
            rels[0].discovered_by,
            Provenance::Extracted {
                at: 1_760_000_000_000_000
            }
        );
        assert!(
            !rels[0].discovered_by.is_evidence_of_execution(),
            "an extracted edge is a claim the SOURCE makes; it is not evidence the \
             edge was ever traversed"
        );
        assert!(
            !rels[0].to_entity.is_pinned(),
            "the source named a path and no version, so the edge must stay unpinned"
        );
        assert!(
            rels[0].from_entity.is_pinned(),
            "the parent is a concrete version"
        );
    }
}
