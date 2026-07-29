//! Neutral, bounded connector values and host traits.
//!
//! Invariant-carrying response fields cannot be initialized directly:
//!
//! ```compile_fail
//! use std::collections::BTreeMap;
//! use donat_connector_abi::BoundedTransportResponse;
//! use donat_value_contract::TypedValue;
//!
//! let _ = BoundedTransportResponse {
//!     status: 200,
//!     selected_headers: BTreeMap::new(),
//!     decoded: TypedValue::Null,
//!     response_bytes: 0,
//! };
//! ```
//!
//! Each response field is independently immutable outside this crate:
//!
//! ```compile_fail
//! use std::collections::BTreeMap;
//! use donat_connector_abi::BoundedTransportResponse;
//! use donat_value_contract::TypedValue;
//!
//! let mut response = BoundedTransportResponse::try_new(
//!     200,
//!     BTreeMap::new(),
//!     TypedValue::Null,
//!     0,
//! ).unwrap();
//! response.status = 500;
//! ```
//!
//! ```compile_fail
//! use std::collections::BTreeMap;
//! use donat_connector_abi::BoundedTransportResponse;
//! use donat_value_contract::TypedValue;
//!
//! let mut response = BoundedTransportResponse::try_new(
//!     200,
//!     BTreeMap::new(),
//!     TypedValue::Null,
//!     0,
//! ).unwrap();
//! response.selected_headers = BTreeMap::new();
//! ```
//!
//! ```compile_fail
//! use std::collections::BTreeMap;
//! use donat_connector_abi::BoundedTransportResponse;
//! use donat_value_contract::TypedValue;
//!
//! let mut response = BoundedTransportResponse::try_new(
//!     200,
//!     BTreeMap::new(),
//!     TypedValue::Null,
//!     0,
//! ).unwrap();
//! response.decoded = TypedValue::Null;
//! ```
//!
//! ```compile_fail
//! use std::collections::BTreeMap;
//! use donat_connector_abi::BoundedTransportResponse;
//! use donat_value_contract::TypedValue;
//!
//! let mut response = BoundedTransportResponse::try_new(
//!     200,
//!     BTreeMap::new(),
//!     TypedValue::Null,
//!     0,
//! ).unwrap();
//! response.response_bytes = 1;
//! ```
//!
//! Correlation authority cannot be constructed or extended by a caller:
//!
//! ```compile_fail
//! use std::collections::BTreeMap;
//! use donat_connector_abi::{
//!     AuthorizedCorrelations, BoundedString, BoundedTransportResponse,
//!     CapabilityId,
//! };
//! use donat_value_contract::TypedValue;
//!
//! let _ = AuthorizedCorrelations { values: BTreeMap::new() };
//! let response = BoundedTransportResponse::try_new(
//!     200,
//!     BTreeMap::new(),
//!     TypedValue::Null,
//!     0,
//! ).unwrap();
//! let value = BoundedString::try_new("req-123", 8_192).unwrap();
//! response
//!     .authorized_correlations()
//!     .insert(CapabilityId::literal("request-id"), value);
//! ```
//!
//! Runtime allocations cannot satisfy the literal-only failure-text API:
//!
//! ```compile_fail
//! use std::string::String;
//! use donat_connector_abi::{StaticErrorCode, StaticSafeMessage};
//!
//! let dynamic = String::from("connector_failed");
//! let _ = StaticErrorCode::literal(dynamic.as_str());
//! let _ = StaticSafeMessage::literal(dynamic.as_str());
//! ```
//!
//! Runtime conversion traits are deliberately absent:
//!
//! ```compile_fail
//! use std::string::String;
//! use donat_connector_abi::{StaticErrorCode, StaticSafeMessage};
//!
//! let _ = StaticErrorCode::try_from(String::from("connector_failed"));
//! let _ = StaticSafeMessage::try_from(String::from("connector failed"));
//! ```
//!
//! Public runtime constructors are deliberately absent:
//!
//! ```compile_fail
//! use donat_connector_abi::{StaticErrorCode, StaticSafeMessage};
//!
//! let _ = StaticErrorCode::parse("connector_failed");
//! let _ = StaticSafeMessage::try_new("connector failed");
//! ```
//!
//! Invariant-carrying values do not expose convenience construction traits:
//!
//! ```compile_fail
//! use donat_connector_abi::{
//!     AuthorizedCorrelations, StaticErrorCode, StaticSafeMessage,
//! };
//!
//! fn require_clone<T: Clone>() {}
//! fn require_default<T: Default>() {}
//! require_clone::<AuthorizedCorrelations>();
//! require_default::<AuthorizedCorrelations>();
//! require_default::<StaticErrorCode>();
//! require_default::<StaticSafeMessage>();
//! ```
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod envelope;
mod host;
mod ids;

