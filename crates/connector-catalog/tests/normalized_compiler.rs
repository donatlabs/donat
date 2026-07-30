use std::collections::BTreeMap;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::path::Path;

use donat_connector_abi::{
    CompiledStepId, ConnectorId, CredentialFieldId, CredentialSpecId, OperationId, OriginId,
};
use donat_connector_catalog::*;
use donat_value_contract::{
    CanonicalNumber, TypeRef, TypedValue, ValueContractCatalog, ValueContractField, ValueScalar,
    ValueType,
};

fn one() -> NonZeroU32 {
    NonZeroU32::new(1).unwrap()
}

fn one64() -> NonZeroU64 {
    NonZeroU64::new(1).unwrap()
}

fn string_contract() -> ValueContractCatalog {
    ValueContractCatalog {
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
    }
}

fn action(correlations: Vec<ErrorCorrelationBinding>) -> ErrorAction {
    ErrorAction::try_new(
        ConnectorErrorClass::Invariant,
        "connector_invariant",
        "connector invariant",
        RetryAfterPolicy::Never,
        correlations,
    )
    .unwrap()
}

fn fallback(correlation: ErrorCorrelationBinding) -> CompleteErrorFallback {
    CompleteErrorFallback {
        transport: action(vec![correlation]),
        timeout: action(Vec::new()),
        http_429: action(Vec::new()),
        http_5xx: action(Vec::new()),
        authentication: action(Vec::new()),
        validation: action(Vec::new()),
        permanent: action(Vec::new()),
        invariant: action(Vec::new()),
    }
}

