//! The token endpoint: what we ask it, what it answers, and how a failure is
//! classified.
//!
//! Only two requests are ever made here — exchange an authorization code, and
//! exchange a refresh token — and both go to the one endpoint the connector
//! instance declared at deploy time. There is no discovery, no metadata
//! document, and no endpoint that comes from a provider response.

use std::fmt;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;

use super::keys::SecretBytes;

/// The activity failure classes a credential operation can produce.
///
/// These mirror the connector SDK's closed set rather than extending it: the
/// connector executor maps them onto `ConnectorErrorClass` at the seam. They
/// are kept separate here so this module does not depend on
/// `crates/connectors`, which is where every provider-facing decision lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialErrorClass {
    /// The credential itself is the problem: no row, an unusable row, a
    /// refused grant.
    Authentication,
    /// The provider asked us to slow down.
    Http429,
    /// The provider failed.
    Http5xx,
    /// We never got an answer.
    Transport,
    /// The provider answered something that is not a token response.
    Contract,
}

impl CredentialErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::Http429 => "http_429",
            Self::Http5xx => "http_5xx",
            Self::Transport => "transport",
            Self::Contract => "contract",
        }
    }
}

/// A credential failure.
///
/// Every field is either a class, a `&'static str` written in this workspace,
/// or a duration. Nothing a provider said can be stored in it, which is how
/// "no provider text is ever forwarded" stays structural rather than reviewed
/// — and it is also why a sealed token can never end up in one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialFailure {
    pub class: CredentialErrorClass,
    pub code: &'static str,
    pub message: &'static str,
    /// True when retrying this exact credential can never help. An
    /// `invalid_grant` is the canonical case: the row is marked and never
    /// refreshed again.
    pub permanent: bool,
    pub retry_after: Option<Duration>,
}

impl CredentialFailure {
    pub const fn new(
        class: CredentialErrorClass,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            class,
            code,
            message,
            permanent: false,
            retry_after: None,
        }
    }

    pub const fn permanent(
        class: CredentialErrorClass,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            class,
            code,
            message,
            permanent: true,
            retry_after: None,
        }
    }

    pub const fn with_retry_after(mut self, retry_after: Option<Duration>) -> Self {
        self.retry_after = retry_after;
        self
    }

    pub fn is_invalid_grant(&self) -> bool {
        self.code == INVALID_GRANT
    }
}

impl fmt::Display for CredentialFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.class.as_str(), self.message)
    }
}

impl std::error::Error for CredentialFailure {}

/// The one provider error code that means "this credential is over".
pub const INVALID_GRANT: &str = "invalid_grant";

/// What is recorded on a row whose refreshed grant no longer covers the
/// instance's declared scopes. Like `invalid_grant`, it is a state only an
/// operator can leave, so it is written where `credentials list` shows it.
pub const SCOPE_SHORTFALL: &str = "scope_shortfall";

pub const SCOPE_SHORTFALL_FAILURE: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Authentication,
    "credential_scope_shortfall",
    "the provider granted fewer scopes than this connector instance declares; an operator must \
     re-authorize it",
);

pub const NO_CREDENTIAL: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Authentication,
    "credential_missing",
    "this connector instance has no stored credential; an operator must run \
     `donat connector authorize`",
);

pub const CREDENTIAL_UNUSABLE: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Authentication,
    "credential_unusable",
    "the stored credential was refused by the provider and has been marked unusable; an \
     operator must re-authorize it",
);

pub const AMBIGUOUS_CREDENTIAL: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Authentication,
    "credential_ambiguous",
    "this connector instance holds more than one provider account; one instance holds one \
     account, so an operator must revoke the ones it no longer uses",
);

/// The request path asked for a credential on an instance that declares none.
/// It is unreachable through the registry, which routes on the same
/// declarations this failure is about, and it exists so that a future caller
/// cannot get an unauthenticated request by asking the wrong question.
pub const INSTANCE_NOT_DECLARED: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Authentication,
    "credential_instance_not_declared",
    "this connector instance declares no `config.oauth2` block, so no credential can be applied \
     to its requests",
);

