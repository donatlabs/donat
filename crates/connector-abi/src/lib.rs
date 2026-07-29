#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod envelope;
mod host;
mod ids;

pub use envelope::{
    BoundedBytes, BoundedString, BoundedTransportResponse, ConnectorErrorClass, ConnectorFailure,
    Hash256, MAXIMUM_BINDING_SLOTS, MAXIMUM_BOUNDED_BYTES, MAXIMUM_CANONICAL_OUTPUT_BYTES,
    MAXIMUM_CORRELATION_IDS, MAXIMUM_FAILURE_CODE_BYTES, MAXIMUM_HEADER_VALUE_BYTES,
    MAXIMUM_JSON_DEPTH, MAXIMUM_JSON_NODES, MAXIMUM_RETAINED_HEADER_BYTES,
    MAXIMUM_RETRY_AFTER_SECONDS, MAXIMUM_SAFE_MESSAGE_BYTES, MAXIMUM_SAFE_STRING_BYTES,
    MAXIMUM_SELECTED_HEADERS, MAXIMUM_TRANSPORT_RESPONSE_BYTES, NonEmptyVec, TypedBindings,
};
pub use host::{BoxFuture, ConnectorIo, ProcessorContext, ProcessorControl};
pub use ids::{
    ABI_ID_CAPACITY, AuthenticatorId, BindingSlotId, CapabilityId, CodecId, CompiledStepId,
    ConnectorId, CredentialFieldId, CredentialSpecId, InlineId, NormalizerId, OperationId,
    OriginId, ProcessorFamilyId, TriggerId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiError {
    InvalidId,
    InvalidValue(&'static str),
    EmptyCollection,
    LimitExceeded(&'static str),
    SizeOverflow,
}