fn accepted_catalog() -> AcceptedRecordCatalog {
    let record = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/donat-owned-record.yaml"),
    )
    .unwrap();
    let record_id = record.record_id();
    AcceptedRecordCatalog::build(
        vec![record],
        &[(
            record_id,
            [OperationId::literal("get")].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        &SourceReviewRegistry::default(),
    )
    .unwrap()
}

fn accepted_catalog_with_provider() -> AcceptedRecordCatalog {
    accepted_catalog_with_exact_provider(provider_record_with_retention())
}

fn accepted_catalog_with_inventory() -> (AcceptedRecordCatalog, ConnectorSourceRecord) {
    let owned = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/donat-owned-record.yaml"),
    )
    .unwrap();
    let owned_id = owned.record_id();
    let inventory = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serpapi-npm-record.yaml"),
    )
    .unwrap();
    let catalog = AcceptedRecordCatalog::build(
        vec![owned, inventory.clone()],
        &[(
            owned_id,
            [OperationId::literal("get")].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        &SourceReviewRegistry::default(),
    )
    .unwrap();
    (catalog, inventory)
}

fn accepted_catalog_with_exact_provider(provider: ConnectorSourceRecord) -> AcceptedRecordCatalog {
    let owned = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/donat-owned-record.yaml"),
    )
    .unwrap();
    let owned_id = owned.record_id();
    let mut reviews = SourceReviewRegistry::default();
    reviews.approve_reviewed_use("review.demo").unwrap();
    AcceptedRecordCatalog::build(
        vec![owned, provider],
        &[(
            owned_id,
            [OperationId::literal("get")].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        &reviews,
    )
    .unwrap()
}

fn provider_record_with_retention() -> ConnectorSourceRecord {
    provider_record_with_retention_order(false)
}

fn provider_record_with_retention_order(reverse_evidence: bool) -> ConnectorSourceRecord {
    load_record_bytes(&provider_record_with_retention_bytes(reverse_evidence)).unwrap()
}

fn provider_record_with_retention_bytes(reverse_evidence: bool) -> Vec<u8> {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap()
    .replace(
        "            normalized_value:\n              kind: string\n              value: Idempotency-Key",
        "            normalized_value:\n              kind: string\n              value: Idempotency-Key\n      - source:\n          kind: repository_file\n          value:\n            repository: https://github.com/example/demo\n            commit: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n            path: openapi.json\n        accessed_on: 2026-07-29\n        content_sha256: '3333333333333333333333333333333333333333333333333333333333333333'\n        terms:\n          kind: reviewed_use\n          value:\n            decision_id: review.demo\n            evidence_url: https://example.test/terms/v1\n        facts:\n          - fact_id: fact.retention\n            location:\n              kind: json_pointer\n              value:\n                path: openapi.json\n                pointer: /paths/~1widgets/post/x-retention\n            normalized_value:\n              kind: u64\n              value: '1000'",
    )
    .replace(
        "    path: openapi.json\nlicense:",
        "    path: openapi.json\n  - artifact_id: artifact.openapi.retention\n    algorithm:\n      kind: sha256\n      value: null\n    digest: '3333333333333333333333333333333333333333333333333333333333333333'\n    path: openapi.json\nlicense:",
    )
    .replace(
        "          fact_id: fact.idempotency\ncompatibility:",
        "          fact_id: fact.idempotency\n      - kind: provider_evidence\n        value:\n          source_record_id: source.demo.provider.v1\n          fact_id: fact.retention\ncompatibility:",
    );
    let mut document: serde_yaml::Value = serde_yaml::from_str(&source).unwrap();
    if reverse_evidence {
        document["subject"]["value"]["evidence"]
            .as_sequence_mut()
            .unwrap()
            .reverse();
        document["artifact_hashes"]
            .as_sequence_mut()
            .unwrap()
            .reverse();
    }
    serde_yaml::to_string(&document).unwrap().into_bytes()
}

fn idempotent_manifest_with_origins_on(
    provider_reference_present: bool,
    provider_bindings_on_provider: bool,
) -> (ConnectorManifest, BTreeMap<DonatPolicyId, TypedValue>) {
    let provider = provider_record_with_retention();
    let mut value = manifest();
    let operation = &mut value.operations[0];
    operation.steps[0].query.clear();
    operation.steps[0].request = CompiledRequestShape::RawBytes {
        binding: "query".to_owned(),
    };
    operation.effect = OperationEffect::ProviderIdempotent {
        side_effect_steps: vec![ProviderIdempotentStep {
            step: CompiledStepId::literal("request"),
            fixed_binding: FixedIdempotencyBinding::BodyField {
                pointer: "query".to_owned(),
            },
            scope: "Idempotency-Key".to_owned(),
            minimum_retention_ms: NonZeroU64::new(1_000).unwrap(),
            clock_safety_margin_ms: NonZeroU64::new(1).unwrap(),
        }],
    };
    let scope_site = "operation.get.step.request.idempotency.scope";
    let retention_site = "operation.get.step.request.idempotency.minimum_retention_ms";
    let margin_site = "operation.get.step.request.idempotency.clock_safety_margin_ms";
    operation.resolved_fact_values = vec![
        ResolvedFactValue {
            use_site: scope_site.to_owned(),
            value: TypedValue::String("Idempotency-Key".to_owned()),
        },
        ResolvedFactValue {
            use_site: retention_site.to_owned(),
            value: TypedValue::Number(CanonicalNumber::U64(1_000)),
        },
        ResolvedFactValue {
            use_site: margin_site.to_owned(),
            value: TypedValue::Number(CanonicalNumber::U64(1)),
        },
    ];
    let provider_bindings = vec![
        ResolvedContractFactBinding {
            use_site: scope_site.to_owned(),
            fact: ContractFact::ProviderEvidence {
                source_record_id: provider.record_id(),
                fact_id: ProviderFactId::literal("fact.idempotency"),
            },
        },
        ResolvedContractFactBinding {
            use_site: retention_site.to_owned(),
            fact: ContractFact::ProviderEvidence {
                source_record_id: provider.record_id(),
                fact_id: ProviderFactId::literal("fact.retention"),
            },
        },
    ];
    value.provenance[0]
        .contract_facts
        .push(ResolvedContractFactBinding {
            use_site: margin_site.to_owned(),
            fact: ContractFact::DonatPolicy {
                policy_id: DonatPolicyId::literal("policy.clock.margin"),
                value: TypedValueMaterialV1::u64("1").unwrap(),
            },
        });
    if provider_bindings_on_provider {
        if provider_reference_present {
            value.provenance.push(ManifestProvenanceReference {
                source_record_id: provider.record_id(),
                artifact_hashes: provider.artifact_hashes().to_vec(),
                license_id: "MIT".to_owned(),
                notice_id: NoticeId::literal("notice.demo"),
                contract_facts: provider_bindings,
            });
        }
    } else {
        value.provenance[0].contract_facts.extend(provider_bindings);
        if provider_reference_present {
            value.provenance.push(ManifestProvenanceReference {
                source_record_id: provider.record_id(),
                artifact_hashes: provider.artifact_hashes().to_vec(),
                license_id: "MIT".to_owned(),
                notice_id: NoticeId::literal("notice.demo"),
                contract_facts: Vec::new(),
            });
        }
    }
    (
        value,
        BTreeMap::from([(
            DonatPolicyId::literal("policy.clock.margin"),
            TypedValue::Number(CanonicalNumber::U64(1)),
        )]),
    )
}

fn add_second_idempotent_step(value: &mut ConnectorManifest, reverse_effects: bool) {
    let second_step = CompiledStepId::literal("request-two");
    let operation = &mut value.operations[0];
    operation.operation_processor = Some(VersionedProcessorRef {
        id: donat_connector_abi::ProcessorFamilyId::literal("processor.operation"),
        implementation_revision: 1,
    });
    operation.steps.push(CompiledStepSpec {
        step: second_step,
        method: "GET".to_owned(),
        origin: OriginId::literal("origin.demo"),
        path: "/v1/widgets".to_owned(),
        query: Vec::new(),
        headers: Vec::new(),
        credential_action: Some(CompiledCredentialAction {
            credential: CredentialSpecId::literal("credential.demo"),
        }),
        request: CompiledRequestShape::RawBytes {
            binding: "query".to_owned(),
        },
        success_statuses: vec![StatusRange {
            minimum: 200,
            maximum: 299,
        }],
        response: CompiledResponseShape::Json {
            mappings: Vec::new(),
        },
        selected_response_headers: Vec::new(),
        bounds: StepBounds {
            maximum_headers: one(),
            maximum_header_bytes: one(),
            maximum_url_bytes: one(),
            maximum_request_bytes: one(),
            maximum_response_bytes: one(),
            maximum_json_depth: one(),
            maximum_json_nodes: one(),
            maximum_inline_binary_bytes: one(),
            deadline_ms: one64(),
        },
    });
    let OperationEffect::ProviderIdempotent { side_effect_steps } = &mut operation.effect else {
        unreachable!("idempotent fixture must use provider evidence");
    };
    side_effect_steps.push(ProviderIdempotentStep {
        step: second_step,
        fixed_binding: FixedIdempotencyBinding::BodyField {
            pointer: "query".to_owned(),
        },
        scope: "Idempotency-Key".to_owned(),
        minimum_retention_ms: NonZeroU64::new(1_000).unwrap(),
        clock_safety_margin_ms: NonZeroU64::new(1).unwrap(),
    });
    if reverse_effects {
        side_effect_steps.reverse();
    }
    let provider_id = SourceRecordId::literal("source.demo.provider.v1");
    let provider_reference = value
        .provenance
        .iter_mut()
        .find(|reference| reference.source_record_id == provider_id)
        .unwrap();
    for (suffix, fact, semantic_value) in [
        (
            "scope",
            ProviderFactId::literal("fact.idempotency"),
            TypedValue::String("Idempotency-Key".to_owned()),
        ),
        (
            "minimum_retention_ms",
            ProviderFactId::literal("fact.retention"),
            TypedValue::Number(CanonicalNumber::U64(1_000)),
        ),
    ] {
        let use_site = format!("operation.get.step.request-two.idempotency.{suffix}");
        operation.resolved_fact_values.push(ResolvedFactValue {
            use_site: use_site.clone(),
            value: semantic_value,
        });
        provider_reference
            .contract_facts
            .push(ResolvedContractFactBinding {
                use_site,
                fact: ContractFact::ProviderEvidence {
                    source_record_id: provider_id,
                    fact_id: fact,
                },
            });
    }
    let margin_site =
        "operation.get.step.request-two.idempotency.clock_safety_margin_ms".to_owned();
    operation.resolved_fact_values.push(ResolvedFactValue {
        use_site: margin_site.clone(),
        value: TypedValue::Number(CanonicalNumber::U64(1)),
    });
    value.provenance[0]
        .contract_facts
        .push(ResolvedContractFactBinding {
            use_site: margin_site,
            fact: ContractFact::DonatPolicy {
                policy_id: DonatPolicyId::literal("policy.clock.margin"),
                value: TypedValueMaterialV1::u64("1").unwrap(),
            },
        });
}

fn manifest() -> ConnectorManifest {
    let connector = ConnectorId::literal("demo");
    let connector_version = StableSemver::new(1, 0, 0);
    let operation_id = OperationId::literal("get");
    let operation_version = StableSemver::new(1, 0, 0);
    let step_id = CompiledStepId::literal("request");
    let origin_id = OriginId::literal("origin.demo");
    let credential_id = CredentialSpecId::literal("credential.demo");
    let field_id = CredentialFieldId::literal("credential.api_key");
    let selected = selected_response_header(
        connector,
        operation_id,
        operation_version,
        step_id,
        "x-request-id",
    )
    .unwrap();
    let correlation = ErrorCorrelationBinding {
        canonical_lowercase_header_name: selected.canonical_lowercase_header_name.clone(),
        capability: selected.capability,
        step: step_id,
    };
    let input = string_contract();
    let output = string_contract();
    let input_hash = *value_contract_sha256(&value_contract_material(&input, 1).unwrap())
        .unwrap()
        .as_bytes();
    let output_hash = *value_contract_sha256(&value_contract_material(&output, 1).unwrap())
        .unwrap()
        .as_bytes();
    let origin = || FixedOrigin {
        origin: origin_id,
        scheme: HttpsOnly,
        host: "api.example.test".to_owned(),
        port: NonZeroU16::new(443).unwrap(),
        network_policy: NetworkPolicy::PublicOnly,
    };
    let operation = OperationSpec {
        connector,
        connector_version,
        operation: operation_id,
        operation_version,
        runtime_abi_epoch: 1,
        value_language_epoch: 1,
        input,
        input_contract_sha256: input_hash,
        output,
        output_contract_sha256: output_hash,
        credential: Some(VersionedCredentialReference {
            credential: credential_id,
            version: StableSemver::new(1, 0, 0),
        }),
        origins: vec![origin()],
        steps: vec![CompiledStepSpec {
            step: step_id,
            method: "GET".to_owned(),
            origin: origin_id,
            path: "/v1/widgets".to_owned(),
            query: vec![CompiledQueryBinding {
                name: "q".to_owned(),
                binding: CompiledBinding {
                    field: "query".to_owned(),
                    source: CompiledBindingSource::Input,
                    required: true,
                    default: None,
                    mapping: None,
                },
            }],
            headers: Vec::new(),
            credential_action: Some(CompiledCredentialAction {
                credential: credential_id,
            }),
            request: CompiledRequestShape::None,
            success_statuses: vec![StatusRange {
                minimum: 200,
                maximum: 299,
            }],
            response: CompiledResponseShape::Json {
                mappings: Vec::new(),
            },
            selected_response_headers: vec![selected],
            bounds: StepBounds {
                maximum_headers: one(),
                maximum_header_bytes: one(),
                maximum_url_bytes: one(),
                maximum_request_bytes: one(),
                maximum_response_bytes: one(),
                maximum_json_depth: one(),
                maximum_json_nodes: one(),
                maximum_inline_binary_bytes: one(),
                deadline_ms: one64(),
            },
        }],
        pre_request_transforms: Vec::new(),
        post_response_transforms: Vec::new(),
        operation_processor: None,
        effect: OperationEffect::ReadOnly,
        pagination: PaginationPlan::None,
        error_map: ErrorMap {
            rules: Vec::new(),
            fallback: fallback(correlation),
        },
        capacity: CapacityDefaults {
            maximum_in_flight: one(),
        },
        rate: RateDefaults {
            burst: one(),
            refill_interval_ms: one64(),
        },
        serialization_key_default: None,
        bounds: OperationBounds {
            maximum_calls: one(),
            maximum_pages: one(),
            maximum_items: one(),
            maximum_aggregate_request_bytes: one(),
            maximum_aggregate_response_bytes: one(),
            maximum_output_canonical_bytes: one(),
            maximum_redirects: 0,
            deadline_ms: one64(),
        },
        resolved_fact_values: Vec::new(),
    };
    ConnectorManifest {
        connector,
        connector_version,
        manifest_version: 1,
        runtime_abi_epoch: 1,
        value_language_epoch: 1,
        provider: "provider.demo".to_owned(),
        api_identity: "demo.v1".to_owned(),
        credentials: vec![CredentialSpec {
            credential: credential_id,
            version: StableSemver::new(1, 0, 0),
            fields: vec![CredentialFieldSpec {
                field: field_id,
                required: true,
                secret: SecretClassification::Secret,
                maximum_bytes: one(),
                redaction: RedactionPlan::Omit,
            }],
            auth_plan: AuthPlan::FixedHeaderApiKey {
                field: field_id,
                header: "authorization".to_owned(),
            },
            allowed_origins: vec![origin_id],
            scopes: Vec::new(),
            auth_processor: None,
            credential_test_operation: Some(VersionedOperationReference {
                operation: operation_id,
                version: operation_version,
            }),
            bounds: CredentialBounds {
                maximum_field_bytes: one(),
                maximum_aggregate_bytes: one(),
                maximum_token_bytes: one(),
            },
        }],
        origins: vec![origin()],
        operations: vec![operation],
        triggers: Vec::new(),
        provenance: vec![ManifestProvenanceReference {
            source_record_id: SourceRecordId::parse("source.donat.http.v1").unwrap(),
            artifact_hashes: Vec::new(),
            license_id: "Apache-2.0".to_owned(),
            notice_id: NoticeId::parse("notice.donat").unwrap(),
            contract_facts: Vec::new(),
        }],
    }
}

fn compile(value: &ConnectorManifest) -> Result<(), CatalogError> {
    let accepted = accepted_catalog();
    compile_connector_manifest(value, &accepted, &BTreeMap::new()).map(|_| ())
}

fn normalized_manifest_document() -> serde_json::Value {
    let manifest = manifest();
    let accepted = accepted_catalog();
    let policies = BTreeMap::new();
    let checked = compile_connector_manifest(&manifest, &accepted, &policies).unwrap();
    let semantic = semantic_material(&checked, 1).unwrap();
    let mut semantic = serde_json::to_value(semantic).unwrap();
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
        serde_json::json!([{
            "source_record_id": "source.donat.http.v1",
            "artifact_hashes": [],
            "license_id": "Apache-2.0",
            "notice_id": "notice.donat",
            "contract_facts": []
        }]),
    );
    serde_json::Value::Object(document)
}

fn normalized_manifest_bytes(value: &serde_json::Value) -> Vec<u8> {
    serde_yaml::to_string(value).unwrap().into_bytes()
}

fn public_pipeline_catalog(provider_source: &[u8]) -> AcceptedRecordCatalog {
    let owned_source = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/donat-owned-record.yaml"),
    )
    .unwrap();
    let owned = load_record_bytes(&owned_source).unwrap();
    let owned_id = owned.record_id();
    let provider = load_record_bytes(provider_source).unwrap();
    let mut reviews = SourceReviewRegistry::default();
    reviews.approve_reviewed_use("review.demo").unwrap();
    AcceptedRecordCatalog::build(
        vec![owned, provider],
        &[(
            owned_id,
            [OperationId::literal("get")].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        &reviews,
    )
    .unwrap()
}

fn public_pipeline_manifest_document(provider_idempotent: bool) -> serde_json::Value {
    let mut document = normalized_manifest_document();
    let operation = &mut document["operations"][0];
    operation["resolved_fact_values"] = serde_json::json!([{
        "use_site": "effect.request.binding",
        "value": {"kind": "string", "value": "Idempotency-Key"}
    }]);
    if provider_idempotent {
        let binding = operation["steps"][0]["query"][0]["binding"].clone();
        operation["steps"][0]["query"] = serde_json::json!([]);
        operation["steps"][0]["headers"] = serde_json::json!([{
            "name": "idempotency-key",
            "binding": binding
        }]);
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
    }
    document["provenance"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "source_record_id": "source.demo.provider.v1",
            "artifact_hashes": [{
                "artifact_id": "artifact.openapi",
                "algorithm": {"kind": "sha256"},
                "digest": "1111111111111111111111111111111111111111111111111111111111111111",
                "path": "openapi.json"
            }],
            "license_id": "MIT",
            "notice_id": "notice.demo",
            "contract_facts": [{
                "use_site": "effect.request.binding",
                "fact": {
                    "kind": "provider_evidence",
                    "value": {
                        "source_record_id": "source.demo.provider.v1",
                        "fact_id": "fact.idempotency"
                    }
                }
            }]
        }));
    document
}

