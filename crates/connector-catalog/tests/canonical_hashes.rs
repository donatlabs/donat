use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use donat_connector_abi::{CompiledStepId, ConnectorId, OperationId};
use donat_connector_catalog::{
    AcceptedRecordCatalog, CANONICAL_PROJECTION_MUTATION_DESCRIPTORS,
    CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS, CANONICAL_PROJECTION_ROUTES,
    CANONICAL_PROJECTION_SCHEMA_DECLARATIONS, CANONICAL_PROVENANCE_DERIVED_DEPENDENCIES,
    CANONICAL_PROVENANCE_LOADER_BRANCH_CANDIDATES, CANONICAL_SEMANTIC_LOADER_BRANCH_CANDIDATES,
    CANONICAL_SOURCE_LOADER_BRANCH_CANDIDATES, CanonicalMutationCase,
    CanonicalProjectionAssignment, CanonicalProjectionDependencyEdge, CanonicalProjectionMount,
    CanonicalProjectionMountSegment, CanonicalProjectionProbeDisposition,
    CanonicalProjectionRoute, CanonicalProjectionRouteId, CanonicalProjectionStaticSegment,
    CanonicalPublicInputProbeId, DonatPolicyId, SourceReviewRegistry, TypedValueMaterialV1,
    ValueContractMaterialV1, canonical_material_bytes, canonical_projection_owner_manifest,
    canonicalize_raw, compile_connector_manifest, decode_source_record_material,
    decode_value_contract_material, load_connector_manifest_bytes, load_record, load_record_bytes,
    provenance_material, provenance_sha256, record_sha256, selected_response_header,
    semantic_material, semantic_sha256, source_record_material, typed_value_material,
    validate_canonical_owner_manifest, value_contract_material, value_contract_sha256,
};
use donat_value_contract::{
    BoundedInlineBytes, CanonicalDecimal, CanonicalNumber, TypeRef, TypedValue,
    ValueContractCatalog, ValueContractField, ValueScalar, ValueType,
};
use sha2::{Digest, Sha256};
use syn::{Attribute, Fields, GenericArgument, Item, LitStr, PathArguments, Type};

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn fixture_document(source: &str) -> serde_json::Value {
    serde_yaml::from_str(source).unwrap()
}

fn positive_source_projection_cases() -> Vec<(&'static str, serde_json::Value)> {
    let npm = fixture_document(include_str!("fixtures/serpapi-npm-record.yaml"));
    let provider = fixture_document(include_str!("fixtures/provider-contract-record.yaml"));
    let donat = fixture_document(include_str!("fixtures/donat-owned-record.yaml"));
    let permissive_license = serde_json::json!({
        "kind": "permissive",
        "value": {
            "spdx_id": "MIT",
            "selected_dual_license_branch": null,
            "license_file_path": "LICENSE",
            "license_file_sha256":
                "2222222222222222222222222222222222222222222222222222222222222222"
        }
    });
    let written_grant = serde_json::json!({
        "kind": "written_grant",
        "value": {
            "decision_id": "review.written.grant",
            "grant_sha256":
                "3333333333333333333333333333333333333333333333333333333333333333"
        }
    });

    let mut npm_collections = npm.clone();
    npm_collections["dependencies"] = serde_json::json!([
        {
            "dependency": "dependency.shipped",
            "disposition": {
                "kind": "shipped",
                "value": {"license": permissive_license.clone()}
            }
        },
        {
            "dependency": "dependency.build",
            "disposition": {
                "kind": "build_only",
                "value": {"license": written_grant.clone()}
            }
        },
        {
            "dependency": "dependency.type",
            "disposition": {
                "kind": "type_only_replaced",
                "value": {"replacement": "donat.value.contract"}
            }
        },
        {
            "dependency": "dependency.behavior",
            "disposition": {
                "kind": "behavior_only",
                "value": {"reason": "finding.behavior.only"}
            }
        },
        {
            "dependency": "dependency.rejected",
            "disposition": {
                "kind": "rejected",
                "value": {"finding": "finding.dependency.rejected"}
            }
        }
    ]);
    npm_collections["embedded_material"] = serde_json::json!([
        {
            "material_id": "embedded.shipped",
            "path": "embedded/shipped.json",
            "sha256":
                "4444444444444444444444444444444444444444444444444444444444444444",
            "disposition": {
                "kind": "shipped",
                "value": {"license": permissive_license.clone()}
            }
        },
        {
            "material_id": "embedded.behavior",
            "path": "embedded/behavior.json",
            "sha256":
                "5555555555555555555555555555555555555555555555555555555555555555",
            "disposition": {
                "kind": "behavior_only",
                "value": {"reason": "finding.embedded.behavior"}
            }
        },
        {
            "material_id": "embedded.rejected",
            "path": "embedded/rejected.json",
            "sha256":
                "6666666666666666666666666666666666666666666666666666666666666666",
            "disposition": {
                "kind": "rejected",
                "value": {"finding": "finding.embedded.rejected"}
            }
        }
    ]);
    npm_collections["provider_contracts"] = serde_json::json!([{
        "contract_id": "contract.policy.values",
        "facts": [
            {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.null",
                    "value": {"kind": "null", "value": null}
                }
            },
            {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.boolean",
                    "value": {"kind": "boolean", "value": true}
                }
            },
            {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.i64",
                    "value": {"kind": "i64", "value": "-1"}
                }
            },
            {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.u64",
                    "value": {"kind": "u64", "value": "1"}
                }
            },
            {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.decimal",
                    "value": {"kind": "decimal", "value": "1.5"}
                }
            },
            {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.list",
                    "value": {
                        "kind": "list",
                        "value": [{"kind": "string", "value": "list-value"}]
                    }
                }
            },
            {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.object",
                    "value": {
                        "kind": "object",
                        "value": {
                            "member": {"kind": "string", "value": "object-value"}
                        }
                    }
                }
            },
            {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.inline",
                    "value": {
                        "kind": "inline_bytes",
                        "value": {
                            "$binary": "Wg",
                            "file_name": null,
                            "media_type": "application/octet-stream"
                        }
                    }
                }
            }
        ]
    }]);
    npm_collections["compatibility"] = serde_json::json!({"kind": "tier_b", "value": null});

    let mut npm_absent_mismatch_written = npm.clone();
    npm_absent_mismatch_written["subject"]["value"]["signature"] = serde_json::json!({
        "kind": "verified_absent",
        "value": {
            "registry_metadata_sha256":
                "8192a3b4c5d6e7f8091a2b3c4d5e6f8091a2b3c4d5e6f708192a3b4c5d6e7f90"
        }
    });
    npm_absent_mismatch_written["subject"]["value"]["repository_owner"] = serde_json::json!({
        "kind": "reviewed_mismatch",
        "value": {"decision_id": "review.owner.mismatch"}
    });
    npm_absent_mismatch_written["license"] = written_grant;
    npm_absent_mismatch_written["compatibility"] =
        serde_json::json!({"kind": "tier_c", "value": null});

    let mut npm_rejected_verified = npm.clone();
    npm_rejected_verified["subject"]["value"]["signature"] = serde_json::json!({
        "kind": "rejected",
        "value": {"finding": "finding.signature.rejected"}
    });
    npm_rejected_verified["subject"]["value"]["provenance"] = serde_json::json!({
        "kind": "verified",
        "value": {
            "statement_sha256":
                "7777777777777777777777777777777777777777777777777777777777777777",
            "source_commit": "0123456789abcdef0123456789abcdef01234567"
        }
    });
    npm_rejected_verified["subject"]["value"]["provenance_commit"] =
        serde_json::json!("0123456789abcdef0123456789abcdef01234567");
    npm_rejected_verified["subject"]["value"]["repository_owner"] = serde_json::json!({
        "kind": "rejected",
        "value": {"finding": "finding.owner.rejected"}
    });
    npm_rejected_verified["compatibility"] = serde_json::json!({"kind": "rejected", "value": null});

    let mut npm_provenance_rejected = npm.clone();
    npm_provenance_rejected["subject"]["value"]["provenance"] = serde_json::json!({
        "kind": "rejected",
        "value": {"finding": "finding.provenance.rejected"}
    });

    let mut provider_permissive = provider.clone();
    provider_permissive["subject"]["value"]["evidence"][0]["terms"] = serde_json::json!({
        "kind": "permissive",
        "value": {
            "license": permissive_license,
            "evidence_url": "https://example.test/terms/permissive"
        }
    });

    let mut provider_versioned = provider.clone();
    provider_versioned["subject"]["value"]["evidence"][0]["source"] = serde_json::json!({
        "kind": "versioned_artifact",
        "value": {
            "url": "https://example.test/releases/v1/openapi.json",
            "provider_revision": "v1"
        }
    });
    provider_versioned["subject"]["value"]["evidence"][0]["facts"][0]["location"] = serde_json::json!({
        "kind": "document_section",
        "value": {
            "path": "releases/v1/openapi.json",
            "section": "Idempotency"
        }
    });
    provider_versioned["reacquisition"] = serde_json::json!({
        "kind": "provider_versioned_artifact_review",
        "value": null
    });
    provider_versioned["artifact_hashes"][0]["path"] =
        serde_json::json!("releases/v1/openapi.json");

    vec![
        ("npm-verified", npm),
        ("provider-repository-reviewed-use", provider),
        ("donat-owned", donat),
        ("npm-collections-and-typed-values", npm_collections),
        (
            "npm-absent-mismatch-written-grant",
            npm_absent_mismatch_written,
        ),
        (
            "npm-rejected-and-verified-provenance",
            npm_rejected_verified,
        ),
        ("npm-rejected-provenance", npm_provenance_rejected),
        ("provider-permissive-terms", provider_permissive),
        ("provider-versioned-document-section", provider_versioned),
    ]
}

struct SourcePublicProbeFamily {
    group: &'static str,
    baseline: serde_json::Value,
    changed: serde_json::Value,
}

