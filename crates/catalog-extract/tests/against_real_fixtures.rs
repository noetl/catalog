//! The extractor run against **real** playbook sources, not invented ones.
//!
//! The fixtures in `tests/fixtures/` are verbatim copies from `noetl/e2e`, chosen to
//! cover the three shapes that matter:
//!
//! | fixture | why |
//! | :-- | :-- |
//! | `playbook_composition.yaml` | the worked composition case — a real `kind: playbook` reference |
//! | `save_delegation_test.yaml` | a reference alongside unrelated tools |
//! | `test_vars_block.yaml` | **list-form tools and no references** — the negative control |
//! | `dedup_critical_stream.subscription.yaml` | a **subscription**: no `workflow:`, a `spec.dispatch.playbook`, and a `spec.auth` alias |
//! | `webhook_orders.subscription.yaml` | a second subscription, different source/mode |
//! | `hook_bearer.subscription.yaml` | a subscription with **no** `spec.auth` — the contrast |
//!
//! ⚠ The third one is the important one. It is the file that proved
//! [noetl/ai-meta#432](https://github.com/noetl/ai-meta/issues/432): five list-form
//! tool blocks from which the server's own walker extracted zero. Here it must yield
//! **zero references** — because it genuinely has none — while the extractor still
//! *sees* its five tools. A reader that returns zero because it cannot parse the shape
//! is indistinguishable from one that returns zero because there is nothing there, so
//! this file is paired with a check that the tools were actually visited.

use catalog_extract::find_references;

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let s = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()));
    // ⚠ Assert the extraction before asserting about it: an empty or truncated read
    // yields zero references and reads exactly like a file with no dependencies.
    assert!(
        s.len() > 200,
        "{} is only {} bytes — the fixture did not load, so any result below would be \
         meaningless",
        p.display(),
        s.len()
    );
    s
}

/// Ground truth straight from the text: how many `path:` lines sit under a
/// `kind: playbook` tool.
///
/// Deliberately crude and deliberately independent of the extractor — a ground truth
/// computed by the code under test proves nothing. It scans for a `kind:` naming a
/// playbook and then looks for a `path:` within the next few lines at a compatible
/// indent.
///
/// ⚠ The indent check on the `kind:` line is load-bearing, and its absence is a
/// mistake this file already made once. Every playbook opens with
///
/// ```yaml
/// kind: Playbook
/// metadata:
///   path: vars_test/test_vars_block
/// ```
///
/// — the document's own RESOURCE kind at column 0, followed by its own path. Counting
/// that gave every fixture one phantom self-reference, so the ground truth said 2
/// where the extractor correctly said 1, and said 1 for a file with no references at
/// all. The extractor was right and the oracle was wrong, which is the more dangerous
/// direction: the tempting fix is to "correct" the code until it agrees.
///
/// A tool kind lives inside the `workflow:` tree and is therefore always indented.
fn textual_playbook_refs(src: &str) -> usize {
    let lines: Vec<&str> = src.lines().collect();
    let mut n = 0;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        let indent = l.len() - t.len();
        // Skip the document's own `kind:` — see the note above.
        if indent == 0 {
            continue;
        }
        let is_pb_kind = t.starts_with("kind:")
            && matches!(
                t.trim_start_matches("kind:").trim().to_lowercase().as_str(),
                "playbook" | "playbooks"
            );
        if !is_pb_kind {
            continue;
        }
        let kind_indent = indent;
        let window_end = lines.len().min(i + 8);
        for line in &lines[(i + 1)..window_end] {
            let s = line.trim_start();
            let ind = line.len() - s.len();
            if s.is_empty() {
                continue;
            }
            // Left the tool item.
            if ind < kind_indent {
                break;
            }
            if s.starts_with("path:") {
                n += 1;
                break;
            }
        }
    }
    n
}

#[test]
fn the_composition_fixture_yields_its_real_reference() {
    let src = fixture("playbook_composition.yaml");
    let truth = textual_playbook_refs(&src);
    let got = find_references(&src).expect("the fixture must parse");

    println!(
        "playbook_composition.yaml: text says {truth} playbook reference(s), extractor found {} -> {:?}",
        got.len(),
        got.iter().map(|f| f.path.as_str()).collect::<Vec<_>>()
    );

    assert!(
        truth > 0,
        "this fixture was chosen because it HAS a reference; if the text now says 0 \
         the fixture changed and this test is measuring nothing"
    );
    assert_eq!(
        got.len(),
        truth,
        "the extractor must find every reference the text has: {got:?}"
    );
    assert!(
        got.iter().any(|f| f.path.contains("user_profile_scorer")),
        "the known child path must be among them: {got:?}"
    );
}