fn public_structural_fact_manifest_document() -> serde_json::Value {
    let mut document = public_pipeline_manifest_document(true);
    let scope_site = "operation.get.step.request.idempotency.scope";
    let retention_site = "operation.get.step.request.idempotency.minimum_retention_ms";
    let margin_site = "operation.get.step.request.idempotency.clock_safety_margin_ms";
    let operation = &mut document["operations"][0];
    operation["effect"]["value"]["side_effect_steps"][0]["scope"] =
        serde_json::json!("Idempotency-Key");
    operation["effect"]["value"]["side_effect_steps"][0]["minimum_retention_ms"] =
        serde_json::json!("1000");
    operation["effect"]["value"]["side_effect_steps"][0]["clock_safety_margin_ms"] =
        serde_json::json!("1");
    operation["resolved_fact_values"]
        .as_array_mut()
        .unwrap()
        .extend([
            serde_json::json!({
                "use_site": scope_site,
                "value": {"kind": "string", "value": "Idempotency-Key"}
            }),
            serde_json::json!({
                "use_site": retention_site,
                "value": {"kind": "u64", "value": "1000"}
            }),
            serde_json::json!({
                "use_site": margin_site,
                "value": {"kind": "u64", "value": "1"}
            }),
        ]);

    let provider = &mut document["provenance"][1];
    provider["artifact_hashes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "artifact_id": "artifact.openapi.retention",
            "algorithm": {"kind": "sha256"},
            "digest": "3333333333333333333333333333333333333333333333333333333333333333",
            "path": "openapi.json"
        }));
    provider["contract_facts"].as_array_mut().unwrap().extend([
        serde_json::json!({
            "use_site": scope_site,
            "fact": {
                "kind": "provider_evidence",
                "value": {
                    "source_record_id": "source.demo.provider.v1",
                    "fact_id": "fact.idempotency"
                }
            }
        }),
        serde_json::json!({
            "use_site": retention_site,
            "fact": {
                "kind": "provider_evidence",
                "value": {
                    "source_record_id": "source.demo.provider.v1",
                    "fact_id": "fact.retention"
                }
            }
        }),
    ]);
    document["provenance"][0]["contract_facts"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "use_site": margin_site,
            "fact": {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.clock.margin",
                    "value": {"kind": "u64", "value": "1"}
                }
            }
        }));
    document
}