pub const NO_REFRESH_TOKEN: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Authentication,
    "credential_not_refreshable",
    "the stored credential has expired and the provider issued no refresh token; an operator \
     must re-authorize it",
);

pub const SEALED_VALUE_UNOPENABLE: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Authentication,
    "credential_unopenable",
    "a stored connector credential did not open under its own identity",
);

pub const INVALID_GRANT_FAILURE: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Authentication,
    INVALID_GRANT,
    "the provider refused the stored refresh token",
);

pub const TOKEN_ENDPOINT_TRANSPORT: CredentialFailure = CredentialFailure::new(
    CredentialErrorClass::Transport,
    "token_endpoint_unreachable",
    "the connector's token endpoint did not answer",
);

pub const TOKEN_ENDPOINT_CONTRACT: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Contract,
    "token_endpoint_contract",
    "the connector's token endpoint answered something that is not a token response",
);

pub const FOREIGN_TOKEN_ORIGIN: CredentialFailure = CredentialFailure::permanent(
    CredentialErrorClass::Authentication,
    "token_origin_foreign",
    "the token response came from an origin the connector instance did not declare",
);

pub const DATABASE_UNAVAILABLE: CredentialFailure = CredentialFailure::new(
    CredentialErrorClass::Transport,
    "credential_store_unavailable",
    "the source-local credential store could not be reached",
);

/// Which of the two grants is being asked for.
#[derive(Debug, Clone, Copy)]
pub enum GrantRequest<'a> {
    AuthorizationCode {
        code: &'a str,
        code_verifier: &'a str,
        redirect_uri: &'a str,
    },
    Refresh {
        refresh_token: &'a str,
    },
}

impl GrantRequest<'_> {
    fn grant_type(&self) -> &'static str {
        match self {
            Self::AuthorizationCode { .. } => "authorization_code",
            Self::Refresh { .. } => "refresh_token",
        }
    }
}

/// One request to one declared token endpoint.
#[derive(Debug, Clone, Copy)]
pub struct TokenRequest<'a> {
    /// The declared endpoint. Never taken from a provider response.
    pub token_endpoint: &'a str,
    pub client_id: &'a str,
    pub client_secret: Option<&'a str>,
    pub grant: GrantRequest<'a>,
    /// Sent with a refresh so a provider that narrows scopes is asked not to.
    pub scopes: &'a [String],
    /// The whole exchange — DNS, connect, headers, *and* body — must finish
    /// inside this. It is not a header deadline: the refresh that uses it runs
    /// inside the transaction holding the credential row locked, so anything
    /// this call waits for is something every other attempt on that credential
    /// waits for too.
    pub budget: Duration,
}

/// The most of a token response this client will read.
///
/// RFC 6749 §5 responses are a few hundred bytes; the largest thing a real
/// provider adds is an ID token, and 256 KiB is far past any of them. The
/// ceiling exists because the read happens under a credential row lock: a body
/// with no end is a lock with no end, whatever the endpoint's intentions.
const MAX_TOKEN_RESPONSE_BYTES: usize = 256 * 1024;

/// What the provider granted.
pub struct TokenGrant {
    pub access_token: SecretBytes,
    pub refresh_token: Option<SecretBytes>,
    pub expires_in: Option<Duration>,
    /// `None` when the provider did not say, which by RFC 6749 means "exactly
    /// what you asked for".
    pub granted_scopes: Option<Vec<String>>,
    /// The provider's own account identity, when the token response carries
    /// one under a name we recognize.
    pub subject: Option<String>,
    /// The origin the answer actually came from. Compared with the declared
    /// endpoint before anything is written.
    pub issued_by: String,
}

impl fmt::Debug for TokenGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenGrant")
            .field("access_token", &"redacted")
            .field("refresh_token", &self.refresh_token.is_some())
            .field("expires_in", &self.expires_in)
            .field("granted_scopes", &self.granted_scopes)
            .field("subject", &self.subject)
            .field("issued_by", &self.issued_by)
            .finish()
    }
}

