use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use donat_value_contract::{TypedValue, canonical_size};

use crate::{AbiError, BindingSlotId, CapabilityId, InlineId};

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

pub struct TypedBindings {
    slots: BTreeMap<BindingSlotId, TypedValue>,
}

impl TypedBindings {
    pub fn try_new(slots: BTreeMap<BindingSlotId, TypedValue>) -> Result<Self, AbiError> {
        if slots.len() > MAXIMUM_BINDING_SLOTS {
            return Err(AbiError::LimitExceeded("binding slots"));
        }

        let mut canonical_bytes = 0_usize;
        for value in slots.values() {
            validate_typed_value_shape(value)?;
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

pub struct BoundedTransportResponse {
    pub status: u16,
    pub selected_headers: BTreeMap<CapabilityId, BoundedString>,
    pub decoded: TypedValue,
    pub response_bytes: u32,
}

impl BoundedTransportResponse {
    pub fn try_new(
        status: u16,
        selected_headers: BTreeMap<CapabilityId, BoundedString>,
        decoded: TypedValue,
        response_bytes: u32,
    ) -> Result<Self, AbiError> {
        validate_headers(&selected_headers, MAXIMUM_SELECTED_HEADERS)?;
        if response_bytes > MAXIMUM_TRANSPORT_RESPONSE_BYTES {
            return Err(AbiError::LimitExceeded("transport response bytes"));
        }
        validate_typed_value_shape(&decoded)?;
        let output_bytes = canonical_size(&decoded)
            .map_err(|_| AbiError::LimitExceeded("canonical output bytes"))?;
        if output_bytes > MAXIMUM_CANONICAL_OUTPUT_BYTES {
            return Err(AbiError::LimitExceeded("canonical output bytes"));
        }
        Ok(Self {
            status,
            selected_headers,
            decoded,
            response_bytes,
        })
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

pub struct ConnectorFailure {
    class: ConnectorErrorClass,
    code: BoundedString,
    safe_message: BoundedString,
    retry_after_seconds: Option<u32>,
    correlation_ids: BTreeMap<CapabilityId, BoundedString>,
}

impl ConnectorFailure {
    pub fn try_new(
        class: ConnectorErrorClass,
        code: &str,
        safe_message: &str,
        retry_after_seconds: Option<u64>,
        correlation_ids: BTreeMap<CapabilityId, BoundedString>,
        allowed_correlation_ids: &[CapabilityId],
    ) -> Result<Self, AbiError> {
        if InlineId::parse(code).is_err() {
            return Err(AbiError::InvalidValue(
                "connector failure code must be a canonical ABI identifier",
            ));
        }
        let code = BoundedString::try_new(code, MAXIMUM_FAILURE_CODE_BYTES)?;
        let safe_message = BoundedString::try_new(safe_message, MAXIMUM_SAFE_MESSAGE_BYTES)?;
        if safe_message.is_empty() {
            return Err(AbiError::InvalidValue(
                "connector failure safe message must not be empty",
            ));
        }
        if correlation_ids.len() > MAXIMUM_CORRELATION_IDS {
            return Err(AbiError::LimitExceeded("correlation IDs"));
        }
        if correlation_ids
            .keys()
            .any(|candidate| !allowed_correlation_ids.contains(candidate))
        {
            return Err(AbiError::InvalidValue(
                "connector failure correlation ID is not allowlisted",
            ));
        }
        validate_headers(&correlation_ids, MAXIMUM_CORRELATION_IDS)?;

        let retry_after_seconds = retry_after_seconds
            .map(|seconds| seconds.min(u64::from(MAXIMUM_RETRY_AFTER_SECONDS)) as u32);
        Ok(Self {
            class,
            code,
            safe_message,
            retry_after_seconds,
            correlation_ids,
        })
    }

    pub const fn class(&self) -> ConnectorErrorClass {
        self.class
    }

    pub fn code(&self) -> &str {
        self.code.as_str()
    }

    pub fn safe_message(&self) -> &str {
        self.safe_message.as_str()
    }

    pub const fn retry_after_seconds(&self) -> Option<u32> {
        self.retry_after_seconds
    }

    pub const fn correlation_ids(&self) -> &BTreeMap<CapabilityId, BoundedString> {
        &self.correlation_ids
    }
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

fn validate_typed_value_shape(root: &TypedValue) -> Result<(), AbiError> {
    let mut nodes = 0_usize;
    let mut pending = alloc::vec![(root, 0_usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > MAXIMUM_JSON_DEPTH {
            return Err(AbiError::LimitExceeded("typed value nesting depth"));
        }
        nodes = nodes.checked_add(1).ok_or(AbiError::SizeOverflow)?;
        if nodes > MAXIMUM_JSON_NODES {
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
            TypedValue::Null
            | TypedValue::Boolean(_)
            | TypedValue::Number(_)
            | TypedValue::InlineBytes(_) => {}
        }
    }
    Ok(())
}
