//! Acceptance criteria from `design/catalog-model.md` §11 that need a real store.
//!
//! AC2, AC4, AC6 and AC7. The other six are discharged in `catalog-model`.

use catalog_model::{
    Attribute, AttributeValue, Entity, EntityRef, Provenance, Relation, RelationKind, ResourceType,
};
use catalog_store::{CatalogStore, StoreConfig};

fn store() -> (CatalogStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = StoreConfig::new(dir.path());
    let s = CatalogStore::open(&cfg).expect("open");
    (s, dir)
}

fn entity(path: &str, version: u32) -> Entity {
    Entity {
        resource_type: "playbook".into(),
        path: path.into(),
        version,
        entity_id: version as i64,
        content: Some(format!("# v{version}")),
        content_sha256: format!("{version:064}"),
        archived_at: None,
    }
}

/// **AC2** — a read after an append sees it, with no flush between.
///
/// This is the register-then-execute path: the e2e suite registers a playbook and
/// immediately executes it in one loop body, and the predecessor's own comment names
/// this as the open hazard — a cached fold is *"stale by construction for up to the
/// TTL, which is exactly the read-your-writes problem the RFC names"*.
///
/// EHDB makes it work because an L0 read merges the **active, unsealed** part with the
/// sealed and replica parts. The test exists so a TTL cache can never be added
/// quietly: this is what would fail.
#[test]
fn ac2_a_read_after_an_append_sees_it_without_a_flush() {
    let (mut s, _d) = store();
    s.register(entity("muno/playbooks/itinerary", 1))
        .expect("register");

    // No flush, no seal, no tick. Deliberately.
    let got = s.latest("muno/playbooks/itinerary").expect("latest");
    assert_eq!(
        got.as_ref().map(|e| e.version),
        Some(1),
        "the write must be visible immediately; if this fails, either a cache was \
         introduced or reads stopped merging the active part"
    );

    // And again after a second append, so the test is about currency rather than
    // about the first write happening to land.
    s.register(entity("muno/playbooks/itinerary", 2))
        .expect("register v2");
    assert_eq!(
        s.latest("muno/playbooks/itinerary")
            .expect("latest")
            .map(|e| e.version),
        Some(2),
        "a subsequent append must also be immediately visible"
    );
}

/// **AC4** — every attribute on one entity survives the fold.
///
/// ⚠ **This is a positive control, not a happy-path test.** The idiom every in-tree
/// EHDB store uses is `read_index_after(key, 0).last()`, which under one index key
/// returns exactly one record — successfully. Three attributes on one path would come
/// back as one, with no error and a plausible-looking value.
///
/// The assertion is therefore on the *count*, and the count is compared against what
/// the naive idiom would yield, so the test states the hazard rather than merely
/// avoiding it.
#[test]
fn ac4_every_attribute_on_one_entity_survives_the_fold() {
    let (mut s, _d) = store();
    let path = "muno/playbooks/itinerary";
    s.register(entity(path, 1)).expect("register");

    let attrs = [
        Attribute::new(1, "labels.team", AttributeValue::Text("muno".into())),
        Attribute::new(1, "labels.tier", AttributeValue::Text("prod".into())),
        Attribute::new(1, "exposed_in_ui", AttributeValue::Flag(true)),
        Attribute::new(1, "max_runtime_secs", AttributeValue::Integer(900)),
    ];
    for a in &attrs {
        s.set_attribute(path, a.clone()).expect("set");
    }

    let got = s.attributes(path).expect("attributes");

    assert_eq!(
        got.len(),
        attrs.len(),
        "all {} attributes must survive; got {}. A latest-wins fold copied from \
         ProjectionStore returns 1 here and does not error — which is why this \
         assertion is on the COUNT and not on one value.",
        attrs.len(),
        got.len()
    );
    assert!(
        got.len() > 1,
        "the fold returned {} attribute(s). If it returned exactly 1 this test has \
         caught the naive-fold defect; if it returned 0 the scan is broken and the \
         test proves nothing either way.",
        got.len()
    );

    for a in &attrs {
        assert_eq!(
            got.get(&a.name),
            Some(a),
            "attribute {:?} must round-trip through the store unchanged",
            a.name
        );
    }
}

/// Later writes to the SAME attribute name must still collapse to one.
///
/// The companion to AC4: the fix must not over-correct into keeping every historical
/// value. Grouping is per name, and within a name the latest op wins.
#[test]
fn ac4b_repeated_writes_to_one_name_collapse_to_the_latest() {
    let (mut s, _d) = store();
    let path = "p";
    s.register(entity(path, 1)).expect("register");
    s.set_attribute(
        path,
        Attribute::new(1, "tier", AttributeValue::Text("dev".into())),
    )
    .expect("set 1");
    s.set_attribute(
        path,
        Attribute::new(1, "tier", AttributeValue::Text("prod".into())),
    )
    .expect("set 2");
    s.set_attribute(
        path,
        Attribute::new(1, "other", AttributeValue::Flag(false)),
    )
    .expect("set 3");

    let got = s.attributes(path).expect("attributes");
    assert_eq!(got.len(), 2, "two distinct names, not three ops: {got:?}");
    assert_eq!(
        got["tier"].value,
        AttributeValue::Text("prod".into()),
        "the later write to `tier` must win"
    );
}