/// The one thing that talks to a token endpoint.
///
/// A trait, so every test in this workspace runs against a local stub and no
/// test ever reaches a real provider.
pub trait TokenExchange: Send + Sync {
    fn exchange<'a>(
        &'a self,
        request: TokenRequest<'a>,
    ) -> BoxFuture<'a, Result<TokenGrant, CredentialFailure>>;
}

/// The wire shape of a successful token response (RFC 6749 §5.1) plus the
/// account-identity fields the common providers add to it.
#[derive(Deserialize)]
struct TokenResponseBody {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    // Account identity, spelled differently by every provider. Nothing here
    // reaches a permission decision; it is a label an operator reads.
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
}

#[derive(Deserialize)]
struct TokenErrorBody {
    #[serde(default)]
    error: Option<String>,
}

/// The real client. It follows no redirect, so the origin that answers is the
/// origin we asked; it sends the client secret in the body only, so it never
/// lands in a proxy's log line the way basic auth in a URL would.
pub struct HttpTokenExchange {
    client: reqwest::Client,
}

impl Default for HttpTokenExchange {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTokenExchange {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("the token-endpoint HTTP client builds"),
        }
    }
}

impl TokenExchange for HttpTokenExchange {
    fn exchange<'a>(
        &'a self,
        request: TokenRequest<'a>,
    ) -> BoxFuture<'a, Result<TokenGrant, CredentialFailure>> {
        Box::pin(async move {
            let mut form: Vec<(&str, &str)> = vec![
                ("grant_type", request.grant.grant_type()),
                ("client_id", request.client_id),
            ];
            if let Some(secret) = request.client_secret {
                form.push(("client_secret", secret));
            }
            let joined_scopes = request.scopes.join(" ");
            match &request.grant {
                GrantRequest::AuthorizationCode {
                    code,
                    code_verifier,
                    redirect_uri,
                } => {
                    form.push(("code", code));
                    form.push(("code_verifier", code_verifier));
                    form.push(("redirect_uri", redirect_uri));
                }
                GrantRequest::Refresh { refresh_token } => {
                    form.push(("refresh_token", refresh_token));
                    if !joined_scopes.is_empty() {
                        form.push(("scope", &joined_scopes));
                    }
                }
            }

            // One deadline over the whole exchange. A budget that covered only
            // the response headers would leave the body unbounded, and the body
            // is read with the credential row locked: a provider that answers
            // `200` and then trickles would hold that lock, and the pooled
            // connection under it, until the database's statement timeout
            // failed every other attempt on the credential in turn.
            let exchange = async {
                let response = self
                    .client
                    .post(request.token_endpoint)
                    .header("accept", "application/json")
                    .form(&form)
                    .send()
                    .await
                    .map_err(|_| TOKEN_ENDPOINT_TRANSPORT)?;

                let issued_by = response.url().to_string();
                let status = response.status();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let body = read_bounded(response).await;
                Ok((issued_by, status, retry_after, body))
            };
            let (issued_by, status, retry_after, body) =
                match tokio::time::timeout(request.budget, exchange).await {
                    Ok(Ok(answer)) => answer,
                    Ok(Err(failure)) => return Err(failure),
                    Err(_) => return Err(TOKEN_ENDPOINT_TRANSPORT),
                };

            // A body that was cut short or refused for its size cannot be
            // parsed. On a failing status the status still classifies the
            // answer; on a successful one there is no token in it, and a
            // response too large to be a token response is not one.
            let body = match body {
                BoundedBody::Complete(body) => body,
                BoundedBody::Oversized if status.is_success() => {
                    return Err(TOKEN_ENDPOINT_CONTRACT);
                }
                BoundedBody::Interrupted if status.is_success() => {
                    return Err(TOKEN_ENDPOINT_TRANSPORT);
                }
                BoundedBody::Oversized | BoundedBody::Interrupted => Vec::new(),
            };

            if !status.is_success() {
                // RFC 6749 §5.2 puts the machine-readable reason in the body,
                // and `invalid_grant` is the one that must never be retried.
                let code = serde_json::from_slice::<TokenErrorBody>(&body)
                    .ok()
                    .and_then(|error| error.error);
                if code.as_deref() == Some(INVALID_GRANT) {
                    return Err(INVALID_GRANT_FAILURE);
                }
                if status.as_u16() == 429 {
                    return Err(CredentialFailure::new(
                        CredentialErrorClass::Http429,
                        "token_endpoint_throttled",
                        "the connector's token endpoint asked us to slow down",
                    )
                    .with_retry_after(retry_after));
                }
                if status.is_server_error() {
                    return Err(CredentialFailure::new(
                        CredentialErrorClass::Http5xx,
                        "token_endpoint_unavailable",
                        "the connector's token endpoint failed",
                    )
                    .with_retry_after(retry_after));
                }
                return Err(CredentialFailure::permanent(
                    CredentialErrorClass::Authentication,
                    "token_endpoint_refused",
                    "the connector's token endpoint refused the request",
                ));
            }

            let parsed: TokenResponseBody = match serde_json::from_slice(&body) {
                Ok(parsed) => parsed,
                Err(_) => return Err(TOKEN_ENDPOINT_CONTRACT),
            };
            if parsed.access_token.is_empty() {
                return Err(TOKEN_ENDPOINT_CONTRACT);
            }

            Ok(TokenGrant {
                access_token: SecretBytes::new(parsed.access_token.into_bytes()),
                refresh_token: parsed
                    .refresh_token
                    .filter(|token| !token.is_empty())
                    .map(|token| SecretBytes::new(token.into_bytes())),
                expires_in: parsed.expires_in.map(Duration::from_secs),
                granted_scopes: parsed.scope.map(|scope| {
                    scope
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                }),
                subject: parsed
                    .sub
                    .or(parsed.account_id)
                    .or(parsed.user_id)
                    .or(parsed.workspace_id)
                    .filter(|subject| !subject.is_empty()),
                issued_by,
            })
        })
    }
}

