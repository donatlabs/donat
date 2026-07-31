use core::mem::{align_of, size_of};
use std::collections::BTreeMap;

use donat_connector_abi::{
    AbiError, AuthenticatorId, AuthorizedCorrelations, BindingSlotId, BoundedBytes, BoundedString,
    BoundedTransportResponse, BoxFuture, CapabilityId, CodecId, CompiledStepId,
    ConnectorErrorClass, ConnectorFailure, ConnectorId, ConnectorIo, CredentialFieldId,
    CredentialSpecId, Hash256, InlineId, MAXIMUM_SAFE_STRING_BYTES, NonEmptyVec, NormalizerId,
    OperationId, OriginId, ProcessorContext, ProcessorControl, ProcessorFamilyId, StaticErrorCode,
    StaticSafeMessage, TriggerId, TypedBindings, VerifiedInboundEvent, catalog_construction,
    host_construction,
};
use donat_value_contract::{BoundedInlineBytes, TypedValue};

const SERPAPI: ConnectorId = ConnectorId::literal("serpapi");
const OPERATION: OperationId = OperationId::literal("search.google");
const PROCESSOR: ProcessorFamilyId = ProcessorFamilyId::literal("serpapi.search");
const AUTHENTICATOR: AuthenticatorId = AuthenticatorId::literal("api-key");
const CODEC: CodecId = CodecId::literal("json");
const NORMALIZER: NormalizerId = NormalizerId::literal("organic-results");
const TRIGGER: TriggerId = TriggerId::literal("poll");
const CREDENTIAL_SPEC: CredentialSpecId = CredentialSpecId::literal("serpapi-key");
const CREDENTIAL_FIELD: CredentialFieldId = CredentialFieldId::literal("api-key");
const CAPABILITY: CapabilityId = CapabilityId::literal("request-id");
const BINDING_SLOT: BindingSlotId = BindingSlotId::literal("query");
const ORIGIN: OriginId = OriginId::literal("serpapi-public");
const RAW_ID: InlineId = InlineId::literal("raw");
const FAILURE_CODE: StaticErrorCode = StaticErrorCode::literal("connector_rate_limited");
static FAILURE_MESSAGE: StaticSafeMessage =
    StaticSafeMessage::literal("provider rate limit reached");

static STEPS: [CompiledStepId; 1] = [CompiledStepId::literal("search")];

fn assert_abi_error<T>(result: Result<T, AbiError>, expected: AbiError) {
    match result {
        Err(error) => assert_eq!(error, expected),
        Ok(_) => panic!("expected ABI construction to reject the value"),
    }
}

#[test]
fn abi_ids_are_canonical_and_bounded() {
    for accepted in [
        "a",
        "0",
        "serpapi",
        "search.google-v1_api",
        "a..--__b",
        &"a".repeat(95),
        &"a".repeat(96),
    ] {
        assert!(
            ConnectorId::parse(accepted).is_ok(),
            "expected `{accepted}` to be accepted"
        );
    }

    for rejected in [
        "",
        "Serp API",
        "A",
        "-leading",
        ".leading",
        "_leading",
        "trailing-",
        "trailing.",
        "trailing_",
        "a/b",
        "a:b",
        "a++b",
        "a ! b",
        "café",
        "nul\0byte",
        &"a".repeat(97),
    ] {
        assert!(
            ConnectorId::parse(rejected).is_err(),
            "expected `{rejected}` to be rejected"
        );
    }
}

#[test]
fn abi_ids_are_const_constructible_and_copy_from_statics() {
    fn takes_connector(_: ConnectorId) {}
    fn takes_operation(_: OperationId) {}
    fn takes_step(_: CompiledStepId) {}
    fn private_processor_lookup(_: ProcessorFamilyId) {}

    let connector = SERPAPI;
    takes_connector(connector);
    takes_connector(connector);

    let operation = OPERATION;
    takes_operation(operation);
    takes_operation(operation);

    let step = STEPS[0];
    takes_step(step);
    takes_step(step);

    let processor = PROCESSOR;
    private_processor_lookup(processor);
    private_processor_lookup(processor);

    assert_eq!(SERPAPI.as_str(), "serpapi");
    assert_eq!(STEPS[0].as_str(), "search");
    assert_eq!(RAW_ID.as_str(), "raw");
}

