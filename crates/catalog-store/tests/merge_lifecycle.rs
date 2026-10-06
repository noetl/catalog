//! `tick()` must drive **merges**, not only sealing.
//!
//! ⚠ Both are caller-owned in `ehdb-l0`: the only thing the engine runs on its own is
//! the background uploader. A sealed part that is never merged stays, so parts
//! accumulate monotonically — the shape that filled a prod PVC while the writer still
//! reported `Ready`.
//!
//! # Why this needs a lowered `seal_max_records`
//!
//! Measured before writing the fix: 3,500 records produced **3 parts** and
//! `run_pending_merges()` performed **0**. `MergePolicy::d1` has `trigger_run_len: 4`
//! and `is_small_candidate` only counts parts that are already **durable**, so a
//! partition needs roughly `4 × seal_max_records` ops before a merge is even planned —
//! ~**4,096** at the default.
//!
//! A test that cannot reach the threshold proves nothing about the driver, so
//! `StoreConfig::with_seal_max_records` lowers it. ⚠ That is the point of the knob: an
//! untested merge driver is one nobody can show works.

use catalog_model::Entity;
use catalog_store::{CatalogStore, StoreConfig};

fn entity(path: &str, v: u32) -> Entity {
    Entity {
        resource_type: "playbook".into(),
        path: path.into(),
        version: v,
        entity_id: v as i64,
        content: None,
        content_sha256: format!("{v:064}"),
        archived_at: None,
    }
}

/// With a small part size, enough writes must make merges eligible — and `tick()` must
/// perform them.
#[test]
fn tick_performs_merges_once_they_become_eligible() {
    let dir = tempfile::tempdir().expect("td");
    // 8 records per part: 200 writes is ~25 parts, well past trigger_run_len = 4.
    let cfg = StoreConfig::new(dir.path()).with_seal_max_records(8);
    let mut s = CatalogStore::open(&cfg).expect("open");

    for v in 1..=200u32 {
        s.register(entity("p/merge", v)).expect("register");
    }

    // Let the background uploader make parts durable — `is_small_candidate` requires
    // `is_durable()`, so merges are not eligible until uploads land.
    let mut merged_total = 0usize;
    let mut before = s.part_counts();
    for _ in 0..40 {
        let t = s.tick().expect("tick");
        merged_total += t.merged;
        if merged_total > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let after = s.part_counts();

    println!(
        "parts entities before={} after={}   merges performed={}",
        before.entities, after.entities, merged_total
    );

    // ⚠ Assert the setup reached the threshold before asserting about the driver. If
    // few parts were ever created, zero merges is the correct answer and this test
    // would be reporting a pass for a driver it never exercised.
    before = s.part_counts();
    assert!(
        before.entities >= 4 || merged_total > 0,
        "the fixture produced only {} entity part(s) and no merges — below \
         trigger_run_len=4, so this test did not exercise the merge driver at all",
        before.entities
    );

    assert!(
        merged_total > 0,
        "tick() must perform merges once they are eligible; performed {merged_total} \
         across 40 ticks with {} entity parts. Before this fix nothing called \
         run_pending_merges at all.",
        before.entities
    );

    // And the data is intact afterwards — a merge rewrites parts, so this is the
    // assertion that matters more than the count.
    let versions: Vec<u32> = s
        .versions("p/merge")
        .expect("versions")
        .iter()
        .map(|e| e.version)
        .collect();
    assert_eq!(
        versions.len(),
        200,
        "every version must survive the merge; got {}",
        versions.len()
    );
    assert_eq!(versions.first(), Some(&1));
    assert_eq!(versions.last(), Some(&200));
    assert_eq!(
        s.latest("p/merge").expect("latest").map(|e| e.version),
        Some(200)
    );
}

/// A quiet store ticks cleanly and reports zero of both.
///
/// The companion reading: `merged: 0` is normal and must not be mistaken for a broken
/// driver, which is exactly why `Ticked` reports the two counts separately.
#[test]
fn a_quiet_store_ticks_to_zero_without_error() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    s.register(entity("p/quiet", 1)).expect("register");
    let t = s.tick().expect("tick");
    assert_eq!(t.sealed, 0, "a fresh part is younger than SEAL_MAX_AGE");
    assert_eq!(
        t.merged, 0,
        "one part cannot merge — trigger_run_len is 4, so zero here is CORRECT and not \
         evidence of a missing driver"
    );
    assert_eq!(
        s.latest("p/quiet").expect("latest").map(|e| e.version),
        Some(1),
        "ticking must not make a just-written record unreadable"
    );
}
