//! `list every resource of type X` — the query that would otherwise force a fifth
//! dataset, and the one that makes AC3's claim checkable instead of asserted.
//!
//! Before this index, `catalog list` printed: *"a full path listing needs an index
//! this store does not yet keep"*.
//!
//! Same shape as the c3 reverse index, so the same `.last()` hazard: many paths share
//! one type key. The RED run with `.last()` planted returned **1 of 53**.

use catalog_model::Entity;
use catalog_store::{CatalogStore, StoreConfig};

fn entity_v(path: &str, kind: &str, version: u32) -> Entity {
    Entity {
        resource_type: kind.into(),
        path: path.into(),
        version,
        entity_id: version as i64,
        content: None,
        content_sha256: format!("{version:064}"),
        archived_at: None,
    }
}

#[test]
fn listing_returns_every_resource_of_the_type_not_one() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");

    let mut playbooks: Vec<String> = (0..53).map(|i| format!("adiona/v1/pb_{i:02}")).collect();
    for p in &playbooks {
        store.register(entity_v(p, "playbook", 1)).expect("reg");
    }
    let mut subs: Vec<String> = (0..4).map(|i| format!("hooks/sub_{i}")).collect();
    for p in &subs {
        store.register(entity_v(p, "subscription", 1)).expect("reg");
    }
    playbooks.sort();
    subs.sort();

    let got = store.resources_of_type("playbook").expect("list");
    println!(
        "playbook: {} path(s), expected {}",
        got.len(),
        playbooks.len()
    );
    // ⚠ Set equality. A count of 53 could be the wrong 53.
    assert_eq!(got, playbooks);
    assert_eq!(store.resources_of_type("subscription").expect("list"), subs);

    // The two types must not bleed.
    assert!(got.iter().all(|p| !subs.contains(p)));

    // Case-insensitive, because noetl/server#429 was a real prod bug where
    // 'Playbook' and 'playbook' were two populations of one kind.
    assert_eq!(
        store.resources_of_type("Playbook").expect("list"),
        playbooks
    );
    assert_eq!(
        store.resources_of_type("PLAYBOOK").expect("list"),
        playbooks
    );

    // An undeclared type is empty, not everything and not an error.
    assert!(store
        .resources_of_type("dashboard")
        .expect("list")
        .is_empty());
}

/// ⚠⚠ The case that is easy to get wrong: **archiving a version is not archiving the
/// path.** A path with three versions, one archived, is still listable.
#[test]
fn archiving_one_version_of_many_keeps_the_path_listed() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    let path = "adiona/v1/multi";
    for v in 1..=3 {
        store.register(entity_v(path, "playbook", v)).expect("reg");
    }
    store
        .register(entity_v("adiona/v1/other", "playbook", 1))
        .expect("reg");

    // Archive v1 only — v2 and v3 remain live.
    store.archive(path, 1, 1_700_000_000).expect("archive");
    let got = store.resources_of_type("playbook").expect("list");
    println!("after archiving v1 of 3: {got:?}");
    assert!(
        got.contains(&path.to_string()),
        "archiving one version dropped a path whose other versions are live"
    );
    assert_eq!(got.len(), 2);

    // Archive the remaining two — now the path has no live version and must leave.
    store.archive(path, 2, 1_700_000_001).expect("archive");
    store.archive(path, 3, 1_700_000_002).expect("archive");
    let got = store.resources_of_type("playbook").expect("list");
    println!("after archiving all 3: {got:?}");
    assert!(
        !got.contains(&path.to_string()),
        "a path with no live version is still listed"
    );
    assert_eq!(got, vec!["adiona/v1/other".to_string()]);

    // Restore brings it back.
    store.restore(path, 2).expect("restore");
    let got = store.resources_of_type("playbook").expect("list");
    assert!(
        got.contains(&path.to_string()),
        "restore did not relist the path"
    );
    assert_eq!(got.len(), 2);
}

/// A path appears **once** in a listing however many versions it has — the type index
/// is about the path, not the version.
#[test]
fn a_multi_version_path_is_listed_once() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    let path = "adiona/v1/versioned";
    for v in 1..=7 {
        store.register(entity_v(path, "playbook", v)).expect("reg");
    }
    let got = store.resources_of_type("playbook").expect("list");
    assert_eq!(
        got,
        vec![path.to_string()],
        "a path was listed more than once"
    );

    // And the forward reads are undisturbed by the index rows: 7 versions, latest 7,
    // and no phantom version 0 from the type rows' `version: 0`.
    let vs = store.versions(path).expect("versions");
    assert_eq!(vs.len(), 7, "type-index rows leaked into the version fold");
    assert!(
        !vs.iter().any(|e| e.version == 0),
        "a phantom version 0 appeared from a type-index row"
    );
    assert_eq!(
        store.latest(path).expect("latest").map(|e| e.version),
        Some(7)
    );
}

/// Both synthetic key spaces are refused as resource paths, and the error names which.
#[test]
fn a_path_in_either_synthetic_key_space_is_refused() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");

    for (prefix, want) in [
        (catalog_store::datasets::TYPE_KEY_PREFIX, "type-index"),
        (catalog_store::datasets::REVERSE_KEY_PREFIX, "reverse-index"),
    ] {
        let evil = format!("{prefix}playbook");
        let err = store
            .register(entity_v(&evil, "playbook", 1))
            .expect_err("must be refused");
        println!("refused ({want}): {err}");
        assert!(
            err.to_string().contains(want),
            "wrong sentinel named: {err}"
        );
    }
    assert!(store
        .resources_of_type("playbook")
        .expect("list")
        .is_empty());
}

#[test]
fn the_type_index_survives_a_reopen() {
    let dir = tempfile::tempdir().expect("td");
    let expected: Vec<String> = {
        let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
        let mut v = Vec::new();
        for i in 0..20 {
            let p = format!("adiona/v1/r{i:02}");
            store.register(entity_v(&p, "playbook", 1)).expect("reg");
            v.push(p);
        }
        v.sort();
        v
    };
    let store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("reopen");
    let got = store.resources_of_type("playbook").expect("list");
    println!("after reopen: {} of {}", got.len(), expected.len());
    assert_eq!(got, expected);
}