#[test]
fn every_typed_id_is_copy_const_and_layout_identical() {
    fn assert_copy<T: Copy>(_: T) {}

    macro_rules! assert_id {
        ($value:expr, $expected:literal, $type:ty) => {{
            let first: $type = $value;
            let second = first;
            assert_copy(first);
            assert_copy(second);
            assert_eq!(first.as_str(), $expected);
            assert_eq!(size_of::<$type>(), 97);
            assert_eq!(align_of::<$type>(), 1);
        }};
    }

    assert_eq!(size_of::<InlineId>(), 97);
    assert_eq!(align_of::<InlineId>(), 1);
    assert_id!(SERPAPI, "serpapi", ConnectorId);
    assert_id!(OPERATION, "search.google", OperationId);
    assert_id!(STEPS[0], "search", CompiledStepId);
    assert_id!(PROCESSOR, "serpapi.search", ProcessorFamilyId);
    assert_id!(AUTHENTICATOR, "api-key", AuthenticatorId);
    assert_id!(CODEC, "json", CodecId);
    assert_id!(NORMALIZER, "organic-results", NormalizerId);
    assert_id!(TRIGGER, "poll", TriggerId);
    assert_id!(CREDENTIAL_SPEC, "serpapi-key", CredentialSpecId);
    assert_id!(CREDENTIAL_FIELD, "api-key", CredentialFieldId);
    assert_id!(CAPABILITY, "request-id", CapabilityId);
    assert_id!(BINDING_SLOT, "query", BindingSlotId);
    assert_id!(ORIGIN, "serpapi-public", OriginId);
}

#[test]
fn runtime_parse_copies_into_the_same_inline_representation() {
    let parsed = ConnectorId::parse("serpapi").expect("valid runtime ID");
    assert!(parsed == SERPAPI);
    assert_eq!(parsed.as_str(), "serpapi");
}

#[test]
fn safe_strings_and_bytes_enforce_declared_and_engine_limits() {
    assert!(BoundedString::try_new(&"a".repeat(262_144), 262_144).is_ok());
    assert!(BoundedString::try_new(&"a".repeat(262_145), 262_144).is_err());
    assert!(BoundedString::try_new("", 262_145).is_err());
    assert!(BoundedString::try_new("line\nbreak", 32).is_err());

    let unicode = BoundedString::try_new("café", 5).expect("bounds count UTF-8 bytes");
    assert_eq!(unicode.as_str(), "café");
    assert_eq!(unicode.len(), 5);

    assert!(BoundedBytes::try_new(vec![0; 1_048_576], 1_048_576).is_ok());
    assert!(BoundedBytes::try_new(vec![0; 1_048_577], 1_048_576).is_err());
    assert!(BoundedBytes::try_new(Vec::new(), 1_048_577).is_err());
}

#[test]
fn non_empty_vectors_and_hashes_have_checked_neutral_shapes() {
    assert!(NonEmptyVec::<u8>::try_new(Vec::new()).is_err());

    let values = NonEmptyVec::try_new(vec![1_u8, 2, 3]).expect("non-empty vector");
    assert_eq!(values.len(), 3);
    assert_eq!(values.first(), &1);
    assert_eq!(values.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3]);

    let hash = Hash256::new([7; 32]);
    let copied = hash;
    assert_eq!(hash.as_bytes(), &[7; 32]);
    assert_eq!(copied.as_bytes(), &[7; 32]);
    assert_eq!(size_of::<Hash256>(), 32);
}