fn source_public_probe_family_pairs() -> Vec<SourcePublicProbeFamily> {
    let mut cases = positive_source_projection_cases()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let npm = cases.remove("npm-verified").unwrap();
    let provider = cases
        .remove("provider-repository-reviewed-use")
        .unwrap();
    let donat = cases.remove("donat-owned").unwrap();

    let mut npm_fields = npm.clone();
    npm_fields["record_id"] = serde_json::json!("source.serpapi.npm.probe");
    npm_fields["subject"]["value"]["name"] = serde_json::json!("serpapi-probe");
    npm_fields["subject"]["value"]["version"] = serde_json::json!("0.1.11");
    npm_fields["subject"]["value"]["tarball_url"] =
        serde_json::json!("https://registry.npmjs.org/serpapi-probe/-/serpapi-probe-0.1.11.tgz");
    npm_fields["subject"]["value"]["integrity"] = serde_json::json!(
        "sha512-AgIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCkqKywtLi8wMTIzNDU2Nzg5Ojs8PT4/QA=="
    );
    npm_fields["artifact_hashes"][0]["digest"] = serde_json::json!(
        "0202030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40"
    );
    npm_fields["artifact_hashes"][0]["path"] = serde_json::json!("serpapi-probe-0.1.11.tgz");
    npm_fields["subject"]["value"]["repository"]["url"] =
        serde_json::json!("https://github.com/serpapi/serpapi-probe");
    npm_fields["subject"]["value"]["repository"]["commit"] =
        serde_json::json!("1123456789abcdef0123456789abcdef01234567");
    npm_fields["subject"]["value"]["repository"]["tree"] =
        serde_json::json!("99abcdef0123456789abcdef0123456789abcdef");
    npm_fields["subject"]["value"]["npm_git_head"] =
        npm_fields["subject"]["value"]["repository"]["commit"].clone();
    npm_fields["subject"]["value"]["package_repository"] =
        npm_fields["subject"]["value"]["repository"]["url"].clone();
    npm_fields["subject"]["value"]["tag_commit"] =
        serde_json::json!("eedcba9876543210fedcba9876543210fedcba98");
    npm_fields["subject"]["value"]["signature"]["value"]["signatures"][0]
        ["signature_sha256"] = serde_json::json!(
        "515263748596a7b8c9daebfc0d1e2f405162738495a6b7c8d9eafb0c1d2e3f50"
    );
    npm_fields["subject"]["value"]["signature"]["value"]["signatures"][0]["key_id"] =
        serde_json::json!("npm.key.probe");
    npm_fields["subject"]["value"]["signature"]["value"]["registry_metadata_sha256"] =
        serde_json::json!(
            "9192a3b4c5d6e7f8091a2b3c4d5e6f8091a2b3c4d5e6f708192a3b4c5d6e7f90"
        );
    npm_fields["subject"]["value"]["provenance"]["value"]["registry_metadata_sha256"] =
        npm_fields["subject"]["value"]["signature"]["value"]["registry_metadata_sha256"].clone();
    npm_fields["subject"]["value"]["maintainers"] =
        serde_json::json!(["maintainer.one", "maintainer.three"]);
    npm_fields["subject"]["value"]["repository_owner"]["value"]["package_owner"] =
        serde_json::json!("owner.serpapi.next");
    npm_fields["subject"]["value"]["repository_owner"]["value"]["repository_owner"] =
        serde_json::json!("owner.serpapi.next");
    npm_fields["license"]["value"]["spdx_id"] = serde_json::json!("Apache-2.0");
    npm_fields["license"]["value"]["license_file_path"] = serde_json::json!("LICENSE-APACHE");
    npm_fields["license"]["value"]["license_file_sha256"] = serde_json::json!(
        "d1d2e3f405162738495a6b7c8d9eafc0d1e2f30415263748596a7b8c9daebfd0"
    );
    npm_fields["notice"]["license_file_path"] =
        npm_fields["license"]["value"]["license_file_path"].clone();
    npm_fields["notice"]["license_file_sha256"] =
        npm_fields["license"]["value"]["license_file_sha256"].clone();
    npm_fields["notice"]["id"] = serde_json::json!("notice.serpapi.probe");
    npm_fields["notice"]["required_copyright_lines"] =
        serde_json::json!(["Copyright SerpAPI", "Copyright Probe"]);
    npm_fields["notice"]["notice_bundle_destination"] =
        serde_json::json!("THIRD_PARTY_NOTICES.probe.md");
    npm_fields["entrypoints"] = serde_json::json!(["probe.js"]);
    npm_fields["dependencies"][0]["disposition"]["value"]["replacement"] =
        serde_json::json!("donat.value.contract.probe");
    npm_fields["dependencies"][0]["dependency"] =
        serde_json::json!("n8n-workflow-probe");
    npm_fields["admission"]["value"]["findings"] = serde_json::json!(["finding.probe"]);
    npm_fields["safety_findings"]["findings"][0]["finding_id"] =
        serde_json::json!("finding.probe");
    npm_fields["safety_findings"]["findings"][0]["kind"] = serde_json::json!("port.probe");
    npm_fields["safety_findings"]["findings"][0]["location"] =
        serde_json::json!("crates/probe.rs");
    npm_fields["safety_findings"]["findings"][0]["message"] =
        serde_json::json!("Probe finding.");
    npm_fields["reviewer"] = serde_json::json!("reviewer.probe");
    npm_fields["approval_date"] = serde_json::json!("2026-07-30");
    npm_fields["proposed_manifest"] =
        serde_json::json!("connector-catalog/manifests/serpapi-probe.yaml");
    npm_fields["proposed_destinations"] =
        serde_json::json!(["connector-catalog/sources/records/serpapi-probe.yaml"]);
    npm_fields["red_tests"] = serde_json::json!(["serpapi_probe_red"]);

    let mut provider_fields = provider.clone();
    provider_fields["record_id"] = serde_json::json!("source.demo.provider.probe");
    provider_fields["provider_contracts"][0]["facts"][0]["value"]["source_record_id"] =
        provider_fields["record_id"].clone();
    provider_fields["subject"]["value"]["provider"] = serde_json::json!("demo-probe");
    provider_fields["subject"]["value"]["evidence"][0]["source"]["value"]["repository"] =
        serde_json::json!("https://github.com/example/demo-probe");
    provider_fields["subject"]["value"]["evidence"][0]["source"]["value"]["commit"] =
        serde_json::json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    provider_fields["subject"]["value"]["evidence"][0]["source"]["value"]["path"] =
        serde_json::json!("spec/openapi-probe.json");
    provider_fields["subject"]["value"]["evidence"][0]["content_sha256"] =
        serde_json::json!(
            "3333333333333333333333333333333333333333333333333333333333333333"
        );
    provider_fields["subject"]["value"]["evidence"][0]["accessed_on"] =
        serde_json::json!("2026-07-30");
    provider_fields["subject"]["value"]["evidence"][0]["terms"]["value"]["decision_id"] =
        serde_json::json!("review.demo.probe");
    provider_fields["subject"]["value"]["evidence"][0]["terms"]["value"]["evidence_url"] =
        serde_json::json!("https://example.test/terms/probe");
    provider_fields["subject"]["value"]["evidence"][0]["facts"][0]["normalized_value"] =
        serde_json::json!({"kind": "string", "value": "Idempotency-Key-Probe"});
    provider_fields["subject"]["value"]["evidence"][0]["facts"][0]["fact_id"] =
        serde_json::json!("fact.idempotency.probe");
    provider_fields["subject"]["value"]["evidence"][0]["facts"][0]["location"]["value"]["path"] =
        serde_json::json!("spec/openapi-probe.json");
    provider_fields["subject"]["value"]["evidence"][0]["facts"][0]["location"]["value"]["pointer"] =
        serde_json::json!("/paths/~1widgets-probe/post");
    provider_fields["artifact_hashes"][0]["artifact_id"] =
        serde_json::json!("artifact.openapi.probe");
    provider_fields["artifact_hashes"][0]["digest"] = serde_json::json!(
        "3333333333333333333333333333333333333333333333333333333333333333"
    );
    provider_fields["artifact_hashes"][0]["path"] =
        serde_json::json!("spec/openapi-probe.json");
    provider_fields["entrypoints"][0] = serde_json::json!("spec/openapi-probe.json");
    provider_fields["provider_contracts"][0]["contract_id"] =
        serde_json::json!("contract.demo.probe");
    provider_fields["provider_contracts"][0]["facts"][0]["value"]["fact_id"] =
        provider_fields["subject"]["value"]["evidence"][0]["facts"][0]["fact_id"].clone();
    provider_fields["admission"]["value"]["contracts"][0] =
        provider_fields["provider_contracts"][0]["contract_id"].clone();
    provider_fields["license"]["value"]["spdx_id"] = serde_json::json!("Apache-2.0");
    provider_fields["license"]["value"]["license_file_path"] =
        serde_json::json!("LICENSE-APACHE");
    provider_fields["license"]["value"]["license_file_sha256"] = serde_json::json!(
        "3222222222222222222222222222222222222222222222222222222222222222"
    );
    provider_fields["notice"]["license_file_path"] =
        provider_fields["license"]["value"]["license_file_path"].clone();
    provider_fields["notice"]["license_file_sha256"] =
        provider_fields["license"]["value"]["license_file_sha256"].clone();
    provider_fields["notice"]["id"] = serde_json::json!("notice.demo.probe");
    provider_fields["notice"]["required_copyright_lines"] =
        serde_json::json!(["Copyright Demo", "Copyright Probe"]);
    provider_fields["notice"]["notice_bundle_destination"] =
        serde_json::json!("THIRD_PARTY_NOTICES.probe.md");
    provider_fields["reviewer"] = serde_json::json!("reviewer.provider.probe");
    provider_fields["approval_date"] = serde_json::json!("2026-07-30");
    provider_fields["proposed_manifest"] =
        serde_json::json!("connector-catalog/manifests/provider-probe.yaml");
    provider_fields["proposed_destinations"] =
        serde_json::json!(["connector-catalog/sources/records/provider-probe.yaml"]);
    provider_fields["red_tests"] = serde_json::json!(["provider_probe_red"]);

    let mut donat_fields = donat.clone();
    donat_fields["record_version"] = serde_json::json!(2);
    donat_fields["record_id"] = serde_json::json!("source.donat.http.probe.v1");
    donat_fields["subject"]["value"]["repository_commit"] =
        serde_json::json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    donat_fields["subject"]["value"]["files"][0]["path"] =
        serde_json::json!("crates/server/src/connectors/http_probe.rs");
    donat_fields["subject"]["value"]["files"][0]["sha256"] = serde_json::json!(
        "3111111111111111111111111111111111111111111111111111111111111111"
    );
    donat_fields["entrypoints"][0] =
        serde_json::json!("crates/server/src/connectors/http_probe.rs");
    donat_fields["license"]["value"]["spdx_id"] = serde_json::json!("MIT");
    donat_fields["license"]["value"]["license_file_path"] = serde_json::json!("LICENSE-MIT");
    donat_fields["license"]["value"]["license_file_sha256"] = serde_json::json!(
        "4222222222222222222222222222222222222222222222222222222222222222"
    );
    donat_fields["notice"]["license_file_path"] =
        donat_fields["license"]["value"]["license_file_path"].clone();
    donat_fields["notice"]["license_file_sha256"] =
        donat_fields["license"]["value"]["license_file_sha256"].clone();
    donat_fields["notice"]["id"] = serde_json::json!("notice.donat.probe");
    donat_fields["notice"]["required_copyright_lines"] =
        serde_json::json!(["Copyright Donat"]);
    donat_fields["notice"]["notice_bundle_destination"] =
        serde_json::json!("THIRD_PARTY_NOTICES.probe.md");
    donat_fields["admission"]["value"]["operations"] = serde_json::json!(["post"]);
    donat_fields["reviewer"] = serde_json::json!("reviewer.donat.probe");
    donat_fields["approval_date"] = serde_json::json!("2026-07-30");
    donat_fields["proposed_manifest"] =
        serde_json::json!("connector-catalog/manifests/http-probe.yaml");
    donat_fields["proposed_destinations"] =
        serde_json::json!(["connector-catalog/sources/records/http-probe.yaml"]);
    donat_fields["red_tests"] = serde_json::json!(["http_probe_red"]);

    vec![
        SourcePublicProbeFamily {
            group: "npm-verified",
            baseline: npm.clone(),
            changed: npm_fields,
        },
        SourcePublicProbeFamily {
            group: "provider-repository-reviewed-use",
            baseline: provider.clone(),
            changed: provider_fields,
        },
        SourcePublicProbeFamily {
            group: "donat-owned",
            baseline: donat,
            changed: donat_fields,
        },
        SourcePublicProbeFamily {
            group: "npm-collections-and-typed-values",
            baseline: npm.clone(),
            changed: cases
                .remove("npm-collections-and-typed-values")
                .unwrap(),
        },
        SourcePublicProbeFamily {
            group: "npm-absent-mismatch-written-grant",
            baseline: npm.clone(),
            changed: cases
                .remove("npm-absent-mismatch-written-grant")
                .unwrap(),
        },
        SourcePublicProbeFamily {
            group: "npm-rejected-and-verified-provenance",
            baseline: npm.clone(),
            changed: cases
                .remove("npm-rejected-and-verified-provenance")
                .unwrap(),
        },
        SourcePublicProbeFamily {
            group: "npm-rejected-provenance",
            baseline: npm,
            changed: cases.remove("npm-rejected-provenance").unwrap(),
        },
        SourcePublicProbeFamily {
            group: "provider-permissive-terms",
            baseline: provider.clone(),
            changed: cases.remove("provider-permissive-terms").unwrap(),
        },
        SourcePublicProbeFamily {
            group: "provider-versioned-document-section",
            baseline: provider,
            changed: cases
                .remove("provider-versioned-document-section")
                .unwrap(),
        },
    ]
}

#[test]
fn source_public_probe_family_pairs_pass_the_real_loader() {
    for family in source_public_probe_family_pairs() {
        for (side, document) in [
            ("baseline", &family.baseline),
            ("changed", &family.changed),
        ] {
            let yaml = serde_yaml::to_string(document).unwrap();
            load_record_bytes(yaml.as_bytes()).unwrap_or_else(|error| {
                panic!(
                    "Source public probe family {} {side} failed the real loader: {error}",
                    family.group
                )
            });
        }
    }
}

#[derive(Clone)]
struct MaterialField {
    rust_name: Option<String>,
    wire_name: Option<String>,
    ty: Type,
}

#[derive(Clone)]
struct MaterialVariant {
    rust_name: String,
    wire_name: String,
    fields: Vec<MaterialField>,
}

#[derive(Clone)]
enum MaterialShape {
    Struct(Vec<MaterialField>),
    Enum(Vec<MaterialVariant>),
}

fn serde_rename(attributes: &[Attribute]) -> Option<String> {
    let mut rename = None;
    for attribute in attributes {
        if !attribute.path().is_ident("serde") {
            continue;
        }
        let _ = attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                rename = Some(meta.value()?.parse::<LitStr>()?.value());
            }
            Ok(())
        });
    }
    rename
}

fn snake_case(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut output = String::new();
    for (index, character) in characters.iter().copied().enumerate() {
        let previous = index.checked_sub(1).and_then(|value| characters.get(value));
        let next = characters.get(index + 1);
        if character.is_ascii_uppercase()
            && index != 0
            && (previous.is_some_and(|value| value.is_ascii_lowercase() || value.is_ascii_digit())
                || next.is_some_and(|value| value.is_ascii_lowercase()))
        {
            output.push('_');
        }
        output.push(character.to_ascii_lowercase());
    }
    output
}

fn material_field(rust_name: Option<String>, attributes: &[Attribute], ty: Type) -> MaterialField {
    MaterialField {
        wire_name: serde_rename(attributes).or_else(|| rust_name.clone()),
        rust_name,
        ty,
    }
}

fn material_branch_schema() -> BTreeMap<String, MaterialShape> {
    let file = syn::parse_file(CANONICAL_PROJECTION_SCHEMA_DECLARATIONS).unwrap();
    file.items
        .into_iter()
        .filter_map(|item| match item {
            Item::Struct(value) => {
                let fields = match value.fields {
                    Fields::Named(fields) => fields
                        .named
                        .into_iter()
                        .map(|field| {
                            material_field(
                                field.ident.map(|name| name.to_string()),
                                &field.attrs,
                                field.ty,
                            )
                        })
                        .collect(),
                    Fields::Unnamed(fields) => fields
                        .unnamed
                        .into_iter()
                        .map(|field| material_field(None, &field.attrs, field.ty))
                        .collect(),
                    Fields::Unit => Vec::new(),
                };
                Some((value.ident.to_string(), MaterialShape::Struct(fields)))
            }
            Item::Enum(value) => {
                let variants = value
                    .variants
                    .into_iter()
                    .map(|variant| {
                        let rust_name = variant.ident.to_string();
                        let fields = match variant.fields {
                            Fields::Named(fields) => fields
                                .named
                                .into_iter()
                                .map(|field| {
                                    material_field(
                                        field.ident.map(|name| name.to_string()),
                                        &field.attrs,
                                        field.ty,
                                    )
                                })
                                .collect(),
                            Fields::Unnamed(fields) => fields
                                .unnamed
                                .into_iter()
                                .map(|field| material_field(None, &field.attrs, field.ty))
                                .collect(),
                            Fields::Unit => Vec::new(),
                        };
                        MaterialVariant {
                            wire_name: serde_rename(&variant.attrs)
                                .unwrap_or_else(|| snake_case(&rust_name)),
                            rust_name,
                            fields,
                        }
                    })
                    .collect();
                Some((value.ident.to_string(), MaterialShape::Enum(variants)))
            }
            _ => None,
        })
        .collect()
}

fn generic_types(arguments: &PathArguments) -> Vec<&Type> {
    let PathArguments::AngleBracketed(arguments) = arguments else {
        return Vec::new();
    };
    arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(value) => Some(value),
            _ => None,
        })
        .collect()
}

fn collect_material_branches_for_type(
    schema: &BTreeMap<String, MaterialShape>,
    ty: &Type,
    value: &serde_json::Value,
    output: &mut BTreeSet<String>,
) {
    let Type::Path(ty) = ty else {
        return;
    };
    let Some(segment) = ty.path.segments.last() else {
        return;
    };
    let arguments = generic_types(&segment.arguments);
    match segment.ident.to_string().as_str() {
        "Vec" | "BTreeSet" => {
            if let Some(member_type) = arguments.last()
                && let Some(values) = value.as_array()
            {
                for member in values {
                    collect_material_branches_for_type(schema, member_type, member, output);
                }
            }
        }
        "BTreeMap" => {
            if let Some(member_type) = arguments.last()
                && let Some(values) = value.as_object()
            {
                for member in values.values() {
                    collect_material_branches_for_type(schema, member_type, member, output);
                }
            }
        }
        "Option" => {
            if !value.is_null()
                && let Some(member_type) = arguments.first()
            {
                collect_material_branches_for_type(schema, member_type, value, output);
            }
        }
        "Box" => {
            if let Some(member_type) = arguments.first() {
                collect_material_branches_for_type(schema, member_type, value, output);
            }
        }
        name => collect_material_branches_for_definition(schema, name, value, output),
    }
}

fn collect_material_branches_for_definition(
    schema: &BTreeMap<String, MaterialShape>,
    name: &str,
    value: &serde_json::Value,
    output: &mut BTreeSet<String>,
) {
    let Some(shape) = schema.get(name) else {
        return;
    };
    match shape {
        MaterialShape::Struct(fields) => {
            if fields.len() == 1 && fields[0].rust_name.is_none() {
                collect_material_branches_for_type(schema, &fields[0].ty, value, output);
                return;
            }
            let object = value
                .as_object()
                .unwrap_or_else(|| panic!("{name} material is not an object"));
            for field in fields {
                let Some(wire_name) = &field.wire_name else {
                    continue;
                };
                let member = object
                    .get(wire_name)
                    .unwrap_or_else(|| panic!("{name}.{wire_name} is absent"));
                collect_material_branches_for_type(schema, &field.ty, member, output);
            }
        }
        MaterialShape::Enum(variants) => {
            let object = value
                .as_object()
                .unwrap_or_else(|| panic!("{name} material branch is not an object"));
            let kind = object
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("{name} material branch has no kind"));
            let variant = variants
                .iter()
                .find(|variant| variant.wire_name == kind)
                .unwrap_or_else(|| panic!("{name} has no generated {kind} branch"));
            output.insert(format!("{name}::{}", variant.rust_name));
            let Some(payload) = object.get("value") else {
                assert!(variant.fields.is_empty());
                return;
            };
            if variant.fields.len() == 1 && variant.fields[0].rust_name.is_none() {
                collect_material_branches_for_type(schema, &variant.fields[0].ty, payload, output);
                return;
            }
            let payload = payload
                .as_object()
                .unwrap_or_else(|| panic!("{name}::{kind} payload is not an object"));
            for field in &variant.fields {
                let wire_name = field
                    .wire_name
                    .as_deref()
                    .expect("named branch field has a wire name");
                collect_material_branches_for_type(
                    schema,
                    &field.ty,
                    payload
                        .get(wire_name)
                        .unwrap_or_else(|| panic!("{name}::{kind}.{wire_name} is absent")),
                    output,
                );
            }
        }
    }
}

fn material_path(parent: &str, member: &str) -> String {
    format!("{parent}.{member}")
}

fn collect_generated_epoch_paths_for_type(
    schema: &BTreeMap<String, MaterialShape>,
    ty: &Type,
    value: &serde_json::Value,
    path: &str,
    dependency_members: &BTreeSet<String>,
    output: &mut BTreeSet<String>,
) {
    let Type::Path(ty) = ty else {
        return;
    };
    let Some(segment) = ty.path.segments.last() else {
        return;
    };
    let arguments = generic_types(&segment.arguments);
    match segment.ident.to_string().as_str() {
        "Vec" | "BTreeSet" => {
            if let Some(member_type) = arguments.last()
                && let Some(values) = value.as_array()
            {
                for (index, member) in values.iter().enumerate() {
                    collect_generated_epoch_paths_for_type(
                        schema,
                        member_type,
                        member,
                        &format!("{path}[{index}]"),
                        dependency_members,
                        output,
                    );
                }
            }
        }
        "BTreeMap" => {
            if let Some(member_type) = arguments.last()
                && let Some(values) = value.as_object()
            {
                for (key, member) in values {
                    collect_generated_epoch_paths_for_type(
                        schema,
                        member_type,
                        member,
                        &material_path(path, key),
                        dependency_members,
                        output,
                    );
                }
            }
        }
        "Option" => {
            if !value.is_null()
                && let Some(member_type) = arguments.first()
            {
                collect_generated_epoch_paths_for_type(
                    schema,
                    member_type,
                    value,
                    path,
                    dependency_members,
                    output,
                );
            }
        }
        "Box" => {
            if let Some(member_type) = arguments.first() {
                collect_generated_epoch_paths_for_type(
                    schema,
                    member_type,
                    value,
                    path,
                    dependency_members,
                    output,
                );
            }
        }
        name => collect_generated_epoch_paths_for_definition(
            schema,
            name,
            value,
            path,
            dependency_members,
            output,
        ),
    }
}

