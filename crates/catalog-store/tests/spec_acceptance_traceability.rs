//! Every acceptance criterion in `design/catalog-model.md` is either **cited to a real
//! test** or **recorded as deliberately open with a reason**. Nothing in between.
//!
//! # Why this is a test and not a checklist
//!
//! `representation-drift.md` is about exactly this: a table of ticked boxes is a copy of
//! reality, true only while something forces it to agree. The repo's own history has
//! three instances of shipped work with unticked boxes (#194 T0–T5, ehdb#241 phases
//! 6–10, #201) and they were found by reading issues against a cluster, not by grep.
//!
//! So the citation lives in code: each AC names a test function, and this asserts the
//! function **exists in the file it claims**. A citation to a test that was renamed or
//! deleted fails here rather than rotting into a confident tick.
//!
//! ⚠⚠ **The third state is the point.** A traceability test that only checks "does every
//! AC have a citation" would pass by letting someone cite anything, or would tempt a
//! fake citation for an AC that genuinely is not met. AC10 is not met — it needs the
//! real server, and this crate is not wired into it — so it is recorded as `Open(reason)`
//! and **printed as a non-empty finding**. The audit's job is to make that visible every
//! run, not to report green.

use std::path::{Path, PathBuf};

/// How an AC is discharged.
enum Status {
    /// Proven by these `(test file, test fn)` pairs.
    Cited(&'static [(&'static str, &'static str)]),
    /// Deliberately not met, with the reason. Printed every run.
    Open(&'static str),
}

/// The ten criteria from `design/catalog-model.md` §11, with how each is discharged.
///
/// ⚠ Paths are workspace-relative so the assertion is about a file on disk, not about a
/// string this array happens to contain.
const CRITERIA: [(&str, &str, Status); 10] = [
    (
        "AC1",
        "a catalog write is an EHDB append; no other store is written",
        Status::Cited(&[(
            "crates/catalog-store/tests/spec_acceptance_traceability.rs",
            "ac1_the_crate_runs_no_sql_and_links_no_database_driver",
        )]),
    ),
    (
        "AC2",
        "a read after an append sees it without a flush",
        Status::Cited(&[(
            "crates/catalog-store/tests/acceptance.rs",
            "ac2_a_read_after_an_append_sees_it_without_a_flush",
        )]),
    ),
    (
        "AC3",
        "adding a resource type adds no Dataset impl",
        Status::Cited(&[
            (
                "crates/catalog-store/tests/ac3_dataset_count_is_fixed.rs",
                "the_dataset_count_is_exactly_four_and_must_stay_four",
            ),
            (
                "crates/catalog-model/tests/adding_a_resource_type_costs_no_schema.rs",
                "a_brand_new_resource_type_needs_no_new_storage_shape",
            ),
        ]),
    ),
    (
        "AC4",
        "multiple attributes on one entity all survive the fold (positive control)",
        Status::Cited(&[
            (
                "crates/catalog-store/tests/acceptance.rs",
                "ac4_every_attribute_on_one_entity_survives_the_fold",
            ),
            (
                "crates/catalog-store/tests/acceptance.rs",
                "ac4b_repeated_writes_to_one_name_collapse_to_the_latest",
            ),
        ]),
    ),
    (
        "AC5",
        "a relation records its provenance",
        Status::Cited(&[(
            "crates/catalog-store/tests/acceptance.rs",
            "a_relation_round_trips_with_its_provenance",
        )]),
    ),
    (
        "AC6",
        "version exceeds 32,767",
        Status::Cited(&[(
            "crates/catalog-store/tests/acceptance.rs",
            "ac6_a_version_above_the_smallint_ceiling_round_trips_through_the_store",
        )]),
    ),
    (
        "AC7",
        "seal_max_age is set AND a timer drives seal_aged_parts",
        Status::Cited(&[
            (
                "crates/catalog-store/tests/acceptance.rs",
                "ac7_the_sealer_is_driven_and_not_merely_configured",
            ),
            (
                "crates/catalog-store/tests/merge_lifecycle.rs",
                "tick_performs_merges_once_they_become_eligible",
            ),
            (
                "crates/catalog-store/tests/orphan_reclaim.rs",
                "tick_reclaims_the_parts_a_merge_superseded",
            ),
        ]),
    ),
    (
        "AC8",
        "a kind filter is case-insensitive",
        // Satisfied by the c1 type index, which lowercases its key. This citation was
        // ADDED BY THIS AUDIT: the behaviour and its test already existed, and a sweep
        // for "AC8" across the crate found ZERO mentions — covered but untraceable,
        // which is how a criterion quietly loses its proof.
        Status::Cited(&[(
            "crates/catalog-store/tests/type_index.rs",
            "listing_returns_every_resource_of_the_type_not_one",
        )]),
    ),
    (
        "AC9",
        "content can be projected away while identity survives",
        Status::Cited(&[(
            "crates/catalog-store/tests/spec_acceptance_traceability.rs",
            "ac9_identity_survives_when_content_is_projected_away",
        )]),
    ),
    (
        "AC10",
        "the existing /api/catalog wire shapes are unchanged; the e2e register->execute loop passes",
        Status::Open(
            "NOT MET, and not fakeable from inside this crate. It requires the real \
             noetl-server, and nothing in noetl/server links catalog-store yet — the \
             crate has no consumer on any serving path. Discharging it needs a server \
             integration, which is a separate change with its own owner gate. Recorded \
             rather than ticked.",
        ),
    ),
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/catalog-store.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn every_acceptance_criterion_is_cited_or_recorded_open() {
    let root = workspace_root();
    let mut cited = 0usize;
    let mut open: Vec<(&str, &str)> = Vec::new();
    let mut broken: Vec<String> = Vec::new();

    for (id, what, status) in &CRITERIA {
        match status {
            Status::Cited(refs) => {
                assert!(!refs.is_empty(), "{id} claims Cited with no citations");
                for (file, func) in *refs {
                    let p = root.join(file);
                    let Ok(text) = std::fs::read_to_string(&p) else {
                        broken.push(format!("{id}: cited file {file} does not exist"));
                        continue;
                    };
                    // ⚠ Assert the extraction: an empty read would make the `contains`
                    // below fail for the right reason, but a truncated one might not.
                    if text.len() < 100 {
                        broken.push(format!(
                            "{id}: cited file {file} is only {} bytes",
                            text.len()
                        ));
                        continue;
                    }
                    if !text.contains(&format!("fn {func}(")) {
                        broken.push(format!(
                            "{id}: cited test `{func}` is NOT in {file} — renamed or deleted, \
                             so the criterion has lost its proof"
                        ));
                    }
                }
                cited += 1;
                println!("  {id}  CITED      {what}");
            }
            Status::Open(reason) => {
                open.push((id, reason));
                println!("  {id}  OPEN       {what}");
            }
        }
    }

    println!(
        "\nacceptance criteria: {} total, {} cited, {} deliberately open, {} broken citations",
        CRITERIA.len(),
        cited,
        open.len(),
        broken.len()
    );
    for (id, reason) in &open {
        println!("\n  ⚠ {id} is OPEN: {reason}");
    }

    assert!(
        broken.is_empty(),
        "citations that no longer resolve:\n  {}",
        broken.join("\n  ")
    );
    // ⚠ Deliberately does NOT assert `open.is_empty()`. An audit that fails on a known
    // gap gets disabled; one that prints it every run keeps it visible. The gap is
    // tracked in STATUS and on noetl/ai-meta#427.
    assert_eq!(CRITERIA.len(), 10, "the spec declares ten criteria");
}

/// **AC1** — this crate executes no SQL and links no database driver.
///
/// ⚠ The naive form of this check reports a false violation. There IS a `"SELECT 1;"`
/// literal in the workspace, inside a YAML fixture's `command:` field — SQL in a
/// *document the catalog catalogues*, not SQL the catalog runs. A grep that cannot tell
/// those apart fails on correct code, which is the worse direction: it trains people to
/// ignore the check.
#[test]
fn ac1_the_crate_runs_no_sql_and_links_no_database_driver() {
    let root = workspace_root();

    // No DB driver in any manifest.
    let drivers = ["sqlx", "tokio-postgres", "diesel", "rusqlite", "mysql"];
    let mut manifests = 0;
    for entry in ["Cargo.toml"].iter().map(|f| root.join(f)).chain(
        [
            "catalog-model",
            "catalog-store",
            "catalog-extract",
            "catalog-ingest",
        ]
        .iter()
        .map(|c| root.join("crates").join(c).join("Cargo.toml")),
    ) {
        let text = std::fs::read_to_string(&entry).expect("manifest");
        manifests += 1;
        for d in drivers {
            assert!(
                !text.contains(d),
                "{} links {d}, which AC1 forbids",
                entry.display()
            );
        }
    }
    println!("manifests checked: {manifests} (1 workspace + 4 crates)");
    assert_eq!(manifests, 5, "the manifest sweep missed a crate");

    // No SQL executed from library/binary sources. Tests are excluded by path, because
    // a FIXTURE legitimately contains SQL — see the note above.
    let mut src_files = 0;
    let mut offenders = Vec::new();
    for c in [
        "catalog-model",
        "catalog-store",
        "catalog-extract",
        "catalog-ingest",
    ] {
        let dir = root.join("crates").join(c).join("src");
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).expect("read_dir").flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                    src_files += 1;
                    let text = std::fs::read_to_string(&p).expect("src");
                    for line in text.lines() {
                        let t = line.trim_start();
                        // Skip comments: the prose in this crate discusses SQL at length.
                        if t.starts_with("//") || t.starts_with("*") {
                            continue;
                        }
                        let low = line.to_lowercase();
                        if low.contains("\"select ")
                            || low.contains("\"insert into")
                            || low.contains("\"update ")
                            || low.contains("\"delete from")
                        {
                            offenders.push(format!("{}: {}", p.display(), line.trim()));
                        }
                    }
                }
            }
        }
    }
    println!("source files scanned: {src_files} (comments excluded, tests excluded by path)");
    assert!(
        src_files >= 10,
        "only {src_files} sources scanned — the walk is wrong"
    );
    assert!(
        offenders.is_empty(),
        "SQL in non-test sources: {offenders:?}"
    );
}

