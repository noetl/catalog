//! Every Rust source on disk in this workspace must be git-tracked.
//!
//! # Why this is the first guard in the repo
//!
//! This is the check a developer's machine structurally *cannot* make. An
//! untracked or ignored source file is present locally, so it compiles and its
//! tests pass; `git add` skips it without a word and `git status` never lists
//! it. The first place the absence is observable is a clean checkout — which is
//! CI. By then the failure looks like an unrelated compile error in whatever
//! file referenced the missing module.
//!
//! Adapted from `ehdb-core`'s `workspace_target_hygiene` test, which exists in
//! `noetl/ehdb` for the same reason.
//!
//! # The denominator
//!
//! The test prints how many files it examined and fails if that number is
//! implausibly small. A scan that walks the wrong directory finds zero
//! violations and reports success — indistinguishable from a healthy workspace.
//! Printing and flooring the population is what separates those two readings.
//! (See `agents/rules/representation-drift.md`, "Print the denominator".)

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Lowest plausible number of first-party `.rs` files in this workspace.
///
/// A floor, not an exact count: new files should not have to edit this. But a
/// scan that returns fewer than this examined the wrong tree, and that is a
/// broken test rather than a clean workspace.
const MIN_PLAUSIBLE_SOURCES: usize = 2;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/crates/catalog-model.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.pop(); // <root>
    p
}

/// Every `.rs` file under the workspace, excluding build output.
fn rust_sources_on_disk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                // `target` is build output and `.git` is not source. Everything
                // else is in scope, deliberately: a stray `.rs` outside
                // `crates/` is exactly the kind of thing worth noticing.
                if name != "target" && name != ".git" {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|x| x == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn git_tracked_rust_files(root: &Path) -> BTreeSet<PathBuf> {
    let out = Command::new("git")
        .args(["ls-files", "-z", "--", "*.rs"])
        .current_dir(root)
        .output()
        .expect("`git ls-files` must run; this test requires a git checkout");
    assert!(
        out.status.success(),
        "`git ls-files` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| root.join(s))
        .collect()
}

#[test]
fn every_rust_source_on_disk_is_git_tracked() {
    let root = workspace_root();
    let on_disk = rust_sources_on_disk(&root);
    let tracked = git_tracked_rust_files(&root);

    // Assert the extraction BEFORE asserting about it.
    println!(
        "workspace_target_hygiene: examined {} .rs files on disk, {} tracked by git (root {})",
        on_disk.len(),
        tracked.len(),
        root.display()
    );
    assert!(
        on_disk.len() >= MIN_PLAUSIBLE_SOURCES,
        "found only {} .rs files under {} — this scan examined the wrong tree. \
         A scan that finds nothing reports no violations and looks clean, so \
         this is a failure of the test, not a pass for the workspace.",
        on_disk.len(),
        root.display()
    );
    assert!(
        !tracked.is_empty(),
        "`git ls-files -- *.rs` returned nothing under {}. The comparison would \
         then flag every file, or none, depending on direction — either way the \
         result would be meaningless.",
        root.display()
    );

    let untracked: Vec<String> = on_disk
        .iter()
        .filter(|p| !tracked.contains(*p))
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap_or(p)
                .to_string_lossy()
                .to_string()
        })
        .collect();

    assert!(
        untracked.is_empty(),
        "these .rs files exist on disk but are NOT tracked by git, so a clean \
         checkout does not have them:\n  {}\n\
         Run `git add` on each, or add it to .gitignore deliberately if it is \
         genuinely local-only.",
        untracked.join("\n  ")
    );
}