fn collect_generated_epoch_paths_for_fields(
    schema: &BTreeMap<String, MaterialShape>,
    owner: &str,
    fields: &[MaterialField],
    value: &serde_json::Map<String, serde_json::Value>,
    path: &str,
    dependency_members: &BTreeSet<String>,
    output: &mut BTreeSet<String>,
) {
    for field in fields {
        let Some(wire_name) = &field.wire_name else {
            continue;
        };
        let member = value
            .get(wire_name)
            .unwrap_or_else(|| panic!("{path}.{wire_name} is absent"));
        let member_path = material_path(path, wire_name);
        if dependency_members.contains(&format!("{owner}.{wire_name}")) {
            output.insert(member_path.clone());
        }
        collect_generated_epoch_paths_for_type(
            schema,
            &field.ty,
            member,
            &member_path,
            dependency_members,
            output,
        );
    }
}

fn collect_generated_epoch_paths_for_definition(
    schema: &BTreeMap<String, MaterialShape>,
    name: &str,
    value: &serde_json::Value,
    path: &str,
    dependency_members: &BTreeSet<String>,
    output: &mut BTreeSet<String>,
) {
    let Some(shape) = schema.get(name) else {
        return;
    };
    match shape {
        MaterialShape::Struct(fields) => {
            if fields.len() == 1 && fields[0].rust_name.is_none() {
                collect_generated_epoch_paths_for_type(
                    schema,
                    &fields[0].ty,
                    value,
                    path,
                    dependency_members,
                    output,
                );
                return;
            }
            let object = value
                .as_object()
                .unwrap_or_else(|| panic!("{name} material is not an object"));
            collect_generated_epoch_paths_for_fields(
                schema,
                name,
                fields,
                object,
                path,
                dependency_members,
                output,
            );
        }
        MaterialShape::Enum(variants) => {
            let object = value
                .as_object()
                .unwrap_or_else(|| panic!("{name} material branch is not an object"));
            let kind = object
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("{name} material branch has no kind"));
            let variant = variants
                .iter()
                .find(|variant| variant.wire_name == kind)
                .unwrap_or_else(|| panic!("{name} has no generated {kind} branch"));
            let Some(payload) = object.get("value") else {
                assert!(variant.fields.is_empty());
                return;
            };
            let payload_path = material_path(path, "value");
            if variant.fields.len() == 1 && variant.fields[0].rust_name.is_none() {
                collect_generated_epoch_paths_for_type(
                    schema,
                    &variant.fields[0].ty,
                    payload,
                    &payload_path,
                    dependency_members,
                    output,
                );
                return;
            }
            let payload = payload
                .as_object()
                .unwrap_or_else(|| panic!("{name}::{kind} payload is not an object"));
            collect_generated_epoch_paths_for_fields(
                schema,
                &format!("{name}::{}", variant.rust_name),
                &variant.fields,
                payload,
                &payload_path,
                dependency_members,
                output,
            );
        }
    }
}

fn generated_epoch_dependency_members() -> BTreeSet<String> {
    let mut members = CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS
        .iter()
        .filter(|descriptor| {
            descriptor
                .normalized_owner
                .ends_with(".value_language_epoch")
        })
        .map(|descriptor| descriptor.material_member.to_owned())
        .collect::<BTreeSet<_>>();

    for contract in CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS
        .iter()
        .filter(|descriptor| {
            descriptor.domain == "semantic" && descriptor.branch_type == "ValueContractMaterialV1"
        })
    {
        let hash_normalized_owner = format!("{}_contract_sha256", contract.normalized_owner);
        let hash_normalized_member = format!("{}_contract_sha256", contract.normalized_member);
        if let Some(hash) = CANONICAL_PROJECTION_OWNER_PATH_DESCRIPTORS
            .iter()
            .find(|descriptor| {
                descriptor.domain == contract.domain
                    && descriptor.normalized_owner == hash_normalized_owner
                    && descriptor.normalized_member == hash_normalized_member
                    && descriptor.branch_type == "Hash256"
            })
        {
            members.insert(hash.material_member.to_owned());
        }
    }
    members
}

fn collect_changed_json_paths(
    baseline: &serde_json::Value,
    changed: &serde_json::Value,
    path: &str,
    output: &mut BTreeSet<String>,
) {
    match (baseline, changed) {
        (serde_json::Value::Array(left), serde_json::Value::Array(right))
            if left.len() == right.len() =>
        {
            for (index, (left, right)) in left.iter().zip(right).enumerate() {
                collect_changed_json_paths(left, right, &format!("{path}[{index}]"), output);
            }
        }
        (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
            for key in left.keys().chain(right.keys()).collect::<BTreeSet<_>>() {
                let member_path = material_path(path, key);
                match (left.get(key), right.get(key)) {
                    (Some(left), Some(right)) => {
                        collect_changed_json_paths(left, right, &member_path, output);
                    }
                    (Some(_), None) | (None, Some(_)) => {
                        output.insert(member_path);
                    }
                    (None, None) => unreachable!("the key came from one of the objects"),
                }
            }
        }
        _ if baseline != changed => {
            output.insert(path.to_owned());
        }
        _ => {}
    }
}

#[test]
fn json_leaf_diff_unions_object_keys_and_stops_at_type_mismatches() {
    let baseline = serde_json::json!({
        "removed": true,
        "nested": {
            "changed": 1,
            "type": [],
        },
    });
    let changed = serde_json::json!({
        "added": true,
        "nested": {
            "changed": 2,
            "type": {},
        },
    });
    let mut paths = BTreeSet::new();
    collect_changed_json_paths(&baseline, &changed, "$", &mut paths);
    assert_eq!(
        paths,
        [
            "$.added".to_owned(),
            "$.nested.changed".to_owned(),
            "$.nested.type".to_owned(),
            "$.removed".to_owned(),
        ]
        .into_iter()
        .collect()
    );
}

#[derive(Clone)]
enum PublicProjectionProbeInput {
    ValueContract {
        catalog: ValueContractCatalog,
        value_language_epoch: u32,
    },
    TypedValue(TypedValue),
}

struct PublicProjectionProbeRecipe {
    probe: CanonicalPublicInputProbeId,
    baseline: PublicProjectionProbeInput,
    changed: PublicProjectionProbeInput,
}

struct PublicProjectionProbeObservation {
    canonical_bytes: Vec<u8>,
    digest: [u8; 32],
    canonical_value: serde_json::Value,
}

const VALUE_CONTRACT_EPOCH_PROBE: CanonicalPublicInputProbeId = CanonicalPublicInputProbeId::new(
    CanonicalMutationCase::ValueContract,
    "ValueContractMaterialV1",
    "ValueContractEpoch",
);

macro_rules! typed_value_probe_id {
    ($group:ident) => {
        CanonicalPublicInputProbeId::new(
            CanonicalMutationCase::TypedValue,
            "TypedValue",
            stringify!($group),
        )
    };
}

fn value_contract_epoch_probe_recipe() -> PublicProjectionProbeRecipe {
    let catalog = ValueContractCatalog {
        roots: [(
            "query".to_owned(),
            ValueContractField {
                required: true,
                type_ref: TypeRef {
                    nullable: false,
                    value_type: ValueType::Scalar {
                        scalar: ValueScalar::String,
                    },
                },
            },
        )]
        .into_iter()
        .collect(),
        named_objects: BTreeMap::new(),
    };
    PublicProjectionProbeRecipe {
        probe: VALUE_CONTRACT_EPOCH_PROBE,
        baseline: PublicProjectionProbeInput::ValueContract {
            catalog: catalog.clone(),
            value_language_epoch: 1,
        },
        changed: PublicProjectionProbeInput::ValueContract {
            catalog,
            value_language_epoch: 2,
        },
    }
}

fn inline_typed_value(bytes: Vec<u8>, media_type: &str, file_name: Option<&str>) -> TypedValue {
    let maximum_decoded_bytes = bytes.len();
    TypedValue::InlineBytes(
        BoundedInlineBytes::try_new(bytes, media_type, file_name, maximum_decoded_bytes).unwrap(),
    )
}

fn typed_value_probe_recipe(
    probe: CanonicalPublicInputProbeId,
    baseline: TypedValue,
    changed: TypedValue,
) -> PublicProjectionProbeRecipe {
    PublicProjectionProbeRecipe {
        probe,
        baseline: PublicProjectionProbeInput::TypedValue(baseline),
        changed: PublicProjectionProbeInput::TypedValue(changed),
    }
}

