//! **The D in CRUD, and the enumeration that made the catalog's own claim checkable.**
//!
//! Every store method exercised here had **zero callers outside its own test** before
//! this file existed — measured 2026-10-07:
//!
//! | store method | non-test callers | API callers |
//! | :-- | --: | --: |
//! | `archive` | 0 | 0 |
//! | `restore` | 0 | 0 |
//! | `unset_attribute` | 0 | 0 |
//! | `unset_localized_attribute` | 0 | 0 |
//! | `retract_relation` | 0 | 0 |
//! | `attributes_in` | 0 | 0 |
//! | `languages_of` | 0 | 0 |
//!
//! The capability was built and nothing reached it, so the API had no way to remove an
//! object, retract a wrong edge, drop a wrong label, or read one language. "Built" and
//! "complete" are independent, and this file is the difference.
//!
//! Set equality throughout, never counts — a removal that removes the wrong row and a
//! removal that removes nothing both leave a plausible-looking count.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use catalog_api::{router, ApiState};
use catalog_store::{CatalogStore, StoreConfig};
use http_body_util::BodyExt;
use std::collections::BTreeSet;
use tower::ServiceExt;

const TOKEN: &str = "test-internal-token";

struct Api {
    app: axum::Router,
    _dir: tempfile::TempDir,
}

fn api() -> Api {
    let dir = tempfile::tempdir().expect("td");
    let store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    Api {
        app: router(ApiState::with_token(store, Some(TOKEN.to_string()))),
        _dir: dir,
    }
}

async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = app.clone().oneshot(req).await.expect("response");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::String(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))
    };
    (status, json)
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).expect("r")
}

fn with_body(method: &str, path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::from(body.to_string()))
        .expect("r")
}

fn post(path: &str, body: serde_json::Value) -> Request<Body> {
    with_body("POST", path, body)
}

fn delete(path: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .expect("r")
}