/// What came back of a token endpoint's body.
enum BoundedBody {
    Complete(Vec<u8>),
    /// The endpoint offered more than [`MAX_TOKEN_RESPONSE_BYTES`]. The rest is
    /// not read and the connection is dropped.
    Oversized,
    /// The body stopped arriving before it ended.
    Interrupted,
}

/// Read a token response, never more than [`MAX_TOKEN_RESPONSE_BYTES`] of it.
///
/// Chunk by chunk rather than `Response::bytes`, because the whole point is to
/// stop: a single `bytes()` call allocates whatever the provider decides to
/// send, and this one runs under a credential row lock.
async fn read_bounded(mut response: reqwest::Response) -> BoundedBody {
    let mut body: Vec<u8> = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_TOKEN_RESPONSE_BYTES {
                    return BoundedBody::Oversized;
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return BoundedBody::Complete(body),
            Err(_) => return BoundedBody::Interrupted,
        }
    }
}

/// Whether the granted set covers everything the instance declared.
///
/// A provider that says nothing granted what we asked for (RFC 6749 §3.3), so
/// `None` is not a shortfall. A provider that narrows the set is one, and it
/// must stop the authorization: half a credential fails later, in an activity,
/// where nobody is watching.
pub fn scope_shortfall(declared: &[String], granted: Option<&[String]>) -> Vec<String> {
    let Some(granted) = granted else {
        return Vec::new();
    };
    declared
        .iter()
        .filter(|scope| !granted.iter().any(|got| got == *scope))
        .cloned()
        .collect()
}

/// Two endpoints are the same origin when scheme, host, and port match. The
/// path is compared too: a provider's token endpoint is one path, and a
/// response that came from another one on the same host is still not the
/// endpoint we declared.
pub fn same_endpoint(declared: &str, actual: &str) -> bool {
    match (url::Url::parse(declared), url::Url::parse(actual)) {
        (Ok(declared), Ok(actual)) => {
            declared.scheme() == actual.scheme()
                && declared.host_str() == actual.host_str()
                && declared.port_or_known_default() == actual.port_or_known_default()
                && declared.path() == actual.path()
        }
        _ => false,
    }
}