fn typed_value_probe_recipes() -> Vec<PublicProjectionProbeRecipe> {
    vec![
        typed_value_probe_recipe(
            typed_value_probe_id!(NullKind),
            TypedValue::Boolean(false),
            TypedValue::Null,
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(BooleanKind),
            TypedValue::String("branch".to_owned()),
            TypedValue::Boolean(true),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(BooleanValue),
            TypedValue::Boolean(false),
            TypedValue::Boolean(true),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(StringKind),
            TypedValue::Boolean(false),
            TypedValue::String("branch".to_owned()),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(StringValue),
            TypedValue::String("baseline".to_owned()),
            TypedValue::String("changed".to_owned()),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(I64Kind),
            TypedValue::Number(CanonicalNumber::U64(1)),
            TypedValue::Number(CanonicalNumber::I64(-1)),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(I64Value),
            TypedValue::Number(CanonicalNumber::I64(-1)),
            TypedValue::Number(CanonicalNumber::I64(-2)),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(U64Kind),
            TypedValue::Number(CanonicalNumber::I64(1)),
            TypedValue::Number(CanonicalNumber::U64(2)),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(U64Value),
            TypedValue::Number(CanonicalNumber::U64(1)),
            TypedValue::Number(CanonicalNumber::U64(2)),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(DecimalKind),
            TypedValue::Number(CanonicalNumber::I64(1)),
            TypedValue::Number(CanonicalNumber::Decimal(
                CanonicalDecimal::try_new("1.5").unwrap(),
            )),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(DecimalValue),
            TypedValue::Number(CanonicalNumber::Decimal(
                CanonicalDecimal::try_new("1.5").unwrap(),
            )),
            TypedValue::Number(CanonicalNumber::Decimal(
                CanonicalDecimal::try_new("2.5").unwrap(),
            )),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(ListKind),
            TypedValue::Object(BTreeMap::new()),
            TypedValue::List(vec![TypedValue::Null]),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(ListValue),
            TypedValue::List(Vec::new()),
            TypedValue::List(vec![TypedValue::Null]),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(ObjectKind),
            TypedValue::List(Vec::new()),
            TypedValue::Object(
                [("item".to_owned(), TypedValue::Null)]
                    .into_iter()
                    .collect(),
            ),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(InlineBytesKind),
            TypedValue::Object(
                [
                    (
                        "$binary".to_owned(),
                        TypedValue::String("baseline".to_owned()),
                    ),
                    (
                        "file_name".to_owned(),
                        TypedValue::String("baseline.bin".to_owned()),
                    ),
                    (
                        "media_type".to_owned(),
                        TypedValue::String("application/baseline".to_owned()),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            inline_typed_value(vec![1], "application/octet-stream", Some("value.bin")),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(InlineBytesBinary),
            inline_typed_value(vec![0], "application/octet-stream", Some("value.bin")),
            inline_typed_value(vec![1], "application/octet-stream", Some("value.bin")),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(InlineBytesFileName),
            inline_typed_value(vec![1], "application/octet-stream", None),
            inline_typed_value(vec![1], "application/octet-stream", Some("value.bin")),
        ),
        typed_value_probe_recipe(
            typed_value_probe_id!(InlineBytesMediaType),
            inline_typed_value(vec![1], "application/octet-stream", Some("value.bin")),
            inline_typed_value(vec![1], "text/plain", Some("value.bin")),
        ),
    ]
}

fn public_projection_probe_recipes() -> Vec<PublicProjectionProbeRecipe> {
    let mut recipes = vec![value_contract_epoch_probe_recipe()];
    recipes.extend(typed_value_probe_recipes());
    recipes
}

#[test]
fn public_probe_recipe_ids_are_the_exact_generated_membership_set() {
    let mut recipe_ids = BTreeSet::new();
    for recipe in public_projection_probe_recipes() {
        assert!(
            recipe_ids.insert(recipe.probe),
            "duplicate public probe recipe: {:?}",
            recipe.probe
        );
    }
    let membership_ids = CANONICAL_PROJECTION_ROUTES
        .iter()
        .flat_map(|route| route.probe_memberships)
        .filter(|membership| {
            membership.disposition == CanonicalProjectionProbeDisposition::Accepted
        })
        .map(|membership| membership.probe)
        .collect::<BTreeSet<_>>();
    assert_eq!(recipe_ids, membership_ids);
}

#[test]
fn typed_value_public_probes_match_their_generated_route_mounts() {
    for recipe in typed_value_probe_recipes() {
        let baseline = run_public_projection_probe_input(&recipe.baseline);
        let changed = run_public_projection_probe_input(&recipe.changed);
        let mut actual_paths = BTreeSet::new();
        collect_changed_json_paths(
            &baseline.canonical_value,
            &changed.canonical_value,
            "$",
            &mut actual_paths,
        );
        let route_closure =
            generated_probe_route_closure(CANONICAL_PROJECTION_ROUTES, recipe.probe);
        let expected_paths = generated_route_mounts(CANONICAL_PROJECTION_ROUTES, &route_closure);
        assert_eq!(
            actual_paths, expected_paths,
            "TypedValue public probe escaped its generated mounts: {:?}",
            recipe.probe
        );
    }
}

fn typed_value_probe_observation_snapshot() -> String {
    typed_value_probe_recipes()
        .into_iter()
        .map(|recipe| {
            let baseline = run_public_projection_probe_input(&recipe.baseline);
            let changed = run_public_projection_probe_input(&recipe.changed);
            (
                recipe.probe,
                format!(
                    "{}|baseline={}|baseline_sha256={}|changed={}|changed_sha256={}",
                    recipe.probe.group,
                    String::from_utf8(baseline.canonical_bytes).unwrap(),
                    hex(baseline.digest),
                    String::from_utf8(changed.canonical_bytes).unwrap(),
                    hex(changed.digest),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn typed_value_public_probe_bytes_and_digests_are_exact() {
    assert_eq!(
        typed_value_probe_observation_snapshot(),
        r#"BooleanKind|baseline={"kind":"string","value":"branch"}|baseline_sha256=10918ac9112673c7622fe9dcbf6b00e6a50e896e420f6d2e28fbbe2be5d6b71f|changed={"kind":"boolean","value":true}|changed_sha256=7095789583a9258ce81762ed6444e3dec52734d1353223d2ef6a39578af75a39
BooleanValue|baseline={"kind":"boolean","value":false}|baseline_sha256=fd34cc8b4876147068c6ffaad81c5d85ed2d9d6f919a1ad419efef1eef008ff4|changed={"kind":"boolean","value":true}|changed_sha256=7095789583a9258ce81762ed6444e3dec52734d1353223d2ef6a39578af75a39
DecimalKind|baseline={"kind":"i64","value":"1"}|baseline_sha256=c4fc6c4a21219a09d4d674c0c5681ab97998d1c5ff0c4ec87491a535398c47c0|changed={"kind":"decimal","value":"1.5"}|changed_sha256=bfbf0bdbb5ba9dc34dc9c1f9ab0c23bee14cdf9b3803c474b655873569f65776
DecimalValue|baseline={"kind":"decimal","value":"1.5"}|baseline_sha256=bfbf0bdbb5ba9dc34dc9c1f9ab0c23bee14cdf9b3803c474b655873569f65776|changed={"kind":"decimal","value":"2.5"}|changed_sha256=6b19bd2d35157f77ded33d4875f808c4f3126307b10cc2071ed5a8e36563634f
I64Kind|baseline={"kind":"u64","value":"1"}|baseline_sha256=a6847dc411e1faa7f08a866638a3ed27b4a05350d8667af5ee1d0fdc8db6d781|changed={"kind":"i64","value":"-1"}|changed_sha256=a86bb51890d35f43c9d5b64bf60c414126d509b1e8a0dcf380b189e1d3f21744
I64Value|baseline={"kind":"i64","value":"-1"}|baseline_sha256=a86bb51890d35f43c9d5b64bf60c414126d509b1e8a0dcf380b189e1d3f21744|changed={"kind":"i64","value":"-2"}|changed_sha256=68e8fc9c1d905398ec2dcbf5f73be8adf5aad4d9c05b727f89a9c97b82f1b48b
InlineBytesBinary|baseline={"kind":"inline_bytes","value":{"$binary":"AA","file_name":"value.bin","media_type":"application/octet-stream"}}|baseline_sha256=adcbe22eeff92e3d744ceec036d11c5add3d9d538d9add08fd21319d360c1d0a|changed={"kind":"inline_bytes","value":{"$binary":"AQ","file_name":"value.bin","media_type":"application/octet-stream"}}|changed_sha256=e2f22e42f822335ba3677f60ef8b29a8857ce9cf4561f8abd02a9c2746717220
InlineBytesFileName|baseline={"kind":"inline_bytes","value":{"$binary":"AQ","file_name":null,"media_type":"application/octet-stream"}}|baseline_sha256=5ec2913eae3eac81562461d0a0127c4c4104bfb470b5658c2d00bb8e8c0eae71|changed={"kind":"inline_bytes","value":{"$binary":"AQ","file_name":"value.bin","media_type":"application/octet-stream"}}|changed_sha256=e2f22e42f822335ba3677f60ef8b29a8857ce9cf4561f8abd02a9c2746717220
InlineBytesKind|baseline={"kind":"object","value":{"$binary":{"kind":"string","value":"baseline"},"file_name":{"kind":"string","value":"baseline.bin"},"media_type":{"kind":"string","value":"application/baseline"}}}|baseline_sha256=e908aaa66e5f4721acf72bdf8633dcf617196754d8eb46875a6d0ace81223b81|changed={"kind":"inline_bytes","value":{"$binary":"AQ","file_name":"value.bin","media_type":"application/octet-stream"}}|changed_sha256=e2f22e42f822335ba3677f60ef8b29a8857ce9cf4561f8abd02a9c2746717220
InlineBytesMediaType|baseline={"kind":"inline_bytes","value":{"$binary":"AQ","file_name":"value.bin","media_type":"application/octet-stream"}}|baseline_sha256=e2f22e42f822335ba3677f60ef8b29a8857ce9cf4561f8abd02a9c2746717220|changed={"kind":"inline_bytes","value":{"$binary":"AQ","file_name":"value.bin","media_type":"text/plain"}}|changed_sha256=98b1535030cd3775bab842aebef2acd28cc0c96d8e47e72c87186094225d9b70
ListKind|baseline={"kind":"object","value":{}}|baseline_sha256=065ddf3440294eb887e2dd297beb57d4420cc26054d2a7278fbdcb28365ac2a2|changed={"kind":"list","value":[{"kind":"null"}]}|changed_sha256=99227b95753609800cbc58cba65d8f30e912e6782e7d6cf5b6d9fb69aee1163f
ListValue|baseline={"kind":"list","value":[]}|baseline_sha256=ab4196da7a2561d94454358efaffccd3f92821507e77acf90ed3fd78b39b26dc|changed={"kind":"list","value":[{"kind":"null"}]}|changed_sha256=99227b95753609800cbc58cba65d8f30e912e6782e7d6cf5b6d9fb69aee1163f
NullKind|baseline={"kind":"boolean","value":false}|baseline_sha256=fd34cc8b4876147068c6ffaad81c5d85ed2d9d6f919a1ad419efef1eef008ff4|changed={"kind":"null"}|changed_sha256=f1c0649d0d3ab26a3841bff912e8a70af069512bd632f5c19f87b10439a44f24
ObjectKind|baseline={"kind":"list","value":[]}|baseline_sha256=ab4196da7a2561d94454358efaffccd3f92821507e77acf90ed3fd78b39b26dc|changed={"kind":"object","value":{"item":{"kind":"null"}}}|changed_sha256=1ec1febdd79fc0d6db1c446f7a2b3675b2199b7462877e6778372e7024bfd607
StringKind|baseline={"kind":"boolean","value":false}|baseline_sha256=fd34cc8b4876147068c6ffaad81c5d85ed2d9d6f919a1ad419efef1eef008ff4|changed={"kind":"string","value":"branch"}|changed_sha256=10918ac9112673c7622fe9dcbf6b00e6a50e896e420f6d2e28fbbe2be5d6b71f
StringValue|baseline={"kind":"string","value":"baseline"}|baseline_sha256=e0ae2f0d93a08a2d454b4b9e8074fa672b300d6aab9698419d5c767a1f223354|changed={"kind":"string","value":"changed"}|changed_sha256=b4f925eee643d9736219474a7204698728045ef6bd063eb5f6b5283874203090
U64Kind|baseline={"kind":"i64","value":"1"}|baseline_sha256=c4fc6c4a21219a09d4d674c0c5681ab97998d1c5ff0c4ec87491a535398c47c0|changed={"kind":"u64","value":"2"}|changed_sha256=58bcf1c90a71a8365ea862f01728b84c27f3352ce27abc97bbedded050c54896
U64Value|baseline={"kind":"u64","value":"1"}|baseline_sha256=a6847dc411e1faa7f08a866638a3ed27b4a05350d8667af5ee1d0fdc8db6d781|changed={"kind":"u64","value":"2"}|changed_sha256=58bcf1c90a71a8365ea862f01728b84c27f3352ce27abc97bbedded050c54896"#
    );
}

fn run_public_projection_probe_input(
    input: &PublicProjectionProbeInput,
) -> PublicProjectionProbeObservation {
    match input {
        PublicProjectionProbeInput::ValueContract {
            catalog,
            value_language_epoch,
        } => {
            let material = value_contract_material(catalog, *value_language_epoch).unwrap();
            let canonical_bytes = canonical_material_bytes(&material).unwrap();
            PublicProjectionProbeObservation {
                digest: *value_contract_sha256(&material).unwrap().as_bytes(),
                canonical_value: serde_json::from_slice(&canonical_bytes).unwrap(),
                canonical_bytes,
            }
        }
        PublicProjectionProbeInput::TypedValue(value) => {
            let material = typed_value_material(value);
            let canonical_bytes = canonical_material_bytes(&material).unwrap();
            PublicProjectionProbeObservation {
                digest: Sha256::digest(&canonical_bytes).into(),
                canonical_value: serde_json::from_slice(&canonical_bytes).unwrap(),
                canonical_bytes,
            }
        }
    }
}

fn generated_probe_route_closure(
    routes: &[CanonicalProjectionRoute],
    probe: CanonicalPublicInputProbeId,
) -> BTreeSet<CanonicalProjectionRouteId> {
    let mut closure = routes
        .iter()
        .filter(|route| {
            route.probe_memberships.iter().any(|membership| {
                membership.probe == probe
                    && membership.disposition == CanonicalProjectionProbeDisposition::Accepted
            })
        })
        .map(|route| route.route_id)
        .collect::<BTreeSet<_>>();

    loop {
        let dependents = closure
            .iter()
            .map(|route_id| {
                routes
                    .iter()
                    .find(|route| route.route_id == *route_id)
                    .expect("generated dependency closure names a production route")
            })
            .flat_map(|route| {
                route
                    .dependency_edges
                    .iter()
                    .map(|edge| edge.dependent_route)
            })
            .collect::<BTreeSet<_>>();
        let previous = closure.clone();
        closure.extend(dependents);
        if closure == previous {
            return closure;
        }
    }
}

fn generated_route_mounts(
    routes: &[CanonicalProjectionRoute],
    route_closure: &BTreeSet<CanonicalProjectionRouteId>,
) -> BTreeSet<String> {
    route_closure
        .iter()
        .flat_map(|route_id| {
            let route = routes
                .iter()
                .find(|route| route.route_id == *route_id)
                .expect("generated probe closure names a production route");
            route.mounts.iter().map(|mount| match mount {
                CanonicalProjectionMount::RootField {
                    canonical_json_path,
                } => (*canonical_json_path).to_owned(),
                CanonicalProjectionMount::SourcePath { .. } => {
                    panic!("a non-Source probe referenced a Source structural mount")
                }
            })
        })
        .collect()
}

#[derive(Clone)]
struct SourceMountCursor<'a> {
    baseline: Option<&'a serde_json::Value>,
    changed: Option<&'a serde_json::Value>,
    baseline_path: Option<String>,
    changed_path: Option<String>,
}

fn source_static_key_value(
    mut value: &serde_json::Value,
    path: &[CanonicalProjectionStaticSegment],
) -> Result<Option<String>, String> {
    for segment in path {
        match segment {
            CanonicalProjectionStaticSegment::Field(field) => {
                let Some(object) = value.as_object() else {
                    return Err(format!("key field {field:?} was mounted on a non-object"));
                };
                let Some(member) = object.get(*field) else {
                    return Ok(None);
                };
                value = member;
            }
            CanonicalProjectionStaticSegment::TaggedValue { expected_kind } => {
                let Some(object) = value.as_object() else {
                    return Err(format!(
                        "tag {expected_kind:?} was mounted on a non-object key value"
                    ));
                };
                if object.get("kind").and_then(serde_json::Value::as_str)
                    != Some(*expected_kind)
                {
                    return Ok(None);
                }
                let Some(payload) = object.get("value") else {
                    return Ok(None);
                };
                value = payload;
            }
        }
    }
    serde_json::to_string(value)
        .map(Some)
        .map_err(|error| format!("key value is not serializable: {error}"))
}

fn source_element_key(
    value: &serde_json::Value,
    key: &[donat_connector_catalog::CanonicalProjectionKeyPart],
) -> Result<Vec<Option<String>>, String> {
    key.iter()
        .map(|part| source_static_key_value(value, part.path))
        .collect()
}

fn source_keyed_elements<'a>(
    value: Option<&'a serde_json::Value>,
    key: &[donat_connector_catalog::CanonicalProjectionKeyPart],
) -> Result<BTreeMap<Vec<Option<String>>, (usize, &'a serde_json::Value)>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| "keyed Source mount was applied to a non-array".to_owned())?;
    let mut elements = BTreeMap::new();
    for (index, element) in array.iter().enumerate() {
        let element_key = source_element_key(element, key)?;
        if elements.insert(element_key.clone(), (index, element)).is_some() {
            return Err(format!(
                "keyed Source mount resolved duplicate key {element_key:?}"
            ));
        }
    }
    Ok(elements)
}

fn source_mount_path(
    path: Option<&str>,
    field: &str,
) -> Option<String> {
    path.map(|path| material_path(path, field))
}

fn source_tagged_payload<'a>(
    value: Option<&'a serde_json::Value>,
    expected_kind: &str,
) -> Option<&'a serde_json::Value> {
    value
        .and_then(serde_json::Value::as_object)
        .filter(|object| {
            object.get("kind").and_then(serde_json::Value::as_str) == Some(expected_kind)
        })
        .and_then(|object| object.get("value"))
}

fn source_matching_tag<'a>(
    value: Option<&'a serde_json::Value>,
    expected_kind: &str,
) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
    value
        .and_then(serde_json::Value::as_object)
        .filter(|object| {
            object.get("kind").and_then(serde_json::Value::as_str) == Some(expected_kind)
        })
}

fn resolve_source_mount(
    mount: CanonicalProjectionMount,
    baseline: &serde_json::Value,
    changed: &serde_json::Value,
) -> Result<BTreeSet<String>, String> {
    let CanonicalProjectionMount::SourcePath { segments } = mount else {
        return Err("a Source route used a non-Source mount".to_owned());
    };
    let mut cursors = vec![SourceMountCursor {
        baseline: Some(baseline),
        changed: Some(changed),
        baseline_path: Some("$".to_owned()),
        changed_path: Some("$".to_owned()),
    }];

    for segment in segments {
        let mut next = Vec::new();
        for cursor in cursors {
            match segment {
                CanonicalProjectionMountSegment::Field(field) => {
                    let baseline = cursor
                        .baseline
                        .and_then(serde_json::Value::as_object)
                        .and_then(|object| object.get(*field));
                    let changed = cursor
                        .changed
                        .and_then(serde_json::Value::as_object)
                        .and_then(|object| object.get(*field));
                    if baseline.is_none() && changed.is_none() {
                        continue;
                    }
                    next.push(SourceMountCursor {
                        baseline,
                        changed,
                        baseline_path: source_mount_path(
                            cursor.baseline_path.as_deref(),
                            field,
                        ),
                        changed_path: source_mount_path(
                            cursor.changed_path.as_deref(),
                            field,
                        ),
                    });
                }
                CanonicalProjectionMountSegment::TaggedKind { expected_kind } => {
                    let baseline = source_matching_tag(cursor.baseline, expected_kind)
                        .and_then(|object| object.get("kind"));
                    let changed = source_matching_tag(cursor.changed, expected_kind)
                        .and_then(|object| object.get("kind"));
                    if baseline.is_none() && changed.is_none() {
                        continue;
                    }
                    next.push(SourceMountCursor {
                        baseline,
                        changed,
                        baseline_path: baseline.and_then(|_| {
                            source_mount_path(cursor.baseline_path.as_deref(), "kind")
                        }),
                        changed_path: changed.and_then(|_| {
                            source_mount_path(cursor.changed_path.as_deref(), "kind")
                        }),
                    });
                }
                CanonicalProjectionMountSegment::TaggedValue { expected_kind } => {
                    let baseline = source_tagged_payload(cursor.baseline, expected_kind);
                    let changed = source_tagged_payload(cursor.changed, expected_kind);
                    if baseline.is_none() && changed.is_none() {
                        continue;
                    }
                    next.push(SourceMountCursor {
                        baseline,
                        changed,
                        baseline_path: baseline.and_then(|_| {
                            source_mount_path(cursor.baseline_path.as_deref(), "value")
                        }),
                        changed_path: changed.and_then(|_| {
                            source_mount_path(cursor.changed_path.as_deref(), "value")
                        }),
                    });
                }
                CanonicalProjectionMountSegment::KeyedElement { key } => {
                    let mut baseline = source_keyed_elements(cursor.baseline, key)?;
                    let mut changed = source_keyed_elements(cursor.changed, key)?;
                    let shared = baseline
                        .keys()
                        .filter(|element_key| changed.contains_key(*element_key))
                        .cloned()
                        .collect::<Vec<_>>();
                    for element_key in shared {
                        let (baseline_index, baseline_value) =
                            baseline.remove(&element_key).unwrap();
                        let (changed_index, changed_value) =
                            changed.remove(&element_key).unwrap();
                        next.push(SourceMountCursor {
                            baseline: Some(baseline_value),
                            changed: Some(changed_value),
                            baseline_path: cursor
                                .baseline_path
                                .as_ref()
                                .map(|path| format!("{path}[{baseline_index}]")),
                            changed_path: cursor
                                .changed_path
                                .as_ref()
                                .map(|path| format!("{path}[{changed_index}]")),
                        });
                    }

                    match (baseline.len(), changed.len()) {
                        (0, 0) => {}
                        (1, 0) => {
                            let (_, (index, value)) = baseline.pop_first().unwrap();
                            next.push(SourceMountCursor {
                                baseline: Some(value),
                                changed: None,
                                baseline_path: cursor
                                    .baseline_path
                                    .as_ref()
                                    .map(|path| format!("{path}[{index}]")),
                                changed_path: None,
                            });
                        }
                        (0, 1) => {
                            let (_, (index, value)) = changed.pop_first().unwrap();
                            next.push(SourceMountCursor {
                                baseline: None,
                                changed: Some(value),
                                baseline_path: None,
                                changed_path: cursor
                                    .changed_path
                                    .as_ref()
                                    .map(|path| format!("{path}[{index}]")),
                            });
                        }
                        (1, 1) => {
                            let (_, (baseline_index, baseline_value)) =
                                baseline.pop_first().unwrap();
                            let (_, (changed_index, changed_value)) =
                                changed.pop_first().unwrap();
                            next.push(SourceMountCursor {
                                baseline: Some(baseline_value),
                                changed: Some(changed_value),
                                baseline_path: cursor
                                    .baseline_path
                                    .as_ref()
                                    .map(|path| format!("{path}[{baseline_index}]")),
                                changed_path: cursor
                                    .changed_path
                                    .as_ref()
                                    .map(|path| format!("{path}[{changed_index}]")),
                            });
                        }
                        _ => {
                            return Err(format!(
                                "keyed Source mount replacement is ambiguous: {} baseline-only and {} changed-only elements",
                                baseline.len(),
                                changed.len()
                            ));
                        }
                    }
                }
            }
        }
        cursors = next;
    }

    Ok(cursors
        .into_iter()
        .filter(|cursor| cursor.baseline != cursor.changed)
        .filter_map(|cursor| cursor.changed_path.or(cursor.baseline_path))
        .collect())
}

fn generated_source_route_mounts(
    routes: &[CanonicalProjectionRoute],
    route_closure: &BTreeSet<CanonicalProjectionRouteId>,
    baseline: &serde_json::Value,
    changed: &serde_json::Value,
) -> Result<BTreeSet<String>, String> {
    let mut paths = BTreeSet::new();
    for route_id in route_closure {
        let route = routes
            .iter()
            .find(|route| route.route_id == *route_id)
            .expect("generated Source probe closure names a production route");
        for mount in route.mounts {
            paths.extend(resolve_source_mount(*mount, baseline, changed)?);
        }
    }
    Ok(paths)
}

fn source_material_value(document: &serde_json::Value) -> serde_json::Value {
    let yaml = serde_yaml::to_string(document).unwrap();
    let record = load_record_bytes(yaml.as_bytes()).unwrap();
    let material = source_record_material(&record).unwrap();
    serde_json::from_slice(&canonical_material_bytes(&material).unwrap()).unwrap()
}

fn source_public_probe_family_route_deltas(
) -> BTreeMap<&'static str, BTreeSet<CanonicalProjectionRouteId>> {
    source_public_probe_family_pairs()
        .into_iter()
        .map(|family| {
            let baseline = source_material_value(&family.baseline);
            let changed = source_material_value(&family.changed);
            let routes = CANONICAL_PROJECTION_ROUTES
                .iter()
                .filter(|route| route.route_id.case == CanonicalMutationCase::SourceRecord)
                .filter_map(|route| {
                    route
                        .mounts
                        .iter()
                        .map(|mount| resolve_source_mount(*mount, &baseline, &changed))
                        .filter_map(Result::ok)
                        .any(|paths| !paths.is_empty())
                        .then_some(route.route_id)
                })
                .collect();
            (family.group, routes)
        })
        .collect()
}

#[test]
fn source_public_probe_family_route_delta_report() {
    let deltas = source_public_probe_family_route_deltas();
    for (group, routes) in &deltas {
        println!("{group}");
        for route in routes {
            println!("  {}.{}", route.material_owner, route.material_field);
        }
    }
    let covered = deltas
        .values()
        .flat_map(BTreeSet::iter)
        .copied()
        .collect::<BTreeSet<_>>();
    let uncovered = CANONICAL_PROJECTION_ROUTES
        .iter()
        .filter(|route| {
            route.route_id.case == CanonicalMutationCase::SourceRecord
                && route.disposition == donat_connector_catalog::CanonicalMutationDisposition::Mutable
                && !covered.contains(&route.route_id)
        })
        .map(|route| route.route_id)
        .collect::<BTreeSet<_>>();
    println!("UNCOVERED");
    for route in &uncovered {
        println!("  {}.{}", route.material_owner, route.material_field);
    }
}

#[test]
fn artifact_hash_digest_route_resolves_its_keyed_public_delta() {
    let baseline_document =
        fixture_document(include_str!("fixtures/provider-contract-record.yaml"));
    let mut changed_document = baseline_document.clone();
    changed_document["artifact_hashes"][0]["digest"] = serde_json::json!(
        "3333333333333333333333333333333333333333333333333333333333333333"
    );
    changed_document["subject"]["value"]["evidence"][0]["content_sha256"] =
        changed_document["artifact_hashes"][0]["digest"].clone();
    let baseline = source_material_value(&baseline_document);
    let changed = source_material_value(&changed_document);
    let mut actual_paths = BTreeSet::new();
    collect_changed_json_paths(&baseline, &changed, "$", &mut actual_paths);
    let route_closure = [CanonicalProjectionRouteId {
        case: CanonicalMutationCase::SourceRecord,
        material_owner: "ArtifactHashMaterialV1",
        material_field: "digest",
    }]
    .into_iter()
    .collect();
    let expected_paths = generated_source_route_mounts(
        CANONICAL_PROJECTION_ROUTES,
        &route_closure,
        &baseline,
        &changed,
    )
    .unwrap();

    assert!(
        expected_paths.is_subset(&actual_paths),
        "the artifact-hash digest delta escaped its generated keyed mount"
    );
    assert_eq!(
        expected_paths,
        ["$.artifact_hashes[0].digest".to_owned()]
            .into_iter()
            .collect()
    );
}

const CONTROLLED_DEPENDENT_ROUTE_ID: CanonicalProjectionRouteId = CanonicalProjectionRouteId {
    case: CanonicalMutationCase::ValueContract,
    material_owner: "ControlledDependencyMaterial",
    material_field: "dependent",
};
const CONTROLLED_DEPENDENCY_EDGE: &[CanonicalProjectionDependencyEdge] =
    &[CanonicalProjectionDependencyEdge {
        dependent_route: CONTROLLED_DEPENDENT_ROUTE_ID,
    }];
const CONTROLLED_DEPENDENT_MOUNT: &[CanonicalProjectionMount] =
    &[CanonicalProjectionMount::RootField {
        canonical_json_path: "$.controlled_dependency",
    }];

fn controlled_dependency_routes() -> [CanonicalProjectionRoute; 2] {
    let seed = *CANONICAL_PROJECTION_ROUTES
        .iter()
        .find(|route| {
            route
                .probe_memberships
                .iter()
                .any(|membership| membership.probe == VALUE_CONTRACT_EPOCH_PROBE)
        })
        .expect("the controlled closure fixture needs its generated seed route");
    let dependent = CanonicalProjectionRoute {
        route_id: CONTROLLED_DEPENDENT_ROUTE_ID,
        assignment: CanonicalProjectionAssignment::NormalizedMember {
            normalized_owner: "ControlledDependency",
            normalized_member: "dependent",
            target: CONTROLLED_DEPENDENT_ROUTE_ID,
        },
        probe_memberships: &[],
        mounts: CONTROLLED_DEPENDENT_MOUNT,
        dependency_edges: &[],
        ..seed
    };
    [
        CanonicalProjectionRoute {
            dependency_edges: CONTROLLED_DEPENDENCY_EDGE,
            ..seed
        },
        dependent,
    ]
}

#[test]
fn dependency_edge_adds_the_dependent_routes_global_mount() {
    let with_dependency = controlled_dependency_routes();
    let mut without_dependency = with_dependency;
    without_dependency[0].dependency_edges = &[];

    let with_closure = generated_probe_route_closure(&with_dependency, VALUE_CONTRACT_EPOCH_PROBE);
    let without_closure =
        generated_probe_route_closure(&without_dependency, VALUE_CONTRACT_EPOCH_PROBE);
    let with_paths = generated_route_mounts(&with_dependency, &with_closure);
    let without_paths = generated_route_mounts(&without_dependency, &without_closure);
    assert_eq!(
        with_paths
            .difference(&without_paths)
            .cloned()
            .collect::<BTreeSet<_>>(),
        ["$.controlled_dependency".to_owned()].into_iter().collect()
    );
}

#[test]
fn canonical_projection_domains_and_calculation_order_are_exact() {
    let vectors = [
        (
            b"donat.connector.source-record.v1\0".as_slice(),
            b"{}".as_slice(),
            "210c9ca679adf8e51a22e107484e4dd5e27a1d894901541bf5b5abd5a71fcbd4",
        ),
        (
            b"donat.connector.semantic.v1\0".as_slice(),
            b"{}".as_slice(),
            "799ea52772e70c9b45d9af5fcd185ae47f0fdcccd5957214d5425a1941c36f19",
        ),
        (
            b"donat.connector.provenance.v1\0".as_slice(),
            b"{}".as_slice(),
            "a0b89c2c2f1c7e90d8427e2c4251234ce596f2af9105f23745237aa08b1e06f4",
        ),
        (
            b"donat.connector.value-contract.v1\0".as_slice(),
            b"{}".as_slice(),
            "6f72f51c0e8b4f09a064c507a1d879921d4753cc4378fb6fefecb27e25e3dd2f",
        ),
        (
            b"donat.connector.source-record.v1\0".as_slice(),
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "d6c4fc943d8ed980d248ffa25f2d8d16be65953603705d5afc29e5e8a045269f",
        ),
        (
            b"donat.connector.semantic.v1\0".as_slice(),
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "2f7116c006c1fdfccdd12b1fa954cd94feffee889ceac39a0f76df616da7be34",
        ),
        (
            b"donat.connector.provenance.v1\0".as_slice(),
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "4e31e445b6c8d06e6b93fd5cc66731b850a84853dd4ee28d6a76663138217a23",
        ),
        (
            b"donat.connector.value-contract.v1\0".as_slice(),
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "e74426ca8fb7b23e99f1f14f4a6d281575489c33312e27df9e9005f37158d4ab",
        ),
    ];

    for (domain, bytes, expected) in vectors {
        let mut hash = Sha256::new();
        hash.update(domain);
        hash.update(bytes);
        assert_eq!(hex(hash.finalize().into()), expected);
    }
}

#[test]
fn value_contract_epoch_public_probe_matches_exact_generated_route_closure() {
    let recipe = value_contract_epoch_probe_recipe();
    let baseline = run_public_projection_probe_input(&recipe.baseline);
    let changed = run_public_projection_probe_input(&recipe.changed);
    assert_eq!(
        baseline.canonical_bytes.as_slice(),
        br#"{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1}"#
    );
    assert_eq!(
        changed.canonical_bytes.as_slice(),
        br#"{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":2}"#
    );
    assert_eq!(
        hex(baseline.digest),
        "79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a"
    );
    assert_eq!(
        hex(changed.digest),
        "9b8a1aa8322f2dffed93390502313c4a73cb24d5830e28eca52cc83943c42a38"
    );
    assert_ne!(baseline.canonical_bytes, changed.canonical_bytes);
    assert_ne!(baseline.digest, changed.digest);

    let mut actual_paths = BTreeSet::new();
    collect_changed_json_paths(
        &baseline.canonical_value,
        &changed.canonical_value,
        "$",
        &mut actual_paths,
    );
    let route_closure = generated_probe_route_closure(CANONICAL_PROJECTION_ROUTES, recipe.probe);
    let expected_paths = generated_route_mounts(CANONICAL_PROJECTION_ROUTES, &route_closure);
    assert_eq!(
        actual_paths, expected_paths,
        "the public-input delta diverged from the generated dependency closure"
    );
}

#[test]
fn selected_header_capability_vector_is_exact() {
    let selected = selected_response_header(
        ConnectorId::literal("donat.http"),
        OperationId::literal("get"),
        donat_connector_catalog::StableSemver::new(1, 0, 0),
        CompiledStepId::literal("request"),
        "X-Request-ID",
    )
    .unwrap();
    assert_eq!(selected.canonical_lowercase_header_name, "x-request-id");
    assert_eq!(
        selected.capability.as_str(),
        "response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3"
    );
    assert_eq!(selected.capability.as_str().len(), 80);
}

#[test]
fn jcs_member_names_use_utf16_order() {
    assert_eq!(
        canonicalize_raw("{\"�\":1,\"𐀀\":2}".as_bytes()).unwrap(),
        "{\"𐀀\":2,\"�\":1}".as_bytes()
    );
}

#[test]
fn canonical_projection_field_matrix_is_total() {
    let report = validate_canonical_owner_manifest().unwrap();
    assert!(report.mapping_rows > 0);
    assert!(report.normalized_leaf_and_branch_total > 0);
}

#[test]
fn canonical_owner_manifest_has_no_wildcards_or_duplicate_paths() {
    let manifest = canonical_projection_owner_manifest();
    assert!(!manifest.contains("|*|"));
    assert!(!manifest.contains("|family|"));
    assert!(!manifest.contains("<family>"));
    let report = validate_canonical_owner_manifest().unwrap();
    assert_eq!(manifest.lines().count(), report.mapping_rows + 1);
}

#[test]
fn canonical_owner_manifest_matches_normalized_leaf_and_branch_set() {
    let report = validate_canonical_owner_manifest().unwrap();
    let manifest = canonical_projection_owner_manifest();
    let rows: Vec<_> = manifest.lines().skip(1).collect();
    let normalized_owners: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|row| {
            let columns: Vec<_> = row.split('|').collect();
            (columns[0], columns[1])
        })
        .collect();
    assert_eq!(report.mapping_rows, rows.len());
    assert_eq!(
        report.normalized_leaf_and_branch_total,
        normalized_owners.len()
    );
}