#[derive(Clone)]
enum PathSegment {
    Key(String),
    Index(usize),
}

fn mutation_paths(
    value: &serde_json::Value,
    path: &mut Vec<PathSegment>,
    member_paths: &mut Vec<Vec<PathSegment>>,
    branch_paths: &mut Vec<Vec<PathSegment>>,
) {
    match value {
        serde_json::Value::Object(values) => {
            if values.contains_key("kind") {
                branch_paths.push(path.clone());
            }
            let map_entries = matches!(
                path.last(),
                Some(PathSegment::Key(name))
                    if matches!(name.as_str(), "roots" | "named_objects")
            ) || matches!(
                path.as_slice(),
                [.., PathSegment::Key(parent), PathSegment::Key(name)]
                    if parent == "named_objects" && name == "fields"
            );
            for (name, value) in values {
                path.push(PathSegment::Key(name.clone()));
                if !map_entries {
                    member_paths.push(path.clone());
                }
                mutation_paths(value, path, member_paths, branch_paths);
                path.pop();
            }
        }
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                path.push(PathSegment::Index(index));
                mutation_paths(value, path, member_paths, branch_paths);
                path.pop();
            }
        }
        _ => {}
    }
}

fn value_at_mut<'a>(
    mut value: &'a mut serde_json::Value,
    path: &[PathSegment],
) -> &'a mut serde_json::Value {
    for segment in path {
        value = match segment {
            PathSegment::Key(name) => &mut value.as_object_mut().unwrap()[name],
            PathSegment::Index(index) => &mut value.as_array_mut().unwrap()[*index],
        };
    }
    value
}

