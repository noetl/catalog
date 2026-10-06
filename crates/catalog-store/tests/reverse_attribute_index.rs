//! The reverse index must return the **whole** set of resources carrying an
//! attribute — not one of them, and not most of them.
//!
//! # Why a count assertion is not enough
//!
//! The motivating query is `uses_credential.<alias>`: rotating a keychain alias means
//! knowing which resources break. A *partial* answer there is worse than an empty
//! one, because an empty answer gets investigated and a plausible-looking list of 1
//! gets acted on.
//!
//! Every resource carrying one attribute shares **one** reverse key, which makes this
//! the dataset's worst case for the `.last()` idiom (`crate::fold_latest_by`). The RED
//! run below, with the fold replaced by `.last()`, returned:
//!
//! ```text
//! reverse lookup uses_credential.adiona_actor: 1 path(s), expected 49
//! after unset: 0 path(s), expected 48
//! ```
//!
//! **One of forty-nine, reported successfully.** And the second line is worse, and was
//! not predicted: once any resource unsets the attribute, the last op under the shared
//! key is a *tombstone*, so `.last()` answers **0 of 48** — "nobody uses this
//! credential" while 48 resources do. That reading would green-light a credential
//! rotation that breaks all 48.
//!
//! So these tests assert **set equality**, never a count alone — a count can be right
//! while the membership is wrong. 2 of the 5 tests passed under the RED (the key-space
//! and refusal ones), which is correct: they do not exercise the fold.

use catalog_model::{Attribute, AttributeValue, Entity};
use catalog_store::{CatalogStore, StoreConfig};

fn entity(path: &str) -> Entity {
    Entity {
        resource_type: "playbook".into(),
        path: path.into(),
        version: 1,
        entity_id: 0,
        content: None,
        content_sha256: format!("{:064}", 1),
        archived_at: None,
    }
}

fn flag(name: &str) -> Attribute {
    Attribute::new(0, name.to_string(), AttributeValue::Flag(true))
}

/// Mirrors the measured shape of the real corpus: 53 playbooks, 49 on one credential
/// alias and 4 on another.
fn seed(store: &mut CatalogStore) -> (Vec<String>, Vec<String>) {
    let mut actor = Vec::new();
    let mut migrator = Vec::new();
    for i in 0..53 {
        let path = format!("adiona/v1/pb_{i:02}");
        store.register(entity(&path)).expect("register");
        store
            .set_attribute(&path, flag("uses_tool.postgres"))
            .expect("tool");
        if i < 49 {
            store
                .set_attribute(&path, flag("uses_credential.adiona_actor"))
                .expect("cred");
            actor.push(path);
        } else {
            store
                .set_attribute(&path, flag("uses_credential.adiona_migrator"))
                .expect("cred");
            migrator.push(path);
        }
    }
    actor.sort();
    migrator.sort();
    (actor, migrator)
}

#[test]
fn the_reverse_lookup_returns_every_resource_not_one_of_them() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    let (actor, migrator) = seed(&mut store);

    let got_actor = store
        .resources_with_attribute("uses_credential.adiona_actor")
        .expect("reverse");
    println!(
        "reverse lookup uses_credential.adiona_actor: {} path(s), expected {}",
        got_actor.len(),
        actor.len()
    );

    // ⚠ Set equality, not a count. A count of 49 could still be the wrong 49.
    assert_eq!(
        got_actor,
        actor,
        "the reverse lookup must return exactly the {} resources on this alias",
        actor.len()
    );

    let got_mig = store
        .resources_with_attribute("uses_credential.adiona_migrator")
        .expect("reverse");
    assert_eq!(
        got_mig, migrator,
        "the other alias must be exactly its own 4"
    );

    // And the two aliases must not bleed into each other.
    assert!(
        got_actor.iter().all(|p| !got_mig.contains(p)),
        "a resource appeared under both aliases"
    );

    // The tool attribute is on all 53 — a third key, to show the index is not
    // accidentally returning "everything that has any attribute".
    let got_tool = store
        .resources_with_attribute("uses_tool.postgres")
        .expect("reverse");
    assert_eq!(got_tool.len(), 53, "uses_tool.postgres is on all 53");

    // An attribute nobody carries must be empty, not an error and not everything.
    assert!(store
        .resources_with_attribute("uses_credential.nobody")
        .expect("reverse")
        .is_empty());
}

