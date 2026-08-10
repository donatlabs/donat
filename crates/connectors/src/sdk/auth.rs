//! Credential application.
//!
//! An auth plan is the only thing in the SDK that touches a secret. It receives
//! a resolved [`Credential`], applies exactly one wire form to a
//! [`RequestPlan`], and hands back nothing: the connector sees the applied
//! primitive, never the secret.
//!
//! A provider module cannot define its own plan. [`AuthPlan`] is an opaque
//! struct over a private enum, so there is no variant to add, no field to
//! construct, and no trait to implement — adding a plan is an edit to this
//! file with its own test.
//!
//! ```compile_fail
//! use donat_connectors::sdk::auth::AuthPlan;
//! // The inner representation is private, so a plan cannot be constructed
//! // outside this module.
//! let _ = AuthPlan { kind: () };
//! ```
//!
//! ```compile_fail
//! use donat_connectors::sdk::auth::AuthPlan;
//! // ...and it cannot be taken apart to be extended either.
//! fn extend(plan: AuthPlan) {
//!     match plan {
//!         AuthPlan::Bearer => {}
//!         _ => {}
//!     }
//! }
//! ```
//!
//! ```compile_fail
//! use donat_connectors::sdk::auth::Secret;
//! // A secret cannot be read back out: only this module applies one.
//! let secret = Secret::new("value");
//! let _: &str = secret.expose();
//! ```
//!
//! ```compile_fail
//! use donat_connectors::sdk::auth::AccessToken;
//! // An issued token is not serializable, so it cannot be persisted.
//! let token = AccessToken::new("issued");
//! let _ = serde_json::to_string(&token);
//! ```
//!
//! The control for those four: every path they name resolves, so each one
//! fails for the reason claimed rather than because an import was wrong.
//!
//! ```
//! use donat_connectors::sdk::auth::{AccessToken, AuthPlan, Secret};
//! let _ = serde_json::to_string("control").expect("serde_json is reachable here");
//! let _ = AuthPlan::bearer();
//! let _ = Secret::new("value");
//! let _ = AccessToken::new("issued");
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use reqwest::Url;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use sha2::{Digest, Sha256};

use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};
use crate::sdk::operation::{HttpMethod, OperationError, Origin, RequestPlan};

/// The credential field names the SDK's plans read.  A `CredentialSpec` binds
/// each one to a deploy-time `SecretRef`.
pub mod field {
    pub const SECRET: &str = "secret";
    pub const CLIENT_ID: &str = "client_id";
    pub const CLIENT_SECRET: &str = "client_secret";
    /// AWS Signature Version 4 credential fields.
    ///
    /// [`AWS_REGION`] is the `region_from_config` half of spec 010 §6's
    /// `AwsSigV4 { service, region_from_config }`: a connector's region is
    /// deploy-time material, so it arrives on the resolved credential rather
    /// than through an operation input slot, and there is no API anywhere that
    /// lets a request choose it.
    /// The first half of a two-value query credential.
    ///
    /// Trello is the provider that forced it: "`https://api.trello.com/1/members/me?key={{apiKey}}&token={{apiToken}}`",
    /// where the key identifies the application and the token identifies the
    /// authorization, and neither authenticates alone. Both are secrets, so
    /// neither may live in the declaration.
    pub const API_KEY: &str = "api_key";
    pub const AWS_ACCESS_KEY_ID: &str = "access_key_id";
    pub const AWS_SECRET_ACCESS_KEY: &str = "secret_access_key";
    pub const AWS_REGION: &str = "region";
    /// The optional STS session token. It is not a required field — a
    /// deployment signing with long-term credentials has none — but when it is
    /// configured it is sent and signed.
    pub const AWS_SESSION_TOKEN: &str = "session_token";
}

/// RFC 6750's scheme name, which is what every OAuth2 provider in this
/// workspace but one publishes for its access tokens.
pub const BEARER_SCHEME: &str = "Bearer";

/// The one signature algorithm this plan implements (AWS: "SigV4 – Use
/// `AWS4-HMAC-SHA256` to specify the `HMAC-SHA256` hash algorithm").
const SIGV4_ALGORITHM: &str = "AWS4-HMAC-SHA256";
/// AWS: the credential scope's "`aws4_request` termination string".
const SIGV4_TERMINATOR: &str = "aws4_request";
const X_AMZ_DATE: HeaderName = HeaderName::from_static("x-amz-date");
const X_AMZ_CONTENT_SHA256: HeaderName = HeaderName::from_static("x-amz-content-sha256");
const X_AMZ_SECURITY_TOKEN: HeaderName = HeaderName::from_static("x-amz-security-token");

/// The percent-encode set one credential path segment is written with.
///
/// It encodes every byte outside `A-Z`, `a-z`, `0-9`, `-`, `_`, and `~`, which
/// is what stops a credential value from leaving its own segment: `/`, `?`,
/// `#`, `%`, `@`, `:`, and every control byte become percent triples, so a
/// segment cannot add a path element, a query, a fragment, or an authority.
///
/// Three of RFC 3986's `unreserved` characters are left alone, because §2.3
/// says percent-encoding them "should not be created by URI producers" and a
/// normalizing intermediary may decode them back anyway. The fourth, `.`, is
/// deliberately *not* left alone: a segment that is exactly `.` or `..` is a
/// dot segment, which a URL normalizer resolves rather than sends, and a
/// credential is not something to let a normalizer rewrite.
const CREDENTIAL_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'~');

/// One resolved secret value.
///
/// It has no `Display`, no `Serialize`, and a `Debug` that prints nothing, so
/// the ordinary ways a value reaches a log or a database do not typecheck. The
/// only reader is this module.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Visible to the auth plans and to nothing else — in particular not to
    /// `crate::providers`, so a hand-written processor cannot read a secret.
    fn expose(&self) -> &str {
        &self.0
    }

    /// Authenticate a message under this secret.
    ///
    /// Webhook verification needs a MAC, not the secret: this keeps
    /// [`Secret::expose`] private to this module while
    /// [`crate::sdk::webhook`] still authenticates raw bytes.
    pub(in crate::sdk) fn hmac_sha256(&self, message: &[u8]) -> [u8; 32] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(self.0.as_bytes())
            .expect("HMAC accepts arbitrary key bytes");
        mac.update(message);
        mac.finalize().into_bytes().into()
    }

    /// Whether a candidate equals this secret, compared without an early
    /// return on the first differing byte *and* without measuring either.
    ///
    /// Both sides are reduced to a MAC under this secret before they are
    /// compared, so the comparison is always over 32 bytes: a candidate of the
    /// wrong length costs exactly what a candidate of the right length costs.
    /// That matters here and not for a digest — a webhook signature's width is
    /// published by its algorithm, but a shared secret's length is the
    /// operator's, and a comparison that returns early on it hands out the one
    /// thing about the secret an attacker cannot otherwise sample. Keying the
    /// MAC with the secret itself is what makes the compared bytes
    /// unpredictable to whoever is guessing.
    pub(in crate::sdk) fn constant_time_eq(&self, candidate: &[u8]) -> bool {
        crate::sdk::webhook::constant_time_eq(
            &self.hmac_sha256(self.0.as_bytes()),
            &self.hmac_sha256(candidate),
        )
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

/// A credential resolved for one use.  It is read-only, is not enumerable as
/// values, and carries no write, refresh, or delete operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    fields: BTreeMap<String, Secret>,
}

impl Credential {
    pub fn from_fields<'a>(fields: impl IntoIterator<Item = (&'a str, Secret)>) -> Self {
        Self {
            fields: fields
                .into_iter()
                .map(|(name, secret)| (name.to_owned(), secret))
                .collect(),
        }
    }

    /// The common single-secret credential.
    pub fn secret(value: impl Into<String>) -> Self {
        Self::from_fields([(field::SECRET, Secret::new(value))])
    }

    /// Whether a field was configured.  Field *names* are declaration
    /// material, so startup can report a missing one; values stay unreadable.
    pub fn declares(&self, name: &str) -> bool {
        self.fields.contains_key(name)
    }

    /// A field a plan sends when a deployment configured it and omits
    /// otherwise, such as the AWS session token of a temporary credential.
    fn optional_field(&self, name: &str) -> Option<&Secret> {
        self.fields.get(name)
    }

    fn field(&self, name: &str) -> Result<&Secret, ConnectorFailure> {
        self.fields.get(name).ok_or_else(|| {
            ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "connector_credential_missing_field",
                "a declared connector credential field is not configured",
            )
        })
    }
}

/// A token issued to a logical attempt.  It is deliberately not serializable
/// and not comparable to storage: Phase 1 fetches it per attempt and drops it.
#[derive(Clone)]
pub struct AccessToken(String);