#[test]
fn complete_manifest_compiles_through_recomputed_indexes() {
    let manifest = manifest();
    let accepted = accepted_catalog();
    let policies = BTreeMap::new();
    let checked = compile_connector_manifest(&manifest, &accepted, &policies).unwrap();
    let semantic = semantic_material(&checked, 1).unwrap();
    let semantic_bytes = canonical_material_bytes(&semantic).unwrap();
    let semantic_source = std::str::from_utf8(&semantic_bytes).unwrap();
    assert!(!semantic_source.contains("source_record_id"));
    assert!(!semantic_source.contains("provider_evidence"));
    assert!(semantic_source.contains(r#""scheme":{"kind":"https","value":null}"#));
    let semantic_hash = semantic_sha256(&semantic).unwrap();
    let provenance = provenance_material(&checked, 1, 1, 1).unwrap();
    let provenance_hash = provenance_sha256(&provenance).unwrap();
    assert_ne!(semantic_hash.as_bytes(), &[0; 32]);
    assert_ne!(provenance_hash.as_bytes(), &[0; 32]);
}

#[test]
fn provenance_recomputes_semantic_hash_instead_of_accepting_a_caller_claim() {
    let manifest = manifest();
    let accepted = accepted_catalog();
    let policies = BTreeMap::new();
    let checked = compile_connector_manifest(&manifest, &accepted, &policies).unwrap();
    let semantic = semantic_material(&checked, 1).unwrap();
    let expected_hash = semantic_sha256(&semantic).unwrap();
    let provenance = provenance_material(&checked, 1, 1, 1).unwrap();
    let expected_hash = expected_hash
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let bytes = canonical_material_bytes(&provenance).unwrap();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains(&expected_hash),
        "provenance must commit the recomputed semantic identity"
    );
}

#[test]
fn manifest_fact_origins_are_owned_by_present_provenance_capabilities() {
    let accepted = accepted_catalog_with_provider();
    let (omitted, policies) = idempotent_manifest_with_origins_on(false, false);
    assert_eq!(
        compile_connector_manifest(&omitted, &accepted, &policies)
            .err()
            .unwrap()
            .code(),
        "catalog_fact_origin_unresolved"
    );

    let (foreign, policies) = idempotent_manifest_with_origins_on(true, false);
    assert_eq!(
        compile_connector_manifest(&foreign, &accepted, &policies)
            .err()
            .unwrap()
            .code(),
        "catalog_fact_binding_mismatch"
    );

    let (valid, policies) = idempotent_manifest_with_origins_on(true, true);
    compile_connector_manifest(&valid, &accepted, &policies).unwrap();
}

#[test]
fn generic_provider_facts_compile_for_read_only_operations() {
    let provider = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap();
    let provider_id = provider.record_id();
    let accepted = accepted_catalog_with_exact_provider(provider.clone());
    let mut value = manifest();
    value.operations[0]
        .resolved_fact_values
        .push(ResolvedFactValue {
            use_site: "effect.request.binding".to_owned(),
            value: TypedValue::String("Idempotency-Key".to_owned()),
        });
    value.provenance.push(ManifestProvenanceReference {
        source_record_id: provider_id,
        artifact_hashes: provider.artifact_hashes().to_vec(),
        license_id: "MIT".to_owned(),
        notice_id: NoticeId::literal("notice.demo"),
        contract_facts: vec![ResolvedContractFactBinding {
            use_site: "effect.request.binding".to_owned(),
            fact: ContractFact::ProviderEvidence {
                source_record_id: provider_id,
                fact_id: ProviderFactId::literal("fact.idempotency"),
            },
        }],
    });

    let policies = BTreeMap::new();
    let checked = compile_connector_manifest(&value, &accepted, &policies).unwrap();
    let semantic = semantic_material(&checked, 1).unwrap();
    let provenance = provenance_material(&checked, 1, 1, 1).unwrap();
    assert!(
        std::str::from_utf8(&canonical_material_bytes(&semantic).unwrap())
            .unwrap()
            .contains(r#""use_site":"effect.request.binding""#)
    );
    assert!(
        std::str::from_utf8(&canonical_material_bytes(&provenance).unwrap())
            .unwrap()
            .contains(r#""use_site":"effect.request.binding""#)
    );
}

#[test]
fn provider_idempotent_generic_fact_compiles_through_public_pipeline() {
    let provider_source = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap();
    let accepted = public_pipeline_catalog(&provider_source);
    let manifest_bytes = normalized_manifest_bytes(&public_pipeline_manifest_document(true));
    let manifest = load_connector_manifest_bytes(&manifest_bytes).unwrap();
    let policies = BTreeMap::new();
    let checked = compile_connector_manifest(&manifest, &accepted, &policies).unwrap();
    let semantic = serde_json::from_slice::<serde_json::Value>(
        &canonical_material_bytes(&semantic_material(&checked, 1).unwrap()).unwrap(),
    );
    let semantic = semantic.unwrap();
    assert_eq!(
        semantic["operations"][0]["resolved_fact_values"],
        serde_json::json!([{
            "use_site": "effect.request.binding",
            "value": {"kind": "string", "value": "Idempotency-Key"}
        }])
    );

    let provenance = serde_json::from_slice::<serde_json::Value>(
        &canonical_material_bytes(&provenance_material(&checked, 1, 1, 1).unwrap()).unwrap(),
    )
    .unwrap();
    let provider_reference = provenance["manifest_references"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reference| reference["source_record_id"] == "source.demo.provider.v1")
        .unwrap();
    assert_eq!(
        provider_reference["contract_fact_origins"],
        serde_json::json!([{
            "use_site": "effect.request.binding",
            "origin": {
                "kind": "provider_evidence",
                "value": {
                    "source_record_id": "source.demo.provider.v1",
                    "fact_id": "fact.idempotency",
                    "artifact_content_sha256":
                        "1111111111111111111111111111111111111111111111111111111111111111",
                    "location": {
                        "kind": "json_pointer",
                        "value": {
                            "path": "openapi.json",
                            "pointer": "/paths/~1widgets/post"
                        }
                    }
                }
            }
        }])
    );
}

#[test]
fn read_only_provider_fact_compiles_through_public_pipeline() {
    let provider_source = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap();
    let accepted = public_pipeline_catalog(&provider_source);
    let manifest_bytes = normalized_manifest_bytes(&public_pipeline_manifest_document(false));
    let manifest = load_connector_manifest_bytes(&manifest_bytes).unwrap();
    let policies = BTreeMap::new();
    let checked = compile_connector_manifest(&manifest, &accepted, &policies).unwrap();
    let semantic = canonical_material_bytes(&semantic_material(&checked, 1).unwrap()).unwrap();
    let provenance =
        canonical_material_bytes(&provenance_material(&checked, 1, 1, 1).unwrap()).unwrap();

    assert!(
        std::str::from_utf8(&semantic)
            .unwrap()
            .contains(r#""use_site":"effect.request.binding""#)
    );
    assert!(
        std::str::from_utf8(&provenance)
            .unwrap()
            .contains(r#""use_site":"effect.request.binding""#)
    );
}

#[test]
fn optional_structural_facts_compile_through_public_pipeline() {
    let provider_source = provider_record_with_retention_bytes(false);
    let accepted = public_pipeline_catalog(&provider_source);
    let manifest_bytes = normalized_manifest_bytes(&public_structural_fact_manifest_document());
    let manifest = load_connector_manifest_bytes(&manifest_bytes).unwrap();
    let policies = BTreeMap::from([(
        DonatPolicyId::literal("policy.clock.margin"),
        TypedValue::Number(CanonicalNumber::U64(1)),
    )]);
    let checked = compile_connector_manifest(&manifest, &accepted, &policies).unwrap();
    let semantic = canonical_material_bytes(&semantic_material(&checked, 1).unwrap()).unwrap();
    let provenance =
        canonical_material_bytes(&provenance_material(&checked, 1, 1, 1).unwrap()).unwrap();

    for use_site in [
        "effect.request.binding",
        "operation.get.step.request.idempotency.scope",
        "operation.get.step.request.idempotency.minimum_retention_ms",
        "operation.get.step.request.idempotency.clock_safety_margin_ms",
    ] {
        let needle = format!(r#""use_site":"{use_site}""#);
        assert!(std::str::from_utf8(&semantic).unwrap().contains(&needle));
        assert!(std::str::from_utf8(&provenance).unwrap().contains(&needle));
    }
}

#[test]
fn public_loader_defers_fact_use_site_cardinality_to_the_shared_validator() {
    let provider_source = provider_record_with_retention_bytes(false);
    let accepted = public_pipeline_catalog(&provider_source);
    let base_policies = BTreeMap::from([(
        DonatPolicyId::literal("policy.clock.margin"),
        TypedValue::Number(CanonicalNumber::U64(1)),
    )]);
    let assert_use_site_mismatch = |document: &serde_json::Value| {
        let bytes = normalized_manifest_bytes(document);
        let manifest = load_connector_manifest_bytes(&bytes)
            .expect("fact use-site cardinality is a compiler validation concern");
        assert_eq!(
            compile_connector_manifest(&manifest, &accepted, &base_policies)
                .err()
                .expect("invalid fact use-site cardinality must fail compilation")
                .code(),
            "catalog_fact_use_site_mismatch"
        );
    };

    let mut duplicate_semantic = public_structural_fact_manifest_document();
    let duplicate_binding = duplicate_semantic["operations"][0]["resolved_fact_values"][0].clone();
    duplicate_semantic["operations"][0]["resolved_fact_values"]
        .as_array_mut()
        .unwrap()
        .push(duplicate_binding);
    assert_use_site_mismatch(&duplicate_semantic);

    let mut duplicate_origin = public_structural_fact_manifest_document();
    let duplicate_binding = duplicate_origin["provenance"][1]["contract_facts"][0].clone();
    duplicate_origin["provenance"][1]["contract_facts"]
        .as_array_mut()
        .unwrap()
        .push(duplicate_binding);
    assert_use_site_mismatch(&duplicate_origin);

    let mut omitted_origin = public_structural_fact_manifest_document();
    omitted_origin["provenance"][1]["contract_facts"]
        .as_array_mut()
        .unwrap()
        .retain(|binding| binding["use_site"] != "effect.request.binding");
    assert_use_site_mismatch(&omitted_origin);

    let mut extra_origin = public_structural_fact_manifest_document();
    extra_origin["provenance"][1]["contract_facts"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "use_site": "effect.extra.binding",
            "fact": {
                "kind": "provider_evidence",
                "value": {
                    "source_record_id": "source.demo.provider.v1",
                    "fact_id": "fact.idempotency"
                }
            }
        }));
    assert_use_site_mismatch(&extra_origin);
}

#[test]
fn public_pipeline_fact_failures_keep_their_owned_codes() {
    let provider_source = provider_record_with_retention_bytes(false);
    let accepted = public_pipeline_catalog(&provider_source);
    let base_policies = BTreeMap::from([(
        DonatPolicyId::literal("policy.clock.margin"),
        TypedValue::Number(CanonicalNumber::U64(1)),
    )]);
    let compile_error = |document: &serde_json::Value,
                         policies: &BTreeMap<DonatPolicyId, TypedValue>| {
        let bytes = normalized_manifest_bytes(document);
        match load_connector_manifest_bytes(&bytes) {
            Ok(manifest) => compile_connector_manifest(&manifest, &accepted, policies)
                .err()
                .expect("fact mutation must fail closed")
                .code(),
            Err(error) => error.code(),
        }
    };

    let mut unequal = public_structural_fact_manifest_document();
    unequal["operations"][0]["resolved_fact_values"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|binding| binding["use_site"] == "operation.get.step.request.idempotency.scope")
        .unwrap()["value"]["value"] = serde_json::json!("scope.foreign");
    assert_eq!(
        compile_error(&unequal, &base_policies),
        "catalog_operation_effect_incomplete"
    );

    let margin_site = "operation.get.step.request.idempotency.clock_safety_margin_ms";
    let mut provider_for_policy = public_structural_fact_manifest_document();
    provider_for_policy["provenance"][0]["contract_facts"]
        .as_array_mut()
        .unwrap()
        .retain(|binding| binding["use_site"] != margin_site);
    provider_for_policy["provenance"][1]["contract_facts"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "use_site": margin_site,
            "fact": {
                "kind": "provider_evidence",
                "value": {
                    "source_record_id": "source.demo.provider.v1",
                    "fact_id": "fact.retention"
                }
            }
        }));
    assert_eq!(
        compile_error(&provider_for_policy, &base_policies),
        "catalog_fact_binding_mismatch"
    );

    let scope_site = "operation.get.step.request.idempotency.scope";
    let mut policy_for_provider = public_structural_fact_manifest_document();
    policy_for_provider["provenance"][1]["contract_facts"]
        .as_array_mut()
        .unwrap()
        .retain(|binding| binding["use_site"] != scope_site);
    policy_for_provider["provenance"][0]["contract_facts"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "use_site": scope_site,
            "fact": {
                "kind": "donat_policy",
                "value": {
                    "policy_id": "policy.scope",
                    "value": {"kind": "string", "value": "Idempotency-Key"}
                }
            }
        }));
    let mut policy_for_provider_registry = base_policies.clone();
    policy_for_provider_registry.insert(
        DonatPolicyId::literal("policy.scope"),
        TypedValue::String("Idempotency-Key".to_owned()),
    );
    assert_eq!(
        compile_error(&policy_for_provider, &policy_for_provider_registry),
        "catalog_fact_binding_mismatch"
    );

    let mut unequal_provider_value = public_structural_fact_manifest_document();
    unequal_provider_value["operations"][0]["resolved_fact_values"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|binding| binding["use_site"] == "effect.request.binding")
        .unwrap()["value"]["value"] = serde_json::json!("Different-Key");
    assert_eq!(
        compile_error(&unequal_provider_value, &base_policies),
        "catalog_fact_binding_mismatch"
    );
}

