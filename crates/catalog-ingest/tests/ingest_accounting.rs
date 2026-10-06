//! Ingestion must account for every file it scanned, and say why it skipped each one.
//!
//! # The failure this guards
//!
//! A run that silently drops files reports `scanned=N registered=N skipped=0` — the same
//! output as a healthy run. The only way to tell them apart is to publish the
//! denominator and check it balances, which is why [`Ingested::accounts_for_every_file`]
//! exists and why the binary turns a mismatch into a non-zero exit.

use catalog_ingest::{ingest, sha256_hex, SkipReason, Source};
use catalog_store::{CatalogStore, StoreConfig};

fn write(root: &std::path::Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).expect("mkdir");
    std::fs::write(p, body).expect("write");
}

/// Mirrors `adiona/playbooks/catalog_list.yaml`.
const GOOD: &str = r#"
apiVersion: noetl.io/v2
kind: Playbook
metadata:
  name: adiona_catalog_list
  path: adiona/v1/catalog_list
workflow:
  - step: start
    tool:
      kind: postgres
      auth: adiona_actor
      command: "SELECT 1;"
"#;

#[test]
fn every_scanned_file_is_either_registered_or_skipped_with_a_reason() {
    let dir = tempfile::tempdir().expect("td");
    let src = dir.path().join("src");
    let store_dir = dir.path().join("store");

    write(&src, "pb/good.yaml", GOOD);
    write(
        &src,
        "pb/also_good.yaml",
        &GOOD.replace("catalog_list", "other"),
    );
    // One of each skip reason, so each arm is exercised rather than merely present.
    write(&src, "pb/broken.yaml", "kind: Playbook\n  bad: [unclosed\n");
    write(&src, "pb/no_kind.yaml", "metadata:\n  path: a/b\n");
    write(
        &src,
        "pb/no_path.yaml",
        "kind: Playbook\nmetadata:\n  name: x\n",
    );
    write(
        &src,
        "pb/other_kind.yaml",
        "kind: Dashboard\nmetadata:\n  path: d/1\n",
    );
    // Not YAML at all — must not even be scanned.
    write(&src, "pb/README.md", "# not yaml\n");

    let mut store = CatalogStore::open(&StoreConfig::new(&store_dir)).expect("open");
    let res = ingest(&mut store, &Source::Dir(src.clone()), "pb", 1_700_000_000).expect("ingest");

    println!("{}", res.summary());
    for (p, r) in &res.skipped {
        println!("  skipped {} — {r}", p.display());
    }

    // ⚠ The denominator. 6 yaml files; the .md is not a candidate.
    assert_eq!(
        res.scanned, 6,
        "the walker must see exactly the 6 yaml files"
    );
    assert_eq!(res.registered, 2);
    assert_eq!(res.skipped.len(), 4);
    assert!(
        res.accounts_for_every_file(),
        "scanned {} != registered {} + skipped {}",
        res.scanned,
        res.registered,
        res.skipped.len()
    );

    // Each reason actually occurred — a test that only counts skips would pass if all
    // four collapsed into one arm.
    // Keyed by arm name rather than `mem::discriminant`, which is not `Ord`.
    let reasons: std::collections::BTreeSet<&str> = res
        .skipped
        .iter()
        .map(|(_, r)| match r {
            SkipReason::Unparseable(_) => "unparseable",
            SkipReason::NoMetadataPath => "no_metadata_path",
            SkipReason::NoKind => "no_kind",
            SkipReason::UnknownKind(_) => "unknown_kind",
        })
        .collect();
    println!("skip arms exercised: {reasons:?}");
    assert_eq!(reasons.len(), 4, "all four skip arms must be exercised");
    assert!(res
        .skipped
        .iter()
        .any(|(_, r)| matches!(r, SkipReason::UnknownKind(k) if k == "Dashboard")));
    assert!(res
        .skipped
        .iter()
        .any(|(_, r)| *r == SkipReason::NoMetadataPath));
    assert!(res.skipped.iter().any(|(_, r)| *r == SkipReason::NoKind));

    assert_eq!(res.by_kind.get("playbook"), Some(&2));
    // 2 registrations x (uses_tool.postgres + uses_credential.adiona_actor).
    assert_eq!(res.attributes, 4, "the two playbook facts, per document");

    // And the data is readable back through the fold, not merely appended.
    let v = store.versions("adiona/v1/catalog_list").expect("versions");
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].resource_type, "playbook");
    let attrs = store.attributes("adiona/v1/catalog_list").expect("attrs");
    assert!(attrs.contains_key("uses_credential.adiona_actor"));
    assert!(attrs.contains_key("uses_tool.postgres"));
}

#[test]
fn an_empty_source_is_reported_not_hidden() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path().join("store"))).expect("open");
    let res = ingest(
        &mut store,
        &Source::Dir(dir.path().join("nothing-here")),
        "pb",
        1,
    )
    .expect("ingest");
    assert_eq!(res.scanned, 0);
    assert_eq!(res.registered, 0);
    // Balances trivially — which is the point: `accounts_for_every_file` alone cannot
    // distinguish an empty source from a correct one, so the caller must also look at
    // `scanned`. The binary prints a warning on zero for exactly this reason.
    assert!(res.accounts_for_every_file());
}

/// A bad git ref must be an error, never an empty listing. An empty listing satisfies
/// "0 skipped, 0 unparseable" and reads as a clean run.
#[test]
fn a_bad_git_ref_errors_rather_than_scanning_nothing() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path().join("store"))).expect("open");
    let src = Source::GitRef {
        repo: dir.path().to_path_buf(),
        reference: "refs/heads/definitely-not-a-ref".into(),
    };
    let err = ingest(&mut store, &src, "", 1).expect_err("a bad ref must fail");
    println!("error: {err}");
    assert!(
        err.to_string().contains("ls-tree") || err.kind() == std::io::ErrorKind::NotFound,
        "unexpected error: {err}"
    );
}

/// The content hash is an identity, so it must be the real SHA-256 — a wrong digest would
/// make two different documents look like the same version.
#[test]
fn the_content_hash_is_really_sha256() {
    // Known-answer tests, so a hand-rolled implementation cannot be quietly wrong.
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // A multi-block input, to exercise the padding path rather than one chunk.
    assert_eq!(
        sha256_hex(&b"a".repeat(1000)),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}

/// `Source::label()` must name what was read. A run whose output does not say which ref
/// it read cannot be reproduced, and the stale-checkout trap is invisible.
#[test]
fn the_label_names_the_ref_that_was_read() {
    let s = Source::GitRef {
        repo: std::path::PathBuf::from("/repos/travel"),
        reference: "origin/main".into(),
    };
    assert_eq!(s.label(), "git:/repos/travel@origin/main");
    assert_eq!(
        Source::Dir(std::path::PathBuf::from("/x/y")).label(),
        "dir:/x/y"
    );
}
