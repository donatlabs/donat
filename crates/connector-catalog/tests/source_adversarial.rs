use std::path::Path;

use donat_connector_catalog::{ExactSemver, load_record_bytes};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn mutate_once(name: &str, from: &str, to: &str) -> Vec<u8> {
    let source = fixture(name);
    assert_eq!(
        source.matches(from).count(),
        1,
        "mutation precondition for {name}: {from:?}"
    );
    source.replacen(from, to, 1).into_bytes()
}

fn assert_code(bytes: &[u8], expected: &'static str) {
    let error = load_record_bytes(bytes).expect_err("isolated mutation must fail closed");
    assert_eq!(error.code(), expected, "{error}");
}

fn collect_object_paths(
    value: &serde_json::Value,
    path: &mut Vec<(Option<String>, Option<usize>)>,
    objects: &mut Vec<Vec<(Option<String>, Option<usize>)>>,
    branches: &mut Vec<Vec<(Option<String>, Option<usize>)>>,
    members: &mut Vec<Vec<(Option<String>, Option<usize>)>>,
) {
    match value {
        serde_json::Value::Object(values) => {
            objects.push(path.clone());
            if values.contains_key("kind") {
                branches.push(path.clone());
            }
            for (name, value) in values {
                path.push((Some(name.clone()), None));
                members.push(path.clone());
                collect_object_paths(value, path, objects, branches, members);
                path.pop();
            }
        }
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                path.push((None, Some(index)));
                collect_object_paths(value, path, objects, branches, members);
                path.pop();
            }
        }
        _ => {}
    }
}

fn value_at_mut<'a>(
    mut value: &'a mut serde_json::Value,
    path: &[(Option<String>, Option<usize>)],
) -> &'a mut serde_json::Value {
    for (name, index) in path {
        value = if let Some(name) = name {
            &mut value.as_object_mut().unwrap()[name]
        } else {
            &mut value.as_array_mut().unwrap()[index.unwrap()]
        };
    }
    value
}

#[test]
fn generated_source_member_and_branch_mutations_reach_the_real_loader() {
    let document =
        serde_yaml::from_str::<serde_json::Value>(&fixture("serpapi-npm-record.yaml")).unwrap();
    let bytes = serde_yaml::to_string(&document).unwrap().into_bytes();
    load_record_bytes(&bytes).unwrap();

    let mut objects = Vec::new();
    let mut branches = Vec::new();
    let mut members = Vec::new();
    collect_object_paths(
        &document,
        &mut Vec::new(),
        &mut objects,
        &mut branches,
        &mut members,
    );
    for path in members {
        let (last, parent) = path.split_last().unwrap();
        let (Some(name), None) = last else {
            unreachable!("member paths terminate in an object key");
        };
        let mut changed = document.clone();
        value_at_mut(&mut changed, parent)
            .as_object_mut()
            .unwrap()
            .remove(name);
        assert_code(
            serde_yaml::to_string(&changed).unwrap().as_bytes(),
            "source_record_incomplete",
        );
    }
    for path in objects {
        let mut changed = document.clone();
        value_at_mut(&mut changed, &path)
            .as_object_mut()
            .unwrap()
            .insert("__unknown_member".to_owned(), serde_json::json!(true));
        assert_code(
            serde_yaml::to_string(&changed).unwrap().as_bytes(),
            "source_record_incomplete",
        );
    }
    for path in branches {
        let mut changed = document.clone();
        value_at_mut(&mut changed, &path)["kind"] = serde_json::json!("unknown_branch");
        assert_code(
            serde_yaml::to_string(&changed).unwrap().as_bytes(),
            "source_record_incomplete",
        );
    }
}

#[test]
fn ordinary_kind_members_are_not_treated_as_tagged_unit_envelopes() {
    let bytes = mutate_once(
        "donat-owned-record.yaml",
        "safety_findings:\n  findings: []",
        "safety_findings:\n  findings:\n    - finding_id: finding.source.audit\n      kind: source.audit\n      location: null\n      message: reviewed",
    );
    load_record_bytes(&bytes).unwrap();
}

#[test]
fn exact_semver_validates_unbounded_numeric_identifiers_lexically() {
    assert_eq!(
        ExactSemver::try_new("4294967296.0.0").unwrap().as_str(),
        "4294967296.0.0"
    );
}

#[test]
fn admission_collections_are_nonempty_and_exact() {
    assert_code(
        &mutate_once(
            "serpapi-npm-record.yaml",
            "findings:\n      - finding.awaiting.port",
            "findings: []",
        ),
        "source_record_admission_mismatch",
    );
    assert_code(
        &mutate_once(
            "provider-contract-record.yaml",
            "contracts: [contract.demo]",
            "contracts: []",
        ),
        "source_record_admission_mismatch",
    );
    assert_code(
        &mutate_once(
            "donat-owned-record.yaml",
            "operations: [get]",
            "operations: []",
        ),
        "source_record_admission_mismatch",
    );
}