impl AccessToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AuthKind {
    ApiKeyHeader {
        name: HeaderName,
    },
    ApiKeyQuery {
        key: String,
    },
    /// Two secret values on the query string, under the two keys the provider
    /// publishes.
    ///
    /// This exists because Trello publishes exactly that and publishes nothing
    /// else a deployment can send: "`https://api.trello.com/1/members/me?key={{apiKey}}&token={{apiToken}}`",
    /// where the key names the application and the token names the
    /// authorization. Neither authenticates alone, both are secrets, and
    /// [`AuthKind::ApiKeyQuery`] carries one value — so describing Trello with
    /// it would have to put the other half in the declaration.
    ApiKeyQueryPair {
        key_param: String,
        token_param: String,
    },
    /// The credential is the request's *first* path segment, spelled
    /// `<prefix><percent-encoded secret>`.
    ///
    /// This exists because Telegram's Bot API puts the bot token there and
    /// nowhere else — "All queries to the Telegram Bot API must be served over
    /// HTTPS and need to be presented in this form:
    /// `https://api.telegram.org/bot<token>/METHOD_NAME`" — and publishes no
    /// header form at all. The prefix is compile-time declaration material; the
    /// position is fixed by this plan rather than by an operation, so a
    /// declaration cannot move a credential into the middle of a path it
    /// renders from input.
    ApiKeyPathSegment {
        prefix: String,
    },
    Bearer,
    /// `Authorization: <scheme> <secret>`, for a provider that publishes an
    /// `Authorization` scheme of its own in front of a *configured* key.
    ///
    /// This exists because Discord publishes exactly that and publishes no
    /// other form for a bot credential: "For all authentication types,
    /// authentication is performed with the `Authorization` HTTP header in the
    /// format `Authorization: TOKEN_TYPE TOKEN`", with the worked example
    /// `Authorization: Bot MTk4NjIyNDgzNDcxOTI1MjQ4.Cl2FMQ.…`. Sending the same
    /// token under RFC 6750's `Bearer` authenticates as a *user* rather than as
    /// the bot, which is a different identity and not a spelling difference.
    /// [`AuthKind::Bearer`] fixes the scheme, [`AuthKind::AuthorizationCredential`]
    /// has none, and [`AuthKind::ApiKeyHeader`] refuses the `Authorization`
    /// name on purpose — so no existing plan can describe what reaches the wire.
    /// It is the deploy-time-credential twin of the scheme
    /// [`AuthKind::OAuth2AuthorizationCode`] already carries for stored tokens
    /// ([[064-a-credentials-scheme-and-its-username-are-the-providers]]).
    ApiKeyAuthorizationScheme {
        scheme: &'static str,
    },
    /// `Authorization: <scheme> <parameter>=<secret>` — a scheme followed by one
    /// named authentication parameter carrying the key.
    ///
    /// This exists because PagerDuty publishes exactly that and publishes
    /// nothing else for a REST API key: its own OpenAPI describes the
    /// `Authorization` header as "The API Key with format `Token
    /// token=<API_KEY>`". RFC 9110's `credentials` production admits both
    /// `token68` and a list of `auth-param`s, and PagerDuty chose the second.
    /// [`AuthKind::ApiKeyAuthorizationScheme`] renders `Token <API_KEY>`, which
    /// PagerDuty answers `401`, so the difference is a wire form rather than a
    /// spelling preference. Both names are compile-time declaration material
    /// validated against the same `token` grammar the scheme is.
    ApiKeyAuthorizationParameter {
        scheme: &'static str,
        parameter: &'static str,
    },
    /// The credential *is* the whole `Authorization` header value, with no
    /// scheme in front of it.
    ///
    /// This exists because Linear publishes exactly that and publishes the
    /// contrast beside it: an OAuth access token is sent as "`Authorization:
    /// Bearer <ACCESS_TOKEN>`" while a personal API key is sent as
    /// "`Authorization: <API_KEY>`". `Bearer` would authenticate as nobody, and
    /// `ApiKeyHeader` refuses the `Authorization` name on purpose, so neither
    /// existing plan can describe what reaches the wire.
    AuthorizationCredential,
    Basic {
        username: String,
    },
    /// `Authorization: Basic base64(<secret>:<password>)` — the credential is
    /// the *username* half and the password is a declared constant.
    ///
    /// This exists because Freshdesk publishes exactly that and publishes no
    /// other header form for an API key: "You can use your personal API key to
    /// authenticate the request. If you use the API key, there is no need for a
    /// password. You can use any set of characters as a dummy password", beside
    /// the example `curl -v -u apikey:X`. [`AuthKind::Basic`] cannot describe
    /// it: its username is declaration material, and a declaration carrying an
    /// API key would be a secret in a `Debug` print and in the published
    /// credential contract.
    BasicSecretUsername {
        password: String,
    },
    OAuth2ClientCredentials {
        token_origin: Origin,
        token_path: String,
        scopes: Vec<String>,
    },
    /// Authorization-code OAuth2 (spec 011): the credential is not deploy-time
    /// material at all.
    ///
    /// `scheme` is the `Authorization` scheme name the *provider* publishes for
    /// its access tokens. It is `Bearer` for every provider that follows RFC
    /// 6750 and `Zoho-oauthtoken` for Zoho, which publishes "send the token in
    /// your HTTP authorization header to Zoho CRM API with the value
    /// `Zoho-oauthtoken {access_token}`". The credential lifecycle formats the
    /// applied header with this scheme, and the plan refuses a value in any
    /// other shape.
    ///
    /// Every other plan here reads a value a deployment configured. This one
    /// reads nothing, because the access token is issued, stored sealed,
    /// refreshed under a row lock, and handed to one attempt by
    /// `donat_server::credentials`. The plan's whole job is to say where that
    /// value goes and to refuse to send a request without it — which is what
    /// makes "a declared credential that cannot be applied fails the attempt"
    /// ([[043-the-credential-seam-refuses-before-it-sends]]) structural for a
    /// hand-written connector rather than a rule someone remembers.
    OAuth2AuthorizationCode {
        scheme: &'static str,
    },
    /// AWS Signature Version 4 over the canonical request.
    ///
    /// The service code is compiled into the connector that declares the plan;
    /// the Region arrives on the credential, from deploy-time configuration.
    /// Neither is reachable from operation input.
    AwsSigV4 {
        service: String,
    },
}

/// The closed set of credential application plans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthPlan {
    kind: AuthKind,
}

