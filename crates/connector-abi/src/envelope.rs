use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use donat_value_contract::{TypedValue, canonical_size};

use crate::{AbiError, BindingSlotId, CapabilityId, StaticErrorCode};

pub const MAXIMUM_SAFE_STRING_BYTES: usize = 262_144;
pub const MAXIMUM_BOUNDED_BYTES: usize = 1_048_576;
pub const MAXIMUM_BINDING_SLOTS: usize = 64;
pub const MAXIMUM_SELECTED_HEADERS: usize = 64;
pub const MAXIMUM_HEADER_VALUE_BYTES: usize = 8_192;
pub const MAXIMUM_RETAINED_HEADER_BYTES: usize = 32_768;
pub const MAXIMUM_TRANSPORT_RESPONSE_BYTES: u32 = 1_048_576;
pub const MAXIMUM_CANONICAL_OUTPUT_BYTES: usize = 262_144;
pub const MAXIMUM_JSON_DEPTH: usize = 64;
pub const MAXIMUM_JSON_NODES: usize = 100_000;
pub const MAXIMUM_RETRY_AFTER_SECONDS: u32 = 86_400;
pub const MAXIMUM_CORRELATION_IDS: usize = 64;
pub const MAXIMUM_FAILURE_CODE_BYTES: usize = 96;
pub const MAXIMUM_SAFE_MESSAGE_BYTES: usize = 1_024;
const MAXIMUM_INLINE_VALUES: usize = 16;
const MAXIMUM_DECODED_INLINE_BYTES: usize = 131_072;

#[repr(C)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct StaticSafeMessage {
    len: u16,
    bytes: [u8; MAXIMUM_SAFE_MESSAGE_BYTES],
}