pub use envelope::{
    AuthorizedCorrelations, BoundedBytes, BoundedString, BoundedTransportResponse,
    ConnectorErrorClass, ConnectorFailure, Hash256, MAXIMUM_BINDING_SLOTS, MAXIMUM_BOUNDED_BYTES,
    MAXIMUM_CANONICAL_OUTPUT_BYTES, MAXIMUM_CORRELATION_IDS, MAXIMUM_FAILURE_CODE_BYTES,
    MAXIMUM_HEADER_VALUE_BYTES, MAXIMUM_JSON_DEPTH, MAXIMUM_JSON_NODES,
    MAXIMUM_RETAINED_HEADER_BYTES, MAXIMUM_RETRY_AFTER_SECONDS, MAXIMUM_SAFE_MESSAGE_BYTES,
    MAXIMUM_SAFE_STRING_BYTES, MAXIMUM_SELECTED_HEADERS, MAXIMUM_TRANSPORT_RESPONSE_BYTES,
    NonEmptyVec, StaticSafeMessage, TypedBindings,
};
pub use host::{BoxFuture, ConnectorIo, ProcessorContext, ProcessorControl};
pub use ids::{
    ABI_ID_CAPACITY, AuthenticatorId, BindingSlotId, CapabilityId, CodecId, CompiledStepId,
    ConnectorId, CredentialFieldId, CredentialSpecId, InlineId, NormalizerId, OperationId,
    OriginId, ProcessorFamilyId, StaticErrorCode, TriggerId,
};

#[doc(hidden)]
pub mod catalog_construction {
    use crate::{AbiError, StaticErrorCode, StaticSafeMessage};

    pub fn static_error_code(value: &str) -> Result<StaticErrorCode, AbiError> {
        StaticErrorCode::parse_catalog(value)
    }

    pub fn static_safe_message(value: &str) -> Result<StaticSafeMessage, AbiError> {
        StaticSafeMessage::try_catalog(value)
    }
}

#[doc(hidden)]
pub mod host_construction {
    use alloc::collections::BTreeMap;

    use donat_value_contract::TypedValue;

    use crate::{
        AbiError, AuthorizedCorrelations, BoundedString, BoundedTransportResponse, CapabilityId,
        envelope,
    };

    pub fn transport_response(
        status: u16,
        selected_headers: BTreeMap<CapabilityId, BoundedString>,
        decoded: TypedValue,
        response_bytes: u32,
        allowed_correlations: &[CapabilityId],
    ) -> Result<BoundedTransportResponse, AbiError> {
        envelope::host_transport_response(
            status,
            selected_headers,
            decoded,
            response_bytes,
            allowed_correlations,
        )
    }

    pub fn authorized_correlations(
        selected_headers: &BTreeMap<CapabilityId, BoundedString>,
        allowed_correlations: &[CapabilityId],
    ) -> Result<AuthorizedCorrelations, AbiError> {
        envelope::authorize_correlations(selected_headers, allowed_correlations)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiError {
    InvalidId,
    InvalidValue(&'static str),
    EmptyCollection,
    LimitExceeded(&'static str),
    SizeOverflow,
}