impl AuthPlan {
    /// A fixed header name carrying the secret value.
    pub fn api_key_header(name: &str) -> Result<Self, OperationError> {
        if name.eq_ignore_ascii_case("authorization") {
            return Err(OperationError::new(
                "an api key header must not be Authorization; declare Bearer or Basic instead",
            ));
        }
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| OperationError::new("an api key header name must be static and valid"))?;
        Ok(Self {
            kind: AuthKind::ApiKeyHeader { name },
        })
    }

    /// A fixed query key carrying the secret value.
    pub fn api_key_query(key: &str) -> Result<Self, OperationError> {
        if key.is_empty()
            || !key.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            })
        {
            return Err(OperationError::new(
                "an api key query key must be static and valid",
            ));
        }
        Ok(Self {
            kind: AuthKind::ApiKeyQuery {
                key: key.to_owned(),
            },
        })
    }

    /// Two fixed query keys carrying two secret values: the application key
    /// from [`field::API_KEY`] and the authorization token from
    /// [`field::SECRET`].
    ///
    /// Both halves are read from the resolved credential, so neither is
    /// declaration material, and the rendered URL is marked as carrying a
    /// credential exactly as [`AuthPlan::api_key_query`]'s is — one `Debug`,
    /// one diagnostic, and one fingerprint print the origin rather than the
    /// query.
    pub fn api_key_query_pair(key_param: &str, token_param: &str) -> Result<Self, OperationError> {
        if key_param == token_param {
            return Err(OperationError::new(
                "a two-value query credential must use two distinct query keys",
            ));
        }
        for key in [key_param, token_param] {
            if key.is_empty()
                || !key.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
                })
            {
                return Err(OperationError::new(
                    "an api key query key must be static and valid",
                ));
            }
        }
        Ok(Self {
            kind: AuthKind::ApiKeyQueryPair {
                key_param: key_param.to_owned(),
                token_param: token_param.to_owned(),
            },
        })
    }

    /// The credential as the request's first path segment, behind a fixed
    /// prefix: `https://api.telegram.org/bot<token>/sendMessage`.
    ///
    /// The value is percent-encoded with [`CREDENTIAL_SEGMENT`], so a
    /// credential that contained a `/`, a `?`, a `#`, or a `..` stays one
    /// segment and cannot re-aim the request. The rendered URL then carries a
    /// secret, which no other plan's does, so a request whose credential landed
    /// here is marked: [`RequestPlan`]'s `Debug` prints a redacted URL, and
    /// [`RequestPlan::redacted_url`] is what a log line, a metric label, or a
    /// diagnostic may use.
    pub fn api_key_path_segment(prefix: &str) -> Result<Self, OperationError> {
        // The prefix is a literal path segment, so it obeys the segment
        // grammar the value does — with no percent-encoding to fall back on,
        // because a declaration writes it directly.
        if prefix.len() > 32
            || !prefix.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_' | '~')
            })
        {
            return Err(OperationError::new(
                "an api key path segment prefix must be static unreserved ASCII",
            ));
        }
        Ok(Self {
            kind: AuthKind::ApiKeyPathSegment {
                prefix: prefix.to_owned(),
            },
        })
    }

    /// `Authorization: Bearer <secret>`.
    pub fn bearer() -> Self {
        Self {
            kind: AuthKind::Bearer,
        }
    }

    /// `Authorization: <scheme> <secret>` for a configured key, where `scheme`
    /// is the spelling the provider publishes.
    ///
    /// Discord is the one: "For all authentication types, authentication is
    /// performed with the `Authorization` HTTP header in the format
    /// `Authorization: TOKEN_TYPE TOKEN`", with `Bot` as the token type of a bot
    /// credential. The scheme is a compile-time constant of the connector,
    /// validated against RFC 9110's `token` grammar so it cannot forge a second
    /// header field value, and [`AuthPlan::bearer`] is still the plan for every
    /// provider whose token type is RFC 6750's.
    pub fn api_key_authorization_scheme(scheme: &'static str) -> Result<Self, OperationError> {
        if !is_authorization_scheme_token(scheme) {
            return Err(OperationError::new(
                "an authorization scheme name must be a static token",
            ));
        }
        Ok(Self {
            kind: AuthKind::ApiKeyAuthorizationScheme { scheme },
        })
    }

    /// `Authorization: <scheme> <parameter>=<secret>`, for a provider that
    /// publishes its key as a named authentication parameter rather than as a
    /// bare token.
    ///
    /// PagerDuty is the one: its published OpenAPI describes the `Authorization`
    /// header as "The API Key with format `Token token=<API_KEY>`". The scheme
    /// and the parameter name are both compile-time constants of the connector,
    /// each validated against RFC 9110's `token` grammar so neither can forge a
    /// second header field value, and they must differ from each other so a
    /// declaration cannot spell `Token Token=`.
    pub fn api_key_authorization_parameter(
        scheme: &'static str,
        parameter: &'static str,
    ) -> Result<Self, OperationError> {
        if !is_authorization_scheme_token(scheme) || !is_authorization_scheme_token(parameter) {
            return Err(OperationError::new(
                "an authorization scheme and parameter name must both be static tokens",
            ));
        }
        Ok(Self {
            kind: AuthKind::ApiKeyAuthorizationParameter { scheme, parameter },
        })
    }

    /// `Authorization: <secret>` — the credential as the entire header value.
    ///
    /// Declared by a provider that publishes no scheme in front of its key.
    /// Linear is the one: "To authenticate your requests, you need to pass the
    /// API key with header: `Authorization: <API_KEY>`", beside the OAuth form
    /// "`Authorization: Bearer <ACCESS_TOKEN>`" for its *other* credential kind.
    /// Sending the `Bearer` form of a Linear API key fails authentication, so
    /// this is not a stylistic difference the SDK may paper over.
    pub const fn authorization_credential() -> Self {
        Self {
            kind: AuthKind::AuthorizationCredential,
        }
    }

    /// `Authorization: Basic base64(user:secret)`.
    pub fn basic(username: &str) -> Result<Self, OperationError> {
        if username.is_empty()
            || username.contains(':')
            || !username
                .chars()
                .all(|character| character.is_ascii_graphic())
        {
            return Err(OperationError::new(
                "a basic auth username must be static printable ASCII without a colon",
            ));
        }
        Ok(Self {
            kind: AuthKind::Basic {
                username: username.to_owned(),
            },
        })
    }

    /// `Authorization: Basic base64(<secret>:<password>)`, for a provider that
    /// publishes the API key as the Basic *username*.
    ///
    /// Freshdesk is the one: "You can use your personal API key to authenticate
    /// the request. If you use the API key, there is no need for a password. You
    /// can use any set of characters as a dummy password", with the example
    /// `curl -v -u apikey:X`. The dummy password is declaration material and the
    /// key is the secret, which is the opposite of [`AuthPlan::basic`] — and the
    /// difference matters, because a plan that took the key as its username
    /// would put a secret into the declaration, its `Debug`, and its published
    /// credential contract.
    pub fn basic_secret_username(password: &str) -> Result<Self, OperationError> {
        if password.is_empty()
            || password.contains(':')
            || !password
                .chars()
                .all(|character| character.is_ascii_graphic())
        {
            return Err(OperationError::new(
                "a basic auth password must be static printable ASCII without a colon",
            ));
        }
        Ok(Self {
            kind: AuthKind::BasicSecretUsername {
                password: password.to_owned(),
            },
        })
    }

    /// A token fetched once per logical attempt from a declared token origin
    /// and never persisted.
    pub fn oauth2_client_credentials(
        token_origin: Origin,
        token_path: &str,
        scopes: &[&str],
    ) -> Result<Self, OperationError> {
        if !token_path.starts_with('/')
            || token_path.starts_with("//")
            || token_path.contains(['?', '#', '{', '}', '@'])
            || token_path
                .split('/')
                .any(|segment| matches!(segment, "." | ".."))
        {
            return Err(OperationError::new(
                "a token endpoint path must be a static absolute path",
            ));
        }
        if scopes.iter().any(|scope| {
            scope.is_empty()
                || !scope
                    .chars()
                    .all(|character| character.is_ascii_graphic() && character != ',')
        }) {
            return Err(OperationError::new("a declared scope must be static"));
        }
        Ok(Self {
            kind: AuthKind::OAuth2ClientCredentials {
                token_origin,
                token_path: token_path.to_owned(),
                scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
            },
        })
    }

    /// Authorization-code OAuth2, whose access token one attempt is given.
    ///
    /// The plan declares **no** credential field, so a deployment configures no
    /// secret for it: the value comes from the source-local credential store
    /// through [`crate::sdk::AccessToken`], per attempt, and is dropped after
    /// it. Applying it without one is refused rather than downgraded to an
    /// unauthenticated request, and the value must be the complete
    /// `Authorization` header the credential lifecycle produced — the lifecycle
    /// owns the scheme name as well as the token, and a connector that accepted
    /// anything else would be describing a request it does not make.
    pub const fn oauth2_authorization_code() -> Self {
        Self {
            kind: AuthKind::OAuth2AuthorizationCode {
                scheme: BEARER_SCHEME,
            },
        }
    }

    /// The same plan for a provider that publishes an `Authorization` scheme of
    /// its own instead of RFC 6750's `Bearer`.
    ///
    /// The scheme is the *provider's* published spelling, it is a compile-time
    /// constant of the connector that declares it, and the credential lifecycle
    /// reads it back through [`AuthPlan::oauth2_authorization_scheme`] so that
    /// the header it builds and the header this plan admits are one decision.
    pub fn oauth2_authorization_code_scheme(scheme: &'static str) -> Result<Self, OperationError> {
        if !is_authorization_scheme_token(scheme) {
            return Err(OperationError::new(
                "an authorization scheme name must be a static token",
            ));
        }
        Ok(Self {
            kind: AuthKind::OAuth2AuthorizationCode { scheme },
        })
    }

    /// The `Authorization` scheme a stored-OAuth2 connector's tokens are sent
    /// with, for the credential lifecycle that formats the header.
    ///
    /// `None` for every plan whose credential is deploy-time configuration:
    /// those apply their own wire form and are never handed a token.
    pub const fn oauth2_authorization_scheme(&self) -> Option<&'static str> {
        match self.kind {
            AuthKind::OAuth2AuthorizationCode { scheme } => Some(scheme),
            _ => None,
        }
    }

    /// Whether the executor must obtain an access token for this plan before it
    /// can send the request.
    ///
    /// Exactly one plan answers `true`: client credentials, whose token is
    /// fetched once per logical attempt from [`AuthPlan::token_request`] and
    /// dropped when the attempt ends. It is deliberately *not* true for
    /// [`AuthKind::OAuth2AuthorizationCode`], whose token is the stored
    /// credential the seam in `donat_server::connectors::credential` hands the
    /// attempt — that one is a value the executor receives, this one is a value
    /// the executor must go and get.
    ///
    /// The question exists so that "a declared credential that cannot be
    /// applied fails the attempt"
    /// ([[043-the-credential-seam-refuses-before-it-sends]]) can be enforced at
    /// deploy time: a registry can ask, before a listener opens, whether an
    /// instance needs a token exchange it is able to make.
    pub const fn issues_its_own_token(&self) -> bool {
        matches!(self.kind, AuthKind::OAuth2ClientCredentials { .. })
    }

    /// AWS Signature Version 4 (spec 010 §6, `AwsSigV4 { service,
    /// region_from_config }`).
    ///
    /// `service` is the AWS service code the credential scope names — `s3`,
    /// `sqs`, `ses` — and it is a compile-time constant of the connector that
    /// declares the plan. The Region is the credential's
    /// [`field::AWS_REGION`], which a deployment configures; there is no
    /// argument here, and no input slot anywhere, that selects either one.
    pub fn aws_sigv4(service: &str) -> Result<Self, OperationError> {
        // The credential scope is `YYYYMMDD/region/service/aws4_request`, and
        // AWS documents that "The Region code, service code, and termination
        // string must use lowercase characters". A service code with a slash
        // in it would forge a scope, so the grammar is closed here.
        if service.is_empty()
            || service.len() > 64
            || !service
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        {
            return Err(OperationError::new(
                "an AWS service code must be a static lowercase alphanumeric string",
            ));
        }
        Ok(Self {
            kind: AuthKind::AwsSigV4 {
                service: service.to_owned(),
            },
        })
    }

    /// The credential fields this plan applies.  Startup checks that a
    /// deployment configured each one.
    pub fn required_fields(&self) -> &'static [&'static str] {
        match self.kind {
            AuthKind::ApiKeyHeader { .. }
            | AuthKind::ApiKeyQuery { .. }
            | AuthKind::ApiKeyPathSegment { .. }
            | AuthKind::Bearer
            | AuthKind::ApiKeyAuthorizationScheme { .. }
            | AuthKind::ApiKeyAuthorizationParameter { .. }
            | AuthKind::AuthorizationCredential
            | AuthKind::Basic { .. }
            | AuthKind::BasicSecretUsername { .. } => &[field::SECRET],
            AuthKind::ApiKeyQueryPair { .. } => &[field::API_KEY, field::SECRET],
            AuthKind::OAuth2ClientCredentials { .. } => &[field::CLIENT_ID, field::CLIENT_SECRET],
            // Nothing. The stored credential is the deployment's, not the
            // instance configuration's, and startup proves it exists by reading
            // the credential store rather than by demanding a `SecretRef`.
            AuthKind::OAuth2AuthorizationCode { .. } => &[],
            // The session token is deliberately absent: a deployment signing
            // with long-term credentials has none, and a field startup demands
            // would refuse that deployment.
            AuthKind::AwsSigV4 { .. } => &[
                field::AWS_ACCESS_KEY_ID,
                field::AWS_SECRET_ACCESS_KEY,
                field::AWS_REGION,
            ],
        }
    }

    /// Apply this plan's exact wire form to a rendered request.
    ///
    /// `token` is the value a client-credentials attempt already fetched;
    /// every other plan ignores it, and the client-credentials plan refuses to
    /// send an unauthenticated request without it.
    pub fn apply(
        &self,
        credential: &Credential,
        request: &mut RequestPlan,
        token: Option<&AccessToken>,
    ) -> Result<(), ConnectorFailure> {
        match &self.kind {
            AuthKind::ApiKeyHeader { name } => {
                let secret = credential.field(field::SECRET)?;
                request.set_header(name.clone(), header_value(secret.expose())?, true);
            }
            AuthKind::ApiKeyQuery { key } => {
                let secret = credential.field(field::SECRET)?;
                let applied = format!(
                    "{key}={}",
                    utf8_percent_encode(secret.expose(), NON_ALPHANUMERIC)
                );
                let url = request.url_mut();
                let query = match url.query() {
                    Some(existing) if !existing.is_empty() => format!("{existing}&{applied}"),
                    _ => applied,
                };
                url.set_query(Some(&query));
                request.mark_url_credential();
            }
            AuthKind::ApiKeyQueryPair {
                key_param,
                token_param,
            } => {
                let api_key = credential.field(field::API_KEY)?;
                let token = credential.field(field::SECRET)?;
                let applied = format!(
                    "{key_param}={}&{token_param}={}",
                    utf8_percent_encode(api_key.expose(), NON_ALPHANUMERIC),
                    utf8_percent_encode(token.expose(), NON_ALPHANUMERIC)
                );
                let url = request.url_mut();
                let query = match url.query() {
                    Some(existing) if !existing.is_empty() => format!("{existing}&{applied}"),
                    _ => applied,
                };
                url.set_query(Some(&query));
                request.mark_url_credential();
            }
            AuthKind::ApiKeyPathSegment { prefix } => {
                let secret = credential.field(field::SECRET)?;
                let segment = format!(
                    "{prefix}{}",
                    utf8_percent_encode(secret.expose(), CREDENTIAL_SEGMENT)
                );
                let url = request.url_mut();
                // The rendered path is already percent-encoded, and `set_path`
                // leaves an existing `%` alone, so prefixing here neither
                // double-encodes the declaration's own path nor lets the
                // credential merge into its first segment.
                let path = format!("/{segment}{}", url.path());
                url.set_path(&path);
                request.mark_url_credential();
            }
            AuthKind::Bearer => {
                let secret = credential.field(field::SECRET)?;
                request.set_header(
                    AUTHORIZATION,
                    header_value(&format!("Bearer {}", secret.expose()))?,
                    true,
                );
            }
            AuthKind::ApiKeyAuthorizationScheme { scheme } => {
                let secret = credential.field(field::SECRET)?;
                request.set_header(
                    AUTHORIZATION,
                    header_value(&format!("{scheme} {}", secret.expose()))?,
                    true,
                );
            }
            AuthKind::ApiKeyAuthorizationParameter { scheme, parameter } => {
                let secret = credential.field(field::SECRET)?;
                // `header_value` refuses anything that is not one visible
                // header field value, so a credential carrying a control
                // character cannot open a second header here any more than it
                // can under any other plan.
                request.set_header(
                    AUTHORIZATION,
                    header_value(&format!("{scheme} {parameter}={}", secret.expose()))?,
                    true,
                );
            }
            AuthKind::AuthorizationCredential => {
                let secret = credential.field(field::SECRET)?;
                // No scheme, no prefix, no separator: the provider documents
                // the key itself as the header value, and `header_value`
                // still refuses anything that is not one visible header value.
                request.set_header(AUTHORIZATION, header_value(secret.expose())?, true);
            }
            AuthKind::Basic { username } => {
                let secret = credential.field(field::SECRET)?;
                request.set_header(
                    AUTHORIZATION,
                    header_value(&basic_value(username, secret.expose()))?,
                    true,
                );
            }
            AuthKind::BasicSecretUsername { password } => {
                let secret = credential.field(field::SECRET)?;
                request.set_header(
                    AUTHORIZATION,
                    header_value(&basic_value(secret.expose(), password))?,
                    true,
                );
            }
            AuthKind::OAuth2ClientCredentials { .. } => {
                let token = token.ok_or_else(|| {
                    ConnectorFailure::invariant(
                        "connector operation requires an issued access token",
                    )
                })?;
                request.set_header(
                    AUTHORIZATION,
                    header_value(&format!("Bearer {}", token.0))?,
                    true,
                );
            }
            AuthKind::OAuth2AuthorizationCode { scheme } => {
                let applied = token.ok_or_else(|| {
                    ConnectorFailure::new(
                        ConnectorErrorClass::Invariant,
                        "connector_credential_not_applicable",
                        "connector operation requires a stored OAuth2 credential and was given \
                         none",
                    )
                })?;
                // The credential lifecycle produces `<scheme> <token>` for the
                // scheme this plan declares; a value in any other shape is a
                // seam that changed under this connector, not a credential it
                // may send.
                let value = applied.0.as_str();
                if value
                    .strip_prefix(scheme)
                    .and_then(|rest| rest.strip_prefix(' '))
                    .is_none_or(str::is_empty)
                {
                    return Err(ConnectorFailure::new(
                        ConnectorErrorClass::Invariant,
                        "connector_credential_not_applicable",
                        "an applied OAuth2 credential must be a non-empty authorization in the \
                         scheme the connector declared",
                    ));
                }
                request.set_header(AUTHORIZATION, header_value(value)?, true);
            }
            AuthKind::AwsSigV4 { service } => {
                // The signing time is the engine's clock. AWS rejects a request
                // whose timestamp is outside its own tolerance with its own
                // error, which a connector's error map turns into
                // `authentication`; nothing here second-guesses that.
                sign_aws_sigv4(service, credential, request, SystemTime::now())?;
            }
        }
        Ok(())
    }

    /// Apply this plan at a caller-chosen signing time.
    ///
    /// Only the AWS plan reads the time; every other plan produces the same
    /// wire form it always does. It exists so a signature can be proven against
    /// a fixed vector: [`AuthPlan::apply`] signs at the engine's own clock, and
    /// a test cannot assert a signature it cannot reproduce.
    pub fn apply_at(
        &self,
        credential: &Credential,
        request: &mut RequestPlan,
        token: Option<&AccessToken>,
        signing_time: SystemTime,
    ) -> Result<(), ConnectorFailure> {
        match &self.kind {
            AuthKind::AwsSigV4 { service } => {
                sign_aws_sigv4(service, credential, request, signing_time)
            }
            _ => self.apply(credential, request, token),
        }
    }

    /// The token request this plan needs before the provider request, if any.
    /// The result is a request the caller sends once per logical attempt; the
    /// issued token is passed back to [`AuthPlan::apply`] and then dropped.
    pub fn token_request(
        &self,
        credential: &Credential,
    ) -> Result<Option<RequestPlan>, ConnectorFailure> {
        let AuthKind::OAuth2ClientCredentials {
            token_origin,
            token_path,
            scopes,
        } = &self.kind
        else {
            return Ok(None);
        };
        let client_id = credential.field(field::CLIENT_ID)?;
        let client_secret = credential.field(field::CLIENT_SECRET)?;
        let mut url = token_origin.as_url().clone();
        url.set_path(token_path);
        let mut body = "grant_type=client_credentials".to_owned();
        if !scopes.is_empty() {
            body.push_str(&format!(
                "&scope={}",
                utf8_percent_encode(&scopes.join(" "), NON_ALPHANUMERIC)
            ));
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        let mut request = RequestPlan::new(
            HttpMethod::Post.as_reqwest(),
            url,
            headers,
            body.into_bytes(),
        );
        request.set_header(
            AUTHORIZATION,
            header_value(&basic_value(client_id.expose(), client_secret.expose()))?,
            true,
        );
        Ok(Some(request))
    }
}

