//! Every `SkipReason` arm must have a pinned metric label, and every pinned label must
//! be reachable from a real skip.
//!
//! # Why this guard exists
//!
//! `metrics::SKIP_REASONS` is a hand-written array that mirrors the `SkipReason` enum.
//! Two copies of one list drift — that is the defect that broke both planner harnesses
//! in `noetl/travel` (eight bindings added to a playbook, never added to the harnesses,
//! `NameError` on every run, nothing watching). `record_skip` **drops** an unrecognised
//! reason rather than creating an unpinned series, which is the safe direction but a
//! silent one: a new arm would simply never be counted.
//!
//! So this asserts both directions, and counts what it checked.

use catalog_ingest::{ingest, SkipReason, Source};
use catalog_store::{metrics, CatalogStore, StoreConfig};

/// The label `record_skip` is called with for each arm — mirrors `lib.rs`.
fn label_of(r: &SkipReason) -> &'static str {
    match r {
        SkipReason::Unparseable(_) => "unparseable",
        SkipReason::NoKind => "no_kind",
        SkipReason::NoMetadataPath => "no_metadata_path",
        SkipReason::UnknownKind(_) => "unknown_kind",
    }
}

fn write(root: &std::path::Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).expect("mkdir");
    std::fs::write(p, body).expect("write");
}

#[test]
fn every_skip_reason_arm_is_pinned_and_every_pin_is_reachable() {
    // One instance of every arm. If a new arm is added to `SkipReason`, `label_of`
    // above stops compiling — the match is exhaustive — which is the loudest possible
    // reminder, and better than a runtime check that can be skipped.
    let all = [
        SkipReason::Unparseable("x".into()),
        SkipReason::NoKind,
        SkipReason::NoMetadataPath,
        SkipReason::UnknownKind("Dashboard".into()),
    ];
    println!(
        "SkipReason arms: {}   pinned labels: {}",
        all.len(),
        metrics::SKIP_REASONS.len()
    );
    assert_eq!(
        all.len(),
        metrics::SKIP_REASONS.len(),
        "the enum and the pinned label list have different lengths — one has drifted"
    );

    // Direction 1: every arm's label is pinned.
    for r in &all {
        let l = label_of(r);
        assert!(
            metrics::SKIP_REASONS.contains(&l),
            "SkipReason arm {r:?} maps to label {l:?}, which is NOT in SKIP_REASONS, so \
             record_skip will silently drop it and the reason will never be counted"
        );
    }

    // Direction 2: every pinned label is produced by some arm — a pin nothing can reach
    // is an inert series that reads 0 forever and looks healthy.
    let produced: Vec<&str> = all.iter().map(label_of).collect();
    for l in metrics::SKIP_REASONS {
        assert!(
            produced.contains(&l),
            "pinned label {l:?} is produced by no SkipReason arm — it would read 0 forever"
        );
    }
}

/// An end-to-end check that a real ingestion moves the real counters, so the wiring
/// between `ingest` and `record_*` is exercised rather than assumed.
#[test]
fn a_real_ingestion_moves_the_counters() {
    metrics::init();
    let dir = tempfile::tempdir().expect("td");
    let src = dir.path().join("src");
    write(
        &src,
        "pb/ok.yaml",
        "kind: Playbook\nmetadata:\n  path: a/b\n",
    );
    write(&src, "pb/broken.yaml", "kind: Playbook\n  bad: [unclosed\n");
    write(&src, "pb/nokind.yaml", "metadata:\n  path: c/d\n");

    let mut store = CatalogStore::open(&StoreConfig::new(dir.path().join("store"))).expect("open");
    let before = metrics::render();
    let res = ingest(&mut store, &Source::Dir(src), "pb", 1).expect("ingest");
    let after = metrics::render();

    println!("{}", res.summary());
    assert_eq!(res.scanned, 3);
    assert_eq!(res.registered, 1);
    assert_eq!(res.skipped.len(), 2);

    // ⚠ Compare before/after rather than asserting an absolute, because the registry is
    // a process global and another test in this binary may have already incremented it.
    // An absolute assertion here would pass or fail on test ORDER.
    let val = |text: &str, series: &str| -> u64 {
        text.lines()
            .find(|l| l.starts_with(series))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let d = |s: &str| val(&after, s) - val(&before, s);
    println!(
        "deltas: scanned=+{} registered=+{} skipped=+{}",
        d("catalog_ingest_total{outcome=\"scanned\"}"),
        d("catalog_ingest_total{outcome=\"registered\"}"),
        d("catalog_ingest_total{outcome=\"skipped\"}")
    );
    assert_eq!(d("catalog_ingest_total{outcome=\"scanned\"}"), 3);
    assert_eq!(d("catalog_ingest_total{outcome=\"registered\"}"), 1);
    assert_eq!(d("catalog_ingest_total{outcome=\"skipped\"}"), 2);
    assert_eq!(d("catalog_ingest_skipped_total{reason=\"unparseable\"}"), 1);
    assert_eq!(d("catalog_ingest_skipped_total{reason=\"no_kind\"}"), 1);
}
