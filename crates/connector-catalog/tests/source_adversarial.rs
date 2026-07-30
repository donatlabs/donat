use std::path::Path;

use base64::Engine;
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
            if values.len() == 2 && values.contains_key("kind") && values.contains_key("value") {
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
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut accepted_documents = 0;
    for entry in std::fs::read_dir(fixture_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("yaml") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let Ok(document) = serde_yaml::from_str::<serde_json::Value>(&source) else {
            continue;
        };
        let bytes = serde_yaml::to_string(&document).unwrap().into_bytes();
        if load_record_bytes(&bytes).is_err() {
            continue;
        }
        accepted_documents += 1;

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
        for member_path in members {
            let (last, parent) = member_path.split_last().unwrap();
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
        for object_path in objects {
            let mut changed = document.clone();
            value_at_mut(&mut changed, &object_path)
                .as_object_mut()
                .unwrap()
                .insert("__unknown_member".to_owned(), serde_json::json!(true));
            assert_code(
                serde_yaml::to_string(&changed).unwrap().as_bytes(),
                "source_record_incomplete",
            );
        }
        for branch_path in branches {
            let mut changed = document.clone();
            value_at_mut(&mut changed, &branch_path)["kind"] = serde_json::json!("unknown_branch");
            assert_code(
                serde_yaml::to_string(&changed).unwrap().as_bytes(),
                "source_record_incomplete",
            );
        }
    }
    assert!(
        accepted_documents >= 3,
        "the mutation corpus must traverse every admitted source subject"
    );
}

#[test]
fn ordinary_kind_members_are_not_treated_as_tagged_unit_envelopes() {
    let bytes = String::from_utf8(mutate_once(
        "donat-owned-record.yaml",
        "safety_findings:\n  findings: []",
        "safety_findings:\n  findings:\n    - finding_id: finding.source.audit\n      kind: source.audit\n      location: null\n      message: reviewed",
    ))
    .unwrap()
    .replace(
        "kind: approved_for_port\n  value:\n    operations: [get]",
        "kind: inventory_only\n  value:\n    findings: [finding.source.audit]",
    );
    load_record_bytes(bytes.as_bytes()).unwrap();
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
    digest: '1111111111111111111111111111111111111111111111111111111111111111'
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
    license_file_sha256: '2222222222222222222222222222222222222222222222222222222222222222'
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
            "content_sha256: '1111111111111111111111111111111111111111111111111111111111111111'",
            "content_sha256: '3333333333333333333333333333333333333333333333333333333333333333'",
        ),
        "source_record_evidence_mismatch",
    );
}

#[test]
fn source_loader_preserves_nested_duplicate_and_evidence_error_oracles() {
    assert_code(
        &mutate_once(
            "provider-contract-record.yaml",
            "        content_sha256: '1111111111111111111111111111111111111111111111111111111111111111'",
            "        content_sha256: '1111111111111111111111111111111111111111111111111111111111111111'\n        content_sha256: '1111111111111111111111111111111111111111111111111111111111111111'",
        ),
        "source_record_duplicate",
    );
    let empty_artifacts = fixture("provider-contract-record.yaml").replacen(
        r#"artifact_hashes:
  - artifact_id: artifact.openapi
    algorithm:
      kind: sha256
      value: null
    digest: '1111111111111111111111111111111111111111111111111111111111111111'
    path: openapi.json
"#,
        "artifact_hashes: []\n",
        1,
    );
    assert_code(
        empty_artifacts.as_bytes(),
        "source_record_evidence_mismatch",
    );
}

#[test]
fn source_loader_applies_the_literal_multi_defect_precedence() {
    let unknown_and_primitive = fixture("donat-owned-record.yaml")
        .replacen("approval_date: 2026-07-29", "approval_date: 2026-02-30", 1)
        .replacen(
            "red_tests: [http_exact_source_compiles]",
            "red_tests: [http_exact_source_compiles]\nunknown_member: true",
            1,
        );
    assert_code(unknown_and_primitive.as_bytes(), "source_record_incomplete");

    let primitive_and_duplicate = fixture("donat-owned-record.yaml")
        .replacen(
            "record_version: 1",
            "record_version: 1\nrecord_version: 1",
            1,
        )
        .replacen("approval_date: 2026-07-29", "approval_date: 2026-02-30", 1);
    assert_code(
        primitive_and_duplicate.as_bytes(),
        "source_record_invalid_primitive",
    );

    let duplicate_and_legal = fixture("provider-contract-record.yaml")
        .replacen(
            "record_version: 1",
            "record_version: 1\nrecord_version: 1",
            1,
        )
        .replacen(
            r#"license:
  kind: permissive
  value:
    spdx_id: MIT
    selected_dual_license_branch: null
    license_file_path: LICENSE
    license_file_sha256: '2222222222222222222222222222222222222222222222222222222222222222'
"#,
            "license:\n  kind: rejected\n  value:\n    finding: finding.license.rejected\n",
            1,
        );
    assert_code(duplicate_and_legal.as_bytes(), "source_record_duplicate");
}