fn basic_value(username: &str, secret: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{secret}"))
    )
}

// ---------------------------------------------------------------------------
// AWS Signature Version 4
//
// Implemented from AWS's own published specification, "Create a signed AWS API
// request" (AWS Identity and Access Management User Guide), which defines the
// canonical request, the string to sign, the signing key derivation, and the
// `Authorization` header. Every rule quoted in the comments below is that
// document's wording. No AWS SDK is linked, and no third-party implementation
// or fixture was consulted: the vector tests in this file recompute their
// expectations from the documented algorithm.
// ---------------------------------------------------------------------------

/// AWS's `UriEncode()`: "URI encode every byte except the unreserved
/// characters: 'A'-'Z', 'a'-'z', '0'-'9', '-', '.', '_', and '~'", with each
/// encoded byte written as `%` and two *uppercase* hexadecimal digits.
///
/// `encode_slash` is the document's carve-out — "Encode the forward slash
/// character, '/', everywhere except in the object key name" — so a path keeps
/// its separators and a query name or value does not.
fn aws_uri_encode(bytes: &[u8], encode_slash: bool) -> String {
    let mut encoded = String::with_capacity(bytes.len());
    for byte in bytes {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char);
            }
            b'/' if !encode_slash => encoded.push('/'),
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

fn hex_lowercase(bytes: &[u8]) -> String {
    let mut rendered = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        rendered.push_str(&format!("{byte:02x}"));
    }
    rendered
}

/// `Hex(SHA256Hash(<payload>))`, in lowercase hexadecimal.
fn sha256_hex(bytes: &[u8]) -> String {
    hex_lowercase(&Sha256::digest(bytes))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// The two timestamps a signature needs: `RequestDateTime`, "the current UTC
/// time in ISO 8601 format (for example, `20130524T000000Z`)", and the
/// `YYYYMMDD` date of the credential scope.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SigningInstant {
    request_date_time: String,
    date_stamp: String,
}

fn signing_instant(now: SystemTime) -> Result<SigningInstant, ConnectorFailure> {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            ConnectorFailure::invariant("connector signing clock is before the Unix epoch")
        })?
        .as_secs();
    let days = i64::try_from(seconds / 86_400).expect("a Unix day count fits in i64");
    let time_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    Ok(SigningInstant {
        request_date_time: format!(
            "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
            time_of_day / 3_600,
            (time_of_day % 3_600) / 60,
            time_of_day % 60,
        ),
        date_stamp: format!("{year:04}{month:02}{day:02}"),
    })
}

/// Days since 1970-01-01 to a proleptic Gregorian calendar date.
///
/// The engine has no date library in this crate and a signature needs exactly
/// one calendar conversion, so it is done here rather than by adding a
/// dependency to the signing path.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * shifted_month + 2) / 5 + 1)
        .expect("a day of month fits in u32");
    let month = u32::try_from(if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    })
    .expect("a month fits in u32");
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// `CanonicalUri`: "The URI-encoded version of the absolute path component
/// URI... If the absolute path is empty, use a forward slash character (`/`)."
///
/// Amazon S3 is the documented exception: its canonical URI is the request's
/// own absolute path, which is why the specification's example shows
/// `/amzn-s3-demo-bucket/myphoto.jpg` used as written and why `UriEncode()`
/// tells you not to encode the forward slash inside an object key name. Every
/// other service canonicalises the path the request carries by URI-encoding it,
/// and both sides derive that from the same bytes on the wire, so a path this
/// SDK already percent-encoded stays consistent either way.
fn canonical_uri(service: &str, path: &str) -> String {
    let path = if path.is_empty() { "/" } else { path };
    if service == "s3" {
        path.to_owned()
    } else {
        aws_uri_encode(path.as_bytes(), false)
    }
}

/// `CanonicalQueryString`: "You URI-encode each name and value individually.
/// You must also sort the parameters in the canonical query string
/// alphabetically by key name. The sorting occurs after encoding." A parameter
/// with no value canonicalises to `UriEncode(name) + "=" + ""`.
fn canonical_query_string(query: Option<&str>) -> String {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return String::new();
    };
    let mut parameters = query
        .split('&')
        .filter(|parameter| !parameter.is_empty())
        .map(|parameter| {
            let (name, value) = match parameter.split_once('=') {
                Some((name, value)) => (name, value),
                None => (parameter, ""),
            };
            (
                aws_uri_encode(&percent_decode_str(name).collect::<Vec<u8>>(), true),
                aws_uri_encode(&percent_decode_str(value).collect::<Vec<u8>>(), true),
            )
        })
        .collect::<Vec<_>>();
    parameters.sort();
    parameters
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// The `Host` header value the request will actually carry, which is what the
/// canonical headers must name.
fn host_header_value(url: &Url) -> Result<String, ConnectorFailure> {
    let host = url.host_str().ok_or_else(|| {
        ConnectorFailure::invariant("connector request URL carries no host to sign")
    })?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// AWS: for values "trim any leading or trailing spaces" and "convert
/// sequential spaces to a single space".
fn canonical_header_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `CanonicalHeaders` and `SignedHeaders`.
///
/// The document requires the `host` header, the `Content-Type` header when the
/// request carries one, and "Any `x-amz-*` headers that you plan to include in
/// your request". Nothing else is signed: the remaining headers a client adds
/// are the volatile transport headers the same document tells you to leave out.
fn canonical_headers(host: &str, headers: &HeaderMap) -> (String, String) {
    let mut signed: Vec<(String, String)> = vec![("host".to_owned(), host.to_owned())];
    for (name, value) in headers {
        let name = name.as_str().to_ascii_lowercase();
        if name == "content-type" || name.starts_with("x-amz-") {
            signed.push((
                name,
                canonical_header_value(&String::from_utf8_lossy(value.as_bytes())),
            ));
        }
    }
    signed.sort();
    signed.dedup_by(|left, right| left.0 == right.0);
    let canonical = signed
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect::<String>();
    let names = signed
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    (canonical, names)
}

/// The canonical request: "concatenate the following strings, separated by
/// newline characters" — method, canonical URI, canonical query string,
/// canonical headers, signed headers, hashed payload.
fn canonical_request(
    method: &str,
    canonical_uri: &str,
    canonical_query_string: &str,
    canonical_headers: &str,
    signed_headers: &str,
    payload_hash: &str,
) -> String {
    format!(
        "{method}\n{canonical_uri}\n{canonical_query_string}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    )
}

/// `SigningKey = HMAC-SHA256(HMAC-SHA256(HMAC-SHA256(HMAC-SHA256("AWS4" +
/// SecretAccessKey, Date), Region), Service), "aws4_request")`.
fn sigv4_signing_key(
    secret_access_key: &str,
    date_stamp: &str,
    region: &str,
    service: &str,
) -> [u8; 32] {
    let date_key = hmac_sha256(
        format!("AWS4{secret_access_key}").as_bytes(),
        date_stamp.as_bytes(),
    );
    let date_region_key = hmac_sha256(&date_key, region.as_bytes());
    let date_region_service_key = hmac_sha256(&date_region_key, service.as_bytes());
    hmac_sha256(&date_region_service_key, SIGV4_TERMINATOR.as_bytes())
}

/// A Region code, as the credential scope spells it. AWS: "The Region code,
/// service code, and termination string must use lowercase characters."
fn validate_region(region: &str) -> Result<(), ConnectorFailure> {
    let valid = !region.is_empty()
        && region.len() <= 64
        && region.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        });
    if valid {
        Ok(())
    } else {
        // The message names the field, never the configured value.
        Err(ConnectorFailure::new(
            ConnectorErrorClass::Invariant,
            "connector_credential_not_applicable",
            "a configured AWS region is not a valid region code",
        ))
    }
}

