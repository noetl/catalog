//! `uses_tool.*` and `uses_credential.*` — the two facts every real playbook carries.
//!
//! Driven by a measurement, not a guess. The first run of the ingest driver over the 53
//! `adiona/playbooks/*.yaml` on `noetl/travel@origin/main` reported:
//!
//! ```text
//! scanned=53 registered=53 skipped=0 relations=0 attributes=0 kinds={"playbook": 53}
//! ```
//!
//! `relations=0` is **correct**: they are leaf playbooks calling no child. (A first
//! regex said 53 of 53 carried a child reference — it was matching each document's own
//! `metadata.path`, the same self-reference error that once made an oracle disagree with
//! a correct extractor.) `attributes=0` was a real gap: for a playbook,
//! `find_attributes` only looked at `metadata.labels`, and none of the 53 have labels.
//!
//! What all 53 do carry:
//!
//! ```text
//! tool kinds  : 53x postgres
//! auth aliases: 49x adiona_actor, 4x adiona_migrator
//! ```

use catalog_extract::find_attributes;
use catalog_model::AttributeValue;

/// The exact shape of `adiona/playbooks/catalog_list.yaml`, trimmed.
const REAL_SHAPE: &str = r#"
apiVersion: noetl.io/v2
kind: Playbook
metadata:
  name: adiona_catalog_list
  path: adiona/v1/catalog_list
workload:
  request: {}
workflow:
  - step: start
    tool:
      kind: postgres
      auth: adiona_actor
      params:
        - "{{ request | tojson | b64encode }}"
      command: |
        SELECT 1;
"#;

fn names(src: &str) -> Vec<String> {
    let mut v: Vec<String> = find_attributes(src, 7)
        .expect("parse")
        .into_iter()
        .map(|a| a.name)
        .collect();
    v.sort();
    v
}

#[test]
fn the_real_adiona_shape_yields_its_tool_and_its_credential() {
    let got = names(REAL_SHAPE);
    println!("extracted: {got:?}");
    assert_eq!(
        got,
        vec![
            "uses_credential.adiona_actor".to_string(),
            "uses_tool.postgres".to_string()
        ],
        "the shape that produced attributes=0 across 53 real documents"
    );

    // The value is membership, so the NAME carries which tool/alias.
    let attrs = find_attributes(REAL_SHAPE, 7).expect("parse");
    for a in &attrs {
        assert_eq!(
            a.value,
            AttributeValue::Flag(true),
            "{} should be a membership flag",
            a.name
        );
        assert_eq!(a.entity_id, 7, "entity_id must be threaded through");
    }
}

#[test]
fn a_tool_or_alias_named_in_many_steps_is_one_fact() {
    let src = r#"
kind: Playbook
metadata:
  path: p/multi
workflow:
  - step: a
    tool: { kind: postgres, auth: adiona_actor }
  - step: b
    tool: { kind: postgres, auth: adiona_actor }
  - step: c
    tool: { kind: http, auth: duffel_test }
"#;
    assert_eq!(
        names(src),
        vec![
            "uses_credential.adiona_actor",
            "uses_credential.duffel_test",
            "uses_tool.http",
            "uses_tool.postgres",
        ]
    );
}

/// ⚠⚠ The guard with teeth. `execution-model.md`: a playbook references a credential by
/// **alias**, and the keychain resolves it at execution time. A mapping under `auth:` is
/// an inline credential, and extracting it would copy a secret into a second store.
///
/// This test fails if the `as_str()` check in `collect_playbook_facts` is ever relaxed.
#[test]
fn an_inline_credential_mapping_is_never_extracted() {
    let src = r#"
kind: Playbook
metadata:
  path: p/inline
workflow:
  - step: bad
    tool:
      kind: postgres
      auth:
        user: svc
        password: hunter2
"#;
    let got = names(src);
    println!("extracted from an inline-credential document: {got:?}");

    // The tool fact is still correct and still wanted.
    assert_eq!(
        got,
        vec!["uses_tool.postgres"],
        "a mapping under auth: must contribute NO credential attribute"
    );

    // And belt-and-braces: no extracted value may contain the inline material, whatever
    // name it ended up under.
    for a in find_attributes(src, 1).expect("parse") {
        let rendered = format!("{:?}", a.value);
        assert!(
            !rendered.contains("hunter2") && !rendered.contains("svc"),
            "attribute {} leaked inline credential material: {rendered}",
            a.name
        );
    }
}

/// A templated alias names a binding, not a credential. Cataloguing `{{ db_credential }}`
/// would assert a dependency on something no keychain has under that name.
#[test]
fn a_templated_alias_is_not_a_credential_name() {
    let src = r#"
kind: Playbook
metadata:
  path: p/templated
workflow:
  - step: a
    tool: { kind: postgres, auth: "{{ db_credential }}" }
"#;
    assert_eq!(names(src), vec!["uses_tool.postgres"]);
}

/// A playbook with no `workflow:` must not panic, and a non-playbook kind must not pick
/// these attributes up.
#[test]
fn absent_workflow_and_other_kinds_yield_nothing_new() {
    let no_wf = "kind: Playbook\nmetadata:\n  path: p/empty\n";
    assert!(names(no_wf).is_empty());

    let sub = r#"
kind: Subscription
metadata:
  path: s/one
workflow:
  - step: a
    tool: { kind: postgres, auth: adiona_actor }
"#;
    let got = names(sub);
    println!("subscription extracted: {got:?}");
    assert!(
        !got.iter().any(|n| n.starts_with("uses_tool.")),
        "uses_tool.* is a playbook fact; a subscription carries spec.* instead, got {got:?}"
    );
}
