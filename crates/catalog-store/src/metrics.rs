//! Metrics for the catalog's lifecycle and ingestion counters.
//!
//! # Why this module exists
//!
//! `Ticked { sealed, merged, reclaimed }` and `Ingested { scanned, registered, skipped }`
//! were returned by value and **recorded nowhere**. A caller that ignored the return —
//! which is every caller that is not a test — left the catalog's only health signals
//! invisible.
//!
//! `reclaimed` is the one that matters most, and the reason is historical rather than
//! theoretical: catalog#11 found the merge driver **doubling disk** (94,784 → 189,568
//! bytes across a merge that cut parts 25 → 4), and nothing would have shown it. That
//! is the shape that filled a prod PVC while the writer still reported `Ready`.
//!
//! # ⚠⚠ Absence is the default, so every closed label set is pinned
//!
//! `Registry::gather` **prunes metric families with no children**, so a labelled metric
//! is absent from a scrape until something increments it. Registering it is not enough.
//! An absent series is then indistinguishable from:
//!
//! * a healthy zero,
//! * a broken exporter,
//! * and a binary too old to carry the metric at all.
//!
//! So every label value in a **closed** set is pinned at 0 here, and the pinning is
//! **unconditional** — not inside a config branch. `noetl/server#315` pinned its
//! publish-skip reasons inside `if event_bus_mode.publishes_ehdb()`, leaving them absent
//! on exactly the configuration whose reason someone would be reading.
//!
//! `catalog_build_info{version}` is always 1, so "does this binary predate the metric"
//! is answerable from the scrape itself rather than from a deployment's image tag — a
//! different representation, and one that can disagree with what is running
//! (`noetl/ai-meta#238`).

use prometheus::{IntCounterVec, IntGaugeVec, Opts, Registry};
use std::sync::OnceLock;

/// Outcomes of one [`crate::Ticked`]. A **closed** set, so all three are pinned.
pub const TICK_OUTCOMES: [&str; 3] = ["sealed", "merged", "reclaimed"];

/// Outcomes of one ingestion run. A **closed** set.
pub const INGEST_OUTCOMES: [&str; 3] = ["scanned", "registered", "skipped"];

/// Why a document was skipped. A **closed** set — it mirrors `catalog_ingest::SkipReason`
/// arm for arm, and `every_skip_reason_arm_is_pinned` fails if the two drift.
pub const SKIP_REASONS: [&str; 4] = ["unparseable", "no_kind", "no_metadata_path", "unknown_kind"];

struct Metrics {
    registry: Registry,
    tick: IntCounterVec,
    ingest: IntCounterVec,
    skip: IntCounterVec,
    // ⚠ No `build_info` field: it is registered and `.set(1)` once at init and never
    // read again, so holding a handle would be dead weight. The registry owns it, which
    // is what makes it appear in every render.
}

fn metrics() -> &'static Metrics {
    static M: OnceLock<Metrics> = OnceLock::new();
    M.get_or_init(|| {
        let registry = Registry::new();

        let tick = IntCounterVec::new(
            Opts::new(
                "catalog_tick_total",
                "EHDB lifecycle work performed by CatalogStore::tick, by outcome.",
            ),
            &["outcome"],
        )
        .expect("tick metric");
        let ingest = IntCounterVec::new(
            Opts::new(
                "catalog_ingest_total",
                "Documents seen by an ingestion run, by outcome. `scanned` is the denominator.",
            ),
            &["outcome"],
        )
        .expect("ingest metric");
        let skip = IntCounterVec::new(
            Opts::new(
                "catalog_ingest_skipped_total",
                "Documents an ingestion run skipped, by reason.",
            ),
            &["reason"],
        )
        .expect("skip metric");
        let build_info = IntGaugeVec::new(
            Opts::new(
                "catalog_build_info",
                "Always 1; the version label identifies the running binary.",
            ),
            &["version"],
        )
        .expect("build_info metric");

        registry.register(Box::new(tick.clone())).expect("reg tick");
        registry
            .register(Box::new(ingest.clone()))
            .expect("reg ingest");
        registry.register(Box::new(skip.clone())).expect("reg skip");
        registry
            .register(Box::new(build_info.clone()))
            .expect("reg build_info");

        // ⚠ Unconditional. Not behind a flag, not on first use — see the module note.
        for o in TICK_OUTCOMES {
            tick.with_label_values(&[o]);
        }
        for o in INGEST_OUTCOMES {
            ingest.with_label_values(&[o]);
        }
        for r in SKIP_REASONS {
            skip.with_label_values(&[r]);
        }
        build_info
            .with_label_values(&[env!("CARGO_PKG_VERSION")])
            .set(1);

        Metrics {
            registry,
            tick,
            ingest,
            skip,
        }
    })
}

/// Record one [`crate::Ticked`].
pub fn record_tick(sealed: usize, merged: usize, reclaimed: usize) {
    let m = metrics();
    m.tick.with_label_values(&["sealed"]).inc_by(sealed as u64);
    m.tick.with_label_values(&["merged"]).inc_by(merged as u64);
    m.tick
        .with_label_values(&["reclaimed"])
        .inc_by(reclaimed as u64);
}

/// Record one ingestion run's counts.
pub fn record_ingest(scanned: usize, registered: usize, skipped: usize) {
    let m = metrics();
    m.ingest
        .with_label_values(&["scanned"])
        .inc_by(scanned as u64);
    m.ingest
        .with_label_values(&["registered"])
        .inc_by(registered as u64);
    m.ingest
        .with_label_values(&["skipped"])
        .inc_by(skipped as u64);
}

/// Record one skip, by reason. An unknown reason is dropped rather than creating an
/// unpinned series — a new `SkipReason` arm must be added to [`SKIP_REASONS`], and
/// `every_skip_reason_arm_is_pinned` in `catalog-ingest` fails until it is.
pub fn record_skip(reason: &str) {
    if SKIP_REASONS.contains(&reason) {
        metrics().skip.with_label_values(&[reason]).inc();
    }
}

/// The running crate version, as `catalog_build_info` reports it.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Render the registry in Prometheus text format.
pub fn render() -> String {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let mut buf = Vec::new();
    // A render failure must not be silent: an empty scrape is exactly what a broken
    // exporter and a healthy-but-idle process both look like.
    if encoder
        .encode(&metrics().registry.gather(), &mut buf)
        .is_err()
    {
        return "# catalog metrics: encode failed\n".to_string();
    }
    String::from_utf8(buf).unwrap_or_else(|_| "# catalog metrics: non-utf8\n".to_string())
}

/// Touch the registry so the pinned series exist even if nothing else runs.
pub fn init() {
    let _ = metrics();
}
