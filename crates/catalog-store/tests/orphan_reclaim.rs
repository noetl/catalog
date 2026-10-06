//! A merge leaves its source parts on disk. `tick()` drives merges, so `tick()` must
//! also reclaim.
//!
//! # What was measured before this test existed
//!
//! 200 writes at `seal_max_records = 8` produced 25 parts; one `tick()` merged them down
//! to **4 live parts** — and disk went **up**:
//!
//! ```text
//! parts  before=25  after=4
//! tick   sealed=0 merged=3
//! disk   before: 75 files 266,383 bytes
//! disk   after : 83 files 374,975 bytes   (+8 files, +108,592 bytes)
//! ```
//!
//! Broken down, with the manifest claiming **4** parts:
//!
//! ```text
//! parts/c1_catalog_entity/shard-0             26 files
//! parts/c1_catalog_entity/shard-0/merged       3 files
//! substrate/parts/c1_catalog_entity/shard-0   28 files   94,784 bytes
//! ```
//!
//! ⚠ **54 part files and objects for 4 live parts.** `ehdb-l0`'s `reclaim_orphans` is
//! documented as deleting exactly these — "chiefly the superseded source parts a merge
//! (L0.3) leaves behind" — and it is **caller-owned**. Nothing called it, so the merge
//! driver added in the previous phase traded a growing *part count* for a growing
//! *byte count* and reported success either way.
//!
//! This is the shape that filled a prod PVC while the writer still reported `Ready`
//! (ai-meta, 2026-09-01): a cost that only grows, with nothing bounding it and nothing
//! reporting it.
//!
//! ⚠ Note what the earlier `merge_lifecycle` test asserted: that the **part count** fell
//! 25 → 4. It did, and it passed, while bytes on disk rose 42%. Measuring the number the
//! fix was about hid the number that mattered.

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

/// Count `*.eslog` part files anywhere under `root` — both the local part directory and
/// the substrate, since orphans accumulate in both.
fn part_files(root: &std::path::Path) -> (usize, u64) {
    fn walk(p: &std::path::Path, n: &mut usize, b: &mut u64) {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let meta = match e.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.is_dir() {
                    walk(&e.path(), n, b);
                } else if e.path().extension().and_then(|s| s.to_str()) == Some("eslog") {
                    *n += 1;
                    *b += meta.len();
                }
            }
        }
    }
    let (mut n, mut b) = (0, 0);
    walk(root, &mut n, &mut b);
    (n, b)
}

#[test]
fn tick_reclaims_the_parts_a_merge_superseded() {
    let dir = tempfile::tempdir().expect("td");
    let root = dir.path().to_path_buf();
    let mut store =
        CatalogStore::open(&StoreConfig::new(&root).with_seal_max_records(8)).expect("open");

    for i in 1..=200u32 {
        store.register(entity("pb/a.yaml", i)).expect("register");
    }

    // ⚠ Assert the extraction before asserting about it. If the walker found no part
    // files at all, every ratio below would read perfectly healthy — which is the same
    // output as a repo with no defect.
    let (files_before, bytes_before) = part_files(&root);
    println!("before tick: {files_before} part file(s), {bytes_before} bytes");
    assert!(
        files_before >= 20,
        "found only {files_before} part file(s) before the merge — the walker is wrong, \
         so any 'reclaimed' verdict below would be meaningless"
    );

    std::thread::sleep(std::time::Duration::from_secs(6));
    let ticked = store.tick().expect("tick");
    let live = store.part_counts().entities;
    let (files_after, bytes_after) = part_files(&root);

    println!(
        "tick: sealed={} merged={} reclaimed={}",
        ticked.sealed, ticked.merged, ticked.reclaimed
    );
    println!("live parts per manifest: {live}");
    println!("after tick:  {files_after} part file(s), {bytes_after} bytes");

    // A positive control on the merge itself: if nothing merged there are no orphans to
    // reclaim and this test would pass vacuously.
    assert!(
        ticked.merged > 0,
        "no merge happened, so this test proves nothing about reclaim"
    );
    assert!(
        ticked.reclaimed > 0,
        "{} merge(s) ran and reclaimed 0 — the superseded sources are still on disk",
        ticked.merged
    );

    // The real assertion: part files on disk must be bounded by what the manifest
    // references, not by how many writes have ever happened. Allowance of 2x covers the
    // local file + its substrate object for each live part.
    let ceiling = live * 2 + 4;
    assert!(
        files_after <= ceiling,
        "manifest references {live} part(s) but {files_after} part file(s) remain on \
         disk (ceiling {ceiling}); bytes {bytes_before} -> {bytes_after}. The merge \
         sources were never reclaimed."
    );
    // ⚠ This assertion was wrong on its first draft, and the code was right. It read
    // `bytes_after < bytes_before`, expecting a merge to *shrink* storage. It does not:
    // a merge consolidates 25 parts holding 200 records into 4 parts holding the same
    // 200 records, so the byte total is flat by construction. Measured: 94,784 ->
    // 96,736, a 1.02x merge-overhead rise.
    //
    // The property that actually matters — and the one the pre-fix state violated — is
    // that bytes must not *grow* with merging. Without reclaim the same run measured
    // 94,784 -> 189,568, exactly **2.00x**, because every superseded source stayed. So
    // a 1.10x ceiling still separates fixed from broken by a wide margin, while not
    // asserting a shrink that was never going to happen.
    let ratio = bytes_after as f64 / bytes_before as f64;
    println!("byte ratio across the merge: {ratio:.2}x");
    assert!(
        ratio <= 1.10,
        "bytes grew {bytes_before} -> {bytes_after} ({ratio:.2}x) across a merge that \
         cut parts to {live}. Without reclaim this reads 2.00x; a merge must not cost \
         storage proportional to how many parts it consumed."
    );
}
