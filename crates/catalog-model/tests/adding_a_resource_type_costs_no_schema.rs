//! Acceptance criterion **AC3**: adding a resource type is data, not schema.
//!
//! This is the one test that protects the design premise rather than a behaviour.
//! The reference model fails it by construction — adiona needs an
//! `<entity>_attributes` table and an `<entity>_category` table per entity type, so
//! adding a type is DDL. If this repo ever drifts into the same shape, the symptom
//! is that "adding a type" starts meaning "adding a `Dataset` impl", and every
//! individual change will look reasonable while the property is lost.
//!
//! So the assertion is deliberately structural: exercise a brand-new resource type
//! end to end, and assert the crate's storage-shape surface did not grow.

use catalog_model::{
    Attribute, AttributeValue, Entity, EntityRef, Provenance, Relation, RelationKind, ResourceType,
};

/// Every `.rs` file in this crate's `src/`, as source text.
fn crate_sources() -> Vec<(String, String)> {
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    let mut stack = vec![src_dir.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)
            .unwrap_or_else(|e| panic!("reading {}: {e}", d.display()))
            .flatten()
        {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&p).expect("read source");
                out.push((p.file_name().unwrap().to_string_lossy().into(), text));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_brand_new_resource_type_needs_no_new_storage_shape() {
    let sources = crate_sources();

    // ⚠ Assert the extraction BEFORE asserting about it. A scan that walked the
    // wrong directory finds zero `Dataset` impls and reports a clean pass — which
    // is indistinguishable from the property actually holding.
    println!(
        "AC3: examined {} source files in src/ ({})",
        sources.len(),
        sources
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert!(
        sources.len() >= 5,
        "found only {} .rs files under src/ — this scan examined the wrong tree, \
         which would make the Dataset count below meaningless",
        sources.len()
    );

    // --- Exercise a type that did not exist when this crate was written. ---
    //
    // Not one of the six names today's FK permits, on purpose: if the model had a
    // hard-coded kind list anywhere, this is where it would fail.
    let novel = ResourceType::new("Widget", false, true).specializing("playbook");
    assert_eq!(novel.name, "widget");
    assert_eq!(novel.supertype.as_deref(), Some("playbook"));

    let entity = Entity {
        resource_type: novel.name.clone(),
        path: "muno/widgets/fare-table".into(),
        // Above the i16 ceiling too, so AC3 and AC6 cannot pass vacuously together.
        version: 40_001,
        entity_id: 99,
        content: None,
        content_sha256: "c".repeat(64),
        archived_at: None,
    };

    // Two attributes on one entity — the shape a latest-wins fold would silently
    // collapse. Asserted at the model level here; the storage positive control
    // lands with P2.
    let attrs = vec![
        Attribute::new(
            entity.entity_id,
            "labels.owner",
            AttributeValue::Text("muno".into()),
        ),
        Attribute::new(
            entity.entity_id,
            "exposed_in_ui",
            AttributeValue::Flag(true),
        ),
    ];

    let relation = Relation::new(
        entity.as_ref_pinned(),
        EntityRef::latest("playbook", "muno/playbooks/itinerary-planner"),
        RelationKind::Requires,
        Provenance::Extracted {
            at: 1_760_000_000_000_000,
        },
    );

    // Everything round-trips, for a type the crate has never heard of.
    let novel_json = serde_json::to_string(&novel).expect("ser type");
    let entity_json = serde_json::to_string(&entity).expect("ser entity");
    let rel_json = serde_json::to_string(&relation).expect("ser relation");

    assert_eq!(
        serde_json::from_str::<ResourceType>(&novel_json).expect("de type"),
        novel
    );
    assert_eq!(
        serde_json::from_str::<Entity>(&entity_json).expect("de entity"),
        entity
    );
    assert_eq!(
        serde_json::from_str::<Relation>(&rel_json).expect("de relation"),
        relation
    );
    for a in &attrs {
        let back: Attribute =
            serde_json::from_str(&serde_json::to_string(a).expect("ser attr")).expect("de attr");
        assert_eq!(&back, a);
    }
    assert_eq!(attrs.len(), 2, "both attributes must survive");

    // --- The structural half: the storage surface did not grow. ---
    let dataset_impls: usize = sources
        .iter()
        .map(|(_, text)| {
            text.lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .filter(|l| l.contains("impl Dataset for") || l.contains("impl ehdb_l0::Dataset"))
                .count()
        })
        .sum();

    assert_eq!(
        dataset_impls, 0,
        "adding the `widget` resource type above required no `Dataset` impl, and \
         this crate currently declares {dataset_impls}. P1 is model-only, so the \
         count must be 0 here. When P2 lands the four c1..c4 datasets this becomes \
         exactly 4 — and it must STAY 4 no matter how many resource types exist. \
         If a resource type ever needs its own dataset, the model has regressed to \
         the per-type-table shape this project exists to remove."
    );

    // And no kind list is hard-coded anywhere that would need editing.
    for (name, text) in &sources {
        let code: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in ["\"playbook\" =>", "\"subscription\" =>", "match kind {"] {
            assert!(
                !code.contains(forbidden),
                "{name} dispatches on a hard-coded resource-type name ({forbidden:?}). \
                 A resource type must be data; a match arm per kind is the shape that \
                 makes adding one a code change."
            );
        }
    }
}
