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
    let record_id = record.record_id;
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
    let checked = compile_connector_manifest(&manifest, &accepted, &BTreeMap::new()).unwrap();
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
    let checked = compile_connector_manifest(&manifest, &accepted, &BTreeMap::new()).unwrap();
    let semantic = semantic_material(&checked, 1).unwrap();
    let semantic_bytes = canonical_material_bytes(&semantic).unwrap();
    let semantic_source = std::str::from_utf8(&semantic_bytes).unwrap();
    assert!(!semantic_source.contains("source_record_id"));
    assert!(!semantic_source.contains("provider_evidence"));
    assert!(semantic_source.contains(r#""scheme":{"kind":"https","value":null}"#));
    let semantic_hash = semantic_sha256(&semantic).unwrap();
    let provenance = provenance_material(
        &checked,
        &accepted,
        &BTreeMap::new(),
        semantic_hash,
        1,
        1,
        1,
    )
    .unwrap();
    let provenance_hash = provenance_sha256(&provenance).unwrap();
    let changed = provenance_material(
        &checked,
        &accepted,
        &BTreeMap::new(),
        donat_connector_abi::Hash256::new([0xff; 32]),
        1,
        1,
        1,
    )
    .unwrap();
    assert_ne!(
        provenance_hash.as_bytes(),
        provenance_sha256(&changed).unwrap().as_bytes()
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

    let artifact = ArtifactHash {
        artifact_id: ArtifactId::parse("artifact.source").unwrap(),
        algorithm: HashAlgorithm::Sha256,
        digest: "1111111111111111111111111111111111111111111111111111111111111111".to_owned(),
        path: None,
    };
    let mut record = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/donat-owned-record.yaml"),
    )
    .unwrap();
    record.artifact_hashes.push(artifact.clone());
    let record_id = record.record_id;
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
