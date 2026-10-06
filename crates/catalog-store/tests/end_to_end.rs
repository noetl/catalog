//! Register a real playbook source and read its edges back out of EHDB.
//!
//! The loop P1–P3 built in pieces, closed: source → extracted references → appended
//! relation ops → folded back. Everything here goes through EHDB; nothing else is
//! written.

use catalog_model::{AttributeValue, Entity, Provenance, RelationKind, ResourceType};
use catalog_store::{CatalogStore, StoreConfig};

/// The composition fixture, verbatim from `noetl/e2e` via `catalog-extract`'s copy.
const COMPOSITION: &str =
    include_str!("../../catalog-extract/tests/fixtures/playbook_composition.yaml");
/// Five list-form tool blocks, no playbook references.
const LIST_FORM: &str = include_str!("../../catalog-extract/tests/fixtures/test_vars_block.yaml");

fn store() -> (CatalogStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    (s, dir)
}

fn entity(path: &str) -> Entity {
    Entity {
        resource_type: "playbook".into(),
        path: path.into(),
        version: 1,
        entity_id: 1,
        content: None,
        content_sha256: "e".repeat(64),
        archived_at: None,
    }
}

#[test]
fn a_real_playbook_registers_and_its_edges_are_readable() {
    let (mut s, _d) = store();
    let path = "fixtures/playbooks/playbook_composition/playbook_composition";

    // ⚠ Assert the input before asserting about the output: an empty include_str!
    // would register an entity with no edges and read exactly like a playbook that
    // declares none.
    assert!(
        COMPOSITION.len() > 1_000,
        "the fixture is only {} bytes — it did not load",
        COMPOSITION.len()
    );

    let r = s
        .register_from_source(entity(path), COMPOSITION, 1_760_000_000_000_000)
        .expect("register");
    let n = r.relations;

    assert!(
        n > 0,
        "this fixture declares a `kind: playbook` reference; recording 0 edges means \
         extraction did not run or the source did not reach it"
    );

    let edges = s.relations_from(path).expect("read edges");
    assert_eq!(edges.len(), n, "every recorded edge must be readable back");

    let targets: Vec<&str> = edges.iter().map(|e| e.to_entity.path.as_str()).collect();
    assert!(
        targets.iter().any(|t| t.contains("user_profile_scorer")),
        "the child the playbook actually names must be among the edges: {targets:?}"
    );

    for e in &edges {
        assert_eq!(e.kind, RelationKind::Invokes);
        assert_eq!(
            e.discovered_by,
            Provenance::Extracted {
                at: 1_760_000_000_000_000
            },
            "an edge read from a source is Extracted, not Observed"
        );
        assert!(
            !e.to_entity.is_pinned(),
            "the source named a path and no version, so the stored edge must be \
             unpinned: {e:?}"
        );
        assert!(
            e.from_entity.is_pinned(),
            "the parent is a concrete version"
        );
    }

    // The entity itself is readable, so the edges did not replace the registration.
    assert_eq!(
        s.latest(path).expect("latest").map(|e| e.version),
        Some(1),
        "the entity must be registered as well as its edges"
    );
}

/// A playbook with no references registers, with zero edges and no error.
///
/// ⚠ Paired with a check that the source was genuinely read. Zero edges is also what a
/// broken extractor returns, and this is the exact file whose list-form tools the
/// server's own walker could not see (noetl/ai-meta#432).
#[test]
fn a_playbook_with_no_references_registers_with_no_edges() {
    let (mut s, _d) = store();
    let path = "vars_test/test_vars_block";
    assert!(LIST_FORM.len() > 500, "fixture did not load");

    let r = s
        .register_from_source(entity(path), LIST_FORM, 1)
        .expect("register");
    let n = r.relations;
    assert_eq!(n, 0, "this playbook declares no references");
    assert!(s.relations_from(path).expect("read").is_empty());

    // The registration itself still happened — the zero above is about edges, not
    // about the entity.
    assert_eq!(
        s.latest(path).expect("latest").map(|e| e.version),
        Some(1),
        "a playbook with no dependencies must still register"
    );

    // And the source really was parsed: it has five list-form tools. If this is 0,
    // the zero-edge result above is a parse failure wearing a correct answer.
    let refs = catalog_extract::find_references(LIST_FORM).expect("the fixture parses");
    assert!(refs.is_empty(), "no references expected, found {refs:?}");
    assert!(
        LIST_FORM.contains("kind: python"),
        "the fixture must still contain its list-form tools; if not, it changed and \
         the zero above proves nothing"
    );
}