/// Unsetting must REMOVE a resource from the reverse answer. Without a tombstone the
/// reverse index only ever grows, and the answer accumulates resources that stopped
/// carrying the attribute — a wrong answer that never self-corrects.
#[test]
fn unset_removes_the_resource_from_the_reverse_answer() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    let (mut actor, _) = seed(&mut store);

    let victim = actor.remove(10);
    store
        .unset_attribute(&victim, "uses_credential.adiona_actor")
        .expect("unset");

    let got = store
        .resources_with_attribute("uses_credential.adiona_actor")
        .expect("reverse");
    println!(
        "after unset: {} path(s), expected {}",
        got.len(),
        actor.len()
    );
    assert_eq!(
        got, actor,
        "the unset resource must be gone from the reverse answer"
    );
    assert!(!got.contains(&victim));

    // The forward read must agree — the two views of one write cannot disagree.
    let fwd = store.attributes(&victim).expect("attrs");
    assert!(
        !fwd.contains_key("uses_credential.adiona_actor"),
        "forward read still shows the unset attribute"
    );
    assert!(
        fwd.contains_key("uses_tool.postgres"),
        "unsetting one attribute must not disturb another"
    );

    // Re-setting it brings it back — the tombstone is a state, not a permanent ban.
    store
        .set_attribute(&victim, flag("uses_credential.adiona_actor"))
        .expect("re-set");
    let back = store
        .resources_with_attribute("uses_credential.adiona_actor")
        .expect("reverse");
    assert!(
        back.contains(&victim),
        "re-setting must restore the resource"
    );
    assert_eq!(back.len(), actor.len() + 1);
}

/// The forward read must not see reverse rows, and the reverse read must not see
/// forward rows. The key spaces are disjoint by construction; this proves it rather
/// than assuming it.
#[test]
fn the_two_key_spaces_do_not_leak_into_each_other() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    let path = "adiona/v1/one";
    store.register(entity(path)).expect("register");
    store
        .set_attribute(path, flag("uses_tool.postgres"))
        .expect("set");
    store
        .set_attribute(path, flag("uses_credential.adiona_actor"))
        .expect("set");

    // Forward: exactly the two attributes, no reverse artefacts under other names.
    let fwd = store.attributes(path).expect("attrs");
    let mut names: Vec<&str> = fwd.keys().map(|s| s.as_str()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["uses_credential.adiona_actor", "uses_tool.postgres"],
        "the forward read picked up something that is not a forward attribute"
    );

    // Reverse: querying by the resource PATH must return nothing, because a path is
    // not a reverse key. If this returned the resource, the key spaces would be
    // overlapping and every forward row would be reachable as a reverse answer.
    assert!(
        store
            .resources_with_attribute(path)
            .expect("reverse")
            .is_empty(),
        "a resource path answered a reverse query"
    );
}

/// A path intruding on the reverse key space must be refused, not silently accepted.
#[test]
fn a_path_in_the_reverse_key_space_is_refused() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    let evil = format!(
        "{}uses_credential.adiona_actor",
        catalog_store::datasets::REVERSE_KEY_PREFIX
    );

    let err = store
        .set_attribute(&evil, flag("x"))
        .expect_err("a path in the reverse key space must be refused");
    println!("refused: {err}");
    assert!(
        err.to_string().contains("reverse-index sentinel"),
        "unexpected error: {err}"
    );

    // And the refusal must leave nothing behind — a half-written row would be worse
    // than the collision it prevents.
    assert!(store
        .resources_with_attribute("uses_credential.adiona_actor")
        .expect("reverse")
        .is_empty());
}

/// The reverse index must survive a reopen, like every other fold in this store.
#[test]
fn the_reverse_index_survives_a_reopen() {
    let dir = tempfile::tempdir().expect("td");
    let expected = {
        let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
        let (actor, _) = seed(&mut store);
        actor
    };
    let store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("reopen");
    let got = store
        .resources_with_attribute("uses_credential.adiona_actor")
        .expect("reverse");
    println!(
        "after reopen: {} path(s), expected {}",
        got.len(),
        expected.len()
    );
    assert_eq!(
        got, expected,
        "the reverse index did not survive the reopen"
    );
}