#[test]
fn typed_value_projection_tags_do_not_collide() {
    let values = [
        TypedValue::Number(CanonicalNumber::I64(1)),
        TypedValue::Number(CanonicalNumber::U64(1)),
        TypedValue::String("1".to_owned()),
    ];
    let bytes: Vec<_> = values
        .iter()
        .map(|value| canonical_material_bytes(&typed_value_material(value)).unwrap())
        .collect();
    assert_eq!(bytes[0], br#"{"kind":"i64","value":"1"}"#);
    assert_eq!(bytes[1], br#"{"kind":"u64","value":"1"}"#);
    assert_eq!(bytes[2], br#"{"kind":"string","value":"1"}"#);
    assert_ne!(bytes[0], bytes[1]);
    assert_ne!(bytes[1], bytes[2]);
}

#[test]
fn typed_value_material_constructor_rejects_noncanonical_i64() {
    assert_eq!(
        TypedValueMaterialV1::i64("not-an-integer")
            .unwrap_err()
            .code(),
        "catalog_jcs_schema_mismatch"
    );
}

#[test]
fn typed_value_projection_preserves_u64_decimal_and_base64() {
    let maximum = typed_value_material(&TypedValue::Number(CanonicalNumber::U64(u64::MAX)));
    assert_eq!(
        canonical_material_bytes(&maximum).unwrap(),
        br#"{"kind":"u64","value":"18446744073709551615"}"#
    );
    let bytes =
        BoundedInlineBytes::try_new(vec![0xff, 0x00], "application/octet-stream", None, 2).unwrap();
    let material = typed_value_material(&TypedValue::InlineBytes(bytes));
    assert_eq!(
        canonical_material_bytes(&material).unwrap(),
        br#"{"kind":"inline_bytes","value":{"$binary":"_wA","file_name":null,"media_type":"application/octet-stream"}}"#
    );
}

#[test]
fn typed_value_schema_rows_drive_every_production_match_arm() {
    let value = TypedValue::List(vec![
        TypedValue::Null,
        TypedValue::Boolean(true),
        TypedValue::String("text".to_owned()),
        TypedValue::Number(CanonicalNumber::I64(-7)),
        TypedValue::Number(CanonicalNumber::U64(u64::MAX)),
        TypedValue::Number(CanonicalNumber::Decimal(
            CanonicalDecimal::try_new("1.25").unwrap(),
        )),
        TypedValue::Object(
            [("key".to_owned(), TypedValue::String("value".to_owned()))]
                .into_iter()
                .collect(),
        ),
        TypedValue::InlineBytes(
            BoundedInlineBytes::try_new(
                vec![0xff, 0x00],
                "application/octet-stream",
                Some("value.bin"),
                2,
            )
            .unwrap(),
        ),
    ]);

    assert_eq!(
        canonical_material_bytes(&typed_value_material(&value)).unwrap(),
        br#"{"kind":"list","value":[{"kind":"null"},{"kind":"boolean","value":true},{"kind":"string","value":"text"},{"kind":"i64","value":"-7"},{"kind":"u64","value":"18446744073709551615"},{"kind":"decimal","value":"1.25"},{"kind":"object","value":{"key":{"kind":"string","value":"value"}}},{"kind":"inline_bytes","value":{"$binary":"_wA","file_name":"value.bin","media_type":"application/octet-stream"}}]}"#
    );
}

#[test]
fn typed_value_projection_string_and_object_do_not_collide() {
    let string = TypedValue::String(r#"{"kind":"null","value":null}"#.to_owned());
    let object = TypedValue::Object(
        [("kind".to_owned(), TypedValue::String("null".to_owned()))]
            .into_iter()
            .collect(),
    );
    assert_ne!(
        canonical_material_bytes(&typed_value_material(&string)).unwrap(),
        canonical_material_bytes(&typed_value_material(&object)).unwrap()
    );
}

#[test]
fn source_projection_preserves_declared_entrypoint_order() {
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/donat-owned-record.yaml"),
    )
    .unwrap();
    let fixture = fixture.replace(
        "entrypoints: [crates/server/src/connectors/http.rs]",
        "entrypoints: [z/entrypoint.rs, a/entrypoint.rs]",
    );
    let record = load_record_bytes(fixture.as_bytes()).unwrap();
    let material = source_record_material(&record).unwrap();
    let document: serde_json::Value =
        serde_json::from_slice(&canonical_material_bytes(&material).unwrap()).unwrap();
    assert_eq!(
        document["entrypoints"],
        serde_json::json!(["z/entrypoint.rs", "a/entrypoint.rs"])
    );
}

#[test]
fn source_record_root_fields_flow_through_the_real_builder_without_omission_or_swap() {
    let record = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap();
    let material = source_record_material(&record).unwrap();
    let bytes = canonical_material_bytes(&material).unwrap();
    let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(
        hex(*record_sha256(&material).unwrap().as_bytes()),
        "b882f61f035f53731ff6ae3763f8eada0c3981a80af87ba8fe73b8603d5547b7"
    );
    assert_eq!(document["reviewer"], serde_json::json!("reviewer.demo"));
    assert_eq!(
        document["proposed_destinations"],
        serde_json::json!(["connector-catalog/sources/records/demo.yaml"])
    );
    assert_eq!(
        document["red_tests"],
        serde_json::json!(["provider_fact_red"])
    );
}

#[test]
fn positive_source_loader_branches_have_exact_generated_material_bytes_and_shapes() {
    let mut hashes = Vec::new();
    let schema = material_branch_schema();
    let mut material_branches = BTreeSet::new();
    for (name, document) in positive_source_projection_cases() {
        let yaml = serde_yaml::to_string(&document).unwrap();
        let record = load_record_bytes(yaml.as_bytes())
            .unwrap_or_else(|error| panic!("{name} must pass the real loader: {error}"));
        let material = source_record_material(&record)
            .unwrap_or_else(|error| panic!("{name} must pass the real builder: {error}"));
        let bytes = canonical_material_bytes(&material).unwrap();
        let projected: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        collect_material_branches_for_definition(
            &schema,
            "SourceRecordMaterialV1",
            &projected,
            &mut material_branches,
        );
        hashes.push((name, hex(Sha256::digest(&bytes).into())));
    }

    assert_eq!(
        hashes,
        [
            (
                "npm-verified",
                "334b89a4be9b4a88f9e6757d8ef340172f1e0f5579a4952a39bf0d943a392c15",
            ),
            (
                "provider-repository-reviewed-use",
                "6909a98c851be8e4b69d0b7316c7e52238557e0259028393fcaee7ba74055bc1",
            ),
            (
                "donat-owned",
                "1151fb39997b517fe6d4c81ed842cc340901140dd131618dc7a582795d65f1bb",
            ),
            (
                "npm-collections-and-typed-values",
                "59d6eb1594bc91b86793d0dacc3f71f2a49853130cc2d3af67da8732c7fdb620",
            ),
            (
                "npm-absent-mismatch-written-grant",
                "d0bb3f415a791624c8f8245ed379afb57211feade8256219124cf5f053199725",
            ),
            (
                "npm-rejected-and-verified-provenance",
                "ba5458b068e6a73a81eea809b09c9b286642f85cf6c5e527dde497ac8b926ec2",
            ),
            (
                "npm-rejected-provenance",
                "5ae41f8977e6f0fae690526ccc1b56c3346a2f6a1389dcae88b3ccc86bb2ac52",
            ),
            (
                "provider-permissive-terms",
                "e4ca35757556531313a91d49faa245be0ab61c8b6eab08121198010e2a5c3266",
            ),
            (
                "provider-versioned-document-section",
                "b8f05cfcda4ab7fa9567ed20fc8ee5502833f7e5bc79eb8fc3ffe46016e4ebf0",
            ),
        ]
        .into_iter()
        .map(|(name, hash)| (name, hash.to_owned()))
        .collect::<Vec<_>>()
    );
    let expected_branches = CANONICAL_SOURCE_LOADER_BRANCH_CANDIDATES
        .iter()
        .flatten()
        .map(|branch| (*branch).to_owned())
        .chain(
            CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
                .iter()
                .filter(|descriptor| {
                    descriptor.case == CanonicalMutationCase::TypedValue
                        && descriptor.canonical_path.ends_with(".kind")
                })
                .map(|descriptor| descriptor.material_member.to_owned()),
        )
        .collect::<BTreeSet<_>>();
    assert_eq!(
        material_branches, expected_branches,
        "real-loader cases and generated source/typed branch routes diverged"
    );
}

#[test]
fn npm_integrity_projection_is_a_closed_algorithm_and_digest_object() {
    let record = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serpapi-npm-record.yaml"),
    )
    .unwrap();
    let material = source_record_material(&record).unwrap();
    let document: serde_json::Value =
        serde_json::from_slice(&canonical_material_bytes(&material).unwrap()).unwrap();
    assert_eq!(
        document["subject"]["value"]["integrity"],
        serde_json::json!({
            "algorithm": {"kind": "sha512", "value": null},
            "digest": "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCkqKywtLi8wMTIzNDU2Nzg5Ojs8PT4_QA"
        })
    );
}

fn full_vector(label: &str) -> &'static str {
    let document = include_str!(
        "../../../knowledgebase/declarative-saas/decisions/012-canonical-catalog-projections-and-persisted-header-capabilities.md"
    );
    let marker = format!("\n{label}:\n{{");
    let start = document.rfind(&marker).unwrap() + marker.len() - 1;
    let end = document[start..].find('\n').unwrap() + start;
    &document[start..end]
}

fn compile_valid_semantic_vector() -> String {
    let capability = selected_response_header(
        ConnectorId::literal("demo"),
        OperationId::literal("op.read"),
        donat_connector_catalog::StableSemver::new(1, 0, 0),
        CompiledStepId::literal("request"),
        "x-request-id",
    )
    .unwrap()
    .capability;
    let source = full_vector("semantic")
        .replace(
            r#""token_pointer":"/access_token","token_step":"token""#,
            r#""token_pointer":"/access_token","token_step":"request""#,
        )
        .replace(
            r#""request":{"kind":"json","value":{"bindings":["query"]}}"#,
            r#""request":{"kind":"none","value":null}"#,
        )
        .replace(
            "response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3",
            capability.as_str(),
        );
    let mut value = serde_json::from_str::<serde_json::Value>(&source).unwrap();
    value["operations"][0]["effect"] = serde_json::json!({"kind": "read_only", "value": null});
    value["operations"][0]["resolved_fact_values"] = serde_json::json!([]);
    value["triggers"][1]["value"]["subscription_operations"] = serde_json::Value::Null;
    String::from_utf8(canonicalize_raw(&serde_json::to_vec(&value).unwrap()).unwrap()).unwrap()
}

fn semantic_vector_manifest_document() -> serde_json::Value {
    let mut semantic =
        serde_json::from_str::<serde_json::Value>(&compile_valid_semantic_vector()).unwrap();
    let semantic = semantic.as_object_mut().unwrap();
    semantic.remove("canonical_schema_epoch");
    let value_language_epoch = semantic.remove("value_language_epoch").unwrap();
    let connector = semantic.remove("connector").unwrap();
    let connector = connector.as_object().unwrap();

    let mut document = serde_json::Map::new();
    document.insert("connector".to_owned(), connector["id"].clone());
    document.insert("connector_version".to_owned(), connector["version"].clone());
    document.insert(
        "manifest_version".to_owned(),
        connector["manifest_version"].clone(),
    );
    document.insert(
        "runtime_abi_epoch".to_owned(),
        connector["runtime_abi_epoch"].clone(),
    );
    document.insert("value_language_epoch".to_owned(), value_language_epoch);
    document.insert("provider".to_owned(), connector["provider"].clone());
    document.insert("api_identity".to_owned(), connector["api_identity"].clone());
    for key in ["credentials", "origins", "operations", "triggers"] {
        document.insert(key.to_owned(), semantic.remove(key).unwrap());
    }
    document.insert(
        "provenance".to_owned(),
        serde_json::json!([
            {
                "source_record_id": "source.donat.http.v1",
                "artifact_hashes": [],
                "license_id": "Apache-2.0",
                "notice_id": "notice.donat",
                "contract_facts": []
            },
            {
                "source_record_id": "source.demo.provider.v1",
                "artifact_hashes": [{
                    "artifact_id": "artifact.openapi",
                    "algorithm": {"kind": "sha256"},
                    "digest":
                        "1111111111111111111111111111111111111111111111111111111111111111",
                    "path": "openapi.json"
                }],
                "license_id": "MIT",
                "notice_id": "notice.demo",
                "contract_facts": []
            }
        ]),
    );
    serde_json::Value::Object(document)
}

fn positive_semantic_projection_cases() -> Vec<(&'static str, serde_json::Value)> {
    let baseline = semantic_vector_manifest_document();

    let mut credentials_and_private_network = baseline.clone();
    let credentials = credentials_and_private_network["credentials"]
        .as_array_mut()
        .unwrap();
    let mut credential_template = credentials[0].clone();
    credential_template["fields"]
        .as_array_mut()
        .unwrap()
        .extend([
            serde_json::json!({
                "field": "field.sensitive",
                "required": false,
                "secret": {"kind": "sensitive", "value": null},
                "maximum_bytes": 128,
                "redaction": {
                    "kind": "fixed",
                    "value": {"replacement": "[redacted]"}
                }
            }),
            serde_json::json!({
                "field": "field.preserve",
                "required": false,
                "secret": {"kind": "non_secret", "value": null},
                "maximum_bytes": 128,
                "redaction": {
                    "kind": "preserve_last",
                    "value": {"characters": 4}
                }
            }),
        ]);
    credentials[0] = credential_template.clone();
    for (credential, auth_plan) in [
        (
            "credential.fixed-header",
            serde_json::json!({
                "kind": "fixed_header_api_key",
                "value": {
                    "field": "field.client_secret",
                    "header": "x-api-key"
                }
            }),
        ),
        (
            "credential.fixed-query",
            serde_json::json!({
                "kind": "fixed_query_api_key",
                "value": {
                    "field": "field.client_secret",
                    "query": "api_key"
                }
            }),
        ),
        (
            "credential.bearer",
            serde_json::json!({
                "kind": "bearer",
                "value": {"token": "field.client_secret"}
            }),
        ),
        (
            "credential.basic",
            serde_json::json!({
                "kind": "http_basic",
                "value": {
                    "username": "field.client_id",
                    "password": "field.client_secret"
                }
            }),
        ),
        (
            "credential.preprovisioned",
            serde_json::json!({
                "kind": "preprovisioned_oauth_access_token",
                "value": {"token": "field.client_secret"}
            }),
        ),
    ] {
        let mut credential_value = credential_template.clone();
        credential_value["credential"] = serde_json::json!(credential);
        credential_value["auth_plan"] = auth_plan;
        credentials.push(credential_value);
    }
    for origin in credentials_and_private_network["origins"]
        .as_array_mut()
        .unwrap()
    {
        origin["network_policy"] = serde_json::json!({
            "kind": "private_allowed",
            "value": {"policy": "policy.private-network"}
        });
    }
    for origin in credentials_and_private_network["operations"][0]["origins"]
        .as_array_mut()
        .unwrap()
    {
        origin["network_policy"] = serde_json::json!({
            "kind": "private_allowed",
            "value": {"policy": "policy.private-network"}
        });
    }

    let mut request_and_response_shapes = baseline.clone();
    let base_step = request_and_response_shapes["operations"][0]["steps"][0].clone();
    let mut steps = vec![base_step.clone()];

    let mut constant_step = base_step.clone();
    constant_step["step"] = serde_json::json!("constant");
    constant_step["selected_response_headers"] = serde_json::json!([]);
    constant_step["headers"][0]["binding"]["source"] = serde_json::json!({
        "kind": "constant",
        "value": {
            "value": {"kind": "string", "value": "fixed"}
        }
    });
    steps.push(constant_step);

    for (step_id, request, response) in [
        (
            "json",
            serde_json::json!({"kind": "json", "value": {"bindings": ["query"]}}),
            serde_json::json!({
                "kind": "json",
                "value": {
                    "mappings": [{"pointer": "/result", "target": "query"}]
                }
            }),
        ),
        (
            "form",
            serde_json::json!({
                "kind": "form_urlencoded",
                "value": {"bindings": ["query"]}
            }),
            serde_json::json!({
                "kind": "json",
                "value": {"mappings": []}
            }),
        ),
        (
            "multipart",
            serde_json::json!({
                "kind": "multipart",
                "value": {"bindings": ["query"]}
            }),
            serde_json::json!({
                "kind": "json",
                "value": {"mappings": []}
            }),
        ),
        (
            "raw",
            serde_json::json!({
                "kind": "raw_bytes",
                "value": {"binding": "query"}
            }),
            serde_json::json!({
                "kind": "raw_bytes",
                "value": {"target": "query"}
            }),
        ),
    ] {
        let mut step = base_step.clone();
        step["step"] = serde_json::json!(step_id);
        step["headers"] = serde_json::json!([]);
        step["request"] = request;
        step["response"] = response;
        step["selected_response_headers"] = serde_json::json!([]);
        steps.push(step);
    }
    request_and_response_shapes["operations"][0]["steps"] = serde_json::json!(steps);

    let mut idempotency_bindings = baseline.clone();
    let mut header_step = idempotency_bindings["operations"][0]["steps"][0].clone();
    header_step["headers"][0]["name"] = serde_json::json!("idempotency-key");
    let mut body_step = header_step.clone();
    body_step["step"] = serde_json::json!("body");
    body_step["headers"] = serde_json::json!([]);
    body_step["request"] = serde_json::json!({"kind": "json", "value": {"bindings": ["query"]}});
    body_step["selected_response_headers"] = serde_json::json!([]);
    idempotency_bindings["operations"][0]["steps"] = serde_json::json!([header_step, body_step]);
    idempotency_bindings["operations"][0]["effect"] = serde_json::json!({
        "kind": "provider_idempotent",
        "value": {
            "side_effect_steps": [
                {
                    "step": "request",
                    "fixed_binding": {
                        "kind": "header",
                        "value": {"name": "idempotency-key"}
                    },
                    "scope": "scope.header",
                    "minimum_retention_ms": "86400000",
                    "clock_safety_margin_ms": "1000"
                },
                {
                    "step": "body",
                    "fixed_binding": {
                        "kind": "body_field",
                        "value": {"pointer": "query"}
                    },
                    "scope": "scope.body",
                    "minimum_retention_ms": "86400000",
                    "clock_safety_margin_ms": "1000"
                }
            ]
        }
    });

    let pagination_bounds = baseline["operations"][0]["pagination"]["value"]["bounds"].clone();
    let mut pagination_none = baseline.clone();
    pagination_none["operations"][0]["pagination"] =
        serde_json::json!({"kind": "none", "value": null});

    let mut pagination_offset_limit = baseline.clone();
    pagination_offset_limit["operations"][0]["pagination"] = serde_json::json!({
        "kind": "offset_limit",
        "value": {
            "offset_binding": "query",
            "limit_binding": "limit",
            "initial_offset": "0",
            "page_size": 25,
            "bounds": pagination_bounds.clone()
        }
    });

    let mut pagination_page_number = baseline.clone();
    pagination_page_number["operations"][0]["pagination"] = serde_json::json!({
        "kind": "page_number",
        "value": {
            "page_binding": "query",
            "page_size_binding": "page-size",
            "initial_page": "1",
            "page_size": 25,
            "bounds": pagination_bounds.clone()
        }
    });

    let mut pagination_link_relation = baseline.clone();
    pagination_link_relation["operations"][0]["pagination"] = serde_json::json!({
        "kind": "link_relation",
        "value": {
            "relation": "next",
            "selected_header":
                baseline["operations"][0]["steps"][0]["selected_response_headers"][0].clone(),
            "bounds": pagination_bounds.clone()
        }
    });

    let mut pagination_processor = baseline.clone();
    pagination_processor["operations"][0]["pagination"] = serde_json::json!({
        "kind": "processor",
        "value": {
            "processor": {
                "id": "pagination.demo",
                "implementation_revision": 1
            },
            "bounds": pagination_bounds
        }
    });

    vec![
        ("baseline", baseline),
        (
            "credentials-and-private-network",
            credentials_and_private_network,
        ),
        ("request-and-response-shapes", request_and_response_shapes),
        ("idempotency-bindings", idempotency_bindings),
        ("pagination-none", pagination_none),
        ("pagination-offset-limit", pagination_offset_limit),
        ("pagination-page-number", pagination_page_number),
        ("pagination-link-relation", pagination_link_relation),
        ("pagination-processor", pagination_processor),
    ]
}

fn replace_value_language_epochs(value: &mut serde_json::Value, epoch: u32) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                replace_value_language_epochs(value, epoch);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if key == "value_language_epoch" {
                    *value = serde_json::json!(epoch);
                } else {
                    replace_value_language_epochs(value, epoch);
                }
            }
        }
        _ => {}
    }
}