impl StaticSafeMessage {
    pub const fn literal(value: &'static str) -> Self {
        match Self::try_copy(value) {
            Ok(message) => message,
            Err(_) => panic!("invalid connector failure safe message literal"),
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("validated connector failure messages are UTF-8")
    }

    pub(crate) fn try_catalog(value: &str) -> Result<Self, AbiError> {
        Self::try_copy(value)
    }

    const fn try_copy(value: &str) -> Result<Self, AbiError> {
        let source = value.as_bytes();
        match validate_safe_message_bytes(source) {
            Ok(()) => {}
            Err(error) => return Err(error),
        }

        let mut bytes = [0_u8; MAXIMUM_SAFE_MESSAGE_BYTES];
        let mut index = 0;
        while index < source.len() {
            bytes[index] = source[index];
            index += 1;
        }

        Ok(Self {
            len: source.len() as u16,
            bytes,
        })
    }
}

const fn validate_safe_message_bytes(source: &[u8]) -> Result<(), AbiError> {
    if source.is_empty() {
        return Err(AbiError::InvalidValue(
            "connector failure safe message must not be empty",
        ));
    }
    if source.len() > MAXIMUM_SAFE_MESSAGE_BYTES {
        return Err(AbiError::LimitExceeded(
            "connector failure safe message bytes",
        ));
    }

    let mut index = 0;
    while index < source.len() {
        let byte = source[index];
        let ascii_control = byte <= 0x1f || byte == 0x7f;
        let unicode_c1_control = byte == 0xc2
            && index + 1 < source.len()
            && source[index + 1] >= 0x80
            && source[index + 1] <= 0x9f;
        if ascii_control || unicode_c1_control {
            return Err(AbiError::InvalidValue(
                "connector failure safe message must not contain control characters",
            ));
        }
        index += 1;
    }
    Ok(())
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct BoundedString(String);

impl BoundedString {
    pub fn try_new(value: &str, maximum_bytes: usize) -> Result<Self, AbiError> {
        if maximum_bytes > MAXIMUM_SAFE_STRING_BYTES {
            return Err(AbiError::LimitExceeded("declared safe string bytes"));
        }
        if value.len() > maximum_bytes {
            return Err(AbiError::LimitExceeded("safe string bytes"));
        }
        if value.chars().any(char::is_control) {
            return Err(AbiError::InvalidValue(
                "safe strings must not contain control characters",
            ));
        }
        Ok(Self(String::from(value)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub struct BoundedBytes(Vec<u8>);

impl BoundedBytes {
    pub fn try_new(bytes: Vec<u8>, maximum_bytes: usize) -> Result<Self, AbiError> {
        if maximum_bytes > MAXIMUM_BOUNDED_BYTES {
            return Err(AbiError::LimitExceeded("declared bounded bytes"));
        }
        if bytes.len() > maximum_bytes {
            return Err(AbiError::LimitExceeded("bounded bytes"));
        }
        Ok(Self(bytes))
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub struct NonEmptyVec<T> {
    head: T,
    tail: Vec<T>,
}

impl<T> NonEmptyVec<T> {
    pub fn new(head: T, tail: Vec<T>) -> Self {
        Self { head, tail }
    }

    pub fn try_new(mut values: Vec<T>) -> Result<Self, AbiError> {
        if values.is_empty() {
            return Err(AbiError::EmptyCollection);
        }
        let head = values.remove(0);
        Ok(Self { head, tail: values })
    }

    pub fn first(&self) -> &T {
        &self.head
    }

    pub fn len(&self) -> usize {
        self.tail.len() + 1
    }

    pub const fn is_empty(&self) -> bool {
        false
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        core::iter::once(&self.head).chain(self.tail.iter())
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct Hash256([u8; 32]);

impl Hash256 {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A provider-authenticated, normalized event crossing the connector/runtime
/// boundary. Raw request bytes and signature material can never enter this
/// value; only their fixed digest and a bounded redacted projection survive.
#[derive(Clone)]
pub struct VerifiedInboundEvent {
    provider_event_id: BoundedString,
    event_type: BoundedString,
    output: TypedValue,
    payload_digest: Hash256,
    redacted_metadata: TypedValue,
}

impl VerifiedInboundEvent {
    pub fn try_new(
        provider_event_id: BoundedString,
        event_type: BoundedString,
        output: TypedValue,
        payload_digest: Hash256,
        redacted_metadata: TypedValue,
    ) -> Result<Self, AbiError> {
        if provider_event_id.is_empty() {
            return Err(AbiError::InvalidValue(
                "verified provider event ID must not be empty",
            ));
        }
        if event_type.is_empty() {
            return Err(AbiError::InvalidValue(
                "verified provider event type must not be empty",
            ));
        }
        if !matches!(output, TypedValue::Object(_)) {
            return Err(AbiError::InvalidValue(
                "verified provider event output must be an object",
            ));
        }
        if !matches!(redacted_metadata, TypedValue::Object(_)) {
            return Err(AbiError::InvalidValue(
                "verified event metadata must be an object",
            ));
        }
        validate_inbound_value(&output, "verified event output canonical bytes")?;
        validate_inbound_value(
            &redacted_metadata,
            "verified event metadata canonical bytes",
        )?;
        Ok(Self {
            provider_event_id,
            event_type,
            output,
            payload_digest,
            redacted_metadata,
        })
    }

    pub fn provider_event_id(&self) -> &str {
        self.provider_event_id.as_str()
    }

    pub fn event_type(&self) -> &str {
        self.event_type.as_str()
    }

    pub fn output(&self) -> &TypedValue {
        &self.output
    }

    pub const fn payload_digest(&self) -> &Hash256 {
        &self.payload_digest
    }

    pub fn redacted_metadata(&self) -> &TypedValue {
        &self.redacted_metadata
    }
}

impl core::fmt::Debug for VerifiedInboundEvent {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VerifiedInboundEvent")
            .field("provider_event_id", &self.provider_event_id.as_str())
            .field("event_type", &self.event_type.as_str())
            .field("output", &self.output)
            .field("payload_digest", &"<sha256>")
            .field("redacted_metadata", &self.redacted_metadata)
            .finish()
    }
}

fn validate_inbound_value(value: &TypedValue, bound: &'static str) -> Result<(), AbiError> {
    validate_typed_value_roots(core::iter::once(value))?;
    let size = canonical_size(value).map_err(|_| AbiError::LimitExceeded(bound))?;
    if size > MAXIMUM_CANONICAL_OUTPUT_BYTES {
        return Err(AbiError::LimitExceeded(bound));
    }
    Ok(())
}

pub struct TypedBindings {
    slots: BTreeMap<BindingSlotId, TypedValue>,
}

impl TypedBindings {
    pub fn try_new(slots: BTreeMap<BindingSlotId, TypedValue>) -> Result<Self, AbiError> {
        if slots.len() > MAXIMUM_BINDING_SLOTS {
            return Err(AbiError::LimitExceeded("binding slots"));
        }

        validate_typed_value_roots(slots.values())?;
        let mut canonical_bytes = 0_usize;
        for value in slots.values() {
            let value_bytes = canonical_size(value)
                .map_err(|_| AbiError::LimitExceeded("binding canonical bytes"))?;
            canonical_bytes = canonical_bytes
                .checked_add(value_bytes)
                .ok_or(AbiError::SizeOverflow)?;
            if canonical_bytes > MAXIMUM_CANONICAL_OUTPUT_BYTES {
                return Err(AbiError::LimitExceeded("binding canonical bytes"));
            }
        }
        Ok(Self { slots })
    }

    pub fn get(&self, slot: &BindingSlotId) -> Option<&TypedValue> {
        self.slots.get(slot)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&BindingSlotId, &TypedValue)> {
        self.slots.iter()
    }
}

pub struct AuthorizedCorrelations {
    values: BTreeMap<CapabilityId, BoundedString>,
}

impl AuthorizedCorrelations {
    pub fn get(&self, id: &CapabilityId) -> Option<&BoundedString> {
        self.values.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&CapabilityId, &BoundedString)> {
        self.values.iter()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

pub struct BoundedTransportResponse {
    status: u16,
    selected_headers: BTreeMap<CapabilityId, BoundedString>,
    decoded: TypedValue,
    response_bytes: u32,
    authorized_correlations: AuthorizedCorrelations,
}

impl BoundedTransportResponse {
    pub fn try_new(
        status: u16,
        selected_headers: BTreeMap<CapabilityId, BoundedString>,
        decoded: TypedValue,
        response_bytes: u32,
    ) -> Result<Self, AbiError> {
        validate_response(&selected_headers, &decoded, response_bytes)?;
        Ok(Self {
            status,
            selected_headers,
            decoded,
            response_bytes,
            authorized_correlations: AuthorizedCorrelations {
                values: BTreeMap::new(),
            },
        })
    }

    pub const fn status(&self) -> u16 {
        self.status
    }

    pub fn selected_headers(&self) -> &BTreeMap<CapabilityId, BoundedString> {
        &self.selected_headers
    }

    pub fn decoded(&self) -> &TypedValue {
        &self.decoded
    }

    pub const fn response_bytes(&self) -> u32 {
        self.response_bytes
    }

    pub fn authorized_correlations(&self) -> &AuthorizedCorrelations {
        &self.authorized_correlations
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectorErrorClass {
    Transport,
    Timeout,
    Http429,
    Http5xx,
    Authentication,
    Validation,
    Permanent,
    Invariant,
}

struct StaticFailureText {
    code: StaticErrorCode,
    safe_message: StaticSafeMessage,
}

pub struct ConnectorFailure {
    class: ConnectorErrorClass,
    static_text: Box<StaticFailureText>,
    retry_after_seconds: Option<u32>,
    correlation_ids: BTreeMap<CapabilityId, BoundedString>,
}

impl ConnectorFailure {
    pub fn try_new(
        class: ConnectorErrorClass,
        code: StaticErrorCode,
        safe_message: StaticSafeMessage,
        retry_after_seconds: Option<u64>,
        correlations: Option<&AuthorizedCorrelations>,
    ) -> Result<Self, AbiError> {
        let correlation_ids = correlations
            .map(|authority| authority.values.clone())
            .unwrap_or_default();
        let retry_after_seconds = retry_after_seconds
            .map(|seconds| seconds.min(u64::from(MAXIMUM_RETRY_AFTER_SECONDS)) as u32);
        let static_text = Box::new(StaticFailureText { code, safe_message });

        Ok(Self {
            class,
            static_text,
            retry_after_seconds,
            correlation_ids,
        })
    }

    pub const fn class(&self) -> ConnectorErrorClass {
        self.class
    }

    pub fn code(&self) -> &str {
        self.static_text.code.as_str()
    }

    pub fn safe_message(&self) -> &str {
        self.static_text.safe_message.as_str()
    }

    pub const fn retry_after_seconds(&self) -> Option<u32> {
        self.retry_after_seconds
    }

    pub fn correlation_ids(&self) -> &BTreeMap<CapabilityId, BoundedString> {
        &self.correlation_ids
    }
}

pub(crate) fn host_transport_response(
    status: u16,
    selected_headers: BTreeMap<CapabilityId, BoundedString>,
    decoded: TypedValue,
    response_bytes: u32,
    allowed_correlations: &[CapabilityId],
) -> Result<BoundedTransportResponse, AbiError> {
    validate_response(&selected_headers, &decoded, response_bytes)?;
    let authorized_correlations = authorize_correlations(&selected_headers, allowed_correlations)?;
    Ok(BoundedTransportResponse {
        status,
        selected_headers,
        decoded,
        response_bytes,
        authorized_correlations,
    })
}

pub(crate) fn authorize_correlations(
    selected_headers: &BTreeMap<CapabilityId, BoundedString>,
    allowed_correlations: &[CapabilityId],
) -> Result<AuthorizedCorrelations, AbiError> {
    validate_headers(selected_headers, MAXIMUM_SELECTED_HEADERS)?;
    if allowed_correlations.len() > MAXIMUM_CORRELATION_IDS {
        return Err(AbiError::LimitExceeded("correlation authorization entries"));
    }

    let mut seen = BTreeMap::new();
    let mut values = BTreeMap::new();
    for id in allowed_correlations {
        if seen.insert(*id, ()).is_some() {
            return Err(AbiError::InvalidValue(
                "correlation authorization contains a duplicate capability",
            ));
        }
        if let Some(value) = selected_headers.get(id) {
            values.insert(*id, value.clone());
        }
    }
    Ok(AuthorizedCorrelations { values })
}

fn validate_response(
    selected_headers: &BTreeMap<CapabilityId, BoundedString>,
    decoded: &TypedValue,
    response_bytes: u32,
) -> Result<(), AbiError> {
    validate_headers(selected_headers, MAXIMUM_SELECTED_HEADERS)?;
    if response_bytes > MAXIMUM_TRANSPORT_RESPONSE_BYTES {
        return Err(AbiError::LimitExceeded("transport response bytes"));
    }
    validate_typed_value_roots(core::iter::once(decoded))?;
    let output_bytes =
        canonical_size(decoded).map_err(|_| AbiError::LimitExceeded("canonical output bytes"))?;
    if output_bytes > MAXIMUM_CANONICAL_OUTPUT_BYTES {
        return Err(AbiError::LimitExceeded("canonical output bytes"));
    }
    Ok(())
}

fn validate_headers(
    headers: &BTreeMap<CapabilityId, BoundedString>,
    maximum_entries: usize,
) -> Result<(), AbiError> {
    if headers.len() > maximum_entries {
        return Err(AbiError::LimitExceeded("selected headers"));
    }
    let mut aggregate_bytes = 0_usize;
    for (name, value) in headers {
        if value.len() > MAXIMUM_HEADER_VALUE_BYTES {
            return Err(AbiError::LimitExceeded("selected header value bytes"));
        }
        aggregate_bytes = aggregate_bytes
            .checked_add(name.as_str().len())
            .and_then(|size| size.checked_add(value.len()))
            .ok_or(AbiError::SizeOverflow)?;
        if aggregate_bytes > MAXIMUM_RETAINED_HEADER_BYTES {
            return Err(AbiError::LimitExceeded("aggregate retained header bytes"));
        }
    }
    Ok(())
}

#[derive(Default)]
struct ValueCounters {
    nodes: usize,
    inline_values: usize,
    decoded_inline_bytes: usize,
}

fn validate_typed_value_roots<'a>(
    roots: impl Iterator<Item = &'a TypedValue>,
) -> Result<(), AbiError> {
    let mut counters = ValueCounters::default();
    for root in roots {
        validate_typed_value_root(root, &mut counters)?;
    }
    Ok(())
}

fn validate_typed_value_root(
    root: &TypedValue,
    counters: &mut ValueCounters,
) -> Result<(), AbiError> {
    let mut pending = alloc::vec![(root, 0_usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > MAXIMUM_JSON_DEPTH {
            return Err(AbiError::LimitExceeded("typed value nesting depth"));
        }
        counters.nodes = counters
            .nodes
            .checked_add(1)
            .ok_or(AbiError::SizeOverflow)?;
        if counters.nodes > MAXIMUM_JSON_NODES {
            return Err(AbiError::LimitExceeded("typed value node count"));
        }
        match value {
            TypedValue::String(value) => {
                if value.len() > MAXIMUM_SAFE_STRING_BYTES {
                    return Err(AbiError::LimitExceeded("individual typed string bytes"));
                }
            }
            TypedValue::List(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            TypedValue::Object(values) => {
                for (name, value) in values {
                    if name.len() > MAXIMUM_SAFE_STRING_BYTES {
                        return Err(AbiError::LimitExceeded("individual typed string bytes"));
                    }
                    pending.push((value, depth + 1));
                }
            }
            TypedValue::InlineBytes(value) => {
                counters.inline_values = counters
                    .inline_values
                    .checked_add(1)
                    .ok_or(AbiError::SizeOverflow)?;
                if counters.inline_values > MAXIMUM_INLINE_VALUES {
                    return Err(AbiError::LimitExceeded("inline value count"));
                }
                counters.decoded_inline_bytes = counters
                    .decoded_inline_bytes
                    .checked_add(value.as_slice().len())
                    .ok_or(AbiError::SizeOverflow)?;
                if counters.decoded_inline_bytes > MAXIMUM_DECODED_INLINE_BYTES {
                    return Err(AbiError::LimitExceeded("aggregate decoded inline bytes"));
                }
            }
            TypedValue::Null | TypedValue::Boolean(_) | TypedValue::Number(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::mem::size_of;

    use donat_value_contract::BoundedInlineBytes;

    use super::*;

    fn nested_list(depth: usize) -> TypedValue {
        (0..depth).fold(TypedValue::Null, |value, _| TypedValue::List(vec![value]))
    }

    fn inline_value(bytes: usize) -> TypedValue {
        TypedValue::InlineBytes(
            BoundedInlineBytes::try_new(vec![0; bytes], "application/octet-stream", None, bytes)
                .unwrap(),
        )
    }

    #[test]
    fn aggregate_node_counter_accepts_100_000_and_rejects_100_001() {
        let exact = TypedValue::List(vec![TypedValue::Null; 99_999]);
        assert!(validate_typed_value_roots(core::iter::once(&exact)).is_ok());

        let over = TypedValue::List(vec![TypedValue::Null; 100_000]);
        assert_eq!(
            validate_typed_value_roots(core::iter::once(&over)),
            Err(AbiError::LimitExceeded("typed value node count")),
        );
    }

    #[test]
    fn aggregate_node_counter_is_shared_while_depth_restarts() {
        let first = TypedValue::List(vec![TypedValue::Null; 49_999]);
        let second = TypedValue::List(vec![TypedValue::Null; 49_999]);
        assert!(validate_typed_value_roots([&first, &second].into_iter()).is_ok());

        let one_over = TypedValue::List(vec![TypedValue::Null; 50_000]);
        assert_eq!(
            validate_typed_value_roots([&first, &one_over].into_iter()),
            Err(AbiError::LimitExceeded("typed value node count")),
        );
    }

    #[test]
    fn aggregate_traversal_restarts_depth_for_each_root() {
        let first = nested_list(64);
        let second = nested_list(64);
        assert!(validate_typed_value_roots([&first, &second].into_iter()).is_ok());

        let over = nested_list(65);
        assert_eq!(
            validate_typed_value_roots(core::iter::once(&over)),
            Err(AbiError::LimitExceeded("typed value nesting depth")),
        );
    }

    #[test]
    fn aggregate_traversal_reports_exact_inline_limits() {
        let exact_count: Vec<_> = (0..16).map(|_| inline_value(0)).collect();
        assert!(validate_typed_value_roots(exact_count.iter()).is_ok());
        let over_count: Vec<_> = (0..17).map(|_| inline_value(0)).collect();
        assert_eq!(
            validate_typed_value_roots(over_count.iter()),
            Err(AbiError::LimitExceeded("inline value count")),
        );

        let exact_bytes = [inline_value(65_536), inline_value(65_536)];
        assert!(validate_typed_value_roots(exact_bytes.iter()).is_ok());
        let over_bytes = [inline_value(65_536), inline_value(65_537)];
        assert_eq!(
            validate_typed_value_roots(over_bytes.iter()),
            Err(AbiError::LimitExceeded("aggregate decoded inline bytes",)),
        );
    }

    #[test]
    fn individual_typed_string_bytes_accept_262_144_and_reject_262_145() {
        let exact = TypedValue::String("a".repeat(262_144));
        assert!(validate_typed_value_roots(core::iter::once(&exact)).is_ok());

        let over = TypedValue::String("a".repeat(262_145));
        assert_eq!(
            validate_typed_value_roots(core::iter::once(&over)),
            Err(AbiError::LimitExceeded("individual typed string bytes")),
        );
    }

    #[test]
    fn individual_object_key_bytes_accept_262_144_and_reject_262_145() {
        let mut exact_values = BTreeMap::new();
        exact_values.insert("a".repeat(262_144), TypedValue::Null);
        let exact = TypedValue::Object(exact_values);
        assert!(validate_typed_value_roots(core::iter::once(&exact)).is_ok());

        let mut over_values = BTreeMap::new();
        over_values.insert("a".repeat(262_145), TypedValue::Null);
        let over = TypedValue::Object(over_values);
        assert_eq!(
            validate_typed_value_roots(core::iter::once(&over)),
            Err(AbiError::LimitExceeded("individual typed string bytes")),
        );
    }

    #[test]
    fn static_safe_message_zero_fills_its_private_suffix() {
        const LITERAL: StaticSafeMessage = StaticSafeMessage::literal("connector failed");
        let runtime = StaticSafeMessage::try_catalog("connector failed").unwrap();

        assert!(LITERAL == runtime);
        assert_eq!(LITERAL.as_str(), "connector failed");
        assert!(
            LITERAL.bytes[usize::from(LITERAL.len)..]
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    #[test]
    fn connector_failure_boxes_one_complete_static_text_bundle() {
        let failure = ConnectorFailure::try_new(
            ConnectorErrorClass::Permanent,
            StaticErrorCode::literal("connector_failed"),
            StaticSafeMessage::literal("connector failed"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(failure.static_text.code.as_str(), "connector_failed",);
        assert_eq!(
            failure.static_text.safe_message.as_str(),
            "connector failed",
        );
        assert!(size_of::<ConnectorFailure>() < size_of::<StaticFailureText>());
    }
}