#[test]
fn verified_inbound_events_are_bounded_normalized_values() {
    // This catches letting provider modules hand raw bytes, empty dedupe
    // identities, or an unbounded/non-object payload across the connector ABI.
    let output = TypedValue::Object(BTreeMap::from([
        (
            "provider_event_id".to_owned(),
            TypedValue::String("evt_42".to_owned()),
        ),
        (
            "payment_status".to_owned(),
            TypedValue::String("paid".to_owned()),
        ),
    ]));
    let redacted_metadata = TypedValue::Object(BTreeMap::from([(
        "event_type".to_owned(),
        TypedValue::String("checkout.session.completed".to_owned()),
    )]));
    let event = VerifiedInboundEvent::try_new(
        BoundedString::try_new("evt_42", 256).unwrap(),
        BoundedString::try_new("checkout.session.completed", 256).unwrap(),
        output.clone(),
        Hash256::new([7; 32]),
        redacted_metadata.clone(),
    )
    .expect("a finite normalized event crosses the ABI");

    assert_eq!(event.provider_event_id(), "evt_42");
    assert_eq!(event.event_type(), "checkout.session.completed");
    assert_eq!(event.output(), &output);
    assert_eq!(event.payload_digest().as_bytes(), &[7; 32]);
    assert_eq!(event.redacted_metadata(), &redacted_metadata);

    assert_abi_error(
        VerifiedInboundEvent::try_new(
            BoundedString::try_new("", 256).unwrap(),
            BoundedString::try_new("checkout.session.completed", 256).unwrap(),
            TypedValue::Object(BTreeMap::new()),
            Hash256::new([0; 32]),
            TypedValue::Object(BTreeMap::new()),
        ),
        AbiError::InvalidValue("verified provider event ID must not be empty"),
    );
    assert_abi_error(
        VerifiedInboundEvent::try_new(
            BoundedString::try_new("evt_42", 256).unwrap(),
            BoundedString::try_new("", 256).unwrap(),
            TypedValue::Object(BTreeMap::new()),
            Hash256::new([0; 32]),
            TypedValue::Object(BTreeMap::new()),
        ),
        AbiError::InvalidValue("verified provider event type must not be empty"),
    );
    assert_abi_error(
        VerifiedInboundEvent::try_new(
            BoundedString::try_new("evt_42", 256).unwrap(),
            BoundedString::try_new("checkout.session.completed", 256).unwrap(),
            TypedValue::String("not-an-object".to_owned()),
            Hash256::new([0; 32]),
            TypedValue::Object(BTreeMap::new()),
        ),
        AbiError::InvalidValue("verified provider event output must be an object"),
    );
    assert_abi_error(
        VerifiedInboundEvent::try_new(
            BoundedString::try_new("evt_42", 256).unwrap(),
            BoundedString::try_new("checkout.session.completed", 256).unwrap(),
            TypedValue::Object(BTreeMap::new()),
            Hash256::new([0; 32]),
            TypedValue::String("not-an-object".to_owned()),
        ),
        AbiError::InvalidValue("verified event metadata must be an object"),
    );
}

fn bindings(count: usize) -> BTreeMap<BindingSlotId, TypedValue> {
    (0..count)
        .map(|index| {
            (
                BindingSlotId::parse(&format!("slot-{index}")).expect("canonical slot ID"),
                TypedValue::Null,
            )
        })
        .collect()
}

fn nested_list(depth: usize) -> TypedValue {
    (0..depth).fold(TypedValue::Null, |value, _| TypedValue::List(vec![value]))
}

fn inline_value(bytes: usize) -> TypedValue {
    TypedValue::InlineBytes(
        BoundedInlineBytes::try_new(vec![0; bytes], "application/octet-stream", None, bytes)
            .unwrap(),
    )
}

fn inline_bindings(sizes: &[usize]) -> BTreeMap<BindingSlotId, TypedValue> {
    sizes
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            (
                BindingSlotId::parse(&format!("inline-{index}")).unwrap(),
                inline_value(*bytes),
            )
        })
        .collect()
}

