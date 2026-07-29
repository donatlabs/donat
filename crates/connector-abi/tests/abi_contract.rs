use core::mem::{align_of, size_of};
use std::collections::BTreeMap;

use donat_connector_abi::{
    AuthenticatorId, BindingSlotId, BoundedBytes, BoundedString, BoundedTransportResponse,
    BoxFuture, CapabilityId, CodecId, CompiledStepId, ConnectorErrorClass, ConnectorFailure,
    ConnectorId, ConnectorIo, CredentialFieldId, CredentialSpecId, Hash256, InlineId, NonEmptyVec,
    NormalizerId, OperationId, OriginId, ProcessorContext, ProcessorControl, ProcessorFamilyId,
    TriggerId, TypedBindings,
};
use donat_value_contract::TypedValue;

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

static STEPS: [CompiledStepId; 1] = [CompiledStepId::literal("search")];

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

#[test]
fn typed_binding_maps_enforce_entry_depth_and_canonical_bounds() {
    let accepted = TypedBindings::try_new(bindings(64)).expect("64 bindings are accepted");
    assert_eq!(accepted.len(), 64);
    assert!(accepted.get(&BindingSlotId::literal("slot-0")).is_some());
    assert!(TypedBindings::try_new(bindings(65)).is_err());

    let mut depth_64 = BTreeMap::new();
    depth_64.insert(BindingSlotId::literal("value"), nested_list(64));
    assert!(TypedBindings::try_new(depth_64).is_ok());

    let mut depth_65 = BTreeMap::new();
    depth_65.insert(BindingSlotId::literal("value"), nested_list(65));
    assert!(TypedBindings::try_new(depth_65).is_err());

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
    assert!(TypedBindings::try_new(over_canonical).is_err());
}

fn selected_headers(count: usize, value_bytes: usize) -> BTreeMap<CapabilityId, BoundedString> {
    (0..count)
        .map(|index| {
            (
                CapabilityId::parse(&format!("header-{index}")).expect("canonical capability ID"),
                BoundedString::try_new(&"a".repeat(value_bytes), 262_144)
                    .expect("globally bounded string"),
            )
        })
        .collect()
}

#[test]
fn transport_responses_enforce_header_body_shape_and_output_bounds() {
    let accepted =
        BoundedTransportResponse::try_new(200, selected_headers(64, 1), nested_list(64), 1_048_576)
            .expect("all exact ceilings are accepted");
    assert_eq!(accepted.status, 200);
    assert_eq!(accepted.selected_headers.len(), 64);
    assert_eq!(accepted.response_bytes, 1_048_576);

    assert!(
        BoundedTransportResponse::try_new(200, selected_headers(65, 1), TypedValue::Null, 0,)
            .is_err()
    );
    assert!(
        BoundedTransportResponse::try_new(200, selected_headers(1, 8_193), TypedValue::Null, 0,)
            .is_err()
    );
    assert!(
        BoundedTransportResponse::try_new(200, selected_headers(5, 8_192), TypedValue::Null, 0,)
            .is_err()
    );
    assert!(
        BoundedTransportResponse::try_new(200, BTreeMap::new(), TypedValue::Null, 1_048_577,)
            .is_err()
    );
    assert!(BoundedTransportResponse::try_new(200, BTreeMap::new(), nested_list(65), 0,).is_err());
    assert!(
        BoundedTransportResponse::try_new(
            200,
            BTreeMap::new(),
            TypedValue::String("a".repeat(262_142)),
            0,
        )
        .is_ok()
    );
    assert!(
        BoundedTransportResponse::try_new(
            200,
            BTreeMap::new(),
            TypedValue::String("a".repeat(262_143)),
            0,
        )
        .is_err()
    );
}

#[test]
fn connector_failures_are_closed_redacted_clamped_and_allowlisted() {
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
    let mut correlations = BTreeMap::new();
    correlations.insert(
        request_id,
        BoundedString::try_new("req-123", 8_192).unwrap(),
    );
    let failure = ConnectorFailure::try_new(
        ConnectorErrorClass::Http429,
        "connector_rate_limited",
        "provider rate limit reached",
        Some(86_401),
        correlations,
        &[request_id],
    )
    .expect("safe allowlisted failure");
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

    let mut unlisted = BTreeMap::new();
    unlisted.insert(
        CapabilityId::literal("trace-id"),
        BoundedString::try_new("trace-123", 8_192).unwrap(),
    );
    assert!(
        ConnectorFailure::try_new(
            ConnectorErrorClass::Permanent,
            "connector_failed",
            "connector failed",
            None,
            unlisted,
            &[request_id],
        )
        .is_err()
    );
    assert!(
        ConnectorFailure::try_new(
            ConnectorErrorClass::Permanent,
            &"a".repeat(97),
            "connector failed",
            None,
            BTreeMap::new(),
            &[],
        )
        .is_err()
    );
    assert!(
        ConnectorFailure::try_new(
            ConnectorErrorClass::Permanent,
            "connector_failed",
            &"a".repeat(1_025),
            None,
            BTreeMap::new(),
            &[],
        )
        .is_err()
    );
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
    struct Control;

    impl ProcessorControl for Control {
        fn check(&self) -> Result<(), ConnectorFailure> {
            Ok(())
        }
    }

    let connector = ConnectorId::literal("serpapi");
    let operation = OperationId::literal("search.google");
    let logical_activity_id = BoundedString::try_new("activity-123", 64).unwrap();
    let idempotency_identity = BoundedString::try_new("attempt-123", 64).unwrap();
    let request_fingerprint = Hash256::new([9; 32]);
    let capabilities = [CapabilityId::literal("request-id")];
    let control = Control;
    let context = ProcessorContext {
        connector: &connector,
        operation: &operation,
        logical_activity_id: &logical_activity_id,
        idempotency_identity: &idempotency_identity,
        request_fingerprint: &request_fingerprint,
        capabilities: &capabilities,
        control: &control,
    };

    assert_eq!(context.connector.as_str(), "serpapi");
    assert_eq!(context.operation.as_str(), "search.google");
    assert_eq!(context.logical_activity_id.as_str(), "activity-123");
    assert_eq!(context.idempotency_identity.as_str(), "attempt-123");
    assert_eq!(context.request_fingerprint.as_bytes(), &[9; 32]);
    assert_eq!(context.capabilities[0].as_str(), "request-id");
    assert!(context.control.check().is_ok());
}
