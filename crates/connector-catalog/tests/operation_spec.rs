use std::collections::BTreeMap;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};

use donat_connector_abi::{CompiledStepId, ConnectorId, OperationId, OriginId, ProcessorFamilyId};
use donat_connector_catalog::*;
use donat_value_contract::ValueContractCatalog;

fn one() -> NonZeroU32 {
    NonZeroU32::new(1).unwrap()
}

fn one64() -> NonZeroU64 {
    NonZeroU64::new(1).unwrap()
}

fn bounds() -> OperationBounds {
    OperationBounds {
        maximum_calls: one(),
        maximum_pages: one(),
        maximum_items: one(),
        maximum_aggregate_request_bytes: one(),
        maximum_aggregate_response_bytes: one(),
        maximum_output_canonical_bytes: one(),
        maximum_redirects: 0,
        deadline_ms: one64(),
    }
}

fn step(id: &'static str) -> CompiledStepSpec {
    CompiledStepSpec {
        step: CompiledStepId::literal(id),
        method: "POST".to_owned(),
        origin: OriginId::literal("origin.demo"),
        path: "/widgets".to_owned(),
        query: Vec::new(),
        headers: Vec::new(),
        credential_action: None,
        request: CompiledRequestShape::None,
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
    }
}

fn fallback() -> CompleteErrorFallback {
    let action = || {
        ErrorAction::try_new(
            ConnectorErrorClass::Invariant,
            "connector_invariant",
            "connector invariant",
            RetryAfterPolicy::Never,
            Vec::new(),
        )
        .unwrap()
    };
    CompleteErrorFallback {
        transport: action(),
        timeout: action(),
        http_429: action(),
        http_5xx: action(),
        authentication: action(),
        validation: action(),
        permanent: action(),
        invariant: action(),
    }
}

fn operation(effect: OperationEffect, steps: Vec<CompiledStepSpec>) -> OperationSpec {
    OperationSpec {
        connector: ConnectorId::literal("demo"),
        connector_version: StableSemver::new(1, 0, 0),
        operation: OperationId::literal("op.read"),
        operation_version: StableSemver::new(1, 0, 0),
        runtime_abi_epoch: 1,
        value_language_epoch: 1,
        input: ValueContractCatalog {
            roots: BTreeMap::new(),
            named_objects: BTreeMap::new(),
        },
        input_contract_sha256: [0; 32],
        output: ValueContractCatalog {
            roots: BTreeMap::new(),
            named_objects: BTreeMap::new(),
        },
        output_contract_sha256: [0; 32],
        credential: None,
        origins: vec![FixedOrigin {
            origin: OriginId::literal("origin.demo"),
            scheme: HttpsOnly,
            host: "api.example.test".to_owned(),
            port: NonZeroU16::new(443).unwrap(),
            network_policy: NetworkPolicy::PublicOnly,
        }],
        steps,
        pre_request_transforms: Vec::new(),
        post_response_transforms: Vec::new(),
        operation_processor: None,
        effect,
        pagination: PaginationPlan::None,
        error_map: ErrorMap {
            rules: Vec::new(),
            fallback: fallback(),
        },
        capacity: CapacityDefaults {
            maximum_in_flight: one(),
        },
        rate: RateDefaults {
            burst: one(),
            refill_interval_ms: one64(),
        },
        serialization_key_default: None,
        bounds: bounds(),
        resolved_fact_values: Vec::new(),
    }
}

#[test]
fn operation_effect_is_closed() {
    assert!(
        operation(OperationEffect::ReadOnly, vec![step("request")])
            .validate()
            .is_ok()
    );
    assert!(
        operation(
            OperationEffect::ProviderIdempotent {
                side_effect_steps: Vec::new(),
            },
            vec![step("request")],
        )
        .validate()
        .is_err()
    );
}

#[test]
fn processorless_declarative_operation_has_one_step() {
    let mut value = operation(
        OperationEffect::ReadOnly,
        vec![step("request"), step("request.two")],
    );
    assert_eq!(
        value.validate().unwrap_err().code(),
        "catalog_operation_processor_required"
    );
    value.operation_processor = Some(VersionedProcessorRef {
        id: ProcessorFamilyId::literal("processor.demo"),
        implementation_revision: 1,
    });
    assert!(value.validate().is_ok());
}

#[test]
fn operation_spec_is_complete_self_contained_and_versioned() {
    let value = operation(OperationEffect::ReadOnly, vec![step("request")]);
    assert_eq!(value.runtime_abi_epoch, 1_u32);
    assert_eq!(value.operation_version, StableSemver::new(1, 0, 0));
    assert!(value.validate().is_ok());
}

#[test]
fn selected_header_capability_resolution_rejects_missing_ambiguous_duplicate_and_65() {
    let mut value = operation(OperationEffect::ReadOnly, vec![step("request")]);
    value.steps[0].selected_response_headers = (0..65)
        .map(|index| {
            selected_response_header(
                value.connector,
                value.operation,
                value.operation_version,
                value.steps[0].step,
                &format!("x-header-{index}"),
            )
            .unwrap()
        })
        .collect();
    assert_eq!(
        value.validate().unwrap_err().code(),
        "catalog_selected_header_limit"
    );
}