#[test]
fn typed_binding_maps_enforce_entry_depth_and_canonical_bounds() {
    let accepted = TypedBindings::try_new(bindings(64)).expect("64 bindings are accepted");
    assert_eq!(accepted.len(), 64);
    assert!(accepted.get(&BindingSlotId::literal("slot-0")).is_some());
    assert_abi_error(
        TypedBindings::try_new(bindings(65)),
        AbiError::LimitExceeded("binding slots"),
    );

    let mut depth_64 = BTreeMap::new();
    depth_64.insert(BindingSlotId::literal("value"), nested_list(64));
    assert!(TypedBindings::try_new(depth_64).is_ok());

    let mut depth_65 = BTreeMap::new();
    depth_65.insert(BindingSlotId::literal("value"), nested_list(65));
    assert_abi_error(
        TypedBindings::try_new(depth_65),
        AbiError::LimitExceeded("typed value nesting depth"),
    );

    let mut exact_canonical = BTreeMap::new();
    exact_canonical.insert(
        BindingSlotId::literal("value"),
        TypedValue::String("a".repeat(262_142)),
    );
    assert!(TypedBindings::try_new(exact_canonical).is_ok());

    let mut over_canonical = BTreeMap::new();
    over_canonical.insert(
        BindingSlotId::literal("value"),
        TypedValue::String("a".repeat(262_143)),
    );
    assert_abi_error(
        TypedBindings::try_new(over_canonical),
        AbiError::LimitExceeded("binding canonical bytes"),
    );
}

#[test]
fn typed_binding_aggregate_limits_are_shared_across_roots() {
    assert!(TypedBindings::try_new(inline_bindings(&[0; 16])).is_ok());
    assert_abi_error(
        TypedBindings::try_new(inline_bindings(&[0; 17])),
        AbiError::LimitExceeded("inline value count"),
    );
    assert!(TypedBindings::try_new(inline_bindings(&[65_536, 65_536])).is_ok());
    assert_abi_error(
        TypedBindings::try_new(inline_bindings(&[65_536, 65_537])),
        AbiError::LimitExceeded("aggregate decoded inline bytes"),
    );

    let mut two_deep_roots = BTreeMap::new();
    two_deep_roots.insert(BindingSlotId::literal("first"), nested_list(64));
    two_deep_roots.insert(BindingSlotId::literal("second"), nested_list(64));
    assert!(TypedBindings::try_new(two_deep_roots).is_ok());

    let mut exact_canonical = BTreeMap::new();
    exact_canonical.insert(
        BindingSlotId::literal("first"),
        TypedValue::String("a".repeat(131_070)),
    );
    exact_canonical.insert(
        BindingSlotId::literal("second"),
        TypedValue::String("a".repeat(131_070)),
    );
    assert!(TypedBindings::try_new(exact_canonical).is_ok());

    let mut over_canonical = BTreeMap::new();
    over_canonical.insert(
        BindingSlotId::literal("first"),
        TypedValue::String("a".repeat(131_070)),
    );
    over_canonical.insert(
        BindingSlotId::literal("second"),
        TypedValue::String("a".repeat(131_071)),
    );
    assert_abi_error(
        TypedBindings::try_new(over_canonical),
        AbiError::LimitExceeded("binding canonical bytes"),
    );
}

fn selected_headers(count: usize, value_bytes: usize) -> BTreeMap<CapabilityId, BoundedString> {
    (0..count)
        .map(|index| {
            (
                CapabilityId::parse(&format!("header-{index}")).expect("canonical capability ID"),
                BoundedString::try_new(&"a".repeat(value_bytes), MAXIMUM_SAFE_STRING_BYTES)
                    .expect("globally bounded string"),
            )
        })
        .collect()
}

