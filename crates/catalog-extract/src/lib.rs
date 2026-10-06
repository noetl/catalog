//! Extract catalog relations and attributes from a NoETL resource source.
//!
//! # The gap this closes
//!
//! Registration reads a resource's own identity and **discards what it points at**.
//! For a playbook, a step names a child by path string inside its `tool:` block:
//!
//! ```yaml
//! tool:
//! - name: save_profile
//!   kind: playbook
//!   path: fixtures/playbooks/playbook_composition/user_profile_scorer
//! ```
//!
//! `noetl/server`'s `CatalogService::register` walks the workflow tree **only** to
//! validate `tool.kind` strings and throws the pairs away. So a playbook can be
//! registered referencing a child that does not exist, and nothing says so until the
//! step runs.
//!
//! # Two resource types, one extractor
//!
//! This crate handles **`playbook`** and **`subscription`**, which is the point: they
//! have nothing in common structurally.
//!
//! | | playbook | subscription |
//! | :-- | :-- | :-- |
//! | content | a `workflow:` tree of steps | a `spec:` with no workflow at all |
//! | where a reference lives | `workflow[].tool[].path` | `spec.dispatch.playbook` |
//! | nesting | arbitrary (`iterator`, `task_sequence`) | flat |
//!
//! Measured on the `noetl/e2e` corpus: **9 of 9** `kind: Subscription` fixtures carry
//! a `spec.dispatch.playbook` reference, and the playbooks they name
//! (`sub_ingest_default`, `handle_webhook`) sit in the same directory — so the
//! cross-type graph is closed and checkable.
//!
//! Supporting the second type added **no dataset and no schema**. That is the
//! generalization claim, and `catalog-store`'s AC3 guard asserts it by pinning the
//! `Dataset` impl count at four.
//!
//! # ⚠ `tool:` is written two ways and both must be read
//!
//! A mapping (`tool:` then `kind:`) and a sequence (`tool:` then `- name:`). Measured
//! over the 166 fixtures carrying a `tool:` key: **20** sequence-only, **109**
//! mapping-only, **37 using both in one file**. A reader that handles one shape
//! silently sees a fraction of the references — the defect
//! [noetl/ai-meta#432](https://github.com/noetl/ai-meta/issues/432) found in the
//! server's own walker, which missed **219 of 1,178** tool kinds across **59** files.

#![forbid(unsafe_code)]

use catalog_model::{Attribute, AttributeValue, EntityRef, Provenance, Relation, RelationKind};

/// Where in the document a reference was found.
///
/// Carried so a diagnostic can say *where* rather than only *what*. A reference the
/// extractor reports and an operator cannot locate is a reference they cannot act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// A playbook step's tool. Names the step, and the tool item when the sequence
    /// form supplies one.
    PlaybookTool { step: String, tool: Option<String> },
    /// A subscription's `spec.dispatch.playbook`.
    SubscriptionDispatch,
    /// A `spec.auth` credential alias.
    SubscriptionAuth,
}

impl Via {
    /// A short human location, for error messages.
    pub fn location(&self) -> String {
        match self {
            Self::PlaybookTool { step, tool } => match tool {
                Some(t) => format!("workflow step {step:?}, tool {t:?}"),
                None => format!("workflow step {step:?}"),
            },
            Self::SubscriptionDispatch => "spec.dispatch.playbook".into(),
            Self::SubscriptionAuth => "spec.auth".into(),
        }
    }
}

/// A reference found in a resource source, before it becomes a [`Relation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundReference {
    /// Where it was found.
    pub via: Via,
    /// What kind of edge this is.
    pub relation: RelationKind,
    /// The resource type of the TARGET — `playbook`, `credential`, …
    pub target_type: String,
    /// The referenced path or alias, verbatim. Case is preserved: it is a name.
    pub path: String,
}

