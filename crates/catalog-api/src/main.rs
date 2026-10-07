//! `catalog-server` — serves `/api/catalog/*` over an EHDB-backed store.

use catalog_api::{router, ApiState};
use catalog_store::{CatalogStore, StoreConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::var("CATALOG_STORE_ROOT").unwrap_or_else(|_| "./catalog-store".into());
    let addr = std::env::var("CATALOG_BIND").unwrap_or_else(|_| "127.0.0.1:8091".into());

    let store = CatalogStore::open(&StoreConfig::new(&root))?;
    catalog_store::metrics::init();
    let app = router(ApiState::new(store));

    println!("catalog-server: store={root} listening on {addr}");
    if std::env::var(catalog_api::TOKEN_ENV).is_err() {
        // ⚠ Said at startup, because the alternative is discovering it as a 503 on the
        // first write. Reads work without it by design.
        println!(
            "  ⚠ {} is unset — every WRITE endpoint will return 503. Reads are open.",
            catalog_api::TOKEN_ENV
        );
    }
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
