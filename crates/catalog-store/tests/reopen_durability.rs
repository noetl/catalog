//! The store must keep working across a reopen.
//!
//! ⚠ This is a real defect in the first cut of `CatalogStore`, found by reading
//! `ehdb-l0`'s own contract rather than by a failing test:
//!
//! > **Contract:** records are appended in **ascending `sort_key` order within a
//! > partition** (the single writer guarantees this) — the sparse index, MinMax
//! > pruning, and merge all rely on it.
//!
//! `next_seq` was initialised to `1` in `open()`, so a store reopened against an
//! existing root re-emitted op_seq 1, 2, 3… **below** the ops already there. Nothing
//! errors: the append succeeds, and the damage is to the index and merge invariants
//! the engine is entitled to assume.
//!
//! A catalog is exactly the shape that hits this. It is written rarely and the process
//! restarts often, so *almost every* write after the first deploy is a post-reopen
//! write.

use catalog_model::{Attribute, AttributeValue, Entity};
use catalog_store::{CatalogStore, StoreConfig};

fn entity(path: &str, version: u32) -> Entity {
    Entity {
        resource_type: "playbook".into(),
        path: path.into(),
        version,
        entity_id: version as i64,
        content: None,
        content_sha256: format!("{version:064}"),
        archived_at: None,
    }
}

/// Reopen the same root and keep appending. Every version must survive.
#[test]
fn op_seq_does_not_restart_when_the_store_is_reopened() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = StoreConfig::new(dir.path());
    let path = "p/reopen";

    // --- session 1 ---
    let first_seqs: Vec<u64> = {
        let mut s = CatalogStore::open(&cfg).expect("open 1");
        let a = s.register(entity(path, 1)).expect("v1");
        let b = s.register(entity(path, 2)).expect("v2");
        s.tick().expect("tick");
        vec![a, b]
    }; // dropped — the engine's background uploader joins here

    // --- session 2, SAME root ---
    let mut s = CatalogStore::open(&cfg).expect("open 2");
    let c = s.register(entity(path, 3)).expect("v3");

    println!("session 1 op_seqs: {first_seqs:?}   session 2 first op_seq: {c}");

    assert!(
        c > *first_seqs.last().unwrap(),
        "op_seq must CONTINUE across a reopen, not restart. Session 1 ended at {} and \
         session 2 began at {c}. ehdb-l0's Dataset contract requires ascending sort_key \
         within a partition — the sparse index, MinMax pruning and merge all rely on it, \
         and an append below the existing maximum violates it WITHOUT erroring.",
        first_seqs.last().unwrap()
    );

    // And the user-visible consequence: every version must still be readable.
    let versions: Vec<u32> = s
        .versions(path)
        .expect("versions")
        .iter()
        .map(|e| e.version)
        .collect();
    assert_eq!(
        versions,
        vec![1, 2, 3],
        "all three versions must survive the reopen; got {versions:?}"
    );
    assert_eq!(
        s.latest(path).expect("latest").map(|e| e.version),
        Some(3),
        "the newest version must win after a reopen"
    );
}

/// The same property for attributes, which fold per NAME rather than per version.
#[test]
fn attributes_written_across_a_reopen_all_survive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = StoreConfig::new(dir.path());
    let path = "p/attrs";

    {
        let mut s = CatalogStore::open(&cfg).expect("open 1");
        s.register(entity(path, 1)).expect("register");
        s.set_attribute(path, Attribute::new(1, "a", AttributeValue::Integer(1)))
            .expect("a");
        s.tick().expect("tick");
    }

    let mut s = CatalogStore::open(&cfg).expect("open 2");
    s.set_attribute(path, Attribute::new(1, "b", AttributeValue::Integer(2)))
        .expect("b");

    let got = s.attributes(path).expect("read");
    assert_eq!(
        got.len(),
        2,
        "an attribute written before the reopen and one after must BOTH survive; got {:?}",
        got.keys().collect::<Vec<_>>()
    );
    assert!(got.contains_key("a") && got.contains_key("b"));
}

/// A later write to one attribute name, across a reopen, must still win.
///
/// ⚠ This is the assertion a restarted op_seq breaks most visibly: the fold takes the
/// LAST op per name in iteration order, so a post-reopen write carrying a LOWER sort key
/// can be ordered before the write it is supposed to supersede.
#[test]
fn a_post_reopen_overwrite_still_wins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = StoreConfig::new(dir.path());
    let path = "p/over";

    {
        let mut s = CatalogStore::open(&cfg).expect("open 1");
        s.register(entity(path, 1)).expect("register");
        s.set_attribute(
            path,
            Attribute::new(1, "tier", AttributeValue::Text("dev".into())),
        )
        .expect("first");
        s.tick().expect("tick");
    }

    let mut s = CatalogStore::open(&cfg).expect("open 2");
    s.set_attribute(
        path,
        Attribute::new(1, "tier", AttributeValue::Text("prod".into())),
    )
    .expect("second");

    let got = s.attributes(path).expect("read");
    assert_eq!(
        got.len(),
        1,
        "one name, one value: {:?}",
        got.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        got["tier"].value,
        AttributeValue::Text("prod".into()),
        "the write made AFTER the reopen must win. If this reads \"dev\", a restarted \
         op_seq has ordered the newer write before the older one."
    );
}