fn aggregate_header_boundary(over_by_one: bool) -> BTreeMap<CapabilityId, BoundedString> {
    ["a", "b", "c", "d"]
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            let value_bytes = if over_by_one && index == 0 {
                8_192
            } else {
                8_191
            };
            (
                CapabilityId::parse(id).unwrap(),
                BoundedString::try_new(&"a".repeat(value_bytes), MAXIMUM_SAFE_STRING_BYTES)
                    .unwrap(),
            )
        })
        .collect()
}

#[test]
fn every_u16_status_is_accepted_and_public_construction_has_no_authority() {
    for status in [0, 1, 199, 200, 599, 600, u16::MAX] {
        let response =
            BoundedTransportResponse::try_new(status, BTreeMap::new(), TypedValue::Null, 0)
                .expect("the ABI does not impose HTTP status semantics");
        assert_eq!(response.status(), status);
        assert!(response.authorized_correlations().is_empty());
    }
}

#[test]
fn transport_responses_enforce_header_body_shape_and_output_bounds() {
    let accepted =
        BoundedTransportResponse::try_new(200, selected_headers(64, 1), nested_list(64), 1_048_576)
            .expect("all exact ceilings are accepted");
    assert_eq!(accepted.status(), 200);
    assert_eq!(accepted.selected_headers().len(), 64);
    assert_eq!(accepted.decoded(), &nested_list(64));
    assert_eq!(accepted.response_bytes(), 1_048_576);
    assert!(accepted.authorized_correlations().is_empty());

    assert_abi_error(
        BoundedTransportResponse::try_new(200, selected_headers(65, 1), TypedValue::Null, 0),
        AbiError::LimitExceeded("selected headers"),
    );
    assert!(
        BoundedTransportResponse::try_new(200, selected_headers(1, 8_192), TypedValue::Null, 0,)
            .is_ok()
    );
    assert_abi_error(
        BoundedTransportResponse::try_new(200, selected_headers(1, 8_193), TypedValue::Null, 0),
        AbiError::LimitExceeded("selected header value bytes"),
    );
    assert!(
        BoundedTransportResponse::try_new(
            200,
            aggregate_header_boundary(false),
            TypedValue::Null,
            0,
        )
        .is_ok()
    );
    assert_abi_error(
        BoundedTransportResponse::try_new(
            200,
            aggregate_header_boundary(true),
            TypedValue::Null,
            0,
        ),
        AbiError::LimitExceeded("aggregate retained header bytes"),
    );
    assert!(
        BoundedTransportResponse::try_new(200, BTreeMap::new(), TypedValue::Null, 1_048_576,)
            .is_ok()
    );
    assert_abi_error(
        BoundedTransportResponse::try_new(200, BTreeMap::new(), TypedValue::Null, 1_048_577),
        AbiError::LimitExceeded("transport response bytes"),
    );
    assert!(BoundedTransportResponse::try_new(200, BTreeMap::new(), nested_list(64), 0,).is_ok());
    assert_abi_error(
        BoundedTransportResponse::try_new(200, BTreeMap::new(), nested_list(65), 0),
        AbiError::LimitExceeded("typed value nesting depth"),
    );
    assert!(
        BoundedTransportResponse::try_new(
            200,
            BTreeMap::new(),
            TypedValue::String("a".repeat(262_142)),
            0,
        )
        .is_ok()
    );
    assert_abi_error(
        BoundedTransportResponse::try_new(
            200,
            BTreeMap::new(),
            TypedValue::String("a".repeat(262_143)),
            0,
        ),
        AbiError::LimitExceeded("canonical output bytes"),
    );
}