#[test]
fn malformed_source_primitives_have_the_closed_error() {
    assert_code(
        &mutate_once(
            "donat-owned-record.yaml",
            "approval_date: 2026-07-29",
            "approval_date: 2026-02-30",
        ),
        "source_record_invalid_primitive",
    );
    assert_code(
        &mutate_once(
            "provider-contract-record.yaml",
            "repository: https://github.com/example/demo",
            "repository: http://github.com/example/demo",
        ),
        "source_record_invalid_primitive",
    );
    assert_code(
        &mutate_once(
            "provider-contract-record.yaml",
            "repository: https://github.com/example/demo",
            "repository: https://:443/demo",
        ),
        "source_record_invalid_primitive",
    );
    assert_code(
        &mutate_once(
            "donat-owned-record.yaml",
            "path: crates/server/src/connectors/http.rs",
            "path: ../crates/server/src/connectors/http.rs",
        ),
        "source_record_invalid_primitive",
    );
}

#[test]
fn duplicate_source_set_members_have_the_closed_error() {
    assert_code(
        &mutate_once(
            "serpapi-npm-record.yaml",
            "      - maintainer.two",
            "      - maintainer.one",
        ),
        "source_record_duplicate",
    );
    let source = fixture("provider-contract-record.yaml");
    let artifact = r#"  - artifact_id: artifact.openapi
    algorithm:
      kind: sha256
      value: null
    digest: 1111111111111111111111111111111111111111111111111111111111111111
    path: openapi.json
"#;
    assert_eq!(source.matches(artifact).count(), 1);
    assert_code(
        source
            .replacen(artifact, &format!("{artifact}{artifact}"), 1)
            .as_bytes(),
        "source_record_duplicate",
    );
    assert_code(
        &mutate_once(
            "donat-owned-record.yaml",
            "entrypoints: [crates/server/src/connectors/http.rs]",
            "entrypoints: [crates/server/src/connectors/http.rs, crates/server/src/connectors/http.rs]",
        ),
        "source_record_duplicate",
    );
    assert_code(
        &mutate_once(
            "donat-owned-record.yaml",
            "safety_findings:\n  findings: []",
            "safety_findings:\n  findings:\n    - finding_id: finding.source.audit\n      kind: source.audit\n      location: null\n      message: reviewed\n    - finding_id: finding.source.audit\n      kind: source.audit\n      location: null\n      message: reviewed",
        ),
        "source_record_duplicate",
    );
}

#[test]
fn rejected_legal_decisions_have_the_closed_error() {
    let permissive = r#"  kind: permissive
  value:
    spdx_id: MIT
    selected_dual_license_branch: null
    license_file_path: LICENSE
    license_file_sha256: 2222222222222222222222222222222222222222222222222222222222222222
"#;
    let rejected = "  kind: rejected\n  value:\n    finding: finding.license.rejected\n";
    assert_code(
        &mutate_once("provider-contract-record.yaml", permissive, rejected),
        "source_record_legal_mismatch",
    );
    assert_code(
        &mutate_once(
            "provider-contract-record.yaml",
            r#"        terms:
          kind: reviewed_use
          value:
            decision_id: review.demo
            evidence_url: https://example.test/terms/v1
"#,
            r#"        terms:
          kind: rejected
          value:
            finding: finding.terms.rejected
"#,
        ),
        "source_record_legal_mismatch",
    );
}

#[test]
fn npm_sri_and_identity_fail_with_distinct_closed_errors() {
    assert_code(
        &mutate_once(
            "serpapi-npm-record.yaml",
            "integrity: sha512-AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCkqKywtLi8wMTIzNDU2Nzg5Ojs8PT4/QA==",
            "integrity: sha512-not-base64",
        ),
        "source_record_npm_integrity_invalid",
    );
    assert_code(
        &mutate_once(
            "serpapi-npm-record.yaml",
            "npm_git_head: 0123456789abcdef0123456789abcdef01234567",
            "npm_git_head: 76543210fedcba9876543210fedcba9876543210",
        ),
        "source_record_npm_identity_mismatch",
    );
}

#[test]
fn evidence_reacquisition_and_artifact_joins_are_exact() {
    assert_code(
        &mutate_once(
            "provider-contract-record.yaml",
            "kind: provider_repository_review",
            "kind: provider_versioned_artifact_review",
        ),
        "source_record_evidence_mismatch",
    );
    assert_code(
        &mutate_once(
            "provider-contract-record.yaml",
            "content_sha256: 1111111111111111111111111111111111111111111111111111111111111111",
            "content_sha256: 3333333333333333333333333333333333333333333333333333333333333333",
        ),
        "source_record_evidence_mismatch",
    );
}