#[test]
fn evidence_provenance_may_be_empty_only_when_semantic_facts_are_empty() {
    let provider = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap();
    let provider_id = provider.record_id();
    let accepted = accepted_catalog_with_exact_provider(provider.clone());
    let empty_reference = || ManifestProvenanceReference {
        source_record_id: provider_id,
        artifact_hashes: provider.artifact_hashes().to_vec(),
        license_id: "MIT".to_owned(),
        notice_id: NoticeId::literal("notice.demo"),
        contract_facts: Vec::new(),
    };

    let mut empty = manifest();
    empty.provenance.push(empty_reference());
    compile_connector_manifest(&empty, &accepted, &BTreeMap::new()).unwrap();

    let mut required = manifest();
    required.operations[0]
        .resolved_fact_values
        .push(ResolvedFactValue {
            use_site: "effect.request.binding".to_owned(),
            value: TypedValue::String("Idempotency-Key".to_owned()),
        });
    required.provenance.push(empty_reference());
    assert_eq!(
        compile_connector_manifest(&required, &accepted, &BTreeMap::new())
            .err()
            .unwrap()
            .code(),
        "catalog_fact_use_site_mismatch"
    );
}

#[test]
fn inventory_only_record_cannot_enter_manifest_provenance() {
    let (accepted, inventory) = accepted_catalog_with_inventory();
    let mut value = manifest();
    value.provenance.push(ManifestProvenanceReference {
        source_record_id: inventory.record_id(),
        artifact_hashes: inventory.artifact_hashes().to_vec(),
        license_id: "MIT".to_owned(),
        notice_id: NoticeId::literal("notice.serpapi"),
        contract_facts: Vec::new(),
    });
    assert_eq!(
        compile_connector_manifest(&value, &accepted, &BTreeMap::new())
            .err()
            .unwrap()
            .code(),
        "catalog_manifest_reference_mismatch"
    );
}

