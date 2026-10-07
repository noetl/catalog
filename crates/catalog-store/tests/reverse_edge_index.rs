//! **Who calls this?** — the edge direction that matters when retiring a resource.
//!
//! # Measured on real data before this existed
//!
//! Ingesting the `muno/*` playbooks from `noetl/travel@origin/main` produced edges that
//! matched hand-derived ground truth exactly:
//!
//! ```text
//! muno/playbooks/flights-details    -> automation/agents/mcp/duffel
//! muno/playbooks/hotel-cards        -> automation/agents/mcp/hotelbeds
//! muno/playbooks/itinerary-planner  -> automation/agents/mcp/firestore
//! muno/playbooks/profile            -> automation/agents/mcp/firestore, .../google-places
//! ```
//!
//! 7 reference occurrences folding to **5 distinct edges** — `flights-details` names
//! duffel in two steps and `profile` names firestore in two, and an edge asserted twice
//! is one edge. So the extraction was already correct; what was missing was the reverse
//! direction. `automation/agents/mcp/firestore` has **2 callers**, and answering that
//! meant scanning every path in the catalog.
//!
//! # Where the correctness actually lives — found by two mutations that did NOT fire
//!
//! The hazard was predicted as "folding by `edge_key()` collapses all callers, because
//! under one reverse key every caller shares the same `(to_path, to_version, kind)`".
//! That prediction was **wrong**, and two mutations prove it:
//!
//! | mutation | result | why |
//! | :-- | :-- | :-- |
//! | `relations_to` folds by `edge_key()` | **passed** | `edge_key()`'s `ReverseEdge` arm already keys on `from_path`, so it is not the forward key |
//! | `edge_key()`'s reverse arm keys on the target instead | **passed** | `relations_to` never calls `edge_key()`; it has its own closure |
//! | **`relations_to`'s own closure keys on `index`** (shared by every caller) | **FAILED** | this is the real load-bearing line |
//!
//! ```text
//! callers of automation/agents/mcp/firestore: 1, expected 2
//! callers of shared/dep: 1, expected 40
//! ```
//!
//! So the guard is on `relations_to`'s fold key and nowhere else. Worth stating,
//! because a mutation that passes is not reassurance — it means the thing mutated was
//! not the thing holding the property up, and two confident claims about where the
//! hazard lived were both wrong before the third landed.
//!
//! For a central MCP playbook, "1 caller" is the answer that gets a dependency deleted.

use catalog_model::{Entity, EntityRef, Relation, RelationKind};
use catalog_store::{CatalogStore, StoreConfig};

fn entity(path: &str) -> Entity {
    Entity {
        resource_type: "playbook".into(),
        path: path.into(),
        version: 1,
        entity_id: 0,
        content: None,
        content_sha256: format!("{:064}", 1),
        archived_at: None,
    }
}

fn edge(from: &str, to: &str) -> Relation {
    Relation {
        from_entity: EntityRef {
            resource_type: "playbook".into(),
            path: from.into(),
            version: Some(1),
        },
        to_entity: EntityRef {
            resource_type: "playbook".into(),
            path: to.into(),
            version: None,
        },
        kind: RelationKind::Invokes,
        discovered_by: catalog_model::Provenance::Declared,
    }
}

/// The real corpus's shape, including the two-callers-of-firestore case.
fn seed_real_shape(store: &mut CatalogStore) {
    for p in [
        "muno/playbooks/flights-details",
        "muno/playbooks/hotel-cards",
        "muno/playbooks/itinerary-planner",
        "muno/playbooks/profile",
    ] {
        store.register(entity(p)).expect("reg");
    }
    store
        .assert_relation(edge(
            "muno/playbooks/flights-details",
            "automation/agents/mcp/duffel",
        ))
        .expect("e");
    store
        .assert_relation(edge(
            "muno/playbooks/hotel-cards",
            "automation/agents/mcp/hotelbeds",
        ))
        .expect("e");
    store
        .assert_relation(edge(
            "muno/playbooks/itinerary-planner",
            "automation/agents/mcp/firestore",
        ))
        .expect("e");
    store
        .assert_relation(edge(
            "muno/playbooks/profile",
            "automation/agents/mcp/firestore",
        ))
        .expect("e");
    store
        .assert_relation(edge(
            "muno/playbooks/profile",
            "automation/agents/mcp/google-places",
        ))
        .expect("e");
}

