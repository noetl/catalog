//! `catalog` — the driver that makes the store reachable.
//!
//! Subcommands, each deliberately small:
//!
//! ```text
//! catalog ingest --store <dir> --source <dir|git:REPO@REF> [--subpath P] [--tick]
//! catalog tick   --store <dir> [--every SECS] [--max-ticks N]
//! catalog stats  --store <dir>
//! catalog list   --store <dir> [--type playbook]
//! catalog show   --store <dir> --path <catalog/path>
//! catalog who-uses --store <dir> --attr <attribute.name>
//! ```
//!
//! ⚠ `tick --every` takes `--max-ticks` because an unbounded loop with no cap is the
//! shape `loop-engineering.md` forbids: every loop declares a hard bound before it runs.

use std::path::PathBuf;
use std::process::ExitCode;

use catalog_ingest::{ingest, Source};
use catalog_store::{CatalogStore, StoreConfig};

const USAGE: &str = "\
catalog — NoETL internal-resource catalog, stored in EHDB

USAGE:
  catalog ingest --store <dir> --source <dir|git:REPO@REF> [--subpath P] [--tick]
  catalog tick   --store <dir> [--every SECS] [--max-ticks N]
  catalog stats  --store <dir>
  catalog list   --store <dir> [--type NAME]
  catalog show   --store <dir> --path <catalog/path>
  catalog who-uses --store <dir> --attr <attribute.name>
  catalog who-calls --store <dir> --path <catalog/path>

NOTES:
  --source git:REPO@REF reads the REF, not the working tree. Prefer it: a stale
  checkout is the most reliable way to produce a confident zero.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("catalog: {e}");
            ExitCode::FAILURE
        }
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn open_store(args: &[String]) -> Result<CatalogStore, String> {
    let root = flag(args, "--store").ok_or("--store <dir> is required")?;
    let mut cfg = StoreConfig::new(PathBuf::from(root));
    if let Some(n) = flag(args, "--seal-max-records").and_then(|s| s.parse::<u64>().ok()) {
        cfg = cfg.with_seal_max_records(n);
    }
    CatalogStore::open(&cfg).map_err(|e| format!("open store at {root}: {e}"))
}

