//! **AC3, storage half** — the number of datasets is fixed at four, forever.
//!
//! `catalog-model`'s own AC3 test asserts it declares **zero** `Dataset` impls. That
//! stays true and is the right assertion there, but on its own it would now pass
//! vacuously: the impls moved to this crate, so a scan of the model crate can no
//! longer see the thing AC3 is about.
//!
//! This is the half that bites. The design premise is that **adding a resource type
//! is data**. The way that premise dies is not a single bad commit — it is someone
//! adding a `c5_mcp_entity` dataset because mcp "needs its own shape", which looks
//! entirely reasonable in isolation and silently restores the per-entity-type table
//! model this repo exists to replace.
//!
//! # ⭐ The claim is no longer only structural
//!
//! `subscription` is now supported as a real second resource type, and it shares
//! **nothing** structurally with a playbook: no `workflow:` at all, a `spec:` instead,
//! its reference at `spec.dispatch.playbook`, and a `spec.auth` alias that depends on a
//! resource type the catalog deliberately does not hold. Measured on the `noetl/e2e`
//! corpus, **9 of 9** `kind: Subscription` fixtures carry that dispatch reference.
//!
//! Supporting it added **zero** datasets, which is what this test asserts. So the count
//! below is not an abstract invariant about a synthetic `widget` — it is the record of a
//! second real type having gone in without reshaping the store.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Exactly the four the spec names. Not a floor, not a ceiling.
const EXPECTED_DATASETS: usize = 4;

fn crate_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)
            .unwrap_or_else(|e| panic!("reading {}: {e}", d.display()))
            .flatten()
        {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push((
                    p.file_name().unwrap().to_string_lossy().into(),
                    std::fs::read_to_string(&p).expect("read"),
                ));
            }
        }
    }
    out.sort();
    out
}

/// Non-comment lines only. A doc comment naming `impl Dataset for` would otherwise
/// count as an impl — comments counting as code is a documented false-positive source
/// in this fleet.
fn code_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("//") && !t.starts_with("/*") && !t.starts_with('*')
        })
        .collect()
}

#[test]
fn the_dataset_count_is_exactly_four_and_must_stay_four() {
    let src = crate_src();
    let files = rust_sources(&src);

    // ⚠ Assert the extraction first. A scan of the wrong directory finds zero impls
    // and reports a clean pass, which is indistinguishable from the property holding.
    println!(
        "AC3-storage: examined {} source files under {} ({})",
        files.len(),
        src.display(),
        files
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert!(
        files.len() >= 3,
        "found only {} .rs files under {} — this scan examined the wrong tree, so the \
         count below would be meaningless",
        files.len(),
        src.display()
    );

    let mut impls: Vec<String> = Vec::new();
    for (name, text) in &files {
        for line in code_lines(text) {
            if line.contains("impl Dataset for") || line.contains("impl ehdb_l0::Dataset for") {
                impls.push(format!("{name}: {}", line.trim()));
            }
        }
    }

    assert_eq!(
        impls.len(),
        EXPECTED_DATASETS,
        "expected exactly {EXPECTED_DATASETS} `Dataset` impls, found {}:\n  {}\n\n\
         If you are adding a RESOURCE TYPE, you do not need a dataset — append a \
         `ResourceType` record instead. `subscription` went in that way, with a \
         completely different content shape and a cross-type dependency, and this \
         number did not move. That is the whole point of the model, and this assertion \
         is what keeps it true.\n\
         If you are genuinely adding a fifth *kind of log* (not a resource type), \
         raise this number deliberately and say why in the spec. Do not relax it to \
         a floor: `>=` cannot detect the failure this guards against, because the \
         failure is always an ADDITION.",
        impls.len(),
        impls.join("\n  ")
    );

    // The names must also be the four the spec declares, so swapping one out is
    // caught as well as adding a fifth.
    let mut names = BTreeSet::new();
    for (_, text) in &files {
        for line in code_lines(text) {
            if let Some(rest) = line.split("const NAME: &'static str = ").nth(1) {
                names.insert(rest.trim().trim_end_matches(';').to_string());
            }
        }
    }
    let expected: BTreeSet<String> = [
        "DATASET_C1_ENTITY",
        "DATASET_C2_RELATION",
        "DATASET_C3_ATTRIBUTE",
        "DATASET_C4_TYPE",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        names, expected,
        "the four dataset NAME constants must be exactly the ones the spec declares"
    );
}

#[test]
fn no_dataset_name_collides_with_ehdbs_d_number_space() {
    // ehdb-l0 owns d1…d10 and all ten are allocated — `d7_catalog` already exists
    // there. A `d11_…` here would also falsify ehdb's own lib.rs header, which
    // enumerates the fixed set, from outside the repo that owns it.
    for n in [
        catalog_store::DATASET_C1_ENTITY,
        catalog_store::DATASET_C2_RELATION,
        catalog_store::DATASET_C3_ATTRIBUTE,
        catalog_store::DATASET_C4_TYPE,
    ] {
        assert!(
            !n.starts_with('d'),
            "{n} is in ehdb-l0's D-number namespace, which is fully allocated"
        );
        assert!(
            n.starts_with('c'),
            "{n} must be in the catalog's `c…` namespace"
        );
    }
}
