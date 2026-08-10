//! OAuth2 authorization-code credentials: the one credential the engine writes.
//!
//! Every other Donat credential is a `SecretRef` read from an environment
//! variable at boot — immutable, read-only, never written back. Roughly a
//! quarter of the third-party systems worth integrating will not accept one:
//! they issue an access token that expires in minutes and a refresh token that
//! the client is expected to store, and several of them rotate that refresh
//! token on every use. Storing a value obtained at runtime is the capability
//! this module adds, and it is the only one it adds.
//!
//! What it deliberately is not:
//!
//! - It is not an admin API. The first token is obtained by an operator
//!   running `donat connector authorize` at deploy time. Nothing here is
//!   reachable over GraphQL, REST, MCP, or any route: the engine never accepts
//!   a code, never starts an authorization, and never returns a token.
//! - It is not a permission bypass. A credential names a *provider* account,
//!   not a Donat role. `subject` is recorded so an operator can tell two
//!   authorizations apart and never enters a permission decision.
//! - It is not a background loop. Refresh happens on use, inside the attempt
//!   that needs the header, under that attempt's own deadline.
//!
//! ## The shape of the thing
//!
//! - [`keys`] — AES-256-GCM sealing, and the row identity that is its AAD.
//! - [`store`] — the source-local `donat.connector_credential` table.
//! - [`oauth`] — the token endpoint: the request, the grant, the failure
//!   classes, and the one HTTP client that talks to it.
//! - [`declaration`] — the deploy-time metadata half, resolved and validated.
//! - [`authorize`] — the CLI flow: PKCE, `state`, redirect parsing, and the
//!   single transaction that writes the first row.
//! - [`refresh`] — refresh at use, single-flighted by a transactional row lock.
//! - [`runtime`] — the boot-time resolution the request path uses.
//!
//! ## The seam
//!
//! Connector *execution* — turning a live access token into an `Authorization`
//! header on a provider request — lives in `crates/server/src/connectors/`,
//! which this module still does not touch. The two halves meet at exactly two
//! points: [`runtime::CredentialRuntime`], which the connector registry holds
//! and calls once per attempt, and
//! `crate::connectors::credential`, which maps [`oauth::CredentialErrorClass`]
//! onto the connector SDK's `ConnectorErrorClass`. Keeping the mapping on the
//! connector side is what lets this module stay free of any dependency on
//! `crates/connectors`, where every provider-facing decision lives.

pub mod authorize;
pub mod cli;
pub mod declaration;
pub mod keys;
pub mod oauth;
pub mod refresh;
pub mod runtime;
mod seal;
pub mod store;

pub use declaration::{DeclarationError, OauthDeclaration};
pub use keys::{
    CREDENTIAL_KEY_ENV, CredentialIdentity, KeyError, SealError, SealingKey, SecretBytes,
};
pub use oauth::{
    CredentialErrorClass, CredentialFailure, HttpTokenExchange, TokenExchange, TokenGrant,
    TokenRequest,
};
pub use runtime::{CredentialRuntime, CredentialRuntimeError};
pub use store::{CredentialRow, CredentialSummary};
