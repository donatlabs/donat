use std::collections::BTreeMap;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::path::Path;

use donat_connector_abi::{
    AuthenticatorId, CodecId, CompiledStepId, ConnectorId, CredentialFieldId, NormalizerId,
    OperationId, OriginId, ProcessorFamilyId, TriggerId,
};
use donat_connector_catalog::*;
use donat_value_contract::ValueContractCatalog;

fn one() -> NonZeroU32 {
    NonZeroU32::new(1).unwrap()
}

fn one64() -> NonZeroU64 {
    NonZeroU64::new(1).unwrap()
}

fn empty_contract() -> ValueContractCatalog {
    ValueContractCatalog {
        roots: BTreeMap::new(),
        named_objects: BTreeMap::new(),
    }
}

fn operation_bounds() -> OperationBounds {
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

#[test]
fn credential_auth_plan_is_closed_and_bounded() {
    let field = CredentialFieldId::literal("credential.field");
    let plans = [
        AuthPlan::FixedHeaderApiKey {
            field,
            header: "authorization".to_owned(),
        },
        AuthPlan::FixedQueryApiKey {
            field,
            query: "api_key".to_owned(),
        },
        AuthPlan::Bearer { token: field },
        AuthPlan::HttpBasic {
            username: field,
            password: CredentialFieldId::literal("credential.password"),
        },
        AuthPlan::OAuth2ClientCredentials {
            client_id: field,
            client_secret: CredentialFieldId::literal("credential.secret"),
            token_origin: OriginId::literal("origin.oauth"),
            token_step: CompiledStepId::literal("oauth.token"),
            scopes: vec!["read".to_owned()],
            token_pointer: "/access_token".to_owned(),
        },
        AuthPlan::PreprovisionedOAuthAccessToken { token: field },
    ];
    assert_eq!(plans.len(), 6);
    let bounds = CredentialBounds {
        maximum_field_bytes: one(),
        maximum_aggregate_bytes: one(),
        maximum_token_bytes: one(),
    };
    assert_eq!(bounds.maximum_token_bytes.get(), 1);

    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unknown-auth-plan.yaml"),
    )
    .unwrap();
    assert!(fixture.contains("kind: dynamic_script"));
}

#[test]
fn fixed_origin_step_and_operation_bounds_are_required() {
    let origin = FixedOrigin {
        origin: OriginId::literal("origin.demo"),
        scheme: HttpsOnly,
        host: "api.example.test".to_owned(),
        port: NonZeroU16::new(443).unwrap(),
        network_policy: NetworkPolicy::PublicOnly,
    };
    let step = CompiledStepSpec::minimal_for_identity(CompiledStepId::literal("request"));
    let bounds = operation_bounds();
    assert_eq!(origin.port.get(), 443);
    assert_eq!(step.bounds.maximum_response_bytes.get(), 1);
    assert_eq!(bounds.deadline_ms.get(), 1);
}

fn action(class: ConnectorErrorClass, code: &str) -> ErrorAction {
    ErrorAction::try_new(
        class,
        code,
        "safe static connector message",
        RetryAfterPolicy::Never,
        Vec::new(),
    )
    .unwrap()
}

#[test]
fn error_map_is_complete_closed_and_redacted() {
    let fallback = CompleteErrorFallback {
        transport: action(ConnectorErrorClass::Transport, "connector_transport"),
        timeout: action(ConnectorErrorClass::Timeout, "connector_timeout"),
        http_429: action(ConnectorErrorClass::Http429, "connector_rate_limited"),
        http_5xx: action(ConnectorErrorClass::Http5xx, "connector_unavailable"),
        authentication: action(
            ConnectorErrorClass::Authentication,
            "connector_authentication",
        ),
        validation: action(ConnectorErrorClass::Validation, "connector_validation"),
        permanent: action(ConnectorErrorClass::Permanent, "connector_permanent"),
        invariant: action(ConnectorErrorClass::Invariant, "connector_invariant"),
    };
    assert_eq!(
        fallback.transport.safe_message.as_str(),
        "safe static connector message"
    );
    assert!(
        ErrorAction::try_new(
            ConnectorErrorClass::Invariant,
            "invalid code with spaces",
            "safe",
            RetryAfterPolicy::Never,
            Vec::new(),
        )
        .is_err()
    );
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/incomplete-error-map.yaml"),
    )
    .unwrap();
    assert!(!fixture.contains("http_429:"));
}