/// ⚠ A source that does not parse must still register the entity.
///
/// `noetl/server` accepts documents this extractor finds nothing in — a
/// `kind: Subscription` entry has no `workflow:` at all. Refusing to register on an
/// extraction failure would make this store reject what the platform accepts.
/// Extraction is additive information, never a gate.
#[test]
fn an_unparseable_source_still_registers_the_entity_with_no_edges() {
    let (mut s, _d) = store();
    let path = "broken/one";
    let r = s
        .register_from_source(entity(path), "workflow: [\n  - broken: {{{\n", 1)
        .expect("a storage call must succeed even when extraction cannot");
    assert_eq!(r.relations, 0, "no edges from an unparseable source");
    assert_eq!(
        s.latest(path).expect("latest").map(|e| e.version),
        Some(1),
        "the entity must register regardless — extraction is not a gate"
    );
}

/// A subscription has no `workflow:` and must register cleanly.
#[test]
fn a_subscription_registers_with_no_edges() {
    let (mut s, _d) = store();
    let mut e = entity("subs/one");
    e.resource_type = "subscription".into();
    let r = s
        .register_from_source(e, "kind: Subscription\nspec: {}\n", 1)
        .expect("register");
    assert_eq!(r.relations, 0);
    assert_eq!(
        s.latest("subs/one")
            .expect("latest")
            .map(|x| x.resource_type),
        Some("subscription".to_string())
    );
}

/// Two playbooks naming the same child both record their own edge.
///
/// The reverse question — "what references X" — is the one the current catalog cannot
/// answer without re-parsing every stored body. Here each parent's edges are readable
/// from its own index key.
#[test]
fn two_parents_naming_one_child_each_keep_their_own_edge() {
    let (mut s, _d) = store();
    let src = |child: &str| {
        format!("workflow:\n  - step: s\n    tool:\n      kind: playbook\n      path: {child}\n")
    };
    s.register_from_source(entity("parent/a"), &src("shared/child"), 1)
        .expect("a");
    s.register_from_source(entity("parent/b"), &src("shared/child"), 1)
        .expect("b");

    for p in ["parent/a", "parent/b"] {
        let edges = s.relations_from(p).expect("read");
        assert_eq!(edges.len(), 1, "{p} must keep its own edge: {edges:?}");
        assert_eq!(edges[0].to_entity.path, "shared/child");
        assert_eq!(
            edges[0].from_entity.path, p,
            "the edge must name its own parent"
        );
    }
}

// ============================================================================
// ⭐ subscription — a SECOND resource type, through the SAME four datasets
// ============================================================================

const SUBSCRIPTION: &str =
    include_str!("../../catalog-extract/tests/fixtures/dedup_critical_stream.subscription.yaml");

fn subscription_entity(path: &str) -> Entity {
    Entity {
        resource_type: "subscription".into(),
        path: path.into(),
        version: 1,
        entity_id: 77,
        content: None,
        content_sha256: "s".repeat(64),
        archived_at: None,
    }
}