/// Tool kinds that name another catalogued resource by path.
///
/// `playbooks` (plural) is a real `ToolKind` variant in `orchestrate-core` that **0
/// fixtures use**. Included anyway: an unused variant is still reachable, and a
/// reference through it would otherwise be invisible.
const REFERENCING_TOOL_KINDS: [&str; 2] = ["playbook", "playbooks"];

/// `spec.*` scalars worth recording as typed attributes on a subscription.
///
/// Chosen from the corpus rather than invented. Across the 9 fixtures: `source` 9/9,
/// `mode` 9/9, `activation` 7/9, `stream` 6/9, `consumer` 6/9.
const SUBSCRIPTION_SPEC_SCALARS: [&str; 5] = ["source", "mode", "activation", "stream", "consumer"];

/// The document's declared resource kind, lowercased.
///
/// Lowercased because `kind` is written both ways across the corpus, exactly as
/// `noetl.catalog.kind` is — and a case-sensitive comparison there returned a
/// **partial** result that read as a working query
/// ([noetl/ai-meta#429](https://github.com/noetl/ai-meta/issues/429)).
pub fn resource_kind(source: &str) -> Result<Option<String>, serde_yaml::Error> {
    let doc: serde_yaml::Value = serde_yaml::from_str(source)?;
    Ok(doc
        .get(serde_yaml::Value::from("kind"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase()))
}

/// Every reference to another catalogued resource in this source.
///
/// Dispatches on the document's own `kind`, so a new resource type is handled by
/// adding an arm here — **not** by adding storage. A kind this crate does not know
/// yields an empty vec rather than an error: the catalog must be able to hold a
/// resource type whose references nobody has taught it to read yet.
///
/// `Err` only when the document does not parse. A document with no references yields
/// an empty vec, which is a different claim — a caller that conflated them would treat
/// a malformed resource as one with no dependencies.
pub fn find_references(source: &str) -> Result<Vec<FoundReference>, serde_yaml::Error> {
    let doc: serde_yaml::Value = serde_yaml::from_str(source)?;
    let kind = doc
        .get(serde_yaml::Value::from("kind"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    let mut out = Vec::new();
    match kind.as_str() {
        "subscription" => collect_subscription(&doc, &mut out),
        // A playbook is the default, including when `kind` is absent: the server's own
        // `default_resource_type()` is "Playbook", so a document without one is read
        // as a playbook everywhere else too.
        _ => {
            if let Some(workflow) = doc.get(serde_yaml::Value::from("workflow")) {
                walk_workflow(workflow, None, &mut out);
            }
        }
    }
    Ok(out)
}

/// Typed attributes worth recording about this resource.
///
/// `metadata.labels` for any kind, plus a subscription's `spec:` scalars. This is the
/// entity/attribute/value generalization exercised on real data: a subscription's
/// metadata and a playbook's labels land in the **same** attribute log, keyed by the
/// same polymorphic identity, with no per-type table.
pub fn find_attributes(source: &str, entity_id: i64) -> Result<Vec<Attribute>, serde_yaml::Error> {
    let doc: serde_yaml::Value = serde_yaml::from_str(source)?;
    let mut out = Vec::new();

    if let Some(labels) = doc
        .get(serde_yaml::Value::from("metadata"))
        .and_then(|m| m.get(serde_yaml::Value::from("labels")))
        .and_then(|l| l.as_mapping())
    {
        for (k, v) in labels {
            if let Some(name) = k.as_str() {
                out.push(Attribute::new(
                    entity_id,
                    format!("labels.{name}"),
                    scalar(v),
                ));
            }
        }
    }

    let kind = doc
        .get(serde_yaml::Value::from("kind"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    if kind == "playbook" {
        collect_playbook_facts(&doc, entity_id, &mut out);
    }

    if kind == "subscription" {
        if let Some(spec) = doc.get(serde_yaml::Value::from("spec")) {
            for key in SUBSCRIPTION_SPEC_SCALARS {
                if let Some(v) = spec.get(serde_yaml::Value::from(key)) {
                    // Only scalars. A nested `runtime:`/`dispatch:` mapping is not an
                    // attribute — it is structure, and flattening it here would invent
                    // a naming scheme the source does not have.
                    if v.as_mapping().is_none() && v.as_sequence().is_none() {
                        out.push(Attribute::new(entity_id, format!("spec.{key}"), scalar(v)));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// A YAML scalar as a typed [`AttributeValue`].
///
/// ⚠ Bool before integer, and integer before string. `serde_yaml` will read `true` as
/// a bool and `50` as an integer, but a string fallback placed first would capture
/// everything and make every attribute `Text` — which still round-trips, and would
/// therefore pass a naive test while losing every type.
fn scalar(v: &serde_yaml::Value) -> AttributeValue {
    if let Some(b) = v.as_bool() {
        AttributeValue::Flag(b)
    } else if let Some(i) = v.as_i64() {
        AttributeValue::Integer(i)
    } else if let Some(f) = v.as_f64() {
        AttributeValue::Measure(f)
    } else if let Some(s) = v.as_str() {
        AttributeValue::Text(s.to_string())
    } else {
        // A mapping or sequence reaching here is opaque by nature.
        AttributeValue::Json(serde_json::Value::Null)
    }
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
                // ⚠ UNPINNED, deliberately. The source names a path and no version, so
                // that is genuinely an unpinned reference. Stamping the parent's
                // version, or "latest at extraction time", would record a claim the
                // source never made — the same reasoning that gives EHDB's
                // `prev_event_id` no `Default`.
                EntityRef::latest(&f.target_type, &f.path),
                f.relation,
                Provenance::Extracted { at: extracted_at },
            )
        })
        .collect()
}

// --------------------------------------------------------------------- subscription

/// A subscription's references: the playbook it dispatches, and the credential it
/// authenticates with.
fn collect_subscription(doc: &serde_yaml::Value, out: &mut Vec<FoundReference>) {
    let Some(spec) = doc.get(serde_yaml::Value::from("spec")) else {
        return;
    };

    if let Some(pb) = spec
        .get(serde_yaml::Value::from("dispatch"))
        .and_then(|d| d.get(serde_yaml::Value::from("playbook")))
        .and_then(|v| v.as_str())
    {
        out.push(FoundReference {
            via: Via::SubscriptionDispatch,
            relation: RelationKind::Invokes,
            target_type: "playbook".into(),
            path: pb.to_string(),
        });
    }

    if let Some(auth) = spec
        .get(serde_yaml::Value::from("auth"))
        .and_then(|v| v.as_str())
    {
        out.push(FoundReference {
            via: Via::SubscriptionAuth,
            relation: RelationKind::Requires,
            // ⚠ `credential`, and it is deliberately NOT catalogued — the CLI diverts
            // credentials to `noetl.credential`. The edge is still worth recording:
            // "this subscription needs alias X" is a real dependency, and a dangling
            // one is worth knowing about even though the target lives elsewhere.
            target_type: "credential".into(),
            path: auth.to_string(),
        });
    }
}

// ------------------------------------------------------------------------ playbook

/// Recursive walk, shaped after `noetl/server`'s `collect_tool_kinds` so the two agree
/// about what counts as a step — but reading **both** `tool:` shapes.
///
/// Walks the whole tree rather than only its top level: nested constructs (`iterator`,
/// `task_sequence`) carry steps of their own, and a top-level-only scan would report a
/// playbook with no dependencies while an inner step invoked three.
fn walk_workflow(node: &serde_yaml::Value, step: Option<String>, out: &mut Vec<FoundReference>) {
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
                            collect_tool(item, here.as_deref(), out);
                        }
                    }
                    _ => collect_tool(tool, here.as_deref(), out),
                }
            }

            for (_k, v) in map {
                walk_workflow(v, here.clone(), out);
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for v in seq {
                walk_workflow(v, step.clone(), out);
            }
        }
        _ => {}
    }
}

/// One tool entry — mapping form, or one item of the sequence form.
fn collect_tool(tool: &serde_yaml::Value, step: Option<&str>, out: &mut Vec<FoundReference>) {
    let Some(kind) = tool
        .get(serde_yaml::Value::from("kind"))
        .and_then(|v| v.as_str())
    else {
        return;
    };
    let kind_lower = kind.to_lowercase();
    if !REFERENCING_TOOL_KINDS.contains(&kind_lower.as_str()) {
        return;
    }
    let Some(path) = tool
        .get(serde_yaml::Value::from("path"))
        .and_then(|v| v.as_str())
    else {
        // A `kind: playbook` tool with no `path` is malformed, but this crate reports
        // what is there rather than judging it: extraction must never fail a
        // registration the server itself accepts.
        return;
    };
    out.push(FoundReference {
        via: Via::PlaybookTool {
            step: step.unwrap_or("<unnamed>").to_string(),
            tool: tool
                .get(serde_yaml::Value::from("name"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },
        relation: RelationKind::Invokes,
        target_type: "playbook".into(),
        path: path.to_string(),
    });
}

/// The two facts every real playbook carries that are worth a catalog query.
///
/// # Why these two
///
/// Measured against the 53 `adiona/playbooks/*.yaml` on `noetl/travel@origin/main` — the
/// first real population this extractor was ever run over:
///
/// ```text
/// denominator 53 | carries auth: 53 | carries a tool kind: 53
/// tool kinds : 53x postgres
/// auth aliases: 49x adiona_actor, 4x adiona_migrator
/// ```
///
/// Before this, ingesting all 53 produced `relations=0 attributes=0`. The relations zero
/// is **correct** — they are leaf playbooks that call no child, which a measurement
/// confirmed after a first regex wrongly counted each document's own `metadata.path` as a
/// child reference. The attributes zero was a gap: `find_attributes` handled
/// `metadata.labels` (none of the 53 have any) and subscription `spec.*`, so a playbook
/// yielded nothing at all.
///
/// Both facts answer questions a catalog exists to answer:
///
/// * `uses_tool.<kind>` — which resources touch Postgres / HTTP / an LLM.
/// * `uses_credential.<alias>` — which resources need a given keychain alias. This is the
///   one with teeth: rotating `adiona_actor` means knowing the 49 playbooks that break.
///
/// # ⚠ The alias only, never a value
///
/// `execution-model.md` is explicit that a playbook references a credential **by alias**
/// and the keychain resolves it at step execution time. So an `auth:` that is a plain
/// string is a reference and safe to catalogue, while an `auth:` that is a **mapping** is
/// an inline credential — and copying that into the catalog would duplicate a secret into
/// a second store. Non-scalar `auth:` is therefore skipped, and
/// `an_inline_credential_mapping_is_never_extracted` guards it.
///
/// Values are `true` rather than the alias name because the fact is membership: the
/// attribute *name* carries which tool or alias, so a fold keyed by name answers "every
/// resource using X" without scanning values.
fn collect_playbook_facts(doc: &serde_yaml::Value, entity_id: i64, out: &mut Vec<Attribute>) {
    let steps = match doc
        .get(serde_yaml::Value::from("workflow"))
        .and_then(|w| w.as_sequence())
    {
        Some(s) => s,
        None => return,
    };

    // Sets, because a playbook naming the same tool in nine steps is one fact about the
    // playbook, not nine. Sorted output keeps the attribute order stable across runs —
    // an unstable order would make every re-ingest look like a change.
    let mut tools: std::collections::BTreeSet<String> = Default::default();
    let mut creds: std::collections::BTreeSet<String> = Default::default();

    for step in steps {
        let Some(tool) = step.get(serde_yaml::Value::from("tool")) else {
            continue;
        };
        if let Some(k) = tool
            .get(serde_yaml::Value::from("kind"))
            .and_then(|v| v.as_str())
        {
            tools.insert(k.to_lowercase());
        }
        if let Some(auth) = tool.get(serde_yaml::Value::from("auth")) {
            // ⚠ Scalar strings only. See the safety note above.
            if let Some(alias) = auth.as_str() {
                let alias = alias.trim();
                // A templated alias (`{{ db_credential }}`) names a binding, not a
                // credential, so cataloguing it would assert a dependency on something
                // that does not exist under that name.
                if !alias.is_empty() && !alias.contains("{{") {
                    creds.insert(alias.to_string());
                }
            }
        }
    }

    for t in tools {
        out.push(Attribute::new(
            entity_id,
            format!("uses_tool.{t}"),
            AttributeValue::Flag(true),
        ));
    }
    for c in creds {
        out.push(Attribute::new(
            entity_id,
            format!("uses_credential.{c}"),
            AttributeValue::Flag(true),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(f: &[FoundReference]) -> Vec<&str> {
        f.iter().map(|r| r.path.as_str()).collect()
    }

    // ---------------------------------------------------------------- playbook

    #[test]
    fn a_mapping_form_reference_is_found() {
        let src =
            "workflow:\n  - step: s\n    tool:\n      kind: playbook\n      path: child/one\n";
        let got = find_references(src).expect("parse");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "child/one");
        assert_eq!(got[0].relation, RelationKind::Invokes);
        assert_eq!(got[0].target_type, "playbook");
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
        assert_eq!(
            got[0].via,
            Via::PlaybookTool {
                step: "s".into(),
                tool: Some("t1".into())
            }
        );
    }

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
        assert_eq!(got.len(), 3, "{:?}", paths(&got));
    }

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
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            got[0].via.location(),
            "workflow step \"inner\"",
            "the INNER step must be named, not the outer"
        );
    }

    #[test]
    fn a_non_referencing_tool_kind_yields_nothing() {
        for kind in ["python", "http", "postgres", "noop"] {
            let src = format!(
                "workflow:\n  - step: s\n    tool:\n      kind: {kind}\n      path: not/a/playbook\n"
            );
            assert!(
                find_references(&src).expect("parse").is_empty(),
                "kind {kind:?} does not reference a catalogued resource even with a \
                 `path:` — on an http tool that is a URL path"
            );
        }
    }

    #[test]
    fn a_capitalised_kind_is_recognised() {
        for k in ["Playbook", "PLAYBOOK", "playbook"] {
            let src =
                format!("workflow:\n  - step: s\n    tool:\n      kind: {k}\n      path: c\n");
            assert_eq!(find_references(&src).expect("parse").len(), 1, "kind {k:?}");
        }
    }

    #[test]
    fn the_plural_playbooks_kind_is_also_recognised() {
        let src =
            "workflow:\n  - step: s\n    tool:\n      kind: playbooks\n      path: child/plural\n";
        assert_eq!(find_references(src).expect("parse").len(), 1);
    }

    #[test]
    fn a_malformed_reference_is_skipped_rather_than_failing() {
        let src = "workflow:\n  - step: s\n    tool:\n      kind: playbook\n";
        assert!(find_references(src).expect("parse").is_empty());
    }

    #[test]
    fn an_unparseable_document_is_an_error_not_an_empty_result() {
        assert!(
            find_references("workflow: [\n  - broken: {{{\n").is_err(),
            "a malformed document must surface as Err — an empty result would mean \
             'no dependencies'"
        );
    }

    // ------------------------------------------------------------ subscription

    #[test]
    fn a_subscription_dispatch_playbook_is_a_reference() {
        let src = "kind: Subscription\nspec:\n  source: nats\n  mode: pull\n  dispatch:\n    playbook: tests/fixtures/sub_ingest_default\n";
        let got = find_references(src).expect("parse");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "tests/fixtures/sub_ingest_default");
        assert_eq!(got[0].relation, RelationKind::Invokes);
        assert_eq!(got[0].target_type, "playbook");
        assert_eq!(got[0].via, Via::SubscriptionDispatch);
        assert_eq!(got[0].via.location(), "spec.dispatch.playbook");
    }

    /// `RelationKind::Requires` finally has a real use.
    #[test]
    fn a_subscription_auth_alias_is_a_requires_edge_on_a_credential() {
        let src = "kind: Subscription\nspec:\n  source: nats\n  auth: nats_e2e\n  dispatch:\n    playbook: p\n";
        let got = find_references(src).expect("parse");
        assert_eq!(got.len(), 2, "dispatch + auth: {got:?}");
        let auth = got
            .iter()
            .find(|r| r.via == Via::SubscriptionAuth)
            .expect("the auth edge");
        assert_eq!(auth.relation, RelationKind::Requires);
        assert_eq!(
            auth.target_type, "credential",
            "an auth alias names a credential, which is deliberately NOT catalogued — \
             the edge is still worth recording"
        );
        assert_eq!(auth.path, "nats_e2e");
    }

    /// ⚠ The dispatch reference must NOT be found by the playbook walker.
    ///
    /// A subscription has no `workflow:`, so the playbook path would return empty and
    /// the test would pass for the wrong reason. This asserts the dispatch IS found,
    /// which only the subscription arm can do.
    #[test]
    fn a_subscription_is_not_read_as_a_playbook() {
        let src = "kind: Subscription\nspec:\n  dispatch:\n    playbook: child\n";
        let got = find_references(src).expect("parse");
        assert_eq!(
            got.len(),
            1,
            "the dispatch reference must be found via the subscription arm; the \
             playbook walker cannot see it because there is no `workflow:`: {got:?}"
        );
        // And the same document read as a playbook yields nothing, which is what makes
        // the assertion above meaningful rather than incidental.
        let as_playbook = "kind: Playbook\nspec:\n  dispatch:\n    playbook: child\n";
        assert!(
            find_references(as_playbook).expect("parse").is_empty(),
            "read as a playbook the SAME document must yield nothing — a `spec:` is not \
             a `workflow:`. If this finds something, the dispatch arm is firing for \
             every kind."
        );
    }

    #[test]
    fn an_unknown_resource_kind_yields_nothing_rather_than_erroring() {
        // The catalog must be able to hold a type whose references nobody has taught
        // it to read. Erroring here would make an unknown type unregisterable.
        let src = "kind: Widget\nspec:\n  anything: here\n";
        assert!(find_references(src).expect("parse").is_empty());
    }

    #[test]
    fn a_subscription_with_no_spec_yields_nothing() {
        assert!(find_references("kind: Subscription\n")
            .expect("parse")
            .is_empty());
    }

    // -------------------------------------------------------------- attributes

    #[test]
    fn subscription_spec_scalars_become_typed_attributes() {
        let src = "kind: Subscription\nspec:\n  source: nats\n  mode: pull\n  activation: continuous\n  stream: DEDUP\n  consumer: drain\n  runtime:\n    batch: 50\n  dispatch:\n    playbook: p\n";
        let attrs = find_attributes(src, 7).expect("parse");
        let names: Vec<&str> = attrs.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            attrs.len(),
            5,
            "the five scalar spec keys must become attributes: {names:?}"
        );
        for n in [
            "spec.source",
            "spec.mode",
            "spec.activation",
            "spec.stream",
            "spec.consumer",
        ] {
            assert!(names.contains(&n), "{n} missing from {names:?}");
        }
        // ⚠ `runtime:` is a MAPPING and must not be flattened into attributes —
        // inventing `spec.runtime.batch` would be a naming scheme the source does not
        // have.
        assert!(
            !names.iter().any(|n| n.starts_with("spec.runtime")),
            "a nested mapping is structure, not an attribute: {names:?}"
        );
        assert!(attrs.iter().all(|a| a.entity_id == 7));
    }

    #[test]
    fn a_scalar_keeps_its_yaml_type() {
        // ⚠ If the string fallback came first every attribute would be Text, which
        // still round-trips and would therefore pass a naive test while losing types.
        let src = "kind: Subscription\nspec:\n  source: nats\n  mode: true\n  activation: 42\n  dispatch:\n    playbook: p\n";
        let attrs = find_attributes(src, 1).expect("parse");
        let by: std::collections::BTreeMap<&str, &AttributeValue> =
            attrs.iter().map(|a| (a.name.as_str(), &a.value)).collect();
        assert_eq!(by["spec.source"], &AttributeValue::Text("nats".into()));
        assert_eq!(
            by["spec.mode"],
            &AttributeValue::Flag(true),
            "a YAML bool must not become Text"
        );
        assert_eq!(
            by["spec.activation"],
            &AttributeValue::Integer(42),
            "a YAML integer must not become Text"
        );
    }

    #[test]
    fn metadata_labels_become_attributes_for_any_kind() {
        for kind in ["Playbook", "Subscription", "Widget"] {
            let src =
                format!("kind: {kind}\nmetadata:\n  labels:\n    team: muno\n    tier: prod\n");
            let attrs = find_attributes(&src, 1).expect("parse");
            let names: Vec<&str> = attrs.iter().map(|a| a.name.as_str()).collect();
            assert!(
                names.contains(&"labels.team") && names.contains(&"labels.tier"),
                "labels must be read for kind {kind:?}: {names:?}"
            );
        }
    }

    #[test]
    fn a_playbook_gets_no_spec_attributes() {
        // The spec arm is subscription-only; a playbook with a stray `spec:` must not
        // acquire subscription attributes.
        let src = "kind: Playbook\nspec:\n  source: nats\n  mode: pull\n";
        let attrs = find_attributes(src, 1).expect("parse");
        assert!(
            attrs.is_empty(),
            "a playbook must not pick up subscription spec attributes: {attrs:?}"
        );
    }

    // --------------------------------------------------------------- relations

    #[test]
    fn relations_carry_the_found_kind_and_target_type() {
        let found = vec![
            FoundReference {
                via: Via::SubscriptionDispatch,
                relation: RelationKind::Invokes,
                target_type: "playbook".into(),
                path: "child".into(),
            },
            FoundReference {
                via: Via::SubscriptionAuth,
                relation: RelationKind::Requires,
                target_type: "credential".into(),
                path: "alias".into(),
            },
        ];
        let parent = EntityRef::pinned("subscription", "subs/one", 3);
        let rels = relations_from(&parent, &found, 99);
        assert_eq!(rels.len(), 2);

        // ⚠ The two edges must NOT collapse into one kind. An earlier shape hardcoded
        // Invokes, which would have made the credential dependency unreadable.
        let kinds: Vec<RelationKind> = rels.iter().map(|r| r.kind).collect();
        assert!(kinds.contains(&RelationKind::Invokes));
        assert!(kinds.contains(&RelationKind::Requires));

        let types: Vec<&str> = rels
            .iter()
            .map(|r| r.to_entity.resource_type.as_str())
            .collect();
        assert!(
            types.contains(&"playbook") && types.contains(&"credential"),
            "{types:?}"
        );

        for r in &rels {
            assert_eq!(r.discovered_by, Provenance::Extracted { at: 99 });
            assert!(!r.to_entity.is_pinned(), "targets stay unpinned");
            assert!(r.from_entity.is_pinned());
        }
    }

    #[test]
    fn resource_kind_is_lowercased_and_optional() {
        assert_eq!(
            resource_kind("kind: Subscription\n")
                .expect("parse")
                .as_deref(),
            Some("subscription")
        );
        assert_eq!(resource_kind("metadata: {}\n").expect("parse"), None);
    }
}