#[test]
fn host_authority_is_intersection_only() {
    let allowed_present = CapabilityId::literal("request-id");
    let allowed_absent = CapabilityId::literal("trace-id");
    let selected_unallowed = CapabilityId::literal("server-id");
    let mut selected = BTreeMap::new();
    selected.insert(
        allowed_present,
        BoundedString::try_new("req-123", 8_192).unwrap(),
    );
    selected.insert(
        selected_unallowed,
        BoundedString::try_new("srv-456", 8_192).unwrap(),
    );

    let response = host_construction::transport_response(
        200,
        selected,
        TypedValue::Null,
        0,
        &[allowed_present, allowed_absent],
    )
    .unwrap();

    let authority: &AuthorizedCorrelations = response.authorized_correlations();
    assert_eq!(
        authority.get(&allowed_present).map(BoundedString::as_str),
        Some("req-123"),
    );
    assert!(authority.get(&allowed_absent).is_none());
    assert!(authority.get(&selected_unallowed).is_none());
    assert_eq!(authority.len(), 1);
    assert_eq!(authority.iter().count(), 1);
    assert_eq!(response.selected_headers().len(), 2);
}

#[test]
fn correlation_authorization_enforces_every_boundary() {
    let exact_selected = selected_headers(64, 1);
    let exact_allowed: Vec<_> = exact_selected.keys().copied().collect();
    assert!(host_construction::authorized_correlations(&exact_selected, &exact_allowed).is_ok());

    let too_many_allowed: Vec<_> = (0..65)
        .map(|index| CapabilityId::parse(&format!("allowed-{index}")).unwrap())
        .collect();
    assert_abi_error(
        host_construction::authorized_correlations(&BTreeMap::new(), &too_many_allowed),
        AbiError::LimitExceeded("correlation authorization entries"),
    );

    let duplicate = CapabilityId::literal("duplicate");
    assert!(matches!(
        host_construction::authorized_correlations(&BTreeMap::new(), &[duplicate, duplicate]),
        Err(donat_connector_abi::AbiError::InvalidValue(
            "correlation authorization contains a duplicate capability",
        ))
    ));

    assert!(
        host_construction::authorized_correlations(
            &aggregate_header_boundary(false),
            &[
                CapabilityId::literal("a"),
                CapabilityId::literal("b"),
                CapabilityId::literal("c"),
                CapabilityId::literal("d"),
            ],
        )
        .is_ok()
    );
    assert_abi_error(
        host_construction::authorized_correlations(
            &aggregate_header_boundary(true),
            &[
                CapabilityId::literal("a"),
                CapabilityId::literal("b"),
                CapabilityId::literal("c"),
                CapabilityId::literal("d"),
            ],
        ),
        AbiError::LimitExceeded("aggregate retained header bytes"),
    );
}

#[test]
fn catalog_failure_text_validation_is_exact() {
    assert!(catalog_construction::static_error_code("connector_failed").is_ok());
    assert_abi_error(
        catalog_construction::static_error_code("Connector Failed"),
        AbiError::InvalidValue("connector failure code must be a canonical ABI identifier"),
    );
    assert!(catalog_construction::static_safe_message("a").is_ok());
    assert_abi_error(
        catalog_construction::static_safe_message(""),
        AbiError::InvalidValue("connector failure safe message must not be empty"),
    );
    assert!(catalog_construction::static_safe_message(&"a".repeat(1_024)).is_ok());
    assert_abi_error(
        catalog_construction::static_safe_message(&"a".repeat(1_025)),
        AbiError::LimitExceeded("connector failure safe message bytes"),
    );
    assert_abi_error(
        catalog_construction::static_safe_message("line\nbreak"),
        AbiError::InvalidValue(
            "connector failure safe message must not contain control characters",
        ),
    );
    assert_abi_error(
        catalog_construction::static_safe_message("line\u{0085}break"),
        AbiError::InvalidValue(
            "connector failure safe message must not contain control characters",
        ),
    );
}