fn value_contract_document_sha256(value: &serde_json::Value) -> String {
    let bytes = canonicalize_raw(&serde_json::to_vec(value).unwrap()).unwrap();
    let material = decode_value_contract_material(&bytes).unwrap();
    hex(*value_contract_sha256(&material).unwrap().as_bytes())
}

fn semantic_manifest_at_value_language_epoch(
    baseline: &serde_json::Value,
    epoch: u32,
) -> serde_json::Value {
    let mut changed = baseline.clone();
    replace_value_language_epochs(&mut changed, epoch);
    for operation in changed["operations"].as_array_mut().unwrap() {
        operation["input_contract_sha256"] =
            serde_json::json!(value_contract_document_sha256(&operation["input"]));
        operation["output_contract_sha256"] =
            serde_json::json!(value_contract_document_sha256(&operation["output"]));
    }
    changed
}

fn public_pipeline_semantic_bytes(
    document: &serde_json::Value,
    canonical_schema_epoch: u32,
) -> Vec<u8> {
    let owned_source = std::str::from_utf8(include_bytes!("fixtures/donat-owned-record.yaml"))
        .unwrap()
        .replace("operations: [get]", "operations: [op.read]");
    let owned = load_record_bytes(owned_source.as_bytes()).unwrap();
    let owned_id = owned.record_id();
    let provider =
        load_record_bytes(include_bytes!("fixtures/provider-contract-record.yaml")).unwrap();
    let mut reviews = SourceReviewRegistry::default();
    reviews.approve_reviewed_use("review.demo").unwrap();
    let accepted = AcceptedRecordCatalog::build(
        vec![owned, provider],
        &[(
            owned_id,
            [OperationId::literal("op.read")].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        &reviews,
    )
    .unwrap();
    let bytes = serde_yaml::to_string(document).unwrap();
    let manifest = load_connector_manifest_bytes(bytes.as_bytes()).unwrap();
    let policies = BTreeMap::new();
    let checked = compile_connector_manifest(&manifest, &accepted, &policies).unwrap();
    canonical_material_bytes(&semantic_material(&checked, canonical_schema_epoch).unwrap()).unwrap()
}

struct PublicProvenanceBuild {
    bytes: Vec<u8>,
    domain_hash: String,
    semantic_sha256: String,
    source_sha256: BTreeMap<String, String>,
}

fn public_pipeline_provenance(
    document: &serde_json::Value,
    canonical_schema_epoch: u32,
    classifier_epoch: u32,
    generator_epoch: u32,
) -> PublicProvenanceBuild {
    let owned_source = std::str::from_utf8(include_bytes!("fixtures/donat-owned-record.yaml"))
        .unwrap()
        .replace("operations: [get]", "operations: [op.read]");
    public_pipeline_provenance_with_inputs(PublicProvenanceInput {
        document,
        owned_source: owned_source.as_bytes(),
        provider_source: include_bytes!("fixtures/provider-contract-record.yaml"),
        approved_reviews: &["review.demo"],
        policies: BTreeMap::new(),
        canonical_schema_epoch,
        classifier_epoch,
        generator_epoch,
    })
}

struct PublicProvenanceInput<'input> {
    document: &'input serde_json::Value,
    owned_source: &'input [u8],
    provider_source: &'input [u8],
    approved_reviews: &'input [&'input str],
    policies: BTreeMap<donat_connector_catalog::DonatPolicyId, TypedValue>,
    canonical_schema_epoch: u32,
    classifier_epoch: u32,
    generator_epoch: u32,
}