#[test]
fn compiler_rejects_duplicate_steps_with_the_operation_oracle() {
    let accepted = accepted_catalog();
    let mut document = normalized_manifest_document();
    document["operations"][0]["operation_processor"] = serde_json::json!({
        "id": "processor.duplicate.guard",
        "implementation_revision": 1
    });
    let mut duplicate = document["operations"][0]["steps"][0].clone();
    duplicate["query"] = serde_json::json!([]);
    duplicate["headers"] = serde_json::json!([]);
    duplicate["credential_action"] = serde_json::Value::Null;
    duplicate["request"] = serde_json::json!({"kind": "none", "value": null});
    duplicate["selected_response_headers"] = serde_json::json!([]);
    document["operations"][0]["steps"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let loaded = load_connector_manifest_bytes(&normalized_manifest_bytes(&document)).unwrap();
    assert_eq!(
        compile_connector_manifest(&loaded, &accepted, &BTreeMap::new())
            .err()
            .unwrap()
            .code(),
        "catalog_operation_duplicate_step"
    );
}

#[test]
fn invalid_value_contract_definitions_never_enter_checked_material() {
    let accepted = accepted_catalog();
    let invalid_types = [
        ValueType::Ref {
            name: "Missing".to_owned(),
        },
        ValueType::Enum {
            name: "Duplicate".to_owned(),
            values: vec!["same".to_owned(), "same".to_owned()],
        },
        ValueType::Enum {
            name: "Empty".to_owned(),
            values: Vec::new(),
        },
        ValueType::Object {
            fields: [(
                "nested".to_owned(),
                ValueContractField {
                    required: true,
                    type_ref: TypeRef {
                        nullable: false,
                        value_type: ValueType::Ref {
                            name: "MissingNested".to_owned(),
                        },
                    },
                },
            )]
            .into_iter()
            .collect(),
        },
    ];
    for invalid_type in invalid_types {
        let mut value = manifest();
        value.operations[0]
            .input
            .roots
            .get_mut("query")
            .unwrap()
            .type_ref = TypeRef {
            nullable: false,
            value_type: invalid_type,
        };
        value.operations[0].input_contract_sha256 = *value_contract_sha256(
            &value_contract_material(&value.operations[0].input, 1).unwrap(),
        )
        .unwrap()
        .as_bytes();
        assert_eq!(
            compile_connector_manifest(&value, &accepted, &BTreeMap::new())
                .err()
                .unwrap()
                .code(),
            "catalog_contract_hash_mismatch"
        );
    }
}

#[test]
fn set_like_status_ranges_have_canonical_permutation_identity() {
    let accepted = accepted_catalog();
    let mut left = normalized_manifest_document();
    left["operations"][0]["steps"][0]["success_statuses"] = serde_json::json!([
        {"minimum": 200, "maximum": 204},
        {"minimum": 205, "maximum": 299}
    ]);
    let mut right = left.clone();
    right["operations"][0]["steps"][0]["success_statuses"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let left = load_connector_manifest_bytes(&normalized_manifest_bytes(&left)).unwrap();
    let right = load_connector_manifest_bytes(&normalized_manifest_bytes(&right)).unwrap();
    let policies = BTreeMap::new();
    let left = compile_connector_manifest(&left, &accepted, &policies).unwrap();
    let right = compile_connector_manifest(&right, &accepted, &policies).unwrap();
    let left = semantic_material(&left, 1).unwrap();
    let right = semantic_material(&right, 1).unwrap();
    assert_eq!(
        canonical_material_bytes(&left).unwrap(),
        canonical_material_bytes(&right).unwrap()
    );
    assert_eq!(
        semantic_sha256(&left).unwrap().as_bytes(),
        semantic_sha256(&right).unwrap().as_bytes()
    );
}

#[test]
fn set_like_side_effect_steps_have_canonical_permutation_identity() {
    let accepted = accepted_catalog_with_provider();
    let (mut left, policies) = idempotent_manifest_with_origins_on(true, true);
    add_second_idempotent_step(&mut left, false);
    let (mut right, _) = idempotent_manifest_with_origins_on(true, true);
    add_second_idempotent_step(&mut right, true);

    let left = compile_connector_manifest(&left, &accepted, &policies).unwrap();
    let right = compile_connector_manifest(&right, &accepted, &policies).unwrap();
    let left = semantic_material(&left, 1).unwrap();
    let right = semantic_material(&right, 1).unwrap();
    assert_eq!(
        canonical_material_bytes(&left).unwrap(),
        canonical_material_bytes(&right).unwrap()
    );
    assert_eq!(
        semantic_sha256(&left).unwrap().as_bytes(),
        semantic_sha256(&right).unwrap().as_bytes()
    );
}

#[test]
fn set_like_provider_evidence_has_canonical_source_and_provenance_identity() {
    let left_record = provider_record_with_retention_order(false);
    let right_record = provider_record_with_retention_order(true);
    let left_source = source_record_material(&left_record).unwrap();
    let right_source = source_record_material(&right_record).unwrap();
    assert_eq!(
        canonical_material_bytes(&left_source).unwrap(),
        canonical_material_bytes(&right_source).unwrap()
    );
    assert_eq!(
        record_sha256(&left_source).unwrap().as_bytes(),
        record_sha256(&right_source).unwrap().as_bytes()
    );

    let left_catalog = accepted_catalog_with_exact_provider(left_record);
    let right_catalog = accepted_catalog_with_exact_provider(right_record);
    let (manifest, policies) = idempotent_manifest_with_origins_on(true, true);
    let left_checked = compile_connector_manifest(&manifest, &left_catalog, &policies).unwrap();
    let right_checked = compile_connector_manifest(&manifest, &right_catalog, &policies).unwrap();
    let left = provenance_material(&left_checked, 1, 1, 1).unwrap();
    let right = provenance_material(&right_checked, 1, 1, 1).unwrap();
    assert_eq!(
        canonical_material_bytes(&left).unwrap(),
        canonical_material_bytes(&right).unwrap()
    );
    assert_eq!(
        provenance_sha256(&left).unwrap().as_bytes(),
        provenance_sha256(&right).unwrap().as_bytes()
    );
}

#[test]
fn generated_manifest_member_and_branch_mutations_reach_the_strict_loader() {
    let document = normalized_manifest_document();
    let mut member_paths = Vec::new();
    let mut branch_paths = Vec::new();
    mutation_paths(
        &document,
        &mut Vec::new(),
        &mut member_paths,
        &mut branch_paths,
    );

    for path in member_paths {
        let (last, parent) = path.split_last().unwrap();
        let PathSegment::Key(name) = last else {
            unreachable!("member mutation paths terminate in object keys");
        };
        let mut changed = document.clone();
        value_at_mut(&mut changed, parent)
            .as_object_mut()
            .unwrap()
            .remove(name);
        let error = load_connector_manifest_bytes(&normalized_manifest_bytes(&changed))
            .err()
            .unwrap_or_else(|| panic!("missing member was accepted: {name}"));
        assert_eq!(error.code(), "catalog_manifest_incomplete", "{name}");
    }

    for path in branch_paths {
        let mut changed = document.clone();
        value_at_mut(&mut changed, &path)["kind"] = serde_json::json!("unknown_branch");
        assert_eq!(
            load_connector_manifest_bytes(&normalized_manifest_bytes(&changed))
                .err()
                .unwrap()
                .code(),
            "catalog_manifest_incomplete"
        );
    }
}

#[test]
fn strict_loader_reaches_the_real_compiler_and_denies_recursive_drift() {
    let accepted = accepted_catalog();
    let document = normalized_manifest_document();
    let loaded = load_connector_manifest_bytes(&normalized_manifest_bytes(&document)).unwrap();
    compile_connector_manifest(&loaded, &accepted, &BTreeMap::new()).unwrap();

    let mut unknown = document.clone();
    unknown["operations"][0]["steps"][0]["bounds"]["surprise"] = serde_json::json!(1);
    assert_eq!(
        load_connector_manifest_bytes(&normalized_manifest_bytes(&unknown))
            .err()
            .unwrap()
            .code(),
        "catalog_manifest_incomplete"
    );

    let mut missing = document.clone();
    missing["operations"][0]
        .as_object_mut()
        .unwrap()
        .remove("error_map");
    assert_eq!(
        load_connector_manifest_bytes(&normalized_manifest_bytes(&missing))
            .err()
            .unwrap()
            .code(),
        "catalog_manifest_incomplete"
    );

    let mut unknown_branch = document.clone();
    unknown_branch["operations"][0]["pagination"]["kind"] = serde_json::json!("unknown");
    assert_eq!(
        load_connector_manifest_bytes(&normalized_manifest_bytes(&unknown_branch))
            .err()
            .unwrap()
            .code(),
        "catalog_manifest_incomplete"
    );

    let mut invalid = document;
    invalid["operations"][0]["steps"][0]["method"] = serde_json::json!("get");
    let loaded = load_connector_manifest_bytes(&normalized_manifest_bytes(&invalid)).unwrap();
    assert_eq!(
        compile_connector_manifest(&loaded, &accepted, &BTreeMap::new())
            .err()
            .unwrap()
            .code(),
        "catalog_manifest_invalid_primitive"
    );
}

#[test]
fn normalized_compiler_rejects_every_closed_identity_boundary() {
    let mut value = manifest();
    value.operations[0].steps[0].method = "get".to_owned();
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_manifest_invalid_primitive"
    );

    let mut value = manifest();
    value.credentials[0].fields.clear();
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_manifest_incomplete"
    );

    let mut value = manifest();
    value.credentials[0].auth_plan = AuthPlan::Bearer {
        token: CredentialFieldId::literal("credential.missing"),
    };
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_credential_incomplete"
    );

    let mut value = manifest();
    value.operations[0].input_contract_sha256 = [0; 32];
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_contract_hash_mismatch"
    );

    let mut value = manifest();
    value.operations[0].steps[0].selected_response_headers[0] = selected_response_header(
        value.connector,
        OperationId::literal("foreign"),
        value.operations[0].operation_version,
        value.operations[0].steps[0].step,
        "x-request-id",
    )
    .unwrap();
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_selected_header_invalid"
    );

    let mut value = manifest();
    value.operations[0]
        .error_map
        .fallback
        .transport
        .correlations[0]
        .step = CompiledStepId::literal("foreign");
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_error_map_invalid"
    );

    let mut value = manifest();
    value.operations[0].effect = OperationEffect::ProviderIdempotent {
        side_effect_steps: Vec::new(),
    };
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_operation_effect_incomplete"
    );

    let mut value = manifest();
    value.connector = ConnectorId::literal("foreign");
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_manifest_identity_mismatch"
    );

    let mut value = manifest();
    value.provenance[0].license_id = "MIT".to_owned();
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_manifest_reference_mismatch"
    );

    let mut value = manifest();
    value.credentials[0]
        .credential_test_operation
        .as_mut()
        .unwrap()
        .version = StableSemver::new(2, 0, 0);
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_credential_incomplete"
    );

    let mut value = manifest();
    value.operations[0].origins[0].host = "other.example.test".to_owned();
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_credential_incomplete"
    );

    let mut value = manifest();
    value.operations[0].steps[0].success_statuses[0] = StatusRange {
        minimum: 300,
        maximum: 200,
    };
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_manifest_invalid_primitive"
    );

    let trigger = || TriggerSpec::Poll {
        connector: ConnectorId::literal("demo"),
        connector_version: StableSemver::new(1, 0, 0),
        trigger: donat_connector_abi::TriggerId::literal("trigger.poll"),
        trigger_version: StableSemver::new(1, 0, 0),
        event_version: StableSemver::new(1, 0, 0),
        runtime_abi_epoch: 1,
        checkpoint: string_contract(),
        processor: VersionedProcessorRef {
            id: donat_connector_abi::ProcessorFamilyId::literal("processor.poll"),
            implementation_revision: 1,
        },
        event_type: string_contract(),
        per_poll_event_limit: one(),
        bounds: OperationBounds {
            maximum_calls: one(),
            maximum_pages: one(),
            maximum_items: one(),
            maximum_aggregate_request_bytes: one(),
            maximum_aggregate_response_bytes: one(),
            maximum_output_canonical_bytes: one(),
            maximum_redirects: 0,
            deadline_ms: one64(),
        },
    };
    let mut value = manifest();
    value.triggers = vec![trigger(), trigger()];
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_manifest_identity_mismatch"
    );

    let artifact = ArtifactHash::try_new(
        "artifact.source",
        HashAlgorithm::Sha256,
        "1111111111111111111111111111111111111111111111111111111111111111",
        None,
    )
    .unwrap();
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/donat-owned-record.yaml"),
    )
    .unwrap()
    .replace(
        "artifact_hashes: []",
        "artifact_hashes:\n  - artifact_id: artifact.source\n    algorithm:\n      kind: sha256\n      value: null\n    digest: '1111111111111111111111111111111111111111111111111111111111111111'\n    path: null",
    );
    let record = load_record_bytes(source.as_bytes()).unwrap();
    let record_id = record.record_id();
    let accepted = AcceptedRecordCatalog::build(
        vec![record],
        &[(
            record_id,
            [OperationId::literal("get")].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
        &SourceReviewRegistry::default(),
    )
    .unwrap();
    let mut value = manifest();
    value.provenance[0].artifact_hashes = vec![artifact.clone(), artifact];
    assert_eq!(
        compile_connector_manifest(&value, &accepted, &BTreeMap::new())
            .err()
            .unwrap()
            .code(),
        "catalog_manifest_reference_mismatch"
    );

    let mut value = manifest();
    value.operations[0].steps[0].query.clear();
    value.operations[0].steps[0]
        .headers
        .push(CompiledHeaderBinding {
            name: "idempotency-key".to_owned(),
            binding: CompiledBinding {
                field: "query".to_owned(),
                source: CompiledBindingSource::Input,
                required: true,
                default: None,
                mapping: None,
            },
        });
    value.operations[0].effect = OperationEffect::ProviderIdempotent {
        side_effect_steps: vec![ProviderIdempotentStep {
            step: CompiledStepId::literal("request"),
            fixed_binding: FixedIdempotencyBinding::Header {
                name: "idempotency-key".to_owned(),
            },
            scope: "scope.demo".to_owned(),
            minimum_retention_ms: NonZeroU64::new(86_400_000).unwrap(),
            clock_safety_margin_ms: NonZeroU64::new(1_000).unwrap(),
        }],
    };
    value.operations[0].resolved_fact_values = vec![
        ResolvedFactValue {
            use_site: "operation.get.step.request.idempotency.scope".to_owned(),
            value: TypedValue::String("scope.foreign".to_owned()),
        },
        ResolvedFactValue {
            use_site: "operation.get.step.request.idempotency.minimum_retention_ms".to_owned(),
            value: TypedValue::Number(CanonicalNumber::U64(1)),
        },
        ResolvedFactValue {
            use_site: "operation.get.step.request.idempotency.clock_safety_margin_ms".to_owned(),
            value: TypedValue::Number(CanonicalNumber::U64(1)),
        },
    ];
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_operation_effect_incomplete"
    );

    let mut value = manifest();
    value.operations[0].operation_processor = Some(VersionedProcessorRef {
        id: donat_connector_abi::ProcessorFamilyId::literal("processor.operation"),
        implementation_revision: 0,
    });
    assert_eq!(
        compile(&value).unwrap_err().code(),
        "catalog_manifest_invalid_primitive"
    );
}
