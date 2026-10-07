//! **The acceptance proof, driven entirely over HTTP.** No SQL, no direct store calls
//! for anything the API can do.
//!
//! This also closes the "nothing consumes the catalog on a serving path" gap: the crate
//! had no HTTP surface at all, so every acceptance criterion was discharged by a library
//! test and AC10 was recorded `Open`.
//!
//! # Set equality, never counts
//!
//! Every assertion here compares the **whole set** against ground truth. A count is not
//! enough and the reason is measured: the folds behind these endpoints returned
//! **1 of 49**, **0 of 48**, **1 of 53** and **1 of 40** before they were keyed
//! correctly — partial answers that looked successful. `0 of 48` is the one to remember:
//! it reported *"nobody uses this credential"* while 48 objects did.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use catalog_api::{router, ApiState};
use catalog_store::{CatalogStore, StoreConfig};
use http_body_util::BodyExt;
use tower::ServiceExt;

const TOKEN: &str = "test-internal-token";

/// ⚠ No process-global anywhere in this file. The expected token is injected into
/// `ApiState`, so these tests need no env mutation and therefore no lock and no `unsafe`.
///
/// The workspace's `unsafe_code = "forbid"` lint is what forced that: the first draft read
/// the token from `std::env` inside the auth guard, which can only be tested by mutating a
/// process global — needing `unsafe`, and racing every other test in the binary, which is
/// a race this repo has already shipped once.
struct Api {
    app: axum::Router,
    _dir: tempfile::TempDir,
}

/// An API with the token configured.
fn api() -> Api {
    api_with_token(Some(TOKEN.to_string()))
}

/// An API with an explicit token, including `None` for "unconfigured".
fn api_with_token(token: Option<String>) -> Api {
    let dir = tempfile::tempdir().expect("td");
    let store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    Api {
        app: router(ApiState::with_token(store, token)),
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
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("req")
}

fn post_auth(path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::from(body.to_string()))
        .expect("req")
}

// ===========================================================================
// The full arc, over HTTP: declare a type -> register objects -> query four ways
// ===========================================================================