#[test]
fn webhook_and_poll_trigger_specs_are_closed_and_bounded() {
    let webhook = TriggerSpec::Webhook {
        connector: ConnectorId::literal("demo"),
        connector_version: StableSemver::new(1, 0, 0),
        trigger: TriggerId::literal("trigger.webhook"),
        trigger_version: StableSemver::new(1, 0, 0),
        event_version: StableSemver::new(1, 0, 0),
        runtime_abi_epoch: 1,
        authenticator: VersionedProcessorRef {
            id: AuthenticatorId::literal("auth.webhook"),
            implementation_revision: 1,
        },
        codec: VersionedProcessorRef {
            id: CodecId::literal("codec.json"),
            implementation_revision: 1,
        },
        normalizer: VersionedProcessorRef {
            id: NormalizerId::literal("normalizer.webhook"),
            implementation_revision: 1,
        },
        selected_headers: vec!["x-event-id".to_owned()],
        raw_body_max_bytes: one(),
        timestamp_window_ms: one64(),
        event_id: empty_contract(),
        event_type: empty_contract(),
        output: empty_contract(),
        redaction: RedactionPlan::Omit,
        subscription_operations: None,
    };
    let poll = TriggerSpec::Poll {
        connector: ConnectorId::literal("demo"),
        connector_version: StableSemver::new(1, 0, 0),
        trigger: TriggerId::literal("trigger.poll"),
        trigger_version: StableSemver::new(1, 0, 0),
        event_version: StableSemver::new(1, 0, 0),
        runtime_abi_epoch: 1,
        checkpoint: empty_contract(),
        processor: VersionedProcessorRef {
            id: ProcessorFamilyId::literal("processor.poll"),
            implementation_revision: 1,
        },
        event_type: empty_contract(),
        per_poll_event_limit: one(),
        bounds: operation_bounds(),
    };
    assert!(matches!(webhook, TriggerSpec::Webhook { .. }));
    assert!(matches!(poll, TriggerSpec::Poll { .. }));
}

#[test]
fn manifest_provenance_references_match_exact_records() {
    let record = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap();
    let binding = ResolvedContractFactBinding {
        use_site: "operation.get.step.request.idempotency.scope".to_owned(),
        fact: record.provider_contracts()[0].facts()[0].clone(),
    };
    let values = [ResolvedFactValue {
        use_site: "operation.get.step.request.idempotency.scope".to_owned(),
        value: donat_value_contract::TypedValue::String("Idempotency-Key".to_owned()),
    }];
    let mut reviews = SourceReviewRegistry::default();
    reviews.approve_reviewed_use("review.demo").unwrap();
    let catalog =
        AcceptedRecordCatalog::build(vec![record.clone()], &BTreeMap::new(), &reviews).unwrap();
    let effect = OperationEffect::ReadOnly;
    let requirements = check_fact_requirements(&[OperationFactRequirement::new(
        OperationId::literal("get"),
        &effect,
        &values,
    )])
    .unwrap();
    let (_, origins) = resolve_fact_bindings(
        &values,
        &[binding],
        &requirements,
        &catalog,
        &BTreeMap::new(),
    )
    .unwrap();
    let (source_record_id, _, artifact_content_sha256, _) = origins[0]
        .provider_evidence()
        .expect("provider fact must resolve to immutable evidence");
    assert_eq!(source_record_id, record.record_id().as_str());
    assert_eq!(artifact_content_sha256.len(), 64);
}