fn public_pipeline_provenance_with_inputs(
    input: PublicProvenanceInput<'_>,
) -> PublicProvenanceBuild {
    let PublicProvenanceInput {
        document,
        owned_source,
        provider_source,
        approved_reviews,
        policies,
        canonical_schema_epoch,
        classifier_epoch,
        generator_epoch,
    } = input;
    let owned = load_record_bytes(owned_source).unwrap();
    let owned_id = owned.record_id();
    let provider = load_record_bytes(provider_source).unwrap();
    let source_sha256 = [&owned, &provider]
        .into_iter()
        .map(|record| {
            let material = source_record_material(record).unwrap();
            (
                record.record_id().as_str().to_owned(),
                hex(*record_sha256(&material).unwrap().as_bytes()),
            )
        })
        .collect();
    let mut reviews = SourceReviewRegistry::default();
    for decision in approved_reviews {
        reviews.approve_reviewed_use(decision).unwrap();
        reviews.approve_written_grant(decision).unwrap();
    }
    let accepted = AcceptedRecordCatalog::build(
        vec![owned, provider],
        &[(
            owned_id,
            [OperationId::literal("op.read")].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        &reviews,
    )
    .unwrap();
    let bytes = serde_yaml::to_string(document).unwrap();
    let manifest = load_connector_manifest_bytes(bytes.as_bytes()).unwrap();
    let checked = compile_connector_manifest(&manifest, &accepted, &policies).unwrap();
    let semantic = semantic_material(&checked, canonical_schema_epoch).unwrap();
    let semantic_sha256 = hex(*semantic_sha256(&semantic).unwrap().as_bytes());
    let provenance = provenance_material(
        &checked,
        canonical_schema_epoch,
        classifier_epoch,
        generator_epoch,
    )
    .unwrap();
    PublicProvenanceBuild {
        bytes: canonical_material_bytes(&provenance).unwrap(),
        domain_hash: hex(*provenance_sha256(&provenance).unwrap().as_bytes()),
        semantic_sha256,
        source_sha256,
    }
}

struct PublicProvenanceCase {
    name: &'static str,
    manifest: serde_json::Value,
    owned_source: Vec<u8>,
    provider_source: Vec<u8>,
    approved_reviews: Vec<&'static str>,
    policies: BTreeMap<DonatPolicyId, TypedValue>,
}

fn baseline_public_provenance_sources() -> (serde_json::Value, serde_json::Value) {
    let mut owned = fixture_document(include_str!("fixtures/donat-owned-record.yaml"));
    owned["admission"]["value"]["operations"] = serde_json::json!(["op.read"]);
    let provider = fixture_document(include_str!("fixtures/provider-contract-record.yaml"));
    (owned, provider)
}

fn positive_provenance_projection_cases() -> Vec<PublicProvenanceCase> {
    let baseline_manifest = semantic_vector_manifest_document();
    let (baseline_owned, baseline_provider) = baseline_public_provenance_sources();

    let mut collections_manifest = baseline_manifest.clone();
    let mut collections_owned = baseline_owned.clone();
    let mut collections_provider = baseline_provider.clone();
    let permissive_license = serde_json::json!({
        "kind": "permissive",
        "value": {
            "spdx_id": "MIT",
            "selected_dual_license_branch": null,
            "license_file_path": "LICENSE",
            "license_file_sha256":
                "2222222222222222222222222222222222222222222222222222222222222222"
        }
    });
    let written_grant = serde_json::json!({
        "kind": "written_grant",
        "value": {
            "decision_id": "review.written.grant",
            "grant_sha256":
                "3333333333333333333333333333333333333333333333333333333333333333"
        }
    });
    collections_owned["license"] = written_grant.clone();
    collections_owned["dependencies"] = serde_json::json!([
        {
            "dependency": "dependency.shipped",
            "disposition": {
                "kind": "shipped",
                "value": {"license": permissive_license.clone()}
            }
        },
        {
            "dependency": "dependency.build",
            "disposition": {
                "kind": "build_only",
                "value": {"license": written_grant.clone()}
            }
        },
        {
            "dependency": "dependency.type",
            "disposition": {
                "kind": "type_only_replaced",
                "value": {"replacement": "donat.value.contract"}
            }
        },
        {
            "dependency": "dependency.behavior",
            "disposition": {
                "kind": "behavior_only",
                "value": {"reason": "finding.behavior.only"}
            }
        }
    ]);
    collections_owned["embedded_material"] = serde_json::json!([
        {
            "material_id": "embedded.shipped",
            "path": "embedded/shipped.json",
            "sha256":
                "4444444444444444444444444444444444444444444444444444444444444444",
            "disposition": {
                "kind": "shipped",
                "value": {"license": permissive_license.clone()}
            }
        },
        {
            "material_id": "embedded.behavior",
            "path": "embedded/behavior.json",
            "sha256":
                "5555555555555555555555555555555555555555555555555555555555555555",
            "disposition": {
                "kind": "behavior_only",
                "value": {"reason": "finding.embedded.behavior"}
            }
        }
    ]);
    collections_manifest["provenance"][0]["license_id"] = serde_json::json!("review.written.grant");
    let sha512 = "3".repeat(128);
    collections_owned["artifact_hashes"] = serde_json::json!([{
        "artifact_id": "artifact.owned",
        "algorithm": {"kind": "sha512", "value": null},
        "digest": sha512.clone(),
        "path": "bundle.tar"
    }]);
    collections_manifest["provenance"][0]["artifact_hashes"] = serde_json::json!([{
        "artifact_id": "artifact.owned",
        "algorithm": {"kind": "sha512"},
        "digest": sha512,
        "path": "bundle.tar"
    }]);
    collections_provider["reacquisition"] =
        serde_json::json!({"kind": "provider_versioned_artifact_review", "value": null});
    collections_provider["artifact_hashes"][0]["path"] = serde_json::json!("openapi/v2");
    collections_provider["subject"]["value"]["evidence"][0]["source"] = serde_json::json!({
        "kind": "versioned_artifact",
        "value": {
            "url": "https://example.test/openapi/v2",
            "provider_revision": "revision-2"
        }
    });
    collections_provider["subject"]["value"]["evidence"][0]["terms"] = serde_json::json!({
        "kind": "permissive",
        "value": {
            "license": permissive_license,
            "evidence_url": "https://example.test/terms/v2"
        }
    });
    collections_provider["subject"]["value"]["evidence"][0]["facts"][0]["location"] = serde_json::json!({
        "kind": "document_section",
        "value": {
            "path": "openapi/v2",
            "section": "Idempotency"
        }
    });
    collections_manifest["provenance"][1]["artifact_hashes"][0]["path"] =
        serde_json::json!("openapi/v2");

    let mut facts_manifest = baseline_manifest.clone();
    let operation = &mut facts_manifest["operations"][0];
    operation["steps"][0]["headers"][0]["name"] = serde_json::json!("idempotency-key");
    operation["effect"] = serde_json::json!({
        "kind": "provider_idempotent",
        "value": {
            "side_effect_steps": [{
                "step": "request",
                "fixed_binding": {
                    "kind": "header",
                    "value": {"name": "idempotency-key"}
                },
                "scope": "scope.demo",
                "minimum_retention_ms": "86400000",
                "clock_safety_margin_ms": "1000"
            }]
        }
    });
    let policy_use_site = "operation.op.read.step.request.idempotency.clock_safety_margin_ms";
    operation["resolved_fact_values"] = serde_json::json!([
        {
            "use_site": "effect.request.binding",
            "value": {"kind": "string", "value": "Idempotency-Key"}
        },
        {
            "use_site": policy_use_site,
            "value": {"kind": "u64", "value": "1000"}
        }
    ]);
    facts_manifest["provenance"][1]["contract_facts"] = serde_json::json!([{
        "use_site": "effect.request.binding",
        "fact": {
            "kind": "provider_evidence",
            "value": {
                "source_record_id": "source.demo.provider.v1",
                "fact_id": "fact.idempotency"
            }
        }
    }]);
    facts_manifest["provenance"][0]["contract_facts"] = serde_json::json!([{
        "use_site": policy_use_site,
        "fact": {
            "kind": "donat_policy",
            "value": {
                "policy_id": "policy.clock.margin",
                "value": {"kind": "u64", "value": "1000"}
            }
        }
    }]);
    let policies = [(
        DonatPolicyId::literal("policy.clock.margin"),
        TypedValue::Number(CanonicalNumber::U64(1000)),
    )]
    .into_iter()
    .collect();

    let encode = |value: &serde_json::Value| serde_yaml::to_string(value).unwrap().into_bytes();
    vec![
        PublicProvenanceCase {
            name: "baseline",
            manifest: baseline_manifest,
            owned_source: encode(&baseline_owned),
            provider_source: encode(&baseline_provider),
            approved_reviews: vec!["review.demo"],
            policies: BTreeMap::new(),
        },
        PublicProvenanceCase {
            name: "collections-versioned-permissive",
            manifest: collections_manifest,
            owned_source: encode(&collections_owned),
            provider_source: encode(&collections_provider),
            approved_reviews: vec!["review.written.grant"],
            policies: BTreeMap::new(),
        },
        PublicProvenanceCase {
            name: "resolved-provider-and-policy-origins",
            manifest: facts_manifest,
            owned_source: encode(&baseline_owned),
            provider_source: encode(&baseline_provider),
            approved_reviews: vec!["review.demo"],
            policies,
        },
    ]
}

fn unreachable_provenance_branch_rejections() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let (owned, provider) = baseline_public_provenance_sources();
    let mut rejected_license = provider.clone();
    rejected_license["license"] = serde_json::json!({
        "kind": "rejected",
        "value": {"finding": "finding.license.rejected"}
    });
    let mut rejected_terms = provider;
    rejected_terms["subject"]["value"]["evidence"][0]["terms"] = serde_json::json!({
        "kind": "rejected",
        "value": {"finding": "finding.terms.rejected"}
    });
    let mut rejected_dependency = owned.clone();
    rejected_dependency["dependencies"] = serde_json::json!([{
        "dependency": "dependency.rejected",
        "disposition": {
            "kind": "rejected",
            "value": {"finding": "finding.dependency.rejected"}
        }
    }]);
    let mut rejected_embedded = owned;
    rejected_embedded["embedded_material"] = serde_json::json!([{
        "material_id": "embedded.rejected",
        "path": "embedded/rejected.json",
        "sha256":
            "6666666666666666666666666666666666666666666666666666666666666666",
        "disposition": {
            "kind": "rejected",
            "value": {"finding": "finding.embedded.rejected"}
        }
    }]);
    let encode = |value: &serde_json::Value| serde_yaml::to_string(value).unwrap().into_bytes();
    vec![
        (
            "LicenseDecisionMaterialV1::Rejected",
            encode(&rejected_license),
            "source_record_legal_mismatch: license is rejected",
        ),
        (
            "EvidenceTermsMaterialV1::Rejected",
            encode(&rejected_terms),
            "source_record_legal_mismatch: provider evidence terms are rejected",
        ),
        (
            "DependencyDispositionMaterialV1::Rejected",
            encode(&rejected_dependency),
            "source_record_admission_mismatch: rejected or unresolved executable source state",
        ),
        (
            "EmbeddedMaterialDispositionMaterialV1::Rejected",
            encode(&rejected_embedded),
            "source_record_admission_mismatch: rejected or unresolved executable source state",
        ),
    ]
}

#[test]
fn provenance_public_pipeline_has_fixed_compile_valid_material_hashes() {
    let built = public_pipeline_provenance(&semantic_vector_manifest_document(), 11, 13, 17);
    assert_eq!(
        hex(Sha256::digest(&built.bytes).into()),
        "8de0643ba235fd39c51e3c4041844e947b3f1189a469ef5bfd9d98b64855bcc4"
    );
    assert_eq!(
        built.domain_hash,
        "484116d5864609d78b1a71e9a195e380e50578f266919ba5e92fa27e7a45ab25"
    );
}

#[test]
fn positive_provenance_branches_have_fixed_public_pipeline_hashes() {
    let schema = material_branch_schema();
    let generated_universe = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
        .iter()
        .filter(|descriptor| {
            descriptor.case == CanonicalMutationCase::Provenance
                && descriptor.canonical_path.ends_with(".kind")
        })
        .map(|descriptor| descriptor.material_member.to_owned())
        .collect::<BTreeSet<_>>();
    let provenance_specific = CANONICAL_PROVENANCE_LOADER_BRANCH_CANDIDATES
        .iter()
        .map(|branch| (*branch).to_owned())
        .collect::<BTreeSet<_>>();
    let source_generated = CANONICAL_SOURCE_LOADER_BRANCH_CANDIDATES
        .iter()
        .flatten()
        .map(|branch| (*branch).to_owned())
        .collect::<BTreeSet<_>>();
    let source_declared = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
        .iter()
        .filter(|descriptor| {
            descriptor.case == CanonicalMutationCase::SourceRecord
                && descriptor.canonical_path.ends_with(".kind")
        })
        .map(|descriptor| descriptor.material_member.to_owned())
        .collect::<BTreeSet<_>>();
    let source_branch_universe = source_generated
        .union(&source_declared)
        .cloned()
        .collect::<BTreeSet<_>>();
    let typed_generated = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
        .iter()
        .filter(|descriptor| {
            descriptor.case == CanonicalMutationCase::TypedValue
                && descriptor.canonical_path.ends_with(".kind")
        })
        .map(|descriptor| descriptor.material_member.to_owned())
        .collect::<BTreeSet<_>>();
    let reused_source = generated_universe
        .intersection(&source_branch_universe)
        .cloned()
        .collect::<BTreeSet<_>>();
    let reused_typed = generated_universe
        .intersection(&typed_generated)
        .cloned()
        .collect::<BTreeSet<_>>();
    let classified_universe = provenance_specific
        .union(&reused_source)
        .cloned()
        .collect::<BTreeSet<_>>()
        .union(&reused_typed)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        generated_universe, classified_universe,
        "a generated provenance branch was neither provenance-owned nor an explicitly reused source/typed branch"
    );
    let rejected_universe = unreachable_provenance_branch_rejections()
        .into_iter()
        .map(|(branch, _, _)| branch.to_owned())
        .collect::<BTreeSet<_>>();
    assert!(
        rejected_universe.is_subset(&generated_universe),
        "a rejection oracle names a branch outside the generated provenance universe"
    );
    let expected_accepted = generated_universe
        .difference(&rejected_universe)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    let mut hashes = Vec::new();
    for case in positive_provenance_projection_cases() {
        let built = public_pipeline_provenance_with_inputs(PublicProvenanceInput {
            document: &case.manifest,
            owned_source: &case.owned_source,
            provider_source: &case.provider_source,
            approved_reviews: &case.approved_reviews,
            policies: case.policies,
            canonical_schema_epoch: 11,
            classifier_epoch: 13,
            generator_epoch: 17,
        });
        let material = serde_json::from_slice::<serde_json::Value>(&built.bytes).unwrap();
        collect_material_branches_for_definition(
            &schema,
            "ProvenanceMaterialV1",
            &material,
            &mut observed,
        );
        hashes.push((case.name, hex(Sha256::digest(&built.bytes).into())));
    }
    assert_eq!(
        hashes,
        [
            (
                "baseline",
                "8de0643ba235fd39c51e3c4041844e947b3f1189a469ef5bfd9d98b64855bcc4",
            ),
            (
                "collections-versioned-permissive",
                "02099d56511c3c5e114037e89cecd3815d120ded4cd659a5cc27638ebfa63689",
            ),
            (
                "resolved-provider-and-policy-origins",
                "a242b63b43b65541eefb2f6a9178d705110c2b345d4a93e80e62405e4d4ff405",
            ),
        ]
        .into_iter()
        .map(|(name, hash)| (name, hash.to_owned()))
        .collect::<Vec<_>>()
    );
    assert_eq!(
        observed, expected_accepted,
        "public loader/compiler cases and the complete generated provenance branch universe diverged"
    );
}

