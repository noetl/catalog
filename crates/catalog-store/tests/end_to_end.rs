//! Register a real playbook source and read its edges back out of EHDB.
//!
//! The loop P1–P3 built in pieces, closed: source → extracted references → appended
//! relation ops → folded back. Everything here goes through EHDB; nothing else is
//! written.

use catalog_model::{Entity, Provenance, RelationKind};
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

    let (_seq, n) = s
        .register_from_source(entity(path), COMPOSITION, 1_760_000_000_000_000)
        .expect("register");

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

    let (_seq, n) = s
        .register_from_source(entity(path), LIST_FORM, 1)
        .expect("register");
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
    let (_seq, n) = s
        .register_from_source(entity(path), "workflow: [\n  - broken: {{{\n", 1)
        .expect("a storage call must succeed even when extraction cannot");
    assert_eq!(n, 0, "no edges from an unparseable source");
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
    let (_seq, n) = s
        .register_from_source(e, "kind: Subscription\nspec: {}\n", 1)
        .expect("register");
    assert_eq!(n, 0);
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