#[test]
fn exact_npm_name_tarball_and_artifact_inventory_are_one_identity() {
    let uppercase = fixture("serpapi-npm-record.yaml")
        .replacen("name: serpapi", "name: SerpAPI", 1)
        .replacen(
            "/serpapi/-/serpapi-0.1.10.tgz",
            "/SerpAPI/-/SerpAPI-0.1.10.tgz",
            1,
        );
    assert_code(uppercase.as_bytes(), "source_record_invalid_primitive");
    assert_code(
        &mutate_once(
            "serpapi-npm-record.yaml",
            "https://registry.npmjs.org/serpapi/-/serpapi-0.1.10.tgz",
            "https://evil.example/serpapi/-/serpapi-0.1.10.tgz",
        ),
        "source_record_npm_identity_mismatch",
    );
    let unrelated = fixture("serpapi-npm-record.yaml").replacen(
        "artifact_hashes:\n",
        "artifact_hashes:\n  - artifact_id: artifact.unrelated\n    algorithm:\n      kind: sha256\n      value: null\n    digest: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n    path: unrelated.txt\n",
        1,
    );
    assert_code(unrelated.as_bytes(), "source_record_npm_identity_mismatch");
}

#[test]
fn provider_artifact_inventory_is_an_exact_bidirectional_join() {
    let unrelated = fixture("provider-contract-record.yaml").replacen(
        "artifact_hashes:\n",
        "artifact_hashes:\n  - artifact_id: artifact.unrelated\n    algorithm:\n      kind: sha256\n      value: null\n    digest: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n    path: unrelated.json\n",
        1,
    );
    assert_code(unrelated.as_bytes(), "source_record_evidence_mismatch");

    let versioned = fixture("provider-contract-record.yaml")
        .replacen(
            r#"source:
          kind: repository_file
          value:
            repository: https://github.com/example/demo
            commit: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
            path: openapi.json
"#,
            r#"source:
          kind: versioned_artifact
          value:
            url: https://example.test/releases/v1/openapi.json
            provider_revision: v1
"#,
            1,
        )
        .replacen(
            "kind: provider_repository_review",
            "kind: provider_versioned_artifact_review",
            1,
        )
        .replacen("path: openapi.json", "path: foreign.json", 1);
    assert_code(versioned.as_bytes(), "source_record_evidence_mismatch");
}

#[test]
fn provider_fact_ownership_and_versioned_paths_are_exact() {
    let duplicate_contract_owner = fixture("provider-contract-record.yaml").replacen(
        "provider_contracts:\n",
        "provider_contracts:\n  - contract_id: contract.other\n    facts:\n      - kind: provider_evidence\n        value:\n          source_record_id: source.demo.provider.v1\n          fact_id: fact.idempotency\n",
        1,
    );
    let duplicate_contract_owner = duplicate_contract_owner.replacen(
        "contracts: [contract.demo]",
        "contracts: [contract.demo, contract.other]",
        1,
    );
    assert_code(
        duplicate_contract_owner.as_bytes(),
        "source_record_evidence_mismatch",
    );

    let versioned = fixture("provider-contract-record.yaml")
        .replacen(
            r#"source:
          kind: repository_file
          value:
            repository: https://github.com/example/demo
            commit: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
            path: openapi.json
"#,
            r#"source:
          kind: versioned_artifact
          value:
            url: https://example.test/releases/v1/openapi.json
            provider_revision: v1
"#,
            1,
        )
        .replacen(
            "kind: provider_repository_review",
            "kind: provider_versioned_artifact_review",
            1,
        )
        .replacen("path: openapi.json", "path: releases/v1/openapi.json", 2);
    load_record_bytes(versioned.as_bytes()).unwrap();

    let versioned_same_basename = versioned.replace(
        "path: releases/v1/openapi.json",
        "path: other/v1/openapi.json",
    );
    assert_code(
        versioned_same_basename.as_bytes(),
        "source_record_evidence_mismatch",
    );
}

#[test]
fn inline_bytes_from_source_records_obey_the_accepted_exact_bounds() {
    fn record_with_inline_bytes(
        decoded_len: usize,
        media_type: &str,
        file_name: Option<&str>,
    ) -> Vec<u8> {
        let binary =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(vec![0x5a; decoded_len]);
        let file_name = file_name.map_or_else(|| "null".to_owned(), |value| format!("'{value}'"));
        let replacement = format!(
            "normalized_value:\n              kind: inline_bytes\n              value:\n                $binary: {binary}\n                file_name: {file_name}\n                media_type: '{media_type}'"
        );
        mutate_once(
            "provider-contract-record.yaml",
            "normalized_value:\n              kind: string\n              value: Idempotency-Key",
            &replacement,
        )
    }

    load_record_bytes(&record_with_inline_bytes(
        131_072,
        &"m".repeat(255),
        Some(&"f".repeat(255)),
    ))
    .unwrap();
    for bytes in [
        record_with_inline_bytes(131_073, "application/octet-stream", None),
        record_with_inline_bytes(1, &"m".repeat(256), None),
        record_with_inline_bytes(1, "application/octet-stream", Some(&"f".repeat(256))),
    ] {
        assert_eq!(
            load_record_bytes(&bytes).unwrap_err().code(),
            "catalog_jcs_schema_mismatch"
        );
    }
}
