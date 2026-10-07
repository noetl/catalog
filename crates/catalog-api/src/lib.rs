//! **The catalog's only interface.** `/api/catalog/*` over the EHDB-backed store.
//!
//! # No SQL, of any kind
//!
//! There is no DDL parser, no SQL query interface, no SQL-shaped access layer, and no
//! query language standing in for one. Declaring an object type, writing objects /
//! attributes / relations, and every read — including the reverse lookups and the
//! relation graph — happen through these endpoints. The storage half is enforced
//! mechanically by AC1 (no database driver in any manifest, no SQL literal in any
//! non-test source).
//!
//! # Why writes are serialized behind a mutex
//!
//! Not a shortcut — the honest implementation of a constraint EHDB imposes. The store
//! runs `shard_count = 1` with a **single writer per partition**, because `ehdb-l0` has
//! no authoritative election (spec assumption 5). `append_writer_assigned` also depends
//! on a monotonic `global_sequence`, which two concurrent writers would interleave,
//! violating the ascending-`sort_key` contract that the sparse index, MinMax pruning and
//! merge all rely on. A `Mutex` is what that assumption looks like in code.
//!
//! # Auth
//!
//! Writes and the lifecycle tick require the internal bearer token, mirroring
//! `noetl/server`'s `/api/internal/*` guard exactly: **503** when the token env is unset
//! (no permissive default for a privileged surface), **403** on a missing, malformed or
//! mismatched header. Reads are open, because the catalog's content is the platform's own
//! object inventory rather than tenant data — with one exception: see
//! [`handlers::by_attribute`], where the *question* is sensitive even though each answer
//! is a path.

use std::sync::{Arc, Mutex};

use axum::Router;
use catalog_store::CatalogStore;

pub mod auth;
pub mod handlers;

/// The env var carrying the internal API token, named identically to the server's so a
/// deployment sets one value.
pub const TOKEN_ENV: &str = "NOETL_INTERNAL_API_TOKEN";

/// Shared state: the single-writer store, and the expected internal token.
///
/// ⚠ The token lives **in state**, read from the environment once at startup, rather
/// than being read from a process global inside the auth guard. That is not a test
/// convenience — it is the better design, and the workspace's `unsafe_code = "forbid"`
/// lint is what surfaced it: a guard that reads `std::env` per request can only be tested
/// by mutating a process global, which needs `unsafe` and races every other test in the
/// binary. State-carried config is explicit, injectable, and has no hidden global.
#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<CatalogStore>>,
    /// `None` means **unconfigured**, which refuses every write with 503. It never means
    /// "allow" — a privileged surface gets no permissive default.
    pub token: Option<String>,
}

impl ApiState {
    /// Reads the expected token from [`TOKEN_ENV`]. An unset or blank value is `None`.
    pub fn new(store: CatalogStore) -> Self {
        let token = std::env::var(TOKEN_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        Self {
            store: Arc::new(Mutex::new(store)),
            token,
        }
    }

    /// Explicit token, for tests and for a caller that sources it elsewhere.
    pub fn with_token(store: CatalogStore, token: Option<String>) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            token: token.filter(|v| !v.trim().is_empty()),
        }
    }
}

/// Every route the catalog exposes. **This list is the catalog's entire surface.**
pub fn router(state: ApiState) -> Router {
    use axum::routing::{get, post};
    Router::new()
        .route("/api/catalog/health", get(handlers::health))
        // --- object types ---
        .route(
            "/api/catalog/types",
            post(handlers::declare_type).get(handlers::list_types),
        )
        .route("/api/catalog/types/{name}", get(handlers::get_type))
        // --- objects ---
        .route(
            "/api/catalog/objects",
            post(handlers::register_object).get(handlers::list_objects),
        )
        .route("/api/catalog/objects/{*path}", get(handlers::get_object))
        // --- attributes ---
        .route("/api/catalog/attributes", post(handlers::set_attribute))
        .route(
            "/api/catalog/attributes/{*path}",
            get(handlers::get_attributes),
        )
        // The reverse attribute lookup — "which objects carry attribute N".
        .route("/api/catalog/by-attribute", get(handlers::by_attribute))
        // --- relations ---
        .route("/api/catalog/relations", post(handlers::assert_relation))
        .route(
            "/api/catalog/relations/{*path}",
            get(handlers::relations_from),
        )
        // The reverse relation lookup — "what references this object".
        .route(
            "/api/catalog/relations-to/{*path}",
            get(handlers::relations_to),
        )
        // --- bulk ingest: walk a whole source over the API ---
        .route("/api/catalog/ingest", post(handlers::ingest))
        // --- the constraints this catalog enforces, and where ---
        .route("/api/catalog/constraints", get(handlers::constraints))
        // --- lifecycle + observability ---
        .route("/api/catalog/tick", post(handlers::tick))
        .route("/metrics", get(handlers::metrics))
        .with_state(state)
}
