//! Two properties, and the first is the one that is normally missing.
//!
//! 1. **Every closed label value is present at 0 before anything fires.**
//!    `Registry::gather` prunes empty metric families, so a labelled metric is absent
//!    from a scrape until something increments it. An absent series is
//!    indistinguishable from a healthy zero, a broken exporter, and a binary too old to
//!    carry the metric. The prod gateway once served a **200 with zero bytes** for
//!    exactly this reason.
//! 2. **They actually move.** A pinned-but-never-incremented metric is the inert-gate
//!    shape: it looks instrumented and reports nothing.
//!
//! ⚠ Both properties live in ONE test, because the metrics registry is a process
//! global: a separate test asserting "all zero" would race one asserting "non-zero".
//! `cargo test` does **not** serialise tests, whatever a convenient SAFETY note might
//! claim. The second test in this file is safe to run alongside only because it
//! increments nothing — an undeclared reason is dropped, by design.
//!
//! # The RED, which reproduces the prod shape exactly
//!
//! With the unconditional pinning removed, the entire scrape is **155 bytes**:
//!
//! ```text
//! # HELP catalog_build_info Always 1; the version label identifies the running binary.
//! # TYPE catalog_build_info gauge
//! catalog_build_info{version="0.1.0"} 1
//! ```
//!
//! All three counter families **completely absent** — pruned by `Registry::gather`
//! because nothing had incremented them. That is the shape in which the prod gateway
//! served a 200 with zero bytes.
//!
//! ⭐ And note what survived: `build_info`, because it is a gauge that is `.set(1)`
//! rather than a counter waiting to be incremented. That is exactly why it is the right
//! discriminator — this scrape says "the binary has the metrics code, and nothing is
//! pinned or firing", which is a different diagnosis from an empty response.

use catalog_store::metrics;

#[test]
fn every_closed_label_value_is_pinned_and_then_moves() {
    metrics::init();
    let before = metrics::render();
    println!(
        "--- rendered before any work ({} bytes)\n{before}",
        before.len()
    );

    // ⚠ Assert the extraction before asserting about it: an empty render would make
    // every "is present" check below vacuously... fail, but a tiny one could pass a
    // sloppier check.
    assert!(
        before.len() > 200,
        "render produced {} bytes — too small to be a real scrape",
        before.len()
    );

    // --- property 1: pinned at zero, every closed label value present ---
    let mut missing = Vec::new();
    for o in metrics::TICK_OUTCOMES {
        let want = format!("catalog_tick_total{{outcome=\"{o}\"}} 0");
        if !before.contains(&want) {
            missing.push(want);
        }
    }
    for o in metrics::INGEST_OUTCOMES {
        let want = format!("catalog_ingest_total{{outcome=\"{o}\"}} 0");
        if !before.contains(&want) {
            missing.push(want);
        }
    }
    for r in metrics::SKIP_REASONS {
        let want = format!("catalog_ingest_skipped_total{{reason=\"{r}\"}} 0");
        if !before.contains(&want) {
            missing.push(want);
        }
    }
    println!(
        "pinned series checked: {} (3 tick + 3 ingest + 4 skip), missing: {}",
        metrics::TICK_OUTCOMES.len() + metrics::INGEST_OUTCOMES.len() + metrics::SKIP_REASONS.len(),
        missing.len()
    );
    assert!(
        missing.is_empty(),
        "these series are ABSENT from a fresh scrape, so a reader cannot tell 0 from \
         never-fired: {missing:?}"
    );

    // build_info carries the version, so "does this binary predate the metric" is
    // answerable from the scrape rather than from a deployment's image tag.
    let bi = format!("catalog_build_info{{version=\"{}\"}} 1", metrics::version());
    assert!(before.contains(&bi), "missing {bi}");

    // --- property 2: they move ---
    metrics::record_tick(2, 3, 48);
    metrics::record_ingest(53, 49, 4);
    metrics::record_skip("no_kind");
    metrics::record_skip("unknown_kind");

    let after = metrics::render();
    for want in [
        "catalog_tick_total{outcome=\"sealed\"} 2",
        "catalog_tick_total{outcome=\"merged\"} 3",
        // The one whose invisibility was the silent cost catalog#11 fixed.
        "catalog_tick_total{outcome=\"reclaimed\"} 48",
        "catalog_ingest_total{outcome=\"scanned\"} 53",
        "catalog_ingest_total{outcome=\"registered\"} 49",
        "catalog_ingest_total{outcome=\"skipped\"} 4",
        "catalog_ingest_skipped_total{reason=\"no_kind\"} 1",
        "catalog_ingest_skipped_total{reason=\"unknown_kind\"} 1",
    ] {
        assert!(
            after.contains(want),
            "counter did not move: expected {want}\n{after}"
        );
    }

    // And the reasons that did NOT fire must still read 0, not vanish.
    assert!(after.contains("catalog_ingest_skipped_total{reason=\"unparseable\"} 0"));
    assert!(after.contains("catalog_ingest_skipped_total{reason=\"no_metadata_path\"} 0"));
}

/// An unrecognised reason must not create an unpinned series — that would reintroduce
/// the absent-is-not-zero bug on exactly the new value.
#[test]
fn an_unknown_skip_reason_creates_no_unpinned_series() {
    metrics::init();
    metrics::record_skip("a_reason_nobody_declared");
    let r = metrics::render();
    assert!(
        !r.contains("a_reason_nobody_declared"),
        "an undeclared reason created an unpinned series"
    );
}