fn parse_source(spec: &str) -> Source {
    // `git:/path/to/repo@origin/main` — split on the LAST '@' so a ref containing no '@'
    // and a path containing one both behave.
    if let Some(rest) = spec.strip_prefix("git:") {
        if let Some(at) = rest.rfind('@') {
            return Source::GitRef {
                repo: PathBuf::from(&rest[..at]),
                reference: rest[at + 1..].to_string(),
            };
        }
        return Source::GitRef {
            repo: PathBuf::from(rest),
            reference: "HEAD".to_string(),
        };
    }
    Source::Dir(PathBuf::from(spec))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn run(args: &[String]) -> Result<(), String> {
    match args[0].as_str() {
        "ingest" => {
            let spec = flag(args, "--source").ok_or("--source is required")?;
            let subpath = flag(args, "--subpath").unwrap_or("");
            let source = parse_source(spec);
            let mut store = open_store(args)?;

            println!("source: {}", source.label());
            println!(
                "subpath: {}",
                if subpath.is_empty() { "<all>" } else { subpath }
            );
            let res =
                ingest(&mut store, &source, subpath, now()).map_err(|e| format!("ingest: {e}"))?;

            // ⚠ The denominator first, always.
            println!("{}", res.summary());
            if !res.accounts_for_every_file() {
                return Err(format!(
                    "accounting failed: scanned={} but registered={} + skipped={} — the \
                     walk lost files silently",
                    res.scanned,
                    res.registered,
                    res.skipped.len()
                ));
            }
            if res.scanned == 0 {
                // Not an error — but never silent. An empty source and a wrong path
                // produce the same clean output otherwise.
                println!(
                    "⚠ scanned 0 files. Check --subpath, and if --source is a dir, \
                     whether the checkout is on the branch you meant."
                );
            }
            if !res.skipped.is_empty() {
                println!("skipped:");
                let mut by_reason: std::collections::BTreeMap<String, usize> = Default::default();
                for (path, reason) in &res.skipped {
                    *by_reason.entry(reason.to_string()).or_insert(0) += 1;
                    println!("  {} — {reason}", path.display());
                }
                println!("skip reasons: {by_reason:?}");
            }
            if has(args, "--tick") {
                let t = store.tick().map_err(|e| format!("tick: {e}"))?;
                println!(
                    "tick: sealed={} merged={} reclaimed={}",
                    t.sealed, t.merged, t.reclaimed
                );
            }
            println!("parts: {:?}", store.part_counts());
            Ok(())
        }
        "tick" => {
            let mut store = open_store(args)?;
            let every = flag(args, "--every").and_then(|s| s.parse::<u64>().ok());
            let max = flag(args, "--max-ticks")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(1);
            if every.is_some() && !has(args, "--max-ticks") {
                return Err(
                    "--every requires --max-ticks: a loop declares its hard bound \
                            before it runs"
                        .into(),
                );
            }
            for i in 1..=max {
                let t = store.tick().map_err(|e| format!("tick: {e}"))?;
                println!(
                    "tick {i}/{max}: sealed={} merged={} reclaimed={} parts={:?}",
                    t.sealed,
                    t.merged,
                    t.reclaimed,
                    store.part_counts()
                );
                if let Some(secs) = every {
                    if i < max {
                        std::thread::sleep(std::time::Duration::from_secs(secs));
                    }
                }
            }
            Ok(())
        }
        "stats" => {
            let store = open_store(args)?;
            let pc = store.part_counts();
            println!("parts: {pc:?}");
            println!("total: {}", pc.total());
            Ok(())
        }
        "list" => {
            let store = open_store(args)?;
            let want = flag(args, "--type");
            if let Some(t) = want {
                match store
                    .resource_type(t)
                    .map_err(|e| format!("resource_type: {e}"))?
                {
                    Some(rt) => println!("type {} declared: {:?}", t, rt),
                    None => println!("type {t} is not declared in this store"),
                }
            }
            // The listing, now that c1 carries a type index.
            match want {
                Some(t) => {
                    let paths = store
                        .resources_of_type(t)
                        .map_err(|e| format!("resources_of_type: {e}"))?;
                    println!("{t}: {} live resource(s)", paths.len());
                    for p in &paths {
                        println!("  {p}");
                    }
                    if paths.is_empty() {
                        println!(
                            "⚠ nothing live of type {t}. An empty listing and an \
                             un-ingested store look identical — check --store."
                        );
                    }
                }
                None => println!("--type <NAME> selects what to list (e.g. playbook)"),
            }
            Ok(())
        }
        // The reverse lookup. The query this catalog exists for: rotating a keychain
        // alias means knowing which resources break.
        "who-uses" => {
            let store = open_store(args)?;
            let attr = flag(args, "--attr").ok_or("--attr <attribute.name> is required")?;
            let paths = store
                .resources_with_attribute(attr)
                .map_err(|e| format!("resources_with_attribute: {e}"))?;
            // ⚠ The count IS the headline, because the hazard here is a partial
            // answer that looks complete — a naive fold returned 1 of 49, and 0 of 48
            // once a tombstone was the latest op under the shared key.
            println!("{attr}: {} resource(s)", paths.len());
            for p in &paths {
                println!("  {p}");
            }
            if paths.is_empty() {
                println!(
                    "⚠ nothing carries {attr}. Check the spelling, and whether this \
                     store has been ingested — an empty reverse answer and an \
                     un-ingested store look identical."
                );
            }
            Ok(())
        }
        // The edge direction that matters when retiring a resource.
        "who-calls" => {
            let store = open_store(args)?;
            let path = flag(args, "--path").ok_or("--path is required")?;
            let callers = store
                .relations_to(path)
                .map_err(|e| format!("relations_to: {e}"))?;
            println!("{path}: {} caller(s)", callers.len());
            for (from, kind) in &callers {
                println!("  {from}  ({kind})");
            }
            if callers.is_empty() {
                println!(
                    "⚠ nothing calls {path}. That is the answer that authorises a \
                     deletion, so check the store was ingested — an empty caller list \
                     and an un-ingested store look identical."
                );
            }
            Ok(())
        }
        "show" => {
            let store = open_store(args)?;
            let path = flag(args, "--path").ok_or("--path is required")?;
            let versions = store.versions(path).map_err(|e| format!("versions: {e}"))?;
            println!("{path}: {} live version(s)", versions.len());
            for v in &versions {
                println!(
                    "  v{} type={} sha256={} content={}",
                    v.version,
                    v.resource_type,
                    &v.content_sha256[..16.min(v.content_sha256.len())],
                    v.content.as_ref().map(|c| c.len()).unwrap_or(0)
                );
            }
            let attrs = store
                .attributes(path)
                .map_err(|e| format!("attributes: {e}"))?;
            println!("  attributes: {}", attrs.len());
            for (k, a) in &attrs {
                println!("    {k} = {:?}", a.value);
            }
            let rels = store
                .relations_from(path)
                .map_err(|e| format!("relations_from: {e}"))?;
            println!("  relations out: {}", rels.len());
            for r in &rels {
                println!("    -> {} ({:?})", r.to_entity.path, r.kind);
            }
            Ok(())
        }
        other => Err(format!("unknown subcommand {other:?}\n\n{USAGE}")),
    }
}