/// **AC9** — a listing can drop `content` while identity survives.
#[test]
fn ac9_identity_survives_when_content_is_projected_away() {
    use catalog_model::Entity;
    use catalog_store::{CatalogStore, StoreConfig};

    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    let body = "kind: Playbook\nmetadata:\n  path: a/b\n";
    store
        .register(Entity {
            resource_type: "playbook".into(),
            path: "a/b".into(),
            version: 3,
            entity_id: 7,
            content: Some(body.to_string()),
            content_sha256: "f".repeat(64),
            archived_at: None,
        })
        .expect("reg");
    // And one with content already projected away, which is what a listing stores.
    store
        .register(Entity {
            resource_type: "playbook".into(),
            path: "a/c".into(),
            version: 1,
            entity_id: 8,
            content: None,
            content_sha256: "e".repeat(64),
            archived_at: None,
        })
        .expect("reg");

    let with = store.latest("a/b").expect("latest").expect("some");
    let without = store.latest("a/c").expect("latest").expect("some");

    // Identity survives in both: path, version and the digest.
    for e in [&with, &without] {
        assert!(!e.path.is_empty());
        assert_eq!(e.content_sha256.len(), 64);
    }
    assert_eq!(with.version, 3);
    assert_eq!(without.version, 1);

    // ⚠ The digest is NOT derived from `content`, which is what makes projecting content
    // away safe: a listing keeps a stable identity for a body it no longer holds.
    assert!(without.content.is_none(), "content should be absent");
    assert_eq!(
        without.content_sha256,
        "e".repeat(64),
        "the digest must survive content being projected away"
    );

    // A type listing returns both, content or not.
    let listed = store.resources_of_type("playbook").expect("list");
    assert_eq!(listed, vec!["a/b".to_string(), "a/c".to_string()]);
}