fn paths_of(v: &serde_json::Value) -> BTreeSet<String> {
    v["paths"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ===========================================================================

#[tokio::test]
async fn an_object_can_be_archived_and_restored_and_the_set_changes_both_times() {
    let Api { app, _dir } = api();
    call(
        &app,
        post("/api/catalog/types", serde_json::json!({"name":"playbook"})),
    )
    .await;
    for p in ["a/one", "a/two", "a/three"] {
        let (st, _) = call(
            &app,
            post(
                "/api/catalog/objects",
                serde_json::json!({"resource_type":"playbook","path":p}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "register {p}");
    }
    let (_, b) = call(&app, get("/api/catalog/objects?type=playbook")).await;
    assert_eq!(
        paths_of(&b),
        set(&["a/one", "a/three", "a/two"]),
        "all three live"
    );

    // archive the middle one
    let (st, b) = call(&app, delete("/api/catalog/objects/a/two?version=1")).await;
    assert_eq!(st, StatusCode::OK, "archive: {b}");
    assert_eq!(b["reversible"], true, "nothing here hard-deletes");

    let (_, b) = call(&app, get("/api/catalog/objects?type=playbook")).await;
    assert_eq!(
        paths_of(&b),
        set(&["a/one", "a/three"]),
        "EXACTLY the archived one left the set — not zero, not all three"
    );

    // restore it
    let (st, b) = call(
        &app,
        post(
            "/api/catalog/restore",
            serde_json::json!({"path":"a/two","version":1}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "restore: {b}");
    let (_, b) = call(&app, get("/api/catalog/objects?type=playbook")).await;
    assert_eq!(
        paths_of(&b),
        set(&["a/one", "a/three", "a/two"]),
        "restore brings back exactly the one archived"
    );

    // archiving something absent is a 404, not a silent no-op
    let (st, _) = call(&app, delete("/api/catalog/objects/a/nope?version=1")).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "absent object must not 200");
}

#[tokio::test]
async fn an_attribute_can_be_unset_and_the_reverse_index_follows() {
    let Api { app, _dir } = api();
    call(
        &app,
        post("/api/catalog/types", serde_json::json!({"name":"playbook"})),
    )
    .await;
    for p in ["b/one", "b/two"] {
        call(
            &app,
            post(
                "/api/catalog/objects",
                serde_json::json!({"resource_type":"playbook","path":p}),
            ),
        )
        .await;
        let (st, b) = call(
            &app,
            post(
                "/api/catalog/attributes",
                serde_json::json!({"path":p,"name":"uses_credential.shared","value":{"type":"flag"}}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "set on {p}: {b}");
    }
    let (_, b) = call(
        &app,
        get("/api/catalog/by-attribute?name=uses_credential.shared"),
    )
    .await;
    assert_eq!(paths_of(&b), set(&["b/one", "b/two"]), "both carry it");

    let (st, b) = call(
        &app,
        delete("/api/catalog/attributes/b/one?name=uses_credential.shared"),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "unset: {b}");

    let (_, b) = call(
        &app,
        get("/api/catalog/by-attribute?name=uses_credential.shared"),
    )
    .await;
    assert_eq!(
        paths_of(&b),
        set(&["b/two"]),
        "the REVERSE index follows the unset — a stale reverse index is how 0-of-48 happened"
    );
    let (_, b) = call(&app, get("/api/catalog/attributes/b/one")).await;
    assert_eq!(b["count"], 0, "and the forward read agrees: {b}");
}

#[tokio::test]
async fn a_relation_can_be_retracted_in_both_directions() {
    let Api { app, _dir } = api();
    call(
        &app,
        post("/api/catalog/types", serde_json::json!({"name":"playbook"})),
    )
    .await;
    for p in ["c/parent", "c/other", "c/child"] {
        call(
            &app,
            post(
                "/api/catalog/objects",
                serde_json::json!({"resource_type":"playbook","path":p}),
            ),
        )
        .await;
    }
    let edge = |from: &str| {
        serde_json::json!({"from_type":"playbook","from_path":from,
                           "to_type":"playbook","to_path":"c/child","kind":"invokes"})
    };
    for f in ["c/parent", "c/other"] {
        let (st, b) = call(&app, post("/api/catalog/relations", edge(f))).await;
        assert_eq!(st, StatusCode::OK, "assert {f}: {b}");
    }
    let (_, b) = call(&app, get("/api/catalog/relations-to/c/child?type=playbook")).await;
    let callers: BTreeSet<String> = b["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["from_path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(callers, set(&["c/other", "c/parent"]), "both callers");

    let (st, b) = call(
        &app,
        with_body("DELETE", "/api/catalog/relations", edge("c/parent")),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "retract: {b}");

    let (_, b) = call(&app, get("/api/catalog/relations-to/c/child?type=playbook")).await;
    let callers: BTreeSet<String> = b["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["from_path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        callers,
        set(&["c/other"]),
        "the REVERSE index drops exactly the retracted edge"
    );
    let (_, b) = call(&app, get("/api/catalog/relations/c/parent?type=playbook")).await;
    assert_eq!(b["count"], 0, "and forward agrees: {b}");

    // An unknown kind must be refused, not written as a tombstone that matches nothing.
    let mut bad = edge("c/other");
    bad["kind"] = serde_json::json!("teleports_to");
    let (st, _) = call(&app, with_body("DELETE", "/api/catalog/relations", bad)).await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "unknown kind refused on retract too"
    );
}

#[tokio::test]
async fn a_custom_type_is_enumerable_and_objects_list_without_naming_a_type() {
    let Api { app, _dir } = api();
    // ⚠ Before the type registry, `GET /api/catalog/types` looped over noetl's six
    // known names, so a custom type could NEVER appear however many objects it had —
    // and `GET /api/catalog/objects` REQUIRED a type, so "what is in this catalog"
    // was not an answerable question. Both claims are checked here.
    for t in ["playbook", "widget_schema", "dashboard"] {
        let (st, _) = call(
            &app,
            post("/api/catalog/types", serde_json::json!({"name":t})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "declare {t}");
    }
    let (_, b) = call(&app, get("/api/catalog/types")).await;
    let declared: BTreeSet<String> = b["declared_here"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        declared,
        set(&["dashboard", "playbook", "widget_schema"]),
        "every declared type is enumerable, custom ones included"
    );

    call(
        &app,
        post(
            "/api/catalog/objects",
            serde_json::json!({"resource_type":"playbook","path":"d/pb"}),
        ),
    )
    .await;
    call(
        &app,
        post(
            "/api/catalog/objects",
            serde_json::json!({"resource_type":"widget_schema","path":"d/w"}),
        ),
    )
    .await;
    call(
        &app,
        post(
            "/api/catalog/objects",
            serde_json::json!({"resource_type":"dashboard","path":"d/dash"}),
        ),
    )
    .await;

    let (st, b) = call(&app, get("/api/catalog/objects")).await;
    assert_eq!(st, StatusCode::OK, "no `type` must be allowed: {b}");
    assert_eq!(b["type"], "*");
    assert_eq!(
        paths_of(&b),
        set(&["d/dash", "d/pb", "d/w"]),
        "listing without a type returns EVERY type's objects"
    );
}

#[tokio::test]
async fn a_caller_error_is_a_4xx_and_a_language_scoped_read_works() {
    let Api { app, _dir } = api();
    call(
        &app,
        post("/api/catalog/types", serde_json::json!({"name":"playbook"})),
    )
    .await;
    call(
        &app,
        post(
            "/api/catalog/objects",
            serde_json::json!({"resource_type":"playbook","path":"e/pb"}),
        ),
    )
    .await;

    // ⚠ Every store error mapped to 500 before, so a client could not tell "fix your
    // request" from "retry, the store broke" — and an invalid input paged someone.
    let (st, body) = call(
        &app,
        post(
            "/api/catalog/attributes",
            serde_json::json!({"path":"e/pb","name":"uses_tool.not_a_kind","value":{"type":"flag"}}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "caller error is 4xx: {body}");
    assert!(
        body.as_str().unwrap_or_default().contains("not_a_kind"),
        "and it still names the offender: {body}"
    );

    // localized writes, then a language-scoped read and the language set
    for (lang, val) in [("en", "Welcome"), ("de", "Willkommen")] {
        let (st, b) = call(
            &app,
            post(
                "/api/catalog/attributes",
                serde_json::json!({"path":"e/pb","name":"display_name",
                                   "value":{"type":"text","value":val},"lang":lang}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "localized write {lang}: {b}");
    }
    let (st, b) = call(&app, get("/api/catalog/attributes/e/pb?lang=de")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["lang"], "de");
    assert_eq!(
        b["attributes"]["display_name"]["value"]["value"], "Willkommen",
        "the language-scoped read returns THAT language: {b}"
    );
    let langs: BTreeSet<String> = b["languages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        langs,
        set(&["de", "en"]),
        "`languages_of` had no caller at all before: {b}"
    );

    // and one language can be dropped without touching the other
    let (st, _) = call(
        &app,
        delete("/api/catalog/attributes/e/pb?name=display_name&lang=en"),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, b) = call(&app, get("/api/catalog/attributes/e/pb?lang=de")).await;
    assert_eq!(
        b["attributes"]["display_name"]["value"]["value"], "Willkommen",
        "dropping `en` must leave `de` intact: {b}"
    );
}