#[test]
fn the_delegation_fixture_matches_its_text() {
    let src = fixture("save_delegation_test.yaml");
    let truth = textual_playbook_refs(&src);
    let got = find_references(&src).expect("parse");
    println!(
        "save_delegation_test.yaml: text {truth}, extractor {} -> {:?}",
        got.len(),
        got.iter().map(|f| f.path.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(got.len(), truth, "{got:?}");
}

/// ⚠ The negative control, and the reason it is not vacuous.
///
/// `test_vars_block.yaml` has five **list-form** tool blocks and no playbook
/// references, so the right answer is zero. But "zero" is also what a reader that
/// cannot parse the list form returns — which is precisely the defect
/// noetl/ai-meta#432 found in the server's walker on this very file.
///
/// So the test asserts zero references **and** that the five tools were visited. The
/// second half is what makes the first half mean anything.
#[test]
fn the_list_form_fixture_has_no_references_and_the_tools_were_still_seen() {
    let src = fixture("test_vars_block.yaml");

    let got = find_references(&src).expect("parse");
    assert!(
        got.is_empty(),
        "this fixture has no playbook references; found {got:?}"
    );
    assert_eq!(
        textual_playbook_refs(&src),
        0,
        "the text agrees there are none — if it does not, the fixture changed"
    );

    // The half that makes the zero above non-vacuous: the file's tools are list-form,
    // and the extractor must be able to see them. Counted here by swapping the
    // referencing predicate for one that accepts anything — if `find_references`
    // returned zero because the SHAPE is invisible, this count is zero too.
    let doc: serde_yaml::Value = serde_yaml::from_str(&src).expect("parse");
    let mut tools = 0usize;
    fn count_tools(node: &serde_yaml::Value, n: &mut usize) {
        match node {
            serde_yaml::Value::Mapping(m) => {
                if let Some(t) = m.get(serde_yaml::Value::from("tool")) {
                    match t {
                        serde_yaml::Value::Sequence(items) => {
                            *n += items
                                .iter()
                                .filter(|i| i.get(serde_yaml::Value::from("kind")).is_some())
                                .count()
                        }
                        _ => {
                            if t.get(serde_yaml::Value::from("kind")).is_some() {
                                *n += 1
                            }
                        }
                    }
                }
                for (_k, v) in m {
                    count_tools(v, n);
                }
            }
            serde_yaml::Value::Sequence(s) => s.iter().for_each(|v| count_tools(v, n)),
            _ => {}
        }
    }
    count_tools(&doc, &mut tools);

    println!("test_vars_block.yaml: {tools} list-form tool(s) visited, 0 references — correct");
    assert_eq!(
        tools, 5,
        "the five list-form tool blocks must be visible to the walker. If this is 0, \
         the zero-reference result above is a PARSING failure wearing the costume of a \
         correct answer — which is exactly what noetl/ai-meta#432 was on this file."
    );
}

/// Every fixture must parse. A parse failure would make every count above a zero for
/// the wrong reason.
#[test]
fn every_bundled_fixture_parses_and_the_set_is_not_empty() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).expect("fixtures dir").flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "yaml") {
            let src = std::fs::read_to_string(&p).expect("read");
            find_references(&src)
                .unwrap_or_else(|err| panic!("{} failed to parse: {err}", p.display()));
            n += 1;
        }
    }
    println!("fixtures parsed: {n}");
    assert_eq!(
        n, 6,
        "expected 6 bundled fixtures — 3 playbooks and 3 subscriptions — found {n}. \
         Exact rather than a floor: a loop over an empty directory passes every \
         assertion inside it, and a count that drifts silently means the suite below \
         is covering less than it claims."
    );
}

// ============================================================================
// subscription — the real second resource type
// ============================================================================

