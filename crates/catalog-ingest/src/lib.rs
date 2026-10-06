//! Ingestion — the part that makes the rest of the catalog *reachable*.
//!
//! # Why this crate exists
//!
//! Before it, `catalog-store` had three library crates and **zero binaries**, and a grep
//! for non-test callers of its two most important entry points returned nothing:
//!
//! ```text
//! tick()                 0 non-test call sites
//! register_from_source    0 non-test call sites
//! ```
//!
//! Every acceptance criterion was satisfied by a test. That is the failure class
//! `representation-drift.md` calls reachability-versus-existence: the code existed, was
//! documented, had 89 passing tests, and nothing could run it against a real document.
//! `tick`'s own doc comment says it **must be called on a timer** — and nothing called it
//! at all.
//!
//! # Reading from a git ref, not the working tree
//!
//! [`Source::GitRef`] exists because of a measurement that was wrong on this very repo.
//! A sweep for the catalog's input population reported:
//!
//! ```text
//! repos/travel: 4 yaml under a playbook path
//! adiona yaml: 0
//! ```
//!
//! while `origin/main` carries **53** `adiona/playbooks/*.yaml`. The checkout was on a
//! side branch, 41 commits behind. A stale working tree is the single most reliable way
//! to produce a confident zero, so ingestion names the ref it read and reports it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use catalog_model::Entity;
use catalog_store::CatalogStore;

/// Why one document was not registered.
///
/// ⚠ Every skip carries its reason. A count of successes alone cannot distinguish "the
/// source held 4 documents" from "the walker found 4 of 53", which is exactly the
/// mistake this crate's doc comment records.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    /// The file is not YAML we can parse at all.
    Unparseable(String),
    /// Parsed, but the document has no `metadata.path`, so it has no catalog identity.
    NoMetadataPath,
    /// Parsed, but no top-level `kind:`, so its resource type is unknown.
    NoKind,
    /// A `kind:` this catalog has no resource type for.
    UnknownKind(String),
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unparseable(e) => write!(f, "unparseable: {e}"),
            Self::NoMetadataPath => write!(f, "no metadata.path"),
            Self::NoKind => write!(f, "no top-level kind:"),
            Self::UnknownKind(k) => write!(f, "unknown kind: {k}"),
        }
    }
}

/// What one ingestion run saw — the population, not only the successes.
#[derive(Debug, Clone, Default)]
pub struct Ingested {
    /// Files the walker considered. **The denominator.**
    pub scanned: usize,
    /// Documents registered as entities.
    pub registered: usize,
    /// Relations recorded across all registrations.
    pub relations: usize,
    /// Attributes recorded across all registrations.
    pub attributes: usize,
    /// Per-file skips, with the reason.
    pub skipped: Vec<(PathBuf, SkipReason)>,
    /// Registered count per `kind`, so a run says *what* it catalogued.
    pub by_kind: BTreeMap<String, usize>,
}

impl Ingested {
    /// `scanned` must equal `registered + skipped`, or the walk lost a file silently.
    ///
    /// This is asserted rather than assumed because a run that drops files reports a
    /// clean "N registered, 0 skipped" — indistinguishable from a healthy run.
    pub fn accounts_for_every_file(&self) -> bool {
        self.registered + self.skipped.len() == self.scanned
    }

    /// A one-line summary that always prints the denominator.
    pub fn summary(&self) -> String {
        format!(
            "scanned={} registered={} skipped={} relations={} attributes={} kinds={:?}",
            self.scanned,
            self.registered,
            self.skipped.len(),
            self.relations,
            self.attributes,
            self.by_kind
        )
    }
}

/// A document the walker produced, with the path it came from.
pub struct SourceDoc {
    pub origin: PathBuf,
    pub body: String,
}

/// Where documents come from.
pub enum Source {
    /// A directory on disk. ⚠ Whatever the working tree currently holds, which may be a
    /// side branch — see this module's note.
    Dir(PathBuf),
    /// A git ref in a repository, read with `git ls-tree` / `git show`, so the
    /// population is the ref's and not the checkout's.
    GitRef { repo: PathBuf, reference: String },
}

impl Source {
    /// A label naming exactly what was read, for the run's output.
    pub fn label(&self) -> String {
        match self {
            Self::Dir(p) => format!("dir:{}", p.display()),
            Self::GitRef { repo, reference } => {
                format!("git:{}@{}", repo.display(), reference)
            }
        }
    }

