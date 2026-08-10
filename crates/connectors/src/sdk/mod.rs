//! Shared runtime pieces every connector composes.

pub mod auth;
pub mod connector;
pub mod effect;
pub mod errors;
pub mod operation;
pub mod pagination;
pub mod projection;
pub mod transport;
pub mod webhook;

/// The local provider stub. It is compiled for this crate's own tests, and for
/// an integration test that asks for `--features testing`; it is never part of
/// a production build.
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use auth::{AccessToken, AuthPlan, BEARER_SCHEME, Credential, Secret};
pub use connector::{
    Connector, ConnectorBuilder, ConnectorConfiguration, CredentialApplication, CredentialField,
    CredentialSpec, FieldClassification, MissingCredentialField, OperationRejection, OriginSpec,
    TemplatedHost, Trigger,
};
pub use effect::{
    AbsenceSearch, DeterminismEvidence, Effect, EffectClass, ExplicitKeyEvidence,
    IdempotencyBinding, KeyRetention, NoIdempotencyEvidence,
};
pub use errors::{
    ConnectorErrorClass, ConnectorFailure, ErrorMap, ErrorMapBuilder, MAX_CORRELATION_ID_BYTES,
    MAX_RETRY_AFTER_SECONDS,
};
pub use operation::{
    HttpMethod, JsonTemplate, MAX_HEADER_VALUE_BYTES, MAX_REQUEST_HEADER_BYTES, Operation,
    OperationBuilder, OperationError, Origin, RequestPlan, Required,
};
pub use pagination::{Pagination, PaginationBudget, Walk, undeclared_status_gate};
pub use projection::{
    HeaderProjection, InputProjection, OperationProjection, OutputProjection, QueryProjection,
    RequestBodyProjection, ValueSource,
};
pub use transport::{
    HostResolver, HttpTransport, MAX_HTTP_BODY_BYTES, PreparedHttpRequest, RawHttpResponse,
    ReqwestTransport, ResolveError, SystemResolver, TransportError, TransportErrorKind,
};
pub use webhook::{SignatureEncoding, WebhookRejection, WebhookVerifier};
