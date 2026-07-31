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
//! Correlation authority cannot be constructed by a caller:
//!
//! ```compile_fail
//! use std::collections::BTreeMap;
//! use donat_connector_abi::AuthorizedCorrelations;
//!
//! let _ = AuthorizedCorrelations { values: BTreeMap::new() };
//! ```
//!
//! Correlation authority cannot be extended by a caller:
//!
//! ```compile_fail
//! use std::collections::BTreeMap;
//! use donat_connector_abi::{
//!     BoundedString, BoundedTransportResponse, CapabilityId,
//! };
//! use donat_value_contract::TypedValue;
//!
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
//! A runtime allocation cannot satisfy the literal-only error-code API:
//!
//! ```compile_fail
//! use std::string::String;
//! use donat_connector_abi::StaticErrorCode;
//!
//! let dynamic = String::from("connector_failed");
//! let _ = StaticErrorCode::literal(dynamic.as_str());
//! ```
//!
//! A runtime allocation cannot satisfy the literal-only safe-message API:
//!
//! ```compile_fail
//! use std::string::String;
//! use donat_connector_abi::StaticSafeMessage;
//!
//! let dynamic = String::from("connector failed");
//! let _ = StaticSafeMessage::literal(dynamic.as_str());
//! ```
//!
//! Runtime conversion into an error code is deliberately absent:
//!
//! ```compile_fail
//! use std::string::String;
//! use donat_connector_abi::StaticErrorCode;
//!
//! let _ = StaticErrorCode::try_from(String::from("connector_failed"));
//! ```
//!
//! Runtime conversion into a safe message is deliberately absent:
//!
//! ```compile_fail
//! use std::string::String;
//! use donat_connector_abi::StaticSafeMessage;
//!
//! let _ = StaticSafeMessage::try_from(String::from("connector failed"));
//! ```
//!
//! A public runtime error-code parser is deliberately absent:
//!
//! ```compile_fail
//! use donat_connector_abi::StaticErrorCode;
//!
//! let _ = StaticErrorCode::parse("connector_failed");
//! ```
//!
//! A public runtime safe-message constructor is deliberately absent:
//!
//! ```compile_fail
//! use donat_connector_abi::StaticSafeMessage;
//!
//! let _ = StaticSafeMessage::try_new("connector failed");
//! ```
//!
//! Correlation authority is not cloneable:
//!
//! ```compile_fail
//! use donat_connector_abi::AuthorizedCorrelations;
//!
//! fn require_clone<T: Clone>() {}
//! require_clone::<AuthorizedCorrelations>();
//! ```
//!
//! Correlation authority has no default constructor:
//!
//! ```compile_fail
//! use donat_connector_abi::AuthorizedCorrelations;
//!
//! fn require_default<T: Default>() {}
//! require_default::<AuthorizedCorrelations>();
//! ```
//!
//! Static error codes have no default constructor:
//!
//! ```compile_fail
//! use donat_connector_abi::StaticErrorCode;
//!
//! fn require_default<T: Default>() {}
//! require_default::<StaticErrorCode>();
//! ```
//!
//! Static safe messages have no default constructor:
//!
//! ```compile_fail
//! use donat_connector_abi::StaticSafeMessage;
//!
//! fn require_default<T: Default>() {}
//! require_default::<StaticSafeMessage>();
//! ```
//!
//! Decoded values cannot be acquired in a const context:
//!
//! ```compile_fail
//! use donat_connector_abi::BoundedTransportResponse;
//! use donat_value_contract::TypedValue;
//!
//! const fn decoded(response: &BoundedTransportResponse) -> &TypedValue {
//!     response.decoded()
//! }
//! ```
//!
//! Correlation authority cannot be acquired in a const context:
//!
//! ```compile_fail
//! use donat_connector_abi::{
//!     AuthorizedCorrelations, BoundedTransportResponse,
//! };
//!
//! const fn authorized(
//!     response: &BoundedTransportResponse,
//! ) -> &AuthorizedCorrelations {
//!     response.authorized_correlations()
//! }
//! ```
//!
//! Failure correlation IDs cannot be acquired in a const context:
//!
//! ```compile_fail
//! use std::collections::BTreeMap;
//! use donat_connector_abi::{
//!     BoundedString, CapabilityId, ConnectorFailure,
//! };
//!
//! const fn correlations(
//!     failure: &ConnectorFailure,
//! ) -> &BTreeMap<CapabilityId, BoundedString> {
//!     failure.correlation_ids()
//! }
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
    NonEmptyVec, StaticSafeMessage, TypedBindings, VerifiedInboundEvent,
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