/// An unset must remove exactly one name and leave the others.
#[test]
fn an_unset_removes_one_attribute_and_only_one() {
    let (mut s, _d) = store();
    let path = "p";
    s.register(entity(path, 1)).expect("register");
    s.set_attribute(path, Attribute::new(1, "a", AttributeValue::Integer(1)))
        .expect("a");
    s.set_attribute(path, Attribute::new(1, "b", AttributeValue::Integer(2)))
        .expect("b");
    assert_eq!(s.attributes(path).expect("read").len(), 2);

    s.unset_attribute(path, "a").expect("unset");
    let got = s.attributes(path).expect("read");
    assert_eq!(got.len(), 1, "exactly one attribute must remain: {got:?}");
    assert!(got.contains_key("b"), "the surviving attribute must be `b`");
}

/// **AC6** — a version above the `SMALLSERIAL` ceiling survives the store.
#[test]
fn ac6_a_version_above_the_smallint_ceiling_round_trips_through_the_store() {
    let (mut s, _d) = store();
    let path = "p";
    for v in [32_767u32, 32_768, 40_000, 1_000_000] {
        s.register(entity(path, v)).expect("register");
    }
    let versions: Vec<u32> = s
        .versions(path)
        .expect("versions")
        .iter()
        .map(|e| e.version)
        .collect();
    assert_eq!(
        versions,
        vec![32_767, 32_768, 40_000, 1_000_000],
        "every version must survive, in ascending order"
    );
    assert_eq!(
        s.latest(path).expect("latest").map(|e| e.version),
        Some(1_000_000),
        "the newest version must win — not the highest i16, and not the first"
    );
}

/// Archive and restore must apply to the version they name, not to the path.
#[test]
fn archive_applies_to_one_version_not_to_the_whole_path() {
    let (mut s, _d) = store();
    let path = "p";
    s.register(entity(path, 1)).expect("v1");
    s.register(entity(path, 2)).expect("v2");

    s.archive(path, 2, 1_760_000_000_000_000)
        .expect("archive v2");
    assert_eq!(
        s.latest(path).expect("latest").map(|e| e.version),
        Some(1),
        "archiving v2 must fall back to v1, not hide the whole path"
    );

    s.restore(path, 2).expect("restore v2");
    assert_eq!(
        s.latest(path).expect("latest").map(|e| e.version),
        Some(2),
        "restore must bring v2 back"
    );
}

/// **AC7** — `seal_max_age` is set **and** something drives the sealer.
///
/// ⚠ The flag alone is inert on exactly the shard it protects: an idle shard takes no
/// appends, so no size- or count-based seal ever fires and the records never reach the
/// substrate. A configured-but-undriven sealer is the shape where the config reads
/// correct and the behaviour never happens.
///
/// So this asserts the *driver* exists and runs, not that the field is set.
#[test]
fn ac7_the_sealer_is_driven_and_not_merely_configured() {
    let (mut s, _d) = store();
    s.register(entity("p", 1)).expect("register");

    // `tick` must exist, be callable, and report a count. A zero here is correct —
    // the part is younger than SEAL_MAX_AGE — but the call itself must not be absent,
    // because that absence is the defect.
    let sealed = s.tick().expect("tick must be callable");
    assert_eq!(
        sealed, 0,
        "a part younger than SEAL_MAX_AGE must not seal yet; got {sealed}"
    );

    // And the read must still work after a tick, i.e. ticking does not lose the tail.
    assert_eq!(
        s.latest("p").expect("latest").map(|e| e.version),
        Some(1),
        "a tick must not make a just-written record unreadable"
    );
}

/// Relations round-trip with their provenance intact.
#[test]
fn a_relation_round_trips_with_its_provenance() {
    let (mut s, _d) = store();
    let parent = entity("parent", 1);
    s.register(parent.clone()).expect("register");

    let rel = Relation::new(
        parent.as_ref_pinned(),
        EntityRef::latest("playbook", "child"),
        RelationKind::Invokes,
        Provenance::Extracted {
            at: 1_760_000_000_000_000,
        },
    );
    s.assert_relation(rel.clone()).expect("assert");

    let got = s.relations_from("parent").expect("relations");
    assert_eq!(got.len(), 1, "one edge: {got:?}");
    assert_eq!(
        got[0], rel,
        "the edge must survive unchanged, provenance included"
    );
    assert!(
        !got[0].to_entity.is_pinned(),
        "an unpinned target must not acquire a version through storage"
    );
}

/// Two distinct edges from one source must both survive — the relation-side AC4.
#[test]
fn two_edges_from_one_source_both_survive() {
    let (mut s, _d) = store();
    let parent = entity("parent", 1);
    s.register(parent.clone()).expect("register");

    for (child, kind) in [
        ("child-a", RelationKind::Invokes),
        ("child-b", RelationKind::Invokes),
        ("child-a", RelationKind::Requires),
    ] {
        s.assert_relation(Relation::new(
            parent.as_ref_pinned(),
            EntityRef::latest("playbook", child),
            kind,
            Provenance::Declared,
        ))
        .expect("assert");
    }

    let got = s.relations_from("parent").expect("relations");
    assert_eq!(
        got.len(),
        3,
        "three distinct edges — two targets and two kinds — must all survive. A \
         latest-wins fold over `from_path` returns 1 and does not error: {got:?}"
    );
}

/// **AC3's storage half** — a resource type is data, and the store round-trips one it
/// has never heard of.
#[test]
fn a_resource_type_the_store_has_never_heard_of_round_trips() {
    let (mut s, _d) = store();
    let novel = ResourceType::new("Widget", false, true).specializing("playbook");
    s.declare_type(novel.clone()).expect("declare");

    // Looked up by either spelling, because the name is normalized at construction.
    for spelling in ["widget", "Widget", "WIDGET"] {
        assert_eq!(
            s.resource_type(spelling).expect("read").as_ref(),
            Some(&novel),
            "a type must be findable as {spelling:?} — the prod `kind` column holds \
             mixed case, and a case-sensitive lookup there returned a PARTIAL set"
        );
    }
}