#[test]
fn unreachable_provenance_branches_have_exact_public_loader_rejections() {
    for (branch, bytes, expected) in unreachable_provenance_branch_rejections() {
        let error = match load_record_bytes(&bytes) {
            Ok(_) => panic!("{branch} unexpectedly passed the public source loader"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), expected, "{branch}");
    }
}

#[test]
fn provenance_public_pipeline_derives_epochs_and_hashes_at_exact_paths() {
    let built = public_pipeline_provenance(&semantic_vector_manifest_document(), 11, 13, 17);
    let material = serde_json::from_slice::<serde_json::Value>(&built.bytes).unwrap();
    assert_eq!(material["canonical_schema_epoch"], serde_json::json!(11));
    assert_eq!(material["classifier_epoch"], serde_json::json!(13));
    assert_eq!(material["generator_epoch"], serde_json::json!(17));
    assert_eq!(
        material["connector"]["semantic_sha256"],
        serde_json::json!(built.semantic_sha256)
    );
    let actual_sources = material["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            (
                source["record_id"].as_str().unwrap().to_owned(),
                source["record_sha256"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual_sources, built.source_sha256);
}

fn generated_provenance_dependency_members(changed_input: &str) -> BTreeSet<String> {
    CANONICAL_PROVENANCE_DERIVED_DEPENDENCIES
        .iter()
        .filter(|dependency| dependency.changed_input == changed_input)
        .map(|dependency| dependency.material_member.to_owned())
        .collect()
}

fn generated_provenance_changed_paths(
    baseline: &serde_json::Value,
    changed_input: &str,
) -> BTreeSet<String> {
    let mut expected = BTreeSet::new();
    collect_generated_epoch_paths_for_definition(
        &material_branch_schema(),
        "ProvenanceMaterialV1",
        baseline,
        "$",
        &generated_provenance_dependency_members(changed_input),
        &mut expected,
    );
    expected
}

#[test]
fn provenance_epoch_deltas_follow_generated_dependencies_exactly() {
    let document = semantic_vector_manifest_document();
    let baseline = serde_json::from_slice::<serde_json::Value>(
        &public_pipeline_provenance(&document, 11, 13, 17).bytes,
    )
    .unwrap();
    for (changed_input, epochs) in [
        ("canonical_schema_epoch", (19, 13, 17)),
        ("classifier_epoch", (11, 19, 17)),
        ("generator_epoch", (11, 13, 19)),
    ] {
        let changed = serde_json::from_slice::<serde_json::Value>(
            &public_pipeline_provenance(&document, epochs.0, epochs.1, epochs.2).bytes,
        )
        .unwrap();
        let mut actual = BTreeSet::new();
        collect_changed_json_paths(&baseline, &changed, "$", &mut actual);
        assert_eq!(
            actual,
            generated_provenance_changed_paths(&baseline, changed_input),
            "{changed_input} escaped its generated dependency paths"
        );
    }
}

#[test]
fn provenance_source_hash_delta_follows_generated_dependency_exactly() {
    let document = semantic_vector_manifest_document();
    let baseline = serde_json::from_slice::<serde_json::Value>(
        &public_pipeline_provenance(&document, 11, 13, 17).bytes,
    )
    .unwrap();

    let mut owned = fixture_document(include_str!("fixtures/donat-owned-record.yaml"));
    owned["admission"]["value"]["operations"] = serde_json::json!(["op.read"]);
    owned["reviewer"] = serde_json::json!("reviewer.changed");
    let mut provider = fixture_document(include_str!("fixtures/provider-contract-record.yaml"));
    provider["reviewer"] = serde_json::json!("reviewer.changed");
    let owned = serde_yaml::to_string(&owned).unwrap();
    let provider = serde_yaml::to_string(&provider).unwrap();
    let changed = public_pipeline_provenance_with_inputs(PublicProvenanceInput {
        document: &document,
        owned_source: owned.as_bytes(),
        provider_source: provider.as_bytes(),
        approved_reviews: &["review.demo"],
        policies: BTreeMap::new(),
        canonical_schema_epoch: 11,
        classifier_epoch: 13,
        generator_epoch: 17,
    });
    let changed = serde_json::from_slice::<serde_json::Value>(&changed.bytes).unwrap();
    let mut actual = BTreeSet::new();
    collect_changed_json_paths(&baseline, &changed, "$", &mut actual);
    assert_eq!(
        actual,
        generated_provenance_changed_paths(&baseline, "accepted_source_record"),
        "source-record hashes escaped their generated dependency paths"
    );
}

#[test]
fn semantic_public_pipeline_matches_fixed_compile_valid_adr_derivative() {
    let document = semantic_vector_manifest_document();
    let bytes = public_pipeline_semantic_bytes(&document, 1);
    let expected = compile_valid_semantic_vector();
    assert_eq!(
        hex(Sha256::digest(&bytes).into()),
        "d05f5740e3467e12e0a3002012562111747d62f4889285dcd00e14893e5ff87e"
    );
    assert_eq!(bytes, expected.as_bytes());
}

#[test]
fn positive_semantic_loader_branches_have_fixed_public_pipeline_hashes() {
    let schema = material_branch_schema();
    let expected_branches = CANONICAL_SEMANTIC_LOADER_BRANCH_CANDIDATES
        .iter()
        .map(|branch| (*branch).to_owned())
        .collect::<BTreeSet<_>>();
    let mut material_branches = BTreeSet::new();
    let mut hashes = Vec::new();

    for (name, document) in positive_semantic_projection_cases() {
        let bytes = public_pipeline_semantic_bytes(&document, 1);
        let material = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        collect_material_branches_for_definition(
            &schema,
            "SemanticMaterialV1",
            &material,
            &mut material_branches,
        );
        hashes.push((name, hex(Sha256::digest(&bytes).into())));
    }

    assert_eq!(
        hashes,
        [
            (
                "baseline",
                "d05f5740e3467e12e0a3002012562111747d62f4889285dcd00e14893e5ff87e",
            ),
            (
                "credentials-and-private-network",
                "3a14f81a984f7434025c640a2c753bedf6da3253157d822ad24c6eca8555d1f9",
            ),
            (
                "request-and-response-shapes",
                "9c9b337f0f6a224218af29d8498948963b9a17fba615c676be34f3db6577f6b4",
            ),
            (
                "idempotency-bindings",
                "aa50dcdb3f34d87edad85d3b837e384c344ec643390fb387cb78ad02dc66345c",
            ),
            (
                "pagination-none",
                "a63d39becd12729718b5c9c31b342cc55d42bfca68bb263e1e66c47676fcddfc",
            ),
            (
                "pagination-offset-limit",
                "34e54e2117521308f3019c5c1aad7aa3a072dbd6f2ddd20b0c320fbb84082b42",
            ),
            (
                "pagination-page-number",
                "d077b834cece5800b8941a71abf8abfe1f58a4b0d61f3e4fbfd1d724001d9b9a",
            ),
            (
                "pagination-link-relation",
                "215f4d99e4c748b375d35d602c850feb78dce61e6a43834a35485e56d48ba2cd",
            ),
            (
                "pagination-processor",
                "66e8838ae43510757000e0fca15a4acd3de0126bac2114ec4fbc8c00ffbc4aa0",
            ),
        ]
        .into_iter()
        .map(|(name, hash)| (name, hash.to_owned()))
        .collect::<Vec<_>>()
    );
    let generated_shared_branches = CANONICAL_PROJECTION_MUTATION_DESCRIPTORS
        .iter()
        .filter(|descriptor| {
            matches!(
                descriptor.case,
                CanonicalMutationCase::ValueContract | CanonicalMutationCase::TypedValue
            ) && descriptor.canonical_path.ends_with(".kind")
        })
        .map(|descriptor| descriptor.material_member.to_owned())
        .collect::<BTreeSet<_>>();
    let observed_semantic_branches = material_branches
        .intersection(&expected_branches)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        observed_semantic_branches, expected_branches,
        "compiler-valid public cases and the generated semantic branch universe diverged"
    );
    let unknown_observed_branches = material_branches
        .difference(&expected_branches)
        .filter(|branch| !generated_shared_branches.contains(*branch))
        .cloned()
        .collect::<BTreeSet<_>>();
    assert!(
        unknown_observed_branches.is_empty(),
        "public semantic traversal observed branches outside the generated semantic/value-contract/typed universes: {unknown_observed_branches:#?}"
    );
}

#[test]
fn semantic_canonical_schema_epoch_changes_only_its_owned_generated_path() {
    let document = semantic_vector_manifest_document();
    let baseline =
        serde_json::from_slice::<serde_json::Value>(&public_pipeline_semantic_bytes(&document, 1))
            .unwrap();
    let changed =
        serde_json::from_slice::<serde_json::Value>(&public_pipeline_semantic_bytes(&document, 2))
            .unwrap();
    assert_eq!(changed["canonical_schema_epoch"], serde_json::json!(2));
    let mut restored = changed;
    restored["canonical_schema_epoch"] = baseline["canonical_schema_epoch"].clone();
    assert_eq!(restored, baseline);
}

#[test]
fn semantic_value_language_epoch_changes_exactly_generated_owned_paths() {
    let document = semantic_vector_manifest_document();
    let changed_document = semantic_manifest_at_value_language_epoch(&document, 2);
    let baseline =
        serde_json::from_slice::<serde_json::Value>(&public_pipeline_semantic_bytes(&document, 1))
            .unwrap();
    let changed = serde_json::from_slice::<serde_json::Value>(&public_pipeline_semantic_bytes(
        &changed_document,
        1,
    ))
    .unwrap();

    let mut actual_paths = BTreeSet::new();
    collect_changed_json_paths(&baseline, &changed, "$", &mut actual_paths);

    let schema = material_branch_schema();
    let dependency_members = generated_epoch_dependency_members();
    let mut expected_paths = BTreeSet::new();
    collect_generated_epoch_paths_for_definition(
        &schema,
        "SemanticMaterialV1",
        &baseline,
        "$",
        &dependency_members,
        &mut expected_paths,
    );
    assert_eq!(
        actual_paths, expected_paths,
        "public epoch propagation diverged from generated semantic ownership"
    );
}

#[test]
fn canonical_projection_full_material_vectors_are_exact() {
    let record = decode_source_record_material(full_vector("source-record").as_bytes()).unwrap();
    let value_contract: ValueContractMaterialV1 =
        decode_value_contract_material(full_vector("value-contract").as_bytes()).unwrap();

    assert_eq!(
        hex(*record_sha256(&record).unwrap().as_bytes()),
        "420f0a4efd63b5d02479658c7686ec3da5ee688a0bc6aaf45bebfb98809fe991"
    );
    assert_eq!(
        hex(*value_contract_sha256(&value_contract).unwrap().as_bytes()),
        "79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a"
    );
}

#[test]
fn selected_header_capability_scope_and_abi_fit_are_exact() {
    let derive = |operation, step, header| {
        selected_response_header(
            ConnectorId::literal("donat.http"),
            OperationId::parse(operation).unwrap(),
            donat_connector_catalog::StableSemver::new(1, 0, 0),
            CompiledStepId::parse(step).unwrap(),
            header,
        )
        .unwrap()
        .capability
    };
    let baseline = derive("get", "request", "x-request-id");
    assert!(baseline != derive("post", "request", "x-request-id"));
    assert!(baseline != derive("get", "response", "x-request-id"));
    assert!(baseline != derive("get", "request", "retry-after"));
    assert!(donat_connector_abi::CapabilityId::parse(baseline.as_str()).is_ok());
}