/// Sign one rendered request in place.
///
/// The payload hash covers the exact bytes of the body this request will send;
/// there is no argument, and no declaration, that substitutes AWS's
/// `UNSIGNED-PAYLOAD` for it.
fn sign_aws_sigv4(
    service: &str,
    credential: &Credential,
    request: &mut RequestPlan,
    signing_time: SystemTime,
) -> Result<(), ConnectorFailure> {
    let access_key_id = credential
        .field(field::AWS_ACCESS_KEY_ID)?
        .expose()
        .to_owned();
    let secret_access_key = credential
        .field(field::AWS_SECRET_ACCESS_KEY)?
        .expose()
        .to_owned();
    let region = credential.field(field::AWS_REGION)?.expose().to_owned();
    validate_region(&region)?;
    if access_key_id.is_empty() || access_key_id.contains('/') {
        return Err(ConnectorFailure::new(
            ConnectorErrorClass::Invariant,
            "connector_credential_not_applicable",
            "a configured AWS access key id is not a valid credential scope component",
        ));
    }
    let session_token = credential
        .optional_field(field::AWS_SESSION_TOKEN)
        .map(|token| token.expose().to_owned());

    let instant = signing_instant(signing_time)?;
    let payload_hash = sha256_hex(request.body());

    // These three travel on the wire and are therefore part of the canonical
    // headers: AWS says the `x-amz-content-sha256` header "is required for
    // Amazon S3 AWS requests", and that a temporary credential's
    // `x-amz-security-token` "must add this header in the list of
    // CanonicalHeaders".
    request.set_header(X_AMZ_DATE, header_value(&instant.request_date_time)?, false);
    request.set_header(X_AMZ_CONTENT_SHA256, header_value(&payload_hash)?, false);
    if let Some(session_token) = &session_token {
        request.set_header(X_AMZ_SECURITY_TOKEN, header_value(session_token)?, true);
    }

    let host = host_header_value(request.url())?;
    let (canonical_header_block, signed_headers) = canonical_headers(&host, request.headers());
    let canonical = canonical_request(
        request.method().as_str(),
        &canonical_uri(service, request.url().path()),
        &canonical_query_string(request.url().query()),
        &canonical_header_block,
        &signed_headers,
        &payload_hash,
    );

    let scope = format!(
        "{}/{region}/{service}/{SIGV4_TERMINATOR}",
        instant.date_stamp
    );
    let string_to_sign = format!(
        "{SIGV4_ALGORITHM}\n{}\n{scope}\n{}",
        instant.request_date_time,
        sha256_hex(canonical.as_bytes())
    );
    let signature = hex_lowercase(&hmac_sha256(
        &sigv4_signing_key(&secret_access_key, &instant.date_stamp, &region, service),
        string_to_sign.as_bytes(),
    ));

    request.set_header(
        AUTHORIZATION,
        header_value(&format!(
            "{SIGV4_ALGORITHM} Credential={access_key_id}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
        ))?,
        true,
    );
    Ok(())
}

fn header_value(value: &str) -> Result<HeaderValue, ConnectorFailure> {
    // The message names the plan, never the value: a rejected header value is
    // the secret itself.
    HeaderValue::from_str(value).map_err(|_| {
        ConnectorFailure::new(
            ConnectorErrorClass::Invariant,
            "connector_credential_not_applicable",
            "a resolved connector credential is not a valid header value",
        )
    })
}

/// RFC 9110's `token` grammar, which is what an `auth-scheme` is.
///
/// A scheme with a space, a comma, or a control character in it would forge a
/// second header field value, so the two plans that carry a provider-published
/// scheme share one answer about what a scheme may be.
fn is_authorization_scheme_token(scheme: &str) -> bool {
    !scheme.is_empty()
        && scheme.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_' | '~' | '+')
        })
}

#[cfg(test)]
mod tests {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
    use serde_json::json;

    use super::*;
    use crate::sdk::errors::ConnectorErrorClass;
    use crate::sdk::operation::{Operation, Origin, RequestPlan, Required};
    use donat_value_contract::ValueScalar;
    use reqwest::StatusCode;

    /// A value that must never appear anywhere but the applied wire form.
    const SENTINEL: &str = "donat-secret-sentinel-do-not-log";

    fn origin() -> Origin {
        Origin::parse("https://provider.example.test").expect("static test origin is valid")
    }

    fn plan() -> RequestPlan {
        Operation::get("item.list", "/v1/items")
            .version("1.0.0")
            .query_input("state", "state")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid")
            .plan_request(&origin(), &json!({ "state": "open" }))
            .expect("request renders")
    }

    fn credential() -> Credential {
        Credential::from_fields([
            (field::SECRET, Secret::new(SENTINEL)),
            (field::CLIENT_ID, Secret::new("client-1")),
            (field::CLIENT_SECRET, Secret::new(SENTINEL)),
        ])
    }