    /// Collect every `*.yaml` / `*.yml` the source holds under `subpath`.
    pub fn collect(&self, subpath: &str) -> std::io::Result<Vec<SourceDoc>> {
        match self {
            Self::Dir(root) => {
                let mut out = Vec::new();
                collect_dir(&root.join(subpath), &mut out)?;
                out.sort_by(|a, b| a.origin.cmp(&b.origin));
                Ok(out)
            }
            Self::GitRef { repo, reference } => {
                let listing = std::process::Command::new("git")
                    .arg("-C")
                    .arg(repo)
                    .args(["ls-tree", "-r", "--name-only", reference])
                    .output()?;
                if !listing.status.success() {
                    // ⚠ A bad ref must be an error, never an empty listing. An empty
                    // listing satisfies "0 unparseable, 0 skipped" and reads as a clean
                    // run over a source that happened to be empty.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!(
                            "git ls-tree failed for ref {reference}: {}",
                            String::from_utf8_lossy(&listing.stderr).trim()
                        ),
                    ));
                }
                let names = String::from_utf8_lossy(&listing.stdout);
                let mut out = Vec::new();
                for name in names.lines() {
                    if !name.starts_with(subpath) {
                        continue;
                    }
                    if !(name.ends_with(".yaml") || name.ends_with(".yml")) {
                        continue;
                    }
                    let show = std::process::Command::new("git")
                        .arg("-C")
                        .arg(repo)
                        .args(["show", &format!("{reference}:{name}")])
                        .output()?;
                    if show.status.success() {
                        out.push(SourceDoc {
                            origin: PathBuf::from(name),
                            body: String::from_utf8_lossy(&show.stdout).into_owned(),
                        });
                    }
                }
                Ok(out)
            }
        }
    }
}

fn collect_dir(dir: &Path, out: &mut Vec<SourceDoc>) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if e.file_type()?.is_dir() {
            collect_dir(&p, out)?;
        } else {
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            if ext == "yaml" || ext == "yml" {
                if let Ok(body) = std::fs::read_to_string(&p) {
                    out.push(SourceDoc { origin: p, body });
                }
            }
        }
    }
    Ok(())
}

/// Register every document a source holds.
///
/// `version` is taken from the document when it carries one and defaults to 1 otherwise —
/// matching `noetl/server`, which re-versions on each `catalog load` rather than trusting
/// the document.
pub fn ingest(
    store: &mut CatalogStore,
    source: &Source,
    subpath: &str,
    at: i64,
) -> std::io::Result<Ingested> {
    let docs = source.collect(subpath)?;
    let mut result = Ingested {
        scanned: docs.len(),
        ..Default::default()
    };

    for doc in docs {
        let parsed: serde_yaml::Value = match serde_yaml::from_str(&doc.body) {
            Ok(v) => v,
            Err(e) => {
                result
                    .skipped
                    .push((doc.origin, SkipReason::Unparseable(e.to_string())));
                continue;
            }
        };
        let kind = match parsed.get("kind").and_then(|k| k.as_str()) {
            Some(k) => k.to_string(),
            None => {
                result.skipped.push((doc.origin, SkipReason::NoKind));
                continue;
            }
        };
        // The catalog's resource-type name is the lowercase `kind`, which is what
        // `resource_type()` folds on. ⚠ Case matters: noetl/server#429 was a real prod
        // bug where `kind = $1` compared a mixed-case column to a lowercase parameter
        // and returned 650 rows where 1,525 existed.
        let type_name = kind.to_lowercase();
        if !matches!(type_name.as_str(), "playbook" | "subscription") {
            result
                .skipped
                .push((doc.origin, SkipReason::UnknownKind(kind)));
            continue;
        }
        let path = match parsed
            .get("metadata")
            .and_then(|m| m.get("path"))
            .and_then(|p| p.as_str())
        {
            Some(p) => p.to_string(),
            None => {
                result
                    .skipped
                    .push((doc.origin, SkipReason::NoMetadataPath));
                continue;
            }
        };
        let version = parsed
            .get("metadata")
            .and_then(|m| m.get("version"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32;

        let entity = Entity {
            resource_type: type_name.clone(),
            path,
            version,
            entity_id: 0,
            content: Some(doc.body.clone()),
            content_sha256: sha256_hex(doc.body.as_bytes()),
            archived_at: None,
        };
        let reg = store
            .register_from_source(entity, &doc.body, at)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        result.registered += 1;
        result.relations += reg.relations;
        result.attributes += reg.attributes;
        *result.by_kind.entry(type_name).or_insert(0) += 1;
    }
    Ok(result)
}

/// A dependency-free SHA-256, so ingestion adds no crypto dependency for a content hash
/// that is an identity, never a security boundary.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}
