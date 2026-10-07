//! An edge is identified by **(target, version, kind)** — never by the kind's payload.
//!
//! # The hazard this guards, and why it appeared the moment the FK became first-class
//!
//! Two places key an edge by its kind, and both used `format!("{:?}", kind)`. That was
//! harmless while every `RelationKind` was a bare tag. `References(ForeignKey)` carries
//! a payload, so `{:?}` renders the whole struct — and then the **same foreign key**,
//! re-asserted with `nullable` corrected, becomes a **different edge**:
//!
//! * the catalog reports the row as having two FKs to one target, which no schema has;
//! * and a retraction naming the FK does not match the row it meant to remove, so the
//!   stale edge is unremovable.
//!
//! The payload is a property *of* the edge, not part of its identity.

use catalog_model::{
    Cardinality, Entity, EntityRef, ForeignKey, Provenance, ReferentialAction, Relation,
    RelationKind,
};
use catalog_store::{CatalogStore, StoreConfig};

fn ent(rt: &str, path: &str) -> Entity {
    Entity {
        resource_type: rt.into(),
        path: path.into(),
        version: 1,
        entity_id: 0,
        content: None,
        content_sha256: "0".repeat(64),
        archived_at: None,
    }
}
fn eref(rt: &str, p: &str) -> EntityRef {
    EntityRef {
        resource_type: rt.into(),
        path: p.into(),
        version: None,
    }
}
/// ⚠ Fixtured on **noetl internal object types** (`playbook` → `mcp` / `credential`), not
/// on relational business rows.
///
/// An earlier draft used `table_row` with `categories/10`, `trip_category/1`, `trips/100`
/// — adiona's *business* entities. That was a conflation: the internal catalog holds
/// noetl's own objects, and business/domain data belongs to the **business catalog**,
/// which is part of playbooks and backed by external databases and APIs (see the
/// spec's "Two catalogs" section). A fixture teaches what belongs here, so it has to be
/// right even though the store cannot tell the difference.
fn fk_edge(from: &str, to: &str, fk: ForeignKey) -> Relation {
    Relation {
        from_entity: eref("playbook", from),
        to_entity: eref("mcp", to),
        kind: RelationKind::References(fk),
        discovered_by: Provenance::Declared,
    }
}

#[test]
fn re_asserting_one_fk_with_corrected_metadata_is_still_one_edge() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    s.register(ent("playbook", "muno/playbooks/profile"))
        .expect("r");
    s.register(ent("mcp", "automation/agents/mcp/firestore"))
        .expect("r");

    // The adiona FK, first recorded as NOT NULL (which is what the DDL says).
    s.assert_relation(fk_edge(
        "muno/playbooks/profile",
        "automation/agents/mcp/firestore",
        ForeignKey::many_to_one("mcp_entrypoint").named("r_playbook_mcp_entrypoint"),
    ))
    .expect("fk");

    // Re-asserted after correcting a detail — same constraint, same target.
    s.assert_relation(fk_edge(
        "muno/playbooks/profile",
        "automation/agents/mcp/firestore",
        ForeignKey::many_to_one("mcp_entrypoint")
            .named("r_playbook_mcp_entrypoint")
            .optional()
            .on_delete(ReferentialAction::Cascade),
    ))
    .expect("fk");

    let fwd = s.relations_from("muno/playbooks/profile").expect("fwd");
    println!("edges after re-asserting one FK: {}", fwd.len());
    assert_eq!(
        fwd.len(),
        1,
        "one foreign key became {} edges because its payload entered the edge identity",
        fwd.len()
    );
    // The latest assertion wins, as every other fold in this store does.
    let fk = fwd[0].kind.foreign_key().expect("a References edge");
    assert!(fk.nullable, "the corrected metadata should be the live one");
    assert_eq!(fk.on_delete, ReferentialAction::Cascade);

    // And the reverse answer is one caller, not two.
    let rev = s
        .relations_to("automation/agents/mcp/firestore")
        .expect("rev");
    println!("reverse: {rev:?}");
    assert_eq!(rev.len(), 1, "the reverse index also doubled the edge");
    assert_eq!(
        rev[0].1, "references",
        "the reverse answer must carry the KIND label"
    );
}

#[test]
fn a_retraction_matches_an_fk_whose_metadata_has_since_changed() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    s.register(ent("playbook", "muno/playbooks/hotel-cards"))
        .expect("r");
    s.register(ent("mcp", "automation/agents/mcp/hotelbeds"))
        .expect("r");

    s.assert_relation(fk_edge(
        "muno/playbooks/hotel-cards",
        "automation/agents/mcp/hotelbeds",
        ForeignKey::many_to_one("credential_alias").optional(),
    ))
    .expect("fk");

    // Retract by kind LABEL — a caller removing a constraint knows the target and that
    // it is a FK, not necessarily the exact payload that was stored.
    s.retract_relation(
        "muno/playbooks/hotel-cards",
        eref("mcp", "automation/agents/mcp/hotelbeds"),
        "references",
    )
    .expect("retract");

    let fwd = s.relations_from("muno/playbooks/hotel-cards").expect("fwd");
    println!("edges after retraction: {}", fwd.len());
    assert!(
        fwd.is_empty(),
        "the retraction did not match the FK, so the stale edge is unremovable: {fwd:?}"
    );
    assert!(s
        .relations_to("automation/agents/mcp/hotelbeds")
        .expect("rev")
        .is_empty());
}