    fn header(plan: &RequestPlan, name: reqwest::header::HeaderName) -> Option<String> {
        plan.headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    /// `sdk_auth_plans_apply_exactly`: each plan produces its exact wire form;
    /// a plan cannot be added from a provider module.
    #[test]
    fn sdk_auth_plans_apply_exactly() {
        // ApiKeyHeader: a fixed header name, the secret value.
        let mut request = plan();
        AuthPlan::api_key_header("X-Api-Key")
            .expect("a static header name is valid")
            .apply(&credential(), &mut request, None)
            .expect("the api key applies");
        assert_eq!(
            header(
                &request,
                reqwest::header::HeaderName::from_static("x-api-key")
            ),
            Some(SENTINEL.to_owned())
        );
        assert_eq!(request.url().query(), Some("state=open"));

        // ApiKeyQuery: a fixed query key appended to the declared query, with
        // the secret percent-encoded.
        let mut request = plan();
        AuthPlan::api_key_query("api_key")
            .expect("a static query key is valid")
            .apply(&credential(), &mut request, None)
            .expect("the api key applies");
        assert_eq!(
            request.url().query(),
            Some("state=open&api_key=donat%2Dsecret%2Dsentinel%2Ddo%2Dnot%2Dlog")
        );
        assert!(header(&request, AUTHORIZATION).is_none());

        // Bearer.
        let mut request = plan();
        AuthPlan::bearer()
            .apply(&credential(), &mut request, None)
            .expect("the bearer token applies");
        assert_eq!(
            header(&request, AUTHORIZATION),
            Some(format!("Bearer {SENTINEL}"))
        );

        // ApiKeyAuthorizationScheme: the provider's own token type in front of
        // a configured key. Discord publishes `Bot`, and the same token under
        // `Bearer` authenticates as a different identity.
        let mut request = plan();
        AuthPlan::api_key_authorization_scheme("Bot")
            .expect("a static scheme token is valid")
            .apply(&credential(), &mut request, None)
            .expect("the scheme-carrying key applies");
        assert_eq!(
            header(&request, AUTHORIZATION),
            Some(format!("Bot {SENTINEL}"))
        );
        assert!(
            request
                .headers()
                .get(AUTHORIZATION)
                .expect("the plan set one")
                .is_sensitive(),
            "a configured key under a provider scheme is still a credential"
        );
        // The scheme is a token: a value carrying a space, a comma, or a
        // control character would forge a second header field value.
        for forged in ["Bot token", "Bot,Bearer", "Bot\r\nX-Injected: 1", ""] {
            assert!(
                AuthPlan::api_key_authorization_scheme(Box::leak(
                    forged.to_owned().into_boxed_str()
                ))
                .is_err(),
                "`{forged}` is not an authorization scheme"
            );
        }

        // ApiKeyAuthorizationParameter: a scheme followed by one named
        // authentication parameter. PagerDuty publishes "The API Key with
        // format `Token token=<API_KEY>`", which is RFC 9110's `auth-param`
        // production rather than its `token68` one.
        let mut request = plan();
        AuthPlan::api_key_authorization_parameter("Token", "token")
            .expect("a static scheme and parameter are valid")
            .apply(&credential(), &mut request, None)
            .expect("the parameter-carrying key applies");
        assert_eq!(
            header(&request, AUTHORIZATION),
            Some(format!("Token token={SENTINEL}"))
        );
        assert!(
            request
                .headers()
                .get(AUTHORIZATION)
                .expect("the plan set one")
                .is_sensitive()
        );
        assert_eq!(
            AuthPlan::api_key_authorization_parameter("Token", "token").expect("valid"),
            AuthPlan::api_key_authorization_parameter("Token", "token").expect("valid")
        );
        assert_ne!(
            AuthPlan::api_key_authorization_parameter("Token", "token").expect("valid"),
            AuthPlan::api_key_authorization_scheme("Token").expect("valid"),
            "`Token token=<key>` and `Token <key>` are different wire forms"
        );
        // Both names obey the same `token` grammar the scheme does, in both
        // positions.
        for (scheme, parameter) in [
            ("Token", "token key"),
            ("Token token=", "token"),
            ("Token", "token\r\nX-Injected: 1"),
            ("", "token"),
            ("Token", ""),
        ] {
            assert!(
                AuthPlan::api_key_authorization_parameter(
                    Box::leak(scheme.to_owned().into_boxed_str()),
                    Box::leak(parameter.to_owned().into_boxed_str()),
                )
                .is_err(),
                "`{scheme} {parameter}=` is not an authorization credential"
            );
        }

        // AuthorizationCredential: the key itself, with no scheme in front of
        // it. It is deliberately *not* the `Bearer` form — Linear publishes
        // both and they authenticate different credential kinds.
        let mut request = plan();
        AuthPlan::authorization_credential()
            .apply(&credential(), &mut request, None)
            .expect("the authorization credential applies");
        assert_eq!(header(&request, AUTHORIZATION), Some(SENTINEL.to_owned()));
        assert!(
            !header(&request, AUTHORIZATION)
                .expect("the plan set one")
                .starts_with("Bearer "),
            "a scheme the provider does not publish is never prepended"
        );
        assert!(
            request
                .headers()
                .get(AUTHORIZATION)
                .expect("the plan set one")
                .is_sensitive(),
            "the header carrying a credential is marked sensitive"
        );

        // Basic: base64(user:secret).
        let mut request = plan();
        AuthPlan::basic("api-user")
            .expect("a static username is valid")
            .apply(&credential(), &mut request, None)
            .expect("basic auth applies");
        assert_eq!(
            header(&request, AUTHORIZATION),
            Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("api-user:{SENTINEL}"))
            ))
        );

        // OAuth2 client credentials: the token request is a POST to the
        // declared token origin, and the token — never the client secret —
        // is what reaches the provider request.
        let plan_oauth = AuthPlan::oauth2_client_credentials(
            Origin::parse("https://auth.example.test").expect("token origin is valid"),
            "/oauth/token",
            &["read", "write"],
        )
        .expect("a static token endpoint is valid");
        let token_request = plan_oauth
            .token_request(&credential())
            .expect("the token request renders")
            .expect("this plan fetches a token");
        assert_eq!(token_request.method(), reqwest::Method::POST);
        assert_eq!(
            token_request.url().as_str(),
            "https://auth.example.test/oauth/token"
        );
        assert_eq!(
            header(&token_request, CONTENT_TYPE),
            Some("application/x-www-form-urlencoded".to_owned())
        );
        assert_eq!(
            header(&token_request, AUTHORIZATION),
            Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("client-1:{SENTINEL}"))
            ))
        );
        assert_eq!(
            std::str::from_utf8(token_request.body()).expect("the token body is ASCII"),
            "grant_type=client_credentials&scope=read%20write"
        );

        let mut request = plan();
        plan_oauth
            .apply(
                &credential(),
                &mut request,
                Some(&AccessToken::new("issued-access-token")),
            )
            .expect("the issued token applies");
        assert_eq!(
            header(&request, AUTHORIZATION),
            Some("Bearer issued-access-token".to_owned()),
            "the client secret never reaches the provider request"
        );
        assert!(!format!("{:?}", request.headers()).contains(SENTINEL));

        // Every other plan declares that it needs no token.
        assert!(
            AuthPlan::bearer()
                .token_request(&credential())
                .expect("no token request renders")
                .is_none()
        );
    }

    /// `sdk_api_key_path_segment_is_encoded_and_redacted` (spec 013 §1): the
    /// credential becomes the request's first path segment, percent-encoded, and
    /// a sentinel placed in that position appears in nothing but the URL the
    /// transport receives.
    #[test]
    fn sdk_api_key_path_segment_is_encoded_and_redacted() {
        let plan = AuthPlan::api_key_path_segment("bot").expect("a static prefix is valid");
        assert_eq!(plan.required_fields(), [field::SECRET]);

        // A token in Telegram's own published shape: the `:` is the one
        // character outside RFC 3986's unreserved set, so it — and nothing
        // else — is percent-encoded.
        let mut request = plan_for("/sendMessage");
        plan.apply(
            &Credential::secret("123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11"),
            &mut request,
            None,
        )
        .expect("the path segment applies");
        assert_eq!(
            request.url().path(),
            "/bot123456%3AABC-DEF1234ghIkl-zyx57W2v1u123ew11/sendMessage"
        );
        assert_eq!(request.url().host_str(), Some("provider.example.test"));
        assert_eq!(request.url().query(), Some("state=open"));
        assert!(
            request.headers().get(AUTHORIZATION).is_none(),
            "this plan sends no credential header at all"
        );

        // A hostile credential value stays one segment: it cannot add a path
        // element, a query, a fragment, or an authority.
        let mut hostile = plan_for("/sendMessage");
        plan.apply(
            &Credential::secret("../../attacker?x=1#y/z@evil"),
            &mut hostile,
            None,
        )
        .expect("the path segment applies");
        assert_eq!(
            hostile.url().path(),
            "/bot%2E%2E%2F%2E%2E%2Fattacker%3Fx%3D1%23y%2Fz%40evil/sendMessage",
            "a dot segment, a separator, a query, a fragment, and an authority \
             marker all stay inside the one segment"
        );
        assert_eq!(hostile.url().host_str(), Some("provider.example.test"));
        assert_eq!(hostile.url().query(), Some("state=open"));
        assert_eq!(hostile.url().fragment(), None);

        // The redaction proof: a sentinel in the credential position reaches the
        // URL the transport is handed, and nothing else this crate can print.
        let mut sentinel = plan_for("/sendMessage");
        plan.apply(&Credential::secret(SENTINEL), &mut sentinel, None)
            .expect("the path segment applies");
        assert_eq!(
            sentinel.url().path(),
            format!("/bot{SENTINEL}/sendMessage"),
            "the URL the transport is handed carries the credential; nothing else may"
        );
        assert!(sentinel.url_carries_credential());
        assert_eq!(
            sentinel.redacted_url(),
            "https://provider.example.test/<redacted>"
        );
        let surface = format!(
            "{sentinel:?} {} {:?} {:?} {:?} {:?}",
            sentinel.redacted_url(),
            sentinel.headers(),
            plan,
            crate::sdk::connector::CredentialSpec::for_plan(
                AuthPlan::api_key_path_segment("bot").expect("a static prefix is valid")
            ),
            Credential::secret(SENTINEL),
        );
        assert!(
            !surface.contains(SENTINEL),
            "the credential segment must not appear anywhere: {surface}"
        );

        // The same rule now covers the other URL-borne plan: a query api key is
        // a credential in the URL and is redacted the same way.
        let mut query = plan_for("/sendMessage");
        AuthPlan::api_key_query("api_key")
            .expect("a static query key is valid")
            .apply(&Credential::secret(SENTINEL), &mut query, None)
            .expect("the api key applies");
        assert!(query.url_carries_credential());
        assert!(!format!("{query:?}").contains(SENTINEL));

        // And the two-value form Trello forced: both halves are secrets, both
        // reach the query, and neither is printable.
        const KEY_SENTINEL: &str = "donat-pair-key-sentinel-do-not-log";
        let pair =
            AuthPlan::api_key_query_pair("key", "token").expect("two static query keys are valid");
        assert_eq!(pair.required_fields(), [field::API_KEY, field::SECRET]);
        let mut both = plan_for("/sendMessage");
        pair.apply(
            &Credential::from_fields([
                (field::API_KEY, Secret::new(KEY_SENTINEL)),
                (field::SECRET, Secret::new(SENTINEL)),
            ]),
            &mut both,
            None,
        )
        .expect("both halves apply");
        assert_eq!(
            both.url().query(),
            Some(
                format!(
                    "state=open&key={}&token={}",
                    KEY_SENTINEL.replace('-', "%2D"),
                    SENTINEL.replace('-', "%2D")
                )
                .as_str()
            ),
            "the declared query survives and both halves are appended after it"
        );
        assert!(both.url_carries_credential());
        let surface = format!("{both:?} {} {pair:?}", both.redacted_url());
        for sentinel in [KEY_SENTINEL, SENTINEL] {
            assert!(!surface.contains(sentinel), "{surface}");
        }

        // Either half missing is a refusal before a byte leaves, because
        // neither authenticates alone.
        for partial in [
            Credential::from_fields([(field::API_KEY, Secret::new(KEY_SENTINEL))]),
            Credential::secret(SENTINEL),
        ] {
            let mut request = plan_for("/sendMessage");
            assert_eq!(
                pair.apply(&partial, &mut request, None)
                    .expect_err("a half credential does not authenticate")
                    .code(),
                "connector_credential_missing_field"
            );
        }

        // The two keys are declaration material and must be distinct and
        // static: one key spelled twice would send the same value twice.
        assert!(AuthPlan::api_key_query_pair("key", "key").is_err());
        for hostile in ["", "key=", "key&token", "ke y", "key%20"] {
            assert!(
                AuthPlan::api_key_query_pair(hostile, "token").is_err(),
                "`{hostile}` is not a static query key"
            );
            assert!(AuthPlan::api_key_query_pair("key", hostile).is_err());
        }

        // Every other plan leaves the URL printable, because none of them puts
        // a secret in it.
        let mut bearer = plan_for("/sendMessage");
        AuthPlan::bearer()
            .apply(&Credential::secret(SENTINEL), &mut bearer, None)
            .expect("the bearer token applies");
        assert!(!bearer.url_carries_credential());
        assert_eq!(bearer.redacted_url(), bearer.url().as_str());

        // The prefix is declaration material and obeys the segment grammar.
        for prefix in [
            "bo/t", "bot?", "bot#", "bot ", "b o t", "{tenant}", "bot%2F",
        ] {
            assert!(
                AuthPlan::api_key_path_segment(prefix).is_err(),
                "prefix {prefix} must not build a plan"
            );
        }
        assert!(AuthPlan::api_key_path_segment("").is_ok());
        assert!(AuthPlan::api_key_path_segment(&"b".repeat(33)).is_err());

        // A plan whose credential field is absent never sends the request.
        assert_eq!(
            plan.apply(&Credential::from_fields([]), &mut plan_for("/x"), None)
                .expect_err("an unconfigured credential never renders a path")
                .code(),
            "connector_credential_missing_field"
        );
        assert!(
            plan.token_request(&Credential::secret(SENTINEL))
                .expect("no token request renders")
                .is_none()
        );
    }

    /// One rendered request against a declared path, with a declared query, so
    /// the path-segment plan is proven to leave both alone.
    fn plan_for(path: &str) -> RequestPlan {
        Operation::post("message.send", path)
            .version("1.0.0")
            .query_input("state", "state")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid")
            .plan_request(&origin(), &json!({ "state": "open" }))
            .expect("request renders")
    }

    #[test]
    fn an_applied_credential_header_is_marked_sensitive() {
        let mut request = plan();
        AuthPlan::bearer()
            .apply(&credential(), &mut request, None)
            .expect("the bearer token applies");
        assert!(
            request
                .headers()
                .get(AUTHORIZATION)
                .expect("the header was applied")
                .is_sensitive(),
            "an applied credential is marked sensitive so a header dump redacts it"
        );
    }

    #[test]
    fn a_secret_is_not_printable() {
        let secret = Secret::new(SENTINEL);
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(!format!("{:?}", credential()).contains(SENTINEL));
        assert!(!format!("{:?}", AccessToken::new(SENTINEL)).contains(SENTINEL));
    }

    #[test]
    fn a_plan_whose_credential_field_is_absent_fails_before_the_request_leaves() {
        let empty = Credential::from_fields([]);
        let failure = AuthPlan::bearer()
            .apply(&empty, &mut plan(), None)
            .expect_err("a missing credential field is a declaration failure");
        assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
        assert_eq!(failure.code(), "connector_credential_missing_field");
        assert_eq!(
            AuthPlan::bearer().required_fields(),
            [field::SECRET],
            "each plan declares exactly the credential fields it applies"
        );
        assert_eq!(
            AuthPlan::oauth2_client_credentials(
                Origin::parse("https://auth.example.test").expect("token origin is valid"),
                "/oauth/token",
                &[],
            )
            .expect("a scopeless client-credentials plan is valid")
            .required_fields(),
            [field::CLIENT_ID, field::CLIENT_SECRET]
        );
    }

    #[test]
    fn an_oauth2_plan_without_an_issued_token_never_sends_the_request_unauthenticated() {
        let failure = AuthPlan::oauth2_client_credentials(
            Origin::parse("https://auth.example.test").expect("token origin is valid"),
            "/oauth/token",
            &["read"],
        )
        .expect("a static token endpoint is valid")
        .apply(&credential(), &mut plan(), None)
        .expect_err("an unissued token is not an unauthenticated request");
        assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
    }

    /// The authorization-code plan configures nothing, applies exactly the
    /// header the credential lifecycle produced, and refuses everything else —
    /// including a request with no credential at all, which is the shape
    /// [[043-the-credential-seam-refuses-before-it-sends]] exists to forbid.
    #[test]
    fn the_authorization_code_plan_applies_a_stored_credential_and_nothing_else() {
        let plan_declaration = AuthPlan::oauth2_authorization_code();
        assert_eq!(
            plan_declaration.required_fields(),
            [] as [&str; 0],
            "a stored OAuth2 credential is not deploy-time configuration"
        );

        let mut request = plan();
        plan_declaration
            .apply(
                &Credential::from_fields([]),
                &mut request,
                Some(&AccessToken::new(format!("Bearer {SENTINEL}"))),
            )
            .expect("an applied authorization reaches the request");
        let applied = request
            .headers()
            .get(AUTHORIZATION)
            .expect("the header was applied");
        assert_eq!(
            applied.to_str().ok(),
            Some(format!("Bearer {SENTINEL}").as_str())
        );
        assert!(applied.is_sensitive());

        let missing = plan_declaration
            .apply(&Credential::from_fields([]), &mut plan(), None)
            .expect_err("no stored credential is not an unauthenticated request");
        assert_eq!(missing.class(), ConnectorErrorClass::Invariant);
        assert_eq!(missing.code(), "connector_credential_not_applicable");

        for wrong in ["", "Bearer ", "Basic dXNlcjpwYXNz", SENTINEL] {
            let refused = plan_declaration
                .apply(
                    &Credential::from_fields([]),
                    &mut plan(),
                    Some(&AccessToken::new(wrong)),
                )
                .expect_err("only a Bearer authorization is applied");
            assert_eq!(refused.class(), ConnectorErrorClass::Invariant);
        }
    }

    #[test]
    fn a_plan_declaration_is_static() {
        for name in ["X-Api-{tenant}", "", "X Api Key", "Authorization"] {
            assert!(
                AuthPlan::api_key_header(name).is_err(),
                "header name {name} must not build a plan"
            );
        }
        for key in ["{sort}", "", "api key"] {
            assert!(
                AuthPlan::api_key_query(key).is_err(),
                "query key {key} must not build a plan"
            );
        }
        assert!(AuthPlan::basic("user:name").is_err());
        assert!(
            AuthPlan::oauth2_client_credentials(
                Origin::parse("https://auth.example.test").expect("token origin is valid"),
                "token",
                &[],
            )
            .is_err(),
            "a token path must be an absolute static path"
        );
    }

    // -----------------------------------------------------------------------
    // AWS Signature Version 4 vectors.
    //
    // Every expectation below is recomputed inside the test from AWS's
    // published algorithm — the canonical request layout, the string to sign,
    // and the HMAC chain — rather than copied from any implementation or test
    // suite. The test's arithmetic and the plan's arithmetic are written
    // separately and have to agree.
    // -----------------------------------------------------------------------

    /// AWS's own example timestamp, `20130524T000000Z`, as seconds since the
    /// Unix epoch.
    const VECTOR_SIGNING_SECONDS: u64 = 1_369_353_600;
    const VECTOR_ACCESS_KEY_ID: &str = "AKIDONATEXAMPLE";
    const VECTOR_REGION: &str = "eu-west-1";
    const VECTOR_SESSION_TOKEN: &str = "donat-session-token-sentinel";

    fn vector_signing_time() -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(VECTOR_SIGNING_SECONDS)
    }

    fn aws_credential(with_session_token: bool) -> Credential {
        let mut fields = vec![
            (field::AWS_ACCESS_KEY_ID, Secret::new(VECTOR_ACCESS_KEY_ID)),
            (field::AWS_SECRET_ACCESS_KEY, Secret::new(SENTINEL)),
            (field::AWS_REGION, Secret::new(VECTOR_REGION)),
        ];
        if with_session_token {
            fields.push((field::AWS_SESSION_TOKEN, Secret::new(VECTOR_SESSION_TOKEN)));
        }
        Credential::from_fields(fields)
    }

    fn s3_origin() -> Origin {
        Origin::parse("https://s3.eu-west-1.amazonaws.com").expect("a static AWS origin is valid")
    }

    /// One `PUT` with a body, a declared media type, and a query whose
    /// parameters are declared out of canonical order.
    fn aws_put_request(body: &str) -> RequestPlan {
        Operation::put("object.put", "/donat-fixtures/{key}")
            .version("1.0.0")
            .path_param("key", ValueScalar::String)
            .query_static("x-id", "PutObject")
            .query_static("acl", "")
            .static_header("Content-Type", "text/plain")
            .body(crate::sdk::operation::JsonTemplate::input("payload"))
            .success_statuses([StatusCode::OK])
            .build()
            .expect("a static declaration is valid")
            .plan_request(
                &s3_origin(),
                &json!({ "key": "report.json", "payload": body }),
            )
            .expect("the request renders")
    }

    /// The test's own transcription of the documented hash and MAC primitives,
    /// deliberately not the plan's.
    fn expected_sha256_hex(bytes: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn expected_hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("a MAC key is any length");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    /// `sigv4_canonical_request_is_exact`: the canonical request is the six
    /// documented lines, in order, with nothing else between them.
    #[test]
    fn sigv4_canonical_request_is_exact() {
        let body = "hello donat";
        let request = aws_put_request(body);
        // The hash is over the bytes the *request* carries, which is the
        // rendered JSON body rather than the value that was bound into it.
        let payload_hash = expected_sha256_hex(request.body());

        let host = host_header_value(request.url()).expect("the origin carries a host");
        assert_eq!(host, "s3.eu-west-1.amazonaws.com");

        // The plan's own canonicalisation, with the two headers it adds before
        // it canonicalises anything.
        let mut signed = aws_put_request(body);
        sign_aws_sigv4(
            "s3",
            &aws_credential(false),
            &mut signed,
            vector_signing_time(),
        )
        .expect("the request signs");
        let (canonical_header_block, signed_headers) = canonical_headers(&host, signed.headers());

        let canonical = canonical_request(
            signed.method().as_str(),
            &canonical_uri("s3", signed.url().path()),
            &canonical_query_string(signed.url().query()),
            &canonical_header_block,
            &signed_headers,
            &payload_hash,
        );

        assert_eq!(
            canonical,
            format!(
                "PUT\n\
                 /donat-fixtures/report%2Ejson\n\
                 acl=&x-id=PutObject\n\
                 content-type:text/plain\n\
                 host:s3.eu-west-1.amazonaws.com\n\
                 x-amz-content-sha256:{payload_hash}\n\
                 x-amz-date:20130524T000000Z\n\
                 \n\
                 content-type;host;x-amz-content-sha256;x-amz-date\n\
                 {payload_hash}"
            ),
            "the canonical request is method, URI, query, headers, signed headers, payload hash"
        );

        // Amazon S3 signs the absolute path the request carries; every other
        // service signs the URI-encoded form of that same path, and both sides
        // derive it from the identical bytes on the wire.
        assert_eq!(
            canonical_uri("s3", "/donat-fixtures/report%2Ejson"),
            "/donat-fixtures/report%2Ejson"
        );
        assert_eq!(
            canonical_uri("ses", "/v2/email/templates/welcome%2Dmail"),
            "/v2/email/templates/welcome%252Dmail"
        );
        assert_eq!(canonical_uri("sqs", ""), "/", "an empty path is a slash");
    }

    /// `sigv4_signing_key_is_derived_correctly`: the four documented HMAC
    /// steps, in order, with `AWS4` prefixed to the secret access key.
    #[test]
    fn sigv4_signing_key_is_derived_correctly() {
        let date_key = expected_hmac(format!("AWS4{SENTINEL}").as_bytes(), b"20130524");
        let date_region_key = expected_hmac(&date_key, VECTOR_REGION.as_bytes());
        let date_region_service_key = expected_hmac(&date_region_key, b"s3");
        let expected = expected_hmac(&date_region_service_key, b"aws4_request");

        assert_eq!(
            sigv4_signing_key(SENTINEL, "20130524", VECTOR_REGION, "s3").to_vec(),
            expected,
            "the signing key is HMAC over date, region, service, and the terminator"
        );

        // Each step is genuinely keyed on the previous one: changing any single
        // input changes the key.
        for (secret, date, region, service) in [
            ("other-secret", "20130524", VECTOR_REGION, "s3"),
            (SENTINEL, "20130525", VECTOR_REGION, "s3"),
            (SENTINEL, "20130524", "us-east-1", "s3"),
            (SENTINEL, "20130524", VECTOR_REGION, "sqs"),
        ] {
            assert_ne!(
                sigv4_signing_key(secret, date, region, service).to_vec(),
                expected
            );
        }

        // And the signature is that key applied to the documented string to
        // sign, which the whole plan has to reproduce.
        let mut signed = aws_put_request("hello donat");
        sign_aws_sigv4(
            "s3",
            &aws_credential(false),
            &mut signed,
            vector_signing_time(),
        )
        .expect("the request signs");
        let host = host_header_value(signed.url()).expect("the origin carries a host");
        let (canonical_header_block, signed_headers) = canonical_headers(&host, signed.headers());
        let canonical = canonical_request(
            signed.method().as_str(),
            &canonical_uri("s3", signed.url().path()),
            &canonical_query_string(signed.url().query()),
            &canonical_header_block,
            &signed_headers,
            &expected_sha256_hex(b"\"hello donat\""),
        );
        let scope = format!("20130524/{VECTOR_REGION}/s3/aws4_request");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n20130524T000000Z\n{scope}\n{}",
            expected_sha256_hex(canonical.as_bytes())
        );
        let signature = expected_hmac(&expected, string_to_sign.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        assert_eq!(
            header(&signed, AUTHORIZATION),
            Some(format!(
                "AWS4-HMAC-SHA256 Credential={VECTOR_ACCESS_KEY_ID}/{scope}, \
                 SignedHeaders={signed_headers}, Signature={signature}"
            )),
            "the Authorization header is the documented algorithm, credential scope, signed \
             header list, and signature"
        );
    }

    /// `sigv4_signed_headers_match`: the signed header list is exactly the
    /// lowercase, alphabetically sorted names of the canonical header block,
    /// and a session token is one of them.
    #[test]
    fn sigv4_signed_headers_match() {
        for with_session_token in [false, true] {
            let mut request = aws_put_request("hello donat");
            sign_aws_sigv4(
                "s3",
                &aws_credential(with_session_token),
                &mut request,
                vector_signing_time(),
            )
            .expect("the request signs");
            let host = host_header_value(request.url()).expect("the origin carries a host");
            let (canonical_header_block, signed_headers) =
                canonical_headers(&host, request.headers());

            let named_in_block = canonical_header_block
                .lines()
                .map(|line| {
                    line.split_once(':')
                        .expect("a canonical header line is name:value")
                        .0
                        .to_owned()
                })
                .collect::<Vec<_>>();
            assert_eq!(
                signed_headers,
                named_in_block.join(";"),
                "the signed header list names exactly the canonical header block"
            );
            let mut sorted = named_in_block.clone();
            sorted.sort();
            assert_eq!(named_in_block, sorted, "canonical headers are sorted");
            assert!(
                named_in_block
                    .iter()
                    .all(|name| name.chars().all(|c| !c.is_ascii_uppercase())),
                "canonical header names are lowercase"
            );
            assert!(named_in_block.contains(&"host".to_owned()));
            assert!(named_in_block.contains(&"content-type".to_owned()));
            assert!(named_in_block.contains(&"x-amz-date".to_owned()));
            assert!(named_in_block.contains(&"x-amz-content-sha256".to_owned()));
            assert_eq!(
                named_in_block.contains(&"x-amz-security-token".to_owned()),
                with_session_token,
                "a temporary credential's session token is sent and signed"
            );
            assert_eq!(
                header(
                    &request,
                    reqwest::header::HeaderName::from_static("x-amz-security-token")
                ),
                with_session_token.then(|| VECTOR_SESSION_TOKEN.to_owned())
            );
            // The Authorization header is built after the list, so the two
            // cannot drift apart.
            assert!(
                header(&request, AUTHORIZATION)
                    .expect("the request is signed")
                    .contains(&format!("SignedHeaders={signed_headers},"))
            );
        }

        // Values are trimmed and their internal runs of whitespace collapsed,
        // as the specification requires.
        assert_eq!(canonical_header_value("  a   b  "), "a b");
    }

    /// `sigv4_query_is_canonicalized`: names and values are URI-encoded
    /// individually and sorted after encoding, and a parameter with no value
    /// keeps its `=`.
    #[test]
    fn sigv4_query_is_canonicalized() {
        assert_eq!(canonical_query_string(None), "");
        assert_eq!(canonical_query_string(Some("")), "");
        assert_eq!(
            canonical_query_string(Some("prefix=somePrefix&marker=someMarker&max-keys=2")),
            "marker=someMarker&max-keys=2&prefix=somePrefix",
            "parameters are sorted alphabetically by key name"
        );
        assert_eq!(
            canonical_query_string(Some("acl")),
            "acl=",
            "a subresource parameter canonicalises to name + '=' + empty value"
        );
        assert_eq!(
            canonical_query_string(Some("list%2Dtype=2&prefix=a%2Fb%20c")),
            "list-type=2&prefix=a%2Fb%20c",
            "each name and value is decoded and re-encoded with the documented UriEncode"
        );
        assert_eq!(
            canonical_query_string(Some("k=b&k=a")),
            "k=a&k=b",
            "a repeated key is ordered by its encoded value"
        );
        assert_eq!(
            canonical_query_string(Some("z=1&Z=2")),
            "Z=2&z=1",
            "the sort is over the encoded bytes, so it is case sensitive"
        );

        // Unreserved characters survive; everything else becomes uppercase
        // percent triples, and a space is %20 rather than '+'.
        assert_eq!(aws_uri_encode(b"-._~Aa0", true), "-._~Aa0");
        assert_eq!(aws_uri_encode(b"a b+c", true), "a%20b%2Bc");
        assert_eq!(aws_uri_encode(b"a/b", true), "a%2Fb");
        assert_eq!(aws_uri_encode(b"a/b", false), "a/b");
    }

    /// `sigv4_payload_hash_covers_exact_bytes`: the hash is over the body this
    /// request will send, and an unsigned payload is not an option.
    #[test]
    fn sigv4_payload_hash_covers_exact_bytes() {
        for body in ["hello donat", "hello donat!", ""] {
            let mut request = aws_put_request(body);
            let exact = request.body().to_vec();
            sign_aws_sigv4(
                "s3",
                &aws_credential(false),
                &mut request,
                vector_signing_time(),
            )
            .expect("the request signs");
            assert_eq!(
                header(
                    &request,
                    reqwest::header::HeaderName::from_static("x-amz-content-sha256")
                ),
                Some(expected_sha256_hex(&exact)),
                "the payload hash covers the exact bytes the request carries"
            );
            assert_eq!(
                request.body(),
                exact.as_slice(),
                "signing does not rewrite the body it hashed"
            );
        }

        // A body with no request body at all still hashes the empty string
        // rather than declaring the payload unsigned.
        let mut empty = Operation::get("object.list", "/donat-fixtures")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("a static declaration is valid")
            .plan_request(&s3_origin(), &json!({}))
            .expect("the request renders");
        sign_aws_sigv4(
            "s3",
            &aws_credential(false),
            &mut empty,
            vector_signing_time(),
        )
        .expect("the request signs");
        assert_eq!(
            header(
                &empty,
                reqwest::header::HeaderName::from_static("x-amz-content-sha256")
            ),
            Some(expected_sha256_hex(b"")),
        );
        for rendered in [
            header(
                &empty,
                reqwest::header::HeaderName::from_static("x-amz-content-sha256"),
            )
            .expect("the header is applied"),
            header(&empty, AUTHORIZATION).expect("the request is signed"),
        ] {
            assert!(
                !rendered.contains("UNSIGNED-PAYLOAD"),
                "an unsigned payload is not permitted"
            );
        }

        // One byte of difference is one different signature.
        let signature_of = |body: &str| {
            let mut request = aws_put_request(body);
            sign_aws_sigv4(
                "s3",
                &aws_credential(false),
                &mut request,
                vector_signing_time(),
            )
            .expect("the request signs");
            header(&request, AUTHORIZATION).expect("the request is signed")
        };
        assert_ne!(signature_of("hello donat"), signature_of("hello donau"));
    }

    /// The signing timestamp is the engine's clock, rendered in the one
    /// documented format: `YYYYMMDDTHHMMSSZ`, UTC, without milliseconds.
    #[test]
    fn a_signing_timestamp_is_the_documented_iso8601_basic_utc_format() {
        for (seconds, request_date_time, date_stamp) in [
            (0_u64, "19700101T000000Z", "19700101"),
            (VECTOR_SIGNING_SECONDS, "20130524T000000Z", "20130524"),
            // A leap day, and the last second of a year.
            (1_582_934_400, "20200229T000000Z", "20200229"),
            (1_609_459_199, "20201231T235959Z", "20201231"),
        ] {
            let instant =
                signing_instant(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
                    .expect("an epoch instant renders");
            assert_eq!(instant.request_date_time, request_date_time);
            assert_eq!(instant.date_stamp, date_stamp);
        }
    }

    /// The plan's declaration is static, and the credential scope's components
    /// cannot be forged through it.
    #[test]
    fn an_aws_plan_declaration_and_its_credential_scope_are_static() {
        for service in ["", "S3", "s3/../sqs", "s3 ", "s3-express", "{service}"] {
            assert!(
                AuthPlan::aws_sigv4(service).is_err(),
                "service code {service} must not build a plan"
            );
        }
        assert_eq!(
            AuthPlan::aws_sigv4("s3")
                .expect("a static service code is valid")
                .required_fields(),
            [
                field::AWS_ACCESS_KEY_ID,
                field::AWS_SECRET_ACCESS_KEY,
                field::AWS_REGION
            ],
            "the session token is optional, so startup does not demand one"
        );
        assert!(
            AuthPlan::aws_sigv4("s3")
                .expect("a static service code is valid")
                .token_request(&aws_credential(false))
                .expect("no token request renders")
                .is_none(),
            "a signature is computed, never fetched"
        );

        for region in ["", "eu west 1", "eu-west-1/../us-east-1", "EU-WEST-1"] {
            let credential = Credential::from_fields([
                (field::AWS_ACCESS_KEY_ID, Secret::new(VECTOR_ACCESS_KEY_ID)),
                (field::AWS_SECRET_ACCESS_KEY, Secret::new(SENTINEL)),
                (field::AWS_REGION, Secret::new(region)),
            ]);
            let failure = sign_aws_sigv4(
                "s3",
                &credential,
                &mut aws_put_request("x"),
                vector_signing_time(),
            )
            .expect_err("a configured region that is not a region code is refused");
            assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
            assert!(
                !failure.diagnostic().contains(region) || region.is_empty(),
                "a refusal names the field, never the configured value"
            );
        }

        // A missing credential field fails before the request leaves, as for
        // every other plan.
        assert_eq!(
            AuthPlan::aws_sigv4("s3")
                .expect("a static service code is valid")
                .apply(
                    &Credential::secret(SENTINEL),
                    &mut aws_put_request("x"),
                    None
                )
                .expect_err("an unconfigured AWS credential never signs")
                .code(),
            "connector_credential_missing_field"
        );
    }

    /// The secret access key reaches the derived key and nothing else: not the
    /// wire, not a log line, not an error, not a fingerprint.
    #[test]
    fn an_aws_secret_access_key_never_leaves_the_signing_step() {
        let mut request = aws_put_request("hello donat");
        AuthPlan::aws_sigv4("s3")
            .expect("a static service code is valid")
            .apply_at(
                &aws_credential(true),
                &mut request,
                None,
                vector_signing_time(),
            )
            .expect("the request signs");

        let authorization = header(&request, AUTHORIZATION).expect("the request is signed");
        let surface = format!(
            "{authorization} {:?} {:?} {} {:?}",
            request.headers(),
            aws_credential(true),
            String::from_utf8_lossy(request.body()),
            AuthPlan::aws_sigv4("s3").expect("a static service code is valid"),
        );
        assert!(
            !surface.contains(SENTINEL),
            "the secret access key must not appear anywhere: {surface}"
        );
        assert!(
            authorization.contains(VECTOR_ACCESS_KEY_ID),
            "the access key id is public credential scope material"
        );
        assert!(
            request
                .headers()
                .get(AUTHORIZATION)
                .expect("the header was applied")
                .is_sensitive()
                && request
                    .headers()
                    .get("x-amz-security-token")
                    .expect("the session token was applied")
                    .is_sensitive(),
            "an applied credential is marked sensitive so a header dump redacts it"
        );

        // A fingerprint of the plan and its declared fields carries no value.
        let specification = crate::sdk::connector::CredentialSpec::for_plan(
            AuthPlan::aws_sigv4("s3").expect("a static service code is valid"),
        );
        assert!(!format!("{specification:?}").contains(SENTINEL));
    }

    /// Applying a credential never rewrites the request's destination: the
    /// origin, path, and declared query survive every plan.
    #[test]
    fn applying_a_credential_cannot_move_the_request() {
        let declared = Operation::get("item.get", "/v1/items/{id}")
            .version("1.0.0")
            .path_param("id", ValueScalar::String)
            .success_statuses([StatusCode::OK])
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .build()
            .expect("static declaration is valid");
        for auth in [
            AuthPlan::bearer(),
            AuthPlan::api_key_header("X-Api-Key").expect("static header name"),
            AuthPlan::api_key_query("api_key").expect("static query key"),
            AuthPlan::basic("api-user").expect("static username"),
        ] {
            let mut request = declared
                .plan_request(&origin(), &json!({ "id": "42" }))
                .expect("request renders");
            auth.apply(&credential(), &mut request, None)
                .expect("the credential applies");
            assert_eq!(request.url().host_str(), Some("provider.example.test"));
            assert_eq!(request.url().scheme(), "https");
            assert_eq!(request.url().path(), "/v1/items/42");
        }
    }

    /// One plan, and only one, makes the executor go and get a token before it
    /// can send: the stored authorization-code plan is *handed* one, and every
    /// other plan spends deploy-time configuration.
    ///
    /// The two answers have to agree with [`AuthPlan::token_request`], because
    /// the registry asks this question at deploy time and the executor spends
    /// the answer per attempt ([[072]]).
    #[test]
    fn sdk_only_the_client_credentials_plan_issues_its_own_token() {
        let client_credentials = AuthPlan::oauth2_client_credentials(
            Origin::parse("https://auth.example.test").expect("token origin is valid"),
            "/oauth/token",
            &[],
        )
        .expect("a static token endpoint is valid");
        assert!(client_credentials.issues_its_own_token());
        assert!(
            client_credentials
                .token_request(&credential())
                .expect("the token request renders")
                .is_some(),
            "the plan that says it issues a token renders one"
        );

        for plan in [
            AuthPlan::bearer(),
            AuthPlan::authorization_credential(),
            AuthPlan::oauth2_authorization_code(),
            AuthPlan::oauth2_authorization_code_scheme("Zoho-oauthtoken")
                .expect("a static scheme is valid"),
            AuthPlan::api_key_header("X-Api-Key").expect("static header name"),
            AuthPlan::api_key_query("api_key").expect("static query key"),
            AuthPlan::api_key_path_segment("bot").expect("static prefix"),
            AuthPlan::basic("api-user").expect("static username"),
            AuthPlan::basic_secret_username("X").expect("static password"),
            AuthPlan::aws_sigv4("s3").expect("static service code"),
        ] {
            assert!(
                !plan.issues_its_own_token(),
                "{plan:?} does not fetch a token of its own"
            );
            assert!(
                plan.token_request(&credential())
                    .expect("no token request renders")
                    .is_none(),
                "{plan:?} renders no token request"
            );
        }
    }
}