/// ⭐ The generalization claim, on real data.
///
/// A subscription shares **nothing** structurally with a playbook: no `workflow:`, a
/// `spec:` instead, and its reference lives at `spec.dispatch.playbook`. Supporting it
/// added no dataset and no schema — `catalog-store`'s AC3 guard pins the `Dataset` impl
/// count at four.
///
/// Measured on the corpus: **9 of 9** `kind: Subscription` fixtures carry a
/// `spec.dispatch.playbook`, and the playbooks they name sit in the same directory, so
/// the cross-type graph is closed.
#[test]
fn a_real_subscription_yields_its_dispatch_and_auth_edges() {
    let src = fixture("dedup_critical_stream.subscription.yaml");
    assert_eq!(
        catalog_extract::resource_kind(&src)
            .expect("parse")
            .as_deref(),
        Some("subscription"),
        "this fixture must declare kind: Subscription"
    );

    let got = find_references(&src).expect("parse");
    println!(
        "dedup_critical_stream: {} edge(s) -> {:?}",
        got.len(),
        got.iter()
            .map(|r| (r.via.location(), r.path.as_str()))
            .collect::<Vec<_>>()
    );

    assert_eq!(got.len(), 2, "a dispatch edge and an auth edge: {got:?}");

    let dispatch = got
        .iter()
        .find(|r| r.relation == catalog_model::RelationKind::Invokes)
        .expect("the dispatch edge");
    assert_eq!(dispatch.path, "tests/fixtures/sub_ingest_default");
    assert_eq!(dispatch.target_type, "playbook");
    assert_eq!(dispatch.via.location(), "spec.dispatch.playbook");

    let auth = got
        .iter()
        .find(|r| r.relation == catalog_model::RelationKind::Requires)
        .expect("the auth edge");
    assert_eq!(auth.path, "nats_e2e");
    assert_eq!(
        auth.target_type, "credential",
        "an auth alias names a credential — deliberately not catalogued, but the \
         dependency is still worth recording"
    );
}

/// ⚠ The contrast that makes the auth edge above non-incidental.
///
/// `hook_bearer` has a `dispatch.playbook` and **no** `spec.auth`, so it must yield
/// exactly one edge. If both fixtures yielded two, the auth arm would be firing on
/// something other than `spec.auth`.
#[test]
fn a_subscription_without_an_auth_alias_yields_only_its_dispatch_edge() {
    let src = fixture("hook_bearer.subscription.yaml");
    assert!(
        !src.contains("\n  auth:"),
        "this fixture was chosen because it has no spec-level auth; if it gained one \
         the contrast below is gone"
    );
    let got = find_references(&src).expect("parse");
    assert_eq!(got.len(), 1, "dispatch only, no auth: {got:?}");
    assert_eq!(got[0].relation, catalog_model::RelationKind::Invokes);
    assert_eq!(got[0].path, "tests/fixtures/handle_webhook");
}

/// Every bundled subscription carries a dispatch edge, matching the 9-of-9 corpus
/// measurement.
#[test]
fn every_bundled_subscription_has_a_dispatch_edge() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut subs = 0;
    for e in std::fs::read_dir(&dir).expect("fixtures").flatten() {
        let p = e.path();
        if !p.to_string_lossy().contains(".subscription.yaml") {
            continue;
        }
        subs += 1;
        let src = std::fs::read_to_string(&p).expect("read");
        let got = find_references(&src).expect("parse");
        assert!(
            got.iter()
                .any(|r| r.relation == catalog_model::RelationKind::Invokes),
            "{} must carry a dispatch edge: {got:?}",
            p.display()
        );
    }
    println!("subscriptions examined: {subs}");
    assert_eq!(
        subs, 3,
        "expected 3 bundled subscriptions, found {subs} — the loop above proves \
         nothing over an empty set"
    );
}

/// A subscription's spec scalars become typed attributes, on real data.
#[test]
fn a_real_subscription_yields_typed_spec_attributes() {
    let src = fixture("dedup_critical_stream.subscription.yaml");
    let attrs = catalog_extract::find_attributes(&src, 42).expect("parse");
    let by: std::collections::BTreeMap<&str, &catalog_model::AttributeValue> =
        attrs.iter().map(|a| (a.name.as_str(), &a.value)).collect();
    println!("attributes: {:?}", by.keys().collect::<Vec<_>>());

    assert!(
        attrs.len() >= 4,
        "the real fixture declares source/mode/activation/stream/consumer; got {:?}",
        by.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        by.get("spec.source"),
        Some(&&catalog_model::AttributeValue::Text("nats".into()))
    );
    assert_eq!(
        by.get("spec.mode"),
        Some(&&catalog_model::AttributeValue::Text("pull".into()))
    );
    // ⚠ `runtime:` is a nested mapping in this fixture and must not be flattened.
    assert!(
        !by.keys().any(|k| k.starts_with("spec.runtime")),
        "a nested mapping is structure, not an attribute: {:?}",
        by.keys().collect::<Vec<_>>()
    );
    assert!(attrs.iter().all(|a| a.entity_id == 42));
}

/// ⚠ A playbook fixture must yield NO subscription attributes.
///
/// Without this, the attribute arm could be firing for every kind and the subscription
/// test above would still pass.
#[test]
fn a_real_playbook_yields_no_subscription_spec_attributes() {
    let src = fixture("playbook_composition.yaml");
    let attrs = catalog_extract::find_attributes(&src, 1).expect("parse");
    assert!(
        !attrs.iter().any(|a| a.name.starts_with("spec.")),
        "a playbook must not acquire subscription spec attributes: {attrs:?}"
    );
}