/// Two FKs to the SAME target that are genuinely different references must stay
/// distinct — the identity is (target, version, kind), so a `References` and a
/// `Requires` to one target are two edges, while two `References` are one.
#[test]
fn different_kinds_to_one_target_stay_distinct() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    s.register(ent("playbook", "muno/playbooks/flights-details"))
        .expect("r");
    s.register(ent("mcp", "automation/agents/mcp/duffel"))
        .expect("r");

    s.assert_relation(fk_edge(
        "muno/playbooks/flights-details",
        "automation/agents/mcp/duffel",
        ForeignKey::many_to_one("mcp_entrypoint"),
    ))
    .expect("fk");
    s.assert_relation(Relation {
        from_entity: eref("playbook", "muno/playbooks/flights-details"),
        to_entity: eref("mcp", "automation/agents/mcp/duffel"),
        kind: RelationKind::Requires,
        discovered_by: Provenance::Declared,
    })
    .expect("req");

    let fwd = s
        .relations_from("muno/playbooks/flights-details")
        .expect("fwd");
    println!("distinct kinds to one target: {}", fwd.len());
    assert_eq!(
        fwd.len(),
        2,
        "a References and a Requires are different edges"
    );

    let mut kinds: Vec<&str> = fwd.iter().map(|r| r.kind.discriminant()).collect();
    kinds.sort();
    assert_eq!(kinds, vec!["references", "requires"]);

    // ⚠ The reverse answer must distinguish them — this is the exact failure the probe
    // found before the FK was first-class, where both read "Requires".
    let mut rev = s.relations_to("automation/agents/mcp/duffel").expect("rev");
    rev.sort();
    println!("reverse, distinguishable: {rev:?}");
    assert_eq!(
        rev,
        vec![
            (
                "muno/playbooks/flights-details".to_string(),
                "references".to_string()
            ),
            (
                "muno/playbooks/flights-details".to_string(),
                "requires".to_string()
            ),
        ]
    );
}

/// The FK payload survives a round trip through the store, since a reader asking
/// "can I delete this row" needs `on_delete`, not just the existence of an edge.
#[test]
fn the_fk_payload_round_trips() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    s.register(ent("playbook", "muno/playbooks/itinerary-planner"))
        .expect("r");
    s.register(ent("credential", "credential/adiona_actor"))
        .expect("r");

    let fk = ForeignKey {
        nullable: true,
        cardinality: Cardinality::ManyToMany,
        on_delete: ReferentialAction::SetNull,
        columns: vec!["x_id".into(), "y_id".into()],
        constraint_name: Some("r_composite".into()),
    };
    s.assert_relation(fk_edge(
        "muno/playbooks/itinerary-planner",
        "credential/adiona_actor",
        fk.clone(),
    ))
    .expect("fk");

    let back = s
        .relations_from("muno/playbooks/itinerary-planner")
        .expect("fwd");
    assert_eq!(back.len(), 1);
    assert_eq!(
        back[0].kind.foreign_key().expect("fk"),
        &fk,
        "the FK payload did not survive the round trip"
    );
}

/// An unknown kind label must be **refused**, not written as a tombstone that matches
/// nothing. A no-op retraction is the worst shape: the caller believes the edge is gone,
/// the forward read still returns it, and nothing errored.
///
/// This is not hypothetical — it happened. When the reverse index's kind label changed
/// from `Debug` casing (`"Invokes"`) to the discriminant (`"invokes"`), a retraction
/// passing the old spelling appended a tombstone that matched no edge.
#[test]
fn a_retraction_with_an_unknown_kind_is_refused() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    s.register(ent("playbook", "muno/playbooks/itinerary-planner"))
        .expect("r");
    s.register(ent("credential", "credential/adiona_actor"))
        .expect("r");
    s.assert_relation(fk_edge(
        "muno/playbooks/itinerary-planner",
        "credential/adiona_actor",
        ForeignKey::many_to_one("credential_alias"),
    ))
    .expect("fk");

    // The old Debug spelling, which is exactly the mistake that occurred.
    let err = s
        .retract_relation(
            "muno/playbooks/itinerary-planner",
            eref("credential", "credential/adiona_actor"),
            "References",
        )
        .expect_err("an unknown kind label must be refused");
    println!("refused: {err}");
    assert!(err.to_string().contains("unknown relation kind"));

    // ⚠ And the refusal must leave the edge intact — a half-applied retraction would be
    // worse than the silent no-op it replaces.
    assert_eq!(
        s.relations_from("muno/playbooks/itinerary-planner")
            .expect("fwd")
            .len(),
        1,
        "the refused retraction disturbed the edge"
    );

    // The canonical label works.
    s.retract_relation(
        "muno/playbooks/itinerary-planner",
        eref("credential", "credential/adiona_actor"),
        "references",
    )
    .expect("canonical label");
    assert!(s
        .relations_from("muno/playbooks/itinerary-planner")
        .expect("fwd")
        .is_empty());
}