#[tokio::test]
async fn the_whole_catalog_is_exercisable_over_http() {
    let Api { app, _dir } = api();

    // --- health, before anything ---
    let (st, body) = call(&app, get("/api/catalog/health")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert!(
        body["parts"].is_object(),
        "health must report a measurement"
    );

    // --- declare all six of noetl's internal object types, over the API ---
    for t in [
        "playbook",
        "credential",
        "mcp",
        "agent",
        "memory",
        "subscription",
    ] {
        let (st, b) = call(
            &app,
            post_auth("/api/catalog/types", serde_json::json!({ "name": t })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "declaring {t}: {b}");
        assert_eq!(b["known_noetl_type"], true, "{t} is one of noetl's six");
    }
    // And a SEVENTH type noetl does not know — no code change, reported not refused.
    let (st, b) = call(
        &app,
        post_auth(
            "/api/catalog/types",
            serde_json::json!({ "name": "dashboard" }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "a new type must be accepted: {b}");
    assert_eq!(
        b["known_noetl_type"], false,
        "an unknown type must be REPORTED as unknown, and still accepted"
    );

    let (st, b) = call(&app, get("/api/catalog/types")).await;
    assert_eq!(st, StatusCode::OK);
    // ⚠ Both numbers reported: known, and declared-here.
    let known: Vec<String> = serde_json::from_value(b["known_noetl_types"].clone()).expect("k");
    let declared: Vec<String> = serde_json::from_value(b["declared_here"].clone()).expect("d");
    assert_eq!(known.len(), 6, "noetl has six known internal object types");
    assert_eq!(declared, known, "all six were declared through the API");

    // --- register objects: 9 playbooks + 3 credentials + 2 agents ---
    let playbooks: Vec<String> = (0..9).map(|i| format!("muno/playbooks/pb_{i}")).collect();
    let credentials: Vec<String> = ["adiona_actor", "adiona_migrator", "duffel_test"]
        .iter()
        .map(|c| format!("credential/{c}"))
        .collect();
    let agents: Vec<String> = (0..2).map(|i| format!("agents/mcp/a_{i}")).collect();

    for (ty, paths) in [
        ("playbook", &playbooks),
        ("credential", &credentials),
        ("agent", &agents),
    ] {
        for p in paths {
            let (st, b) = call(
                &app,
                post_auth(
                    "/api/catalog/objects",
                    serde_json::json!({ "resource_type": ty, "path": p }),
                ),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "registering {p}: {b}");
        }
    }

    // --- QUERY BY TYPE: set equality against what we registered ---
    for (ty, want) in [
        ("playbook", &playbooks),
        ("credential", &credentials),
        ("agent", &agents),
    ] {
        let (st, b) = call(&app, get(&format!("/api/catalog/objects?type={ty}"))).await;
        assert_eq!(st, StatusCode::OK);
        let mut got: Vec<String> = serde_json::from_value(b["paths"].clone()).expect("paths");
        got.sort();
        let mut expect = want.clone();
        expect.sort();
        println!("by type {ty}: {} of {}", got.len(), expect.len());
        assert_eq!(got, expect, "query-by-type returned the wrong SET for {ty}");
        assert_eq!(
            b["count"],
            expect.len(),
            "count must match the set it ships"
        );
    }
    // A declared type with no objects: empty, not an error and not everything.
    let (st, b) = call(&app, get("/api/catalog/objects?type=memory")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["count"], 0);

    // --- set attributes: 7 of the 9 playbooks use adiona_actor, 2 use migrator ---
    let actor_users: Vec<String> = playbooks[0..7].to_vec();
    let migrator_users: Vec<String> = playbooks[7..9].to_vec();
    for (alias, users) in [
        ("adiona_actor", &actor_users),
        ("adiona_migrator", &migrator_users),
    ] {
        for p in users {
            let (st, b) = call(
                &app,
                post_auth(
                    "/api/catalog/attributes",
                    serde_json::json!({
                        "path": p,
                        "name": format!("uses_credential.{alias}"),
                        "value": true
                    }),
                ),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "set attribute on {p}: {b}");
        }
    }

    // --- QUERY BY ATTRIBUTE (reverse): the credentials-in-use proof ---
    for (alias, want) in [
        ("adiona_actor", &actor_users),
        ("adiona_migrator", &migrator_users),
    ] {
        let (st, b) = call(
            &app,
            get(&format!(
                "/api/catalog/by-attribute?name=uses_credential.{alias}"
            )),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let mut got: Vec<String> = serde_json::from_value(b["paths"].clone()).expect("paths");
        got.sort();
        let mut expect = want.clone();
        expect.sort();
        println!("by attribute {alias}: {} of {}", got.len(), expect.len());
        // ⚠⚠ SET equality. The fold behind this returned 1 of 49 before it was keyed
        // per path, and 0 of 48 once any object unset the attribute.
        assert_eq!(
            got, expect,
            "the reverse attribute lookup returned a PARTIAL set"
        );
    }
    // The two credential populations must not overlap, and must cover all 9.
    let mut union = actor_users.clone();
    union.extend(migrator_users.clone());
    union.sort();
    let mut all_pb = playbooks.clone();
    all_pb.sort();
    assert_eq!(union, all_pb, "every playbook accounted for exactly once");

    // --- assert relations: a real graph among noetl objects ---
    // 4 playbooks invoke agent a_0; 2 invoke a_1; every playbook requires its credential.
    for p in &playbooks[0..4] {
        let (st, b) = call(
            &app,
            post_auth(
                "/api/catalog/relations",
                serde_json::json!({
                    "from_type": "playbook", "from_path": p,
                    "to_type": "agent", "to_path": &agents[0],
                    "kind": "invokes"
                }),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{b}");
    }
    for p in &playbooks[4..6] {
        let (st, _) = call(
            &app,
            post_auth(
                "/api/catalog/relations",
                serde_json::json!({
                    "from_type": "playbook", "from_path": p,
                    "to_type": "agent", "to_path": &agents[1],
                    "kind": "invokes"
                }),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    // A DIFFERENT kind to the same target, to prove kinds stay distinguishable.
    let (st, _) = call(
        &app,
        post_auth(
            "/api/catalog/relations",
            serde_json::json!({
                "from_type": "playbook", "from_path": &playbooks[0],
                "to_type": "agent", "to_path": &agents[0],
                "kind": "requires"
            }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // --- QUERY BY RELATION (forward) ---
    let (st, b) = call(
        &app,
        get(&format!("/api/catalog/relations/{}", playbooks[0])),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mut kinds: Vec<String> = b["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or("").to_string())
        .collect();
    kinds.sort();
    println!("forward edges from pb_0: {kinds:?}");
    // ⚠ Two kinds to one target are two edges and must stay distinguishable — this is
    // the collapse that made a hierarchy parent and an M:N join read identically.
    assert_eq!(kinds, vec!["invokes", "requires"]);

    // --- QUERY BY REVERSE RELATION: who invokes each agent ---
    let (st, b) = call(
        &app,
        get(&format!("/api/catalog/relations-to/{}", agents[0])),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mut got: Vec<(String, String)> = b["callers"]
        .as_array()
        .expect("callers")
        .iter()
        .map(|c| {
            (
                c["from_path"].as_str().unwrap_or("").to_string(),
                c["kind"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    got.sort();
    let mut expect: Vec<(String, String)> = playbooks[0..4]
        .iter()
        .map(|p| (p.clone(), "invokes".to_string()))
        .collect();
    expect.push((playbooks[0].clone(), "requires".to_string()));
    expect.sort();
    println!("callers of a_0: {} of {}", got.len(), expect.len());
    // ⚠⚠ SET equality: a naive fold returned 1 of 40 callers for a shared dependency,
    // and "1 caller" is the answer that gets a dependency deleted.
    assert_eq!(
        got, expect,
        "the reverse relation lookup returned a PARTIAL set"
    );

    let (st, b) = call(
        &app,
        get(&format!("/api/catalog/relations-to/{}", agents[1])),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["count"], 2, "a_1 has exactly its two callers");

    // An object nothing references: empty, which is the answer that authorises removal.
    let (st, b) = call(&app, get("/api/catalog/relations-to/agents/mcp/unused")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["count"], 0);

    // --- the lifecycle, over the API ---
    let (st, b) = call(&app, post_auth("/api/catalog/tick", serde_json::json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    for k in ["sealed", "merged", "reclaimed"] {
        assert!(b[k].is_number(), "tick must report {k} separately");
    }

    // --- metrics, over the API, with the pins present ---
    let res = app.clone().oneshot(get("/metrics")).await.expect("metrics");
    assert_eq!(res.status(), StatusCode::OK);
    let text = String::from_utf8_lossy(&res.into_body().collect().await.expect("b").to_bytes())
        .into_owned();
    println!("metrics bytes over HTTP: {}", text.len());
    assert!(
        text.contains("catalog_tick_total{outcome=\"reclaimed\"}"),
        "the reclaim series must be present in a scrape, pinned even at 0"
    );
    assert!(text.contains("catalog_build_info"));
}

// ===========================================================================
// Auth
// ===========================================================================

#[tokio::test]
async fn a_write_without_a_token_is_refused_and_reads_still_work() {
    let Api { app, _dir } = api();

    // No Authorization header at all.
    let req = Request::builder()
        .method("POST")
        .uri("/api/catalog/types")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({"name": "x"}).to_string()))
        .expect("req");
    let (st, _) = call(&app, req).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "a write with no token must be 403"
    );

    // Wrong token.
    let req = Request::builder()
        .method("POST")
        .uri("/api/catalog/types")
        .header("content-type", "application/json")
        .header("authorization", "Bearer wrong")
        .body(Body::from(serde_json::json!({"name": "x"}).to_string()))
        .expect("req");
    let (st, _) = call(&app, req).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Wrong scheme.
    let req = Request::builder()
        .method("POST")
        .uri("/api/catalog/types")
        .header("content-type", "application/json")
        .header("authorization", format!("Basic {TOKEN}"))
        .body(Body::from(serde_json::json!({"name": "x"}).to_string()))
        .expect("req");
    let (st, _) = call(&app, req).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "a non-Bearer scheme must be 403");

    // ⚠ And the refused writes left NOTHING behind.
    let (st, b) = call(&app, get("/api/catalog/objects?type=x")).await;
    assert_eq!(st, StatusCode::OK, "reads are open");
    assert_eq!(b["count"], 0, "a refused write created state");
}

/// ⚠⚠ An UNSET token must be 503, not "allow". A privileged surface gets no permissive
/// default — an unset token meaning "open" is how a misconfigured deploy silently exposes
/// a write path.
#[tokio::test]
async fn an_unset_token_is_503_not_permissive() {
    // Unconfigured, injected explicitly — no env mutation needed.
    let Api { app, _dir } = api_with_token(None);

    let (st, body) = call(
        &app,
        post_auth("/api/catalog/types", serde_json::json!({"name": "x"})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::SERVICE_UNAVAILABLE,
        "an unset token must refuse, not allow: {body}"
    );

    // Reads remain available — the catalog's inventory is not secret, its mutation is.
    let (st, _) = call(&app, get("/api/catalog/health")).await;
    assert_eq!(st, StatusCode::OK);
}

/// An unknown relation kind must be refused at the API, not coerced — an edge whose
/// label nothing can query is an edge whose retraction can never match.
#[tokio::test]
async fn an_unknown_relation_kind_is_refused() {
    let Api { app, _dir } = api();

    // ⚠ My first draft of this test expected `"Invokes"` to be REFUSED. That was wrong,
    // and the code was right: the API lowercases the label, so the Debug spelling
    // normalises to the canonical `invokes`. Being forgiving at the edge and canonical in
    // storage is the correct shape — it is the lesson of noetl/server#429, where two
    // spellings of one kind became two populations. So the property to assert is that
    // casing is ACCEPTED and normalised, and that a kind which does not exist at all is
    // refused.
    for spelling in ["Invokes", "INVOKES", "  invokes  "] {
        let (st, b) = call(
            &app,
            post_auth(
                "/api/catalog/relations",
                serde_json::json!({
                    "from_type": "playbook", "from_path": "a",
                    "to_type": "playbook", "to_path": "b",
                    "kind": spelling
                }),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{spelling:?} must normalise: {b}");
    }
    // And all of those are ONE edge, because they are one kind.
    let (st, b) = call(&app, get("/api/catalog/relations/a")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        b["count"], 1,
        "three spellings of one kind must be one edge, not three"
    );
    assert_eq!(b["edges"][0]["kind"], "invokes", "stored canonically");

    // A kind that does not exist at all IS refused — an edge whose label nothing can
    // query is an edge whose retraction can never match.
    let (st, body) = call(
        &app,
        post_auth(
            "/api/catalog/relations",
            serde_json::json!({
                "from_type": "playbook", "from_path": "a",
                "to_type": "playbook", "to_path": "b",
                "kind": "totally_bogus_kind"
            }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    let msg = body.as_str().unwrap_or_default();
    assert!(
        msg.contains("unknown relation kind") && msg.contains("invokes"),
        "the error must name the problem AND offer the valid set: {msg}"
    );
}

/// A read for something absent is 404, not an empty 200 that reads like "exists but
/// empty".
#[tokio::test]
async fn absent_things_are_404_not_empty_200() {
    let Api { app, _dir } = api();
    let (st, _) = call(&app, get("/api/catalog/objects/nothing/here")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(&app, get("/api/catalog/types/nosuchtype")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

// ===========================================================================
// Attribute constraints, over HTTP
// ===========================================================================

/// A constrained attribute is enforced on an **explicit write**, and the rejection offers
/// the valid set so the caller can comply.
#[tokio::test]
async fn a_constrained_attribute_is_enforced_on_an_explicit_write() {
    let Api { app, _dir } = api();
    let (st, _) = call(
        &app,
        post_auth(
            "/api/catalog/objects",
            serde_json::json!({ "resource_type": "subscription", "path": "hooks/s1" }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Every value noetl actually allows is accepted.
    for src in ["pubsub", "nats", "kafka", "webhook"] {
        let (st, b) = call(
            &app,
            post_auth(
                "/api/catalog/attributes",
                serde_json::json!({"path":"hooks/s1","name":"spec.source","value":src}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{src} is a real noetl source: {b}");
    }

    // A value noetl rejects at registration is refused here too — the catalog must not
    // record as fact something the platform would not accept.
    let (st, body) = call(
        &app,
        post_auth(
            "/api/catalog/attributes",
            serde_json::json!({"path":"hooks/s1","name":"spec.source","value":"rabbitmq"}),
        ),
    )
    .await;
    println!("refused: {st} {body}");
    assert_eq!(
        st,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a bad value must not be stored"
    );
    let msg = body.as_str().unwrap_or_default();
    // ⚠ The rejection must name the offender AND offer the valid set.
    assert!(
        msg.contains("rabbitmq"),
        "must quote the offending value: {msg}"
    );
    assert!(msg.contains("pubsub"), "must offer the valid set: {msg}");

    // And the refusal stored nothing.
    let (st, b) = call(&app, get("/api/catalog/attributes/hooks/s1")).await;
    assert_eq!(st, StatusCode::OK);
    // ⚠ My first draft guessed the JSON shape as `{"Text": ...}`. `AttributeValue` is
    // `#[serde(tag = "type", content = "value", rename_all = "snake_case")]`, so it is
    // `{"type":"text","value":"webhook"}`. Asserting the real wire shape, since this is
    // the shape a client has to parse.
    assert_eq!(
        b["spec.source"]["value"]["type"], "text",
        "the typed union must keep its tag on the wire"
    );
    assert_eq!(
        b["spec.source"]["value"]["value"], "webhook",
        "the last VALID write must still be the live value — a refused write must not \
         disturb it"
    );

    // A rejected tool kind is refused, and `agent`/`mcp` are the interesting case: valid
    // resource types, rejected tool kinds.
    for bad in ["agent", "mcp"] {
        let (st, body) = call(
            &app,
            post_auth(
                "/api/catalog/attributes",
                serde_json::json!({"path":"hooks/s1","name":format!("uses_tool.{bad}"),"value":true}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{bad}: {body}");
    }
    // A real tool kind is accepted.
    let (st, _) = call(
        &app,
        post_auth(
            "/api/catalog/attributes",
            serde_json::json!({"path":"hooks/s1","name":"uses_tool.postgres","value":true}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

/// The constraints are **published**, and what is published must match what is enforced —
/// a caller refused by a rule it cannot read has no way to comply.
#[tokio::test]
async fn the_constraints_are_published_and_match_what_is_enforced() {
    let Api { app, _dir } = api();
    let (st, b) = call(&app, get("/api/catalog/constraints")).await;
    assert_eq!(st, StatusCode::OK);
    println!(
        "published: {}",
        serde_json::to_string_pretty(&b).unwrap_or_default()
    );

    let entries = b["constraints"].as_array().expect("constraints");
    assert_eq!(entries.len(), 4, "four constrained attributes");

    // The asymmetry is documented in the response itself.
    assert!(b["enforced_on"]
        .as_str()
        .unwrap_or("")
        .contains("explicit write"));
    assert!(b["not_enforced_on"]
        .as_str()
        .unwrap_or("")
        .contains("extraction"));

    // uses_tool publishes all 25 real kinds.
    let tool = entries
        .iter()
        .find(|e| e["attribute"] == "uses_tool.<kind>")
        .expect("uses_tool entry");
    assert_eq!(
        tool["allowed"].as_array().expect("allowed").len(),
        25,
        "noetl has 25 tool kinds"
    );

    // ⚠ Every published value must actually be accepted — otherwise the published list is
    // a decorative representation of a rule it does not describe.
    let (st, _) = call(
        &app,
        post_auth(
            "/api/catalog/objects",
            serde_json::json!({ "resource_type": "subscription", "path": "hooks/s2" }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    for e in entries {
        let name = e["attribute"].as_str().unwrap_or("");
        if name.contains('<') {
            continue; // the templated one is covered above
        }
        for v in e["allowed"].as_array().expect("allowed") {
            let (st, b) = call(
                &app,
                post_auth(
                    "/api/catalog/attributes",
                    serde_json::json!({"path":"hooks/s2","name":name,"value":v}),
                ),
            )
            .await;
            assert_eq!(
                st,
                StatusCode::OK,
                "{name} publishes {v} but rejects it: {b}"
            );
        }
    }
}

/// ⚠⚠ Ingestion must NOT be gated by the constraints. A document declaring a value noetl
/// rejects is still catalogued — recording it is how anyone finds it.
///
/// This is not hypothetical: `noetl/travel`'s `playbooks/catalog/calendar/list.yaml`
/// declares `tool.kind: agent`, which `validate_tool_kinds` rejects (noetl/ai-meta#256).
/// The first draft validated inside `set_attribute`, which `register_from_source` calls —
/// so enforcement leaked into extraction and the real corpus failed to ingest at all.
#[tokio::test]
async fn ingestion_records_a_value_the_platform_would_reject() {
    let Api { app, _dir } = api();
    let dir = tempfile::tempdir().expect("td");
    let src = dir.path().join("pb");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(
        src.join("unrunnable.yaml"),
        "kind: Playbook\nmetadata:\n  path: pb/unrunnable\nworkflow:\n  - step: s\n    tool:\n      kind: agent\n      path: a/b\n",
    )
    .expect("write");

    let (st, b) = call(
        &app,
        post_auth(
            "/api/catalog/ingest",
            serde_json::json!({"source": format!("dir:{}", dir.path().display()), "subpath": "pb"}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "ingestion must not be gated: {b}");
    println!("ingest: {b}");
    assert_eq!(b["scanned"], 1);
    assert_eq!(
        b["registered"], 1,
        "the document registered despite a rejected tool kind"
    );
    assert_eq!(b["accounts_for_every_file"], true);

    // ⭐ And the whole point: the unrunnable playbook is now QUERYABLE.
    let (st, b) = call(&app, get("/api/catalog/by-attribute?name=uses_tool.agent")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["count"], 1);
    assert_eq!(b["paths"][0], "pb/unrunnable");

    // ⚠ But an EXPLICIT write of the same fact is still refused — the asymmetry holds in
    // both directions at once.
    let (st, _) = call(
        &app,
        post_auth(
            "/api/catalog/attributes",
            serde_json::json!({"path":"pb/unrunnable","name":"uses_tool.agent","value":true}),
        ),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::INTERNAL_SERVER_ERROR,
        "an explicit assertion of a rejected kind must still be refused"
    );
}

/// Bulk ingest over HTTP reports its denominator and every skip reason.
#[tokio::test]
async fn bulk_ingest_over_http_reports_the_denominator() {
    let Api { app, _dir } = api();
    let dir = tempfile::tempdir().expect("td");
    let src = dir.path().join("objs");
    std::fs::create_dir_all(&src).expect("mkdir");
    for (f, body) in [
        ("ok.yaml", "kind: Playbook\nmetadata:\n  path: a/ok\n"),
        ("cred.yaml", "kind: Credential\nmetadata:\n  path: c/one\n"),
        ("new.yaml", "kind: Dashboard\nmetadata:\n  path: d/one\n"),
        ("broken.yaml", "kind: Playbook\n  bad: [unclosed\n"),
        ("nokind.yaml", "metadata:\n  path: x/y\n"),
    ] {
        std::fs::write(src.join(f), body).expect("write");
    }

    let (st, b) = call(
        &app,
        post_auth(
            "/api/catalog/ingest",
            serde_json::json!({"source": format!("dir:{}", dir.path().display()), "subpath": "objs"}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    println!("bulk ingest: {b}");
    assert_eq!(b["scanned"], 5, "the denominator");
    assert_eq!(b["registered"], 3);
    assert_eq!(b["skipped"].as_array().expect("skipped").len(), 2);
    assert_eq!(b["accounts_for_every_file"], true);
    // A non-noetl type registered and is REPORTED, not refused.
    let nt: Vec<String> = serde_json::from_value(b["new_types"].clone()).expect("nt");
    assert_eq!(nt, vec!["dashboard".to_string()]);
    // Every skip carries a reason.
    for s in b["skipped"].as_array().expect("skipped") {
        assert!(!s["reason"].as_str().unwrap_or("").is_empty());
    }

    // A bad source spec is refused rather than silently scanning nothing.
    let (st, _) = call(
        &app,
        post_auth(
            "/api/catalog/ingest",
            serde_json::json!({"source": "nonsense"}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}