#[test]
fn who_calls_this_returns_every_caller_not_one() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed_real_shape(&mut store);

    let firestore = store
        .relations_to("automation/agents/mcp/firestore")
        .expect("rev");
    println!(
        "callers of automation/agents/mcp/firestore: {}, expected 2",
        firestore.len()
    );
    // ⚠ Set equality. A count of 2 could be the wrong 2.
    assert_eq!(
        firestore,
        vec![
            (
                "muno/playbooks/itinerary-planner".to_string(),
                "invokes".to_string()
            ),
            ("muno/playbooks/profile".to_string(), "invokes".to_string()),
        ]
    );

    // Single-caller targets, to show the fold is not merely returning everything.
    assert_eq!(
        store
            .relations_to("automation/agents/mcp/duffel")
            .expect("rev"),
        vec![(
            "muno/playbooks/flights-details".to_string(),
            "invokes".to_string()
        )]
    );
    assert_eq!(
        store
            .relations_to("automation/agents/mcp/google-places")
            .expect("rev"),
        vec![("muno/playbooks/profile".to_string(), "invokes".to_string())]
    );

    // A target nobody calls: empty, not an error and not everything.
    assert!(store
        .relations_to("automation/agents/mcp/nobody")
        .expect("rev")
        .is_empty());

    // The forward direction is undisturbed: profile still has exactly its 2 edges.
    let fwd = store.relations_from("muno/playbooks/profile").expect("fwd");
    assert_eq!(
        fwd.len(),
        2,
        "reverse rows leaked into the forward edge fold"
    );
}

/// The scale case. A shared dependency is the whole reason to ask "who calls this", and
/// it is where a collapsing fold does the most damage.
#[test]
fn a_shared_dependency_reports_all_of_its_callers() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    let mut callers: Vec<(String, String)> = Vec::new();
    for i in 0..40 {
        let from = format!("muno/playbooks/caller_{i:02}");
        store.register(entity(&from)).expect("reg");
        store.assert_relation(edge(&from, "shared/dep")).expect("e");
        callers.push((from, "invokes".to_string()));
    }
    callers.sort();

    let got = store.relations_to("shared/dep").expect("rev");
    println!(
        "callers of shared/dep: {}, expected {}",
        got.len(),
        callers.len()
    );
    assert_eq!(got, callers, "a shared dependency must report every caller");
}

/// Retracting must remove the caller. Without a tombstone the caller list only grows,
/// and "who would break if I delete this" accumulates callers that no longer call it —
/// which argues against deleting something that is in fact unused.
#[test]
fn retracting_removes_the_caller() {
    let dir = tempfile::tempdir().expect("td");
    let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed_real_shape(&mut store);

    store
        .retract_relation(
            "muno/playbooks/profile",
            EntityRef {
                resource_type: "playbook".into(),
                path: "automation/agents/mcp/firestore".into(),
                version: None,
            },
            "invokes",
        )
        .expect("retract");

    let got = store
        .relations_to("automation/agents/mcp/firestore")
        .expect("rev");
    println!("after retract: {got:?}");
    assert_eq!(
        got,
        vec![(
            "muno/playbooks/itinerary-planner".to_string(),
            "invokes".to_string()
        )],
        "the retracted caller is still listed"
    );

    // The forward view must agree — two views of one write cannot disagree.
    let fwd = store.relations_from("muno/playbooks/profile").expect("fwd");
    assert_eq!(fwd.len(), 1, "forward still shows the retracted edge");
    assert_eq!(fwd[0].to_entity.path, "automation/agents/mcp/google-places");
}

#[test]
fn the_reverse_edge_index_survives_a_reopen() {
    let dir = tempfile::tempdir().expect("td");
    {
        let mut store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
        seed_real_shape(&mut store);
    }
    let store = CatalogStore::open(&StoreConfig::new(dir.path())).expect("reopen");
    let got = store
        .relations_to("automation/agents/mcp/firestore")
        .expect("rev");
    println!("after reopen: {} caller(s), expected 2", got.len());
    assert_eq!(got.len(), 2);
}