#[test]
fn connector_failures_are_closed_redacted_clamped_and_authorized() {
    fn class_index(class: ConnectorErrorClass) -> u8 {
        match class {
            ConnectorErrorClass::Transport => 0,
            ConnectorErrorClass::Timeout => 1,
            ConnectorErrorClass::Http429 => 2,
            ConnectorErrorClass::Http5xx => 3,
            ConnectorErrorClass::Authentication => 4,
            ConnectorErrorClass::Validation => 5,
            ConnectorErrorClass::Permanent => 6,
            ConnectorErrorClass::Invariant => 7,
        }
    }

    let classes = [
        ConnectorErrorClass::Transport,
        ConnectorErrorClass::Timeout,
        ConnectorErrorClass::Http429,
        ConnectorErrorClass::Http5xx,
        ConnectorErrorClass::Authentication,
        ConnectorErrorClass::Validation,
        ConnectorErrorClass::Permanent,
        ConnectorErrorClass::Invariant,
    ];
    assert_eq!(classes.map(class_index), [0_u8, 1, 2, 3, 4, 5, 6, 7]);

    let request_id = CapabilityId::literal("request-id");
    let mut selected = BTreeMap::new();
    selected.insert(
        request_id,
        BoundedString::try_new("req-123", 8_192).unwrap(),
    );
    let response =
        host_construction::transport_response(429, selected, TypedValue::Null, 0, &[request_id])
            .unwrap();
    let failure = ConnectorFailure::try_new(
        ConnectorErrorClass::Http429,
        FAILURE_CODE,
        FAILURE_MESSAGE,
        Some(86_400),
        Some(response.authorized_correlations()),
    )
    .expect("safe authorized failure");
    assert_eq!(failure.class(), ConnectorErrorClass::Http429);
    assert_eq!(failure.code(), "connector_rate_limited");
    assert_eq!(failure.safe_message(), "provider rate limit reached");
    assert_eq!(failure.retry_after_seconds(), Some(86_400));
    assert_eq!(
        failure
            .correlation_ids()
            .get(&request_id)
            .map(BoundedString::as_str),
        Some("req-123")
    );

    let clamped = ConnectorFailure::try_new(
        ConnectorErrorClass::Http429,
        FAILURE_CODE,
        FAILURE_MESSAGE,
        Some(86_401),
        None,
    )
    .unwrap();
    assert_eq!(clamped.retry_after_seconds(), Some(86_400));
    assert!(clamped.correlation_ids().is_empty());
}

#[test]
fn failure_accessors_return_the_private_static_text_values() {
    let failure = ConnectorFailure::try_new(
        ConnectorErrorClass::Permanent,
        FAILURE_CODE,
        FAILURE_MESSAGE,
        None,
        None,
    )
    .unwrap();

    assert_eq!(failure.code(), "connector_rate_limited");
    assert_eq!(failure.safe_message(), "provider rate limit reached");
}

#[test]
fn host_traits_are_object_safe_send_and_sync_with_exact_call_types() {
    fn connector_io(_: &dyn ConnectorIo) {}
    fn processor_control(_: &dyn ProcessorControl) {}
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    fn exact_call_signature<'a>(
        io: &'a dyn ConnectorIo,
        step: CompiledStepId,
        bindings: TypedBindings,
    ) -> BoxFuture<'a, Result<BoundedTransportResponse, ConnectorFailure>> {
        io.call(step, bindings)
    }

    assert_send_sync::<dyn ConnectorIo>();
    assert_send_sync::<dyn ProcessorControl>();
    let _: fn(&dyn ConnectorIo) = connector_io;
    let _: fn(&dyn ProcessorControl) = processor_control;
    let _ = exact_call_signature;
}

#[test]
fn processor_context_borrows_only_neutral_bounded_capabilities() {
    fn inspect_context(context: ProcessorContext<'_>) {
        let _: &ConnectorId = context.connector;
        let _: &OperationId = context.operation;
        let _: &BoundedString = context.logical_activity_id;
        let _: &BoundedString = context.idempotency_identity;
        let _: &Hash256 = context.request_fingerprint;
        let _: &[CapabilityId] = context.capabilities;
        let _: &dyn ProcessorControl = context.control;
    }

    let _ = inspect_context;
}