/// The generalization claim, end to end on real data.
///
/// A subscription shares nothing structurally with a playbook — no `workflow:`, a
/// `spec:` instead, its reference at `spec.dispatch.playbook`, and an `auth` alias that
/// is a dependency on a resource type the catalog deliberately does **not** hold. All
/// of it lands in the same `c1`/`c2`/`c3` logs as a playbook, keyed by the same
/// polymorphic identity.
///
/// ⚠ The companion assertion lives in `ac3_dataset_count_is_fixed.rs`: supporting this
/// type added **no** `Dataset` impl. Without that, "it works" would not establish the
/// claim — it would only establish that *something* works.
#[test]
fn a_real_subscription_registers_its_dispatch_auth_and_spec_through_the_same_store() {
    let (mut s, _d) = store();
    let path = "subscriptions/dedup_critical_stream";
    assert!(
        SUBSCRIPTION.len() > 500,
        "the fixture is only {} bytes — it did not load, and every count below would \
         then be a zero for the wrong reason",
        SUBSCRIPTION.len()
    );

    // First declare the type — one appended row, no schema change.
    s.declare_type(ResourceType::new("subscription", true, true))
        .expect("declare type");

    let r = s
        .register_from_source(subscription_entity(path), SUBSCRIPTION, 1_700)
        .expect("register");

    assert_eq!(
        r.relations, 2,
        "a dispatch edge and an auth edge must both be recorded; got {}",
        r.relations
    );
    assert!(
        r.attributes >= 4,
        "the fixture declares source/mode/activation/stream/consumer; got {}",
        r.attributes
    );

    // --- the edges read back, with their kinds intact ---
    let edges = s.relations_from(path).expect("read edges");
    assert_eq!(edges.len(), 2, "{edges:?}");

    let invokes = edges
        .iter()
        .find(|e| e.kind == RelationKind::Invokes)
        .expect("the dispatch edge");
    assert_eq!(invokes.to_entity.path, "tests/fixtures/sub_ingest_default");
    assert_eq!(invokes.to_entity.resource_type, "playbook");

    let requires = edges
        .iter()
        .find(|e| e.kind == RelationKind::Requires)
        .expect("the auth edge");
    assert_eq!(requires.to_entity.path, "nats_e2e");
    assert_eq!(
        requires.to_entity.resource_type, "credential",
        "the auth alias names a credential — a type the catalog deliberately does not \
         hold, yet the dependency is recorded"
    );

    // ⚠ The two edges must not collapse. An earlier shape hardcoded Invokes, which
    // would have made the credential dependency unreadable while leaving a plausible
    // edge count of 2.
    assert_ne!(
        invokes.kind, requires.kind,
        "a cross-type dependency and an invocation are different claims"
    );

    // --- the attributes read back, typed ---
    let attrs = s.attributes(path).expect("read attributes");
    assert_eq!(
        attrs.len(),
        r.attributes,
        "every recorded attribute must be readable back: {:?}",
        attrs.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        attrs.get("spec.source").map(|a| &a.value),
        Some(&AttributeValue::Text("nats".into()))
    );
    assert_eq!(
        attrs.get("spec.mode").map(|a| &a.value),
        Some(&AttributeValue::Text("pull".into()))
    );

    // --- and the entity itself, with its own resource_type ---
    let e = s.latest(path).expect("latest").expect("registered");
    assert_eq!(e.resource_type, "subscription");
    assert_eq!(
        s.resource_type("subscription")
            .expect("type")
            .map(|t| t.name),
        Some("subscription".to_string()),
        "the declared type must be readable from c4"
    );
}

/// ⚠ A playbook and a subscription in ONE store must not contaminate each other.
///
/// Both live in the same four logs, so a partition or index-key mistake would make one
/// readable under the other's path. This is the test that a shared store is actually
/// shared correctly rather than coincidentally.
#[test]
fn a_playbook_and_a_subscription_coexist_without_contaminating_each_other() {
    let (mut s, _d) = store();
    let pb = "fixtures/playbooks/playbook_composition/playbook_composition";
    let sub = "subscriptions/dedup_critical_stream";

    s.register_from_source(entity(pb), COMPOSITION, 1)
        .expect("playbook");
    s.register_from_source(subscription_entity(sub), SUBSCRIPTION, 1)
        .expect("subscription");

    let pb_edges = s.relations_from(pb).expect("pb edges");
    let sub_edges = s.relations_from(sub).expect("sub edges");

    assert!(!pb_edges.is_empty(), "the playbook keeps its edges");
    assert_eq!(sub_edges.len(), 2, "the subscription keeps its two edges");

    for e in &pb_edges {
        assert_eq!(
            e.from_entity.path, pb,
            "a playbook edge must name the playbook"
        );
    }
    for e in &sub_edges {
        assert_eq!(
            e.from_entity.path, sub,
            "a subscription edge must name the subscription"
        );
    }

    // The playbook has no spec attributes; the subscription does. If the attribute log
    // were keyed wrongly, these would bleed.
    let pb_attrs = s.attributes(pb).expect("pb attrs");
    let sub_attrs = s.attributes(sub).expect("sub attrs");
    assert!(
        !pb_attrs.keys().any(|k| k.starts_with("spec.")),
        "the playbook must not acquire the subscription's spec attributes: {:?}",
        pb_attrs.keys().collect::<Vec<_>>()
    );
    assert!(
        sub_attrs.keys().any(|k| k.starts_with("spec.")),
        "the subscription must keep its own: {:?}",
        sub_attrs.keys().collect::<Vec<_>>()
    );

    // And each entity keeps its own resource_type.
    assert_eq!(
        s.latest(pb).expect("l").map(|e| e.resource_type),
        Some("playbook".to_string())
    );
    assert_eq!(
        s.latest(sub).expect("l").map(|e| e.resource_type),
        Some("subscription".to_string())
    );
}
