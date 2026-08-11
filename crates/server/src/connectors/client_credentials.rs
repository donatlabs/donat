//! The OAuth2 **client-credentials** exchange, made by the serving executor.
//!
//! This is the other half of `crate::connectors::credential`, and the two are
//! deliberately not the same code. That one applies a credential the deployment
//! *stores*: an operator authorized it once, it is sealed in
//! `donat.connector_credential`, and refresh is single-flighted by a
//! transactional row lock. This one applies a credential the deployment
//! *configures*: a client id and a client secret, from which an access token is
//! minted for one logical attempt and then dropped.
//!
//! Spec 011 §8's `oauth_client_credentials_is_not_persisted` is the rule, and it
//! is structural rather than remembered. Nothing here can reach the credential
//! store: this module holds no pool, no sealing key, and no
//! [`crate::credentials::runtime::CredentialRuntime`], and an instance whose
//! plan is client credentials declares no `config.oauth2`, so
//! `ConnectorRegistry::execute` never routes it through the stored path at all.
//! The token exists as one [`AccessToken`] local to one attempt — a type with no
//! `Serialize`, no `Display`, and a `Debug` that prints nothing — and the
//! attempt's stack frame is the whole of its lifetime.
//!
//! Three properties are worth naming because they are what a review should look
//! for.
//!
//! * **Fail closed.** A plan that says it issues its own token
//!   ([`AuthPlan::issues_its_own_token`]) and cannot produce one fails the
//!   attempt before a socket to the *provider* is opened. There is no path in
//!   which the request leaves with no `Authorization` header, which is
//!   [[034-a-declaration-the-runtime-ignores-is-a-defect]] applied to a
//!   credential, exactly as
//!   [[043-the-credential-seam-refuses-before-it-sends]] applied it to the
//!   stored one.
//! * **Bounded in time and in bytes, headers and body.** The exchange spends
//!   the operation's own deadline — spec 011 §6's "the same call, byte, and
//!   deadline budget as the operation itself" — and reads at most
//!   [`MAX_TOKEN_RESPONSE_BYTES`] of the answer, chunk by chunk, through the
//!   same transport ceiling every provider response passes.
//!   [[061-a-locked-row-is-held-for-a-bounded-exchange-and-a-grant-may-not-narrow-under-it]]
//!   found the stored path bounded around the response *headers* and unbounded
//!   over the body; that defect is not repeated here.
//! * **It classifies, it does not reclassify.** A token-endpoint failure is a
//!   [`CredentialFailure`] and crosses into the connector set through the one
//!   total mapping in `crate::connectors::credential`, so a token endpoint that
//!   throttles is `http_429` here exactly as it is there, and a Process
//!   declaring `retry_on: [http_429]` routes both the same way.

use std::net::IpAddr;
use std::time::Duration;

use donat_connectors::sdk::{
    AccessToken, AuthPlan, Credential, HostResolver, HttpTransport, RequestPlan, TransportErrorKind,
};
use serde::Deserialize;

use crate::credentials::oauth::{
    CredentialErrorClass, CredentialFailure, TOKEN_ENDPOINT_CONTRACT, TOKEN_ENDPOINT_TRANSPORT,
};

use super::credential::connector_failure;
use super::{ConnectorErrorClass, ConnectorFailure};

/// The most of a token response this executor reads.
///
/// RFC 6749 §5 responses are a few hundred bytes and the largest thing a real
/// provider adds is an ID token; 256 KiB is far past any of them. It is the
/// same number the stored path settled on in ADR 061, for the same reason: a
/// body with no end is a wait with no end, and this one is spent inside an
/// activity's deadline.
pub(crate) const MAX_TOKEN_RESPONSE_BYTES: usize = 256 * 1024;

/// A plan declared its own token exchange and the resolved credential cannot
/// render one. It is an `invariant` because the deployment is describing a
/// request it cannot make, and the registry refuses the same thing at startup.
pub(crate) const NO_TOKEN_REQUEST: ConnectorFailure = ConnectorFailure::new(
    ConnectorErrorClass::Invariant,
    "connector_credential_not_applicable",
    "connector instance declares an OAuth2 client-credentials plan whose token request cannot be \
     rendered",
);

/// The RFC 6749 §5.1 fields this exchange reads. Everything else a provider
/// adds — `app_id`, `nonce`, `scope` — is ignored: a client-credentials grant
/// has no stored row for a scope to narrow, so there is nothing here for a
/// scope check to protect ([[061-*]] guards the *stored* grant).
#[derive(Deserialize)]
struct TokenResponseBody {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// The RFC 6749 §5.2 machine-readable reason, for the one code that means the
/// configured client is not a client at all.
#[derive(Deserialize)]
struct TokenErrorBody {
    #[serde(default)]
    error: Option<String>,
}

/// Mint one access token for one logical attempt.
///
/// The caller is the connector executor, which drops the returned token when
/// the attempt ends. It is never stored, never cached across attempts, and
/// never shared between instances: a second attempt on the same instance mints
/// a second token, which is what "fetched once per logical attempt" means and
/// what makes the absence of a store correct rather than merely convenient.
pub(crate) async fn issue(
    plan: &AuthPlan,
    credential: &Credential,
    resolver: &dyn HostResolver,
    transport: &dyn HttpTransport,
    deadline: tokio::time::Instant,
) -> Result<AccessToken, ConnectorFailure> {
    let Some(request) = plan.token_request(credential)? else {
        return Err(NO_TOKEN_REQUEST);
    };
    exchange(request, resolver, transport, deadline).await
}

/// Send one rendered token request and read the grant out of the answer.
async fn exchange(
    request: RequestPlan,
    resolver: &dyn HostResolver,
    transport: &dyn HttpTransport,
    deadline: tokio::time::Instant,
) -> Result<AccessToken, ConnectorFailure> {
    if deadline <= tokio::time::Instant::now() {
        return Err(ConnectorFailure::timeout());
    }
    // The token endpoint is the connector's own compiled origin, not the
    // operation's, so it is resolved here rather than reusing the origin the
    // attempt already pinned. Nothing in operation input, a provider response,
    // or a continuation can reach it: `AuthPlan::oauth2_client_credentials`
    // takes a compile-time `Origin` and a static absolute path.
    let url = request.url();
    let host = url
        .host_str()
        .ok_or(ConnectorFailure::invariant(
            "connector token endpoint carries no host",
        ))?
        .to_owned();
    let port = url
        .port_or_known_default()
        .ok_or(ConnectorFailure::invariant(
            "connector token endpoint carries no port",
        ))?;
    let destination = tokio::time::timeout_at(deadline, resolver.resolve(&host, port))
        .await
        .map_err(|_| ConnectorFailure::timeout())?
        .map_err(|_| connector_failure(TOKEN_ENDPOINT_TRANSPORT))?;
    if destination.is_empty() {
        return Err(connector_failure(TOKEN_ENDPOINT_TRANSPORT));
    }

    // One bound over the whole exchange — DNS above, then connect, headers and
    // body — and a ceiling on the bytes the body may spend before the read
    // stops. Both halves are ADR 061's, and both are needed: a token endpoint
    // that answers `200` and then trickles is a defect measured in time, and
    // one that answers `200` and then floods is the same defect measured in
    // bytes.
    let prepared = request
        .into_prepared()?
        .with_response_ceiling(MAX_TOKEN_RESPONSE_BYTES);
    let response = tokio::time::timeout_at(
        deadline,
        transport.execute(prepared, &destination, deadline),
    )
    .await
    .map_err(|_| ConnectorFailure::timeout())?
    .map_err(|error| match error.kind() {
        TransportErrorKind::Timeout => ConnectorFailure::timeout(),
        TransportErrorKind::Transport => connector_failure(TOKEN_ENDPOINT_TRANSPORT),
        // A response too large to be a token response is not one, so it is the
        // contract failure rather than a size complaint: the same question gets
        // the same answer.
        TransportErrorKind::ResponseTooLarge => connector_failure(TOKEN_ENDPOINT_CONTRACT),
    })?;
    validate_connected_peer(&destination, &response)?;

    let status = response.status.as_u16();
    if !(200..300).contains(&status) {
        return Err(connector_failure(refusal(status, &response)));
    }
    grant(response.body())
}

/// The connected peer must be one of the addresses this exchange resolved, the
/// same rule every provider request holds to. A token endpoint is the one
/// request in an attempt that carries the client secret, so it is not the place
/// to relax it.
fn validate_connected_peer(
    destination: &[IpAddr],
    response: &donat_connectors::sdk::RawHttpResponse,
) -> Result<(), ConnectorFailure> {
    match response.peer() {
        Some(peer) if destination.contains(&peer.ip()) => Ok(()),
        Some(_) => Err(ConnectorFailure::invariant(
            "connector token endpoint answered from an unresolved peer",
        )),
        None => Err(ConnectorFailure::invariant(
            "connector transport could not verify the token endpoint peer",
        )),
    }
}

/// What a non-success answer from a token endpoint means.
///
/// Every arm is a [`CredentialFailure`] the stored path already publishes, so
/// the two exchanges speak one vocabulary to an operator and one class set to a
/// Process.
fn refusal(status: u16, response: &donat_connectors::sdk::RawHttpResponse) -> CredentialFailure {
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    if status == 429 {
        return CredentialFailure::new(
            CredentialErrorClass::Http429,
            "token_endpoint_throttled",
            "the connector's token endpoint asked us to slow down",
        )
        .with_retry_after(retry_after);
    }
    if (500..600).contains(&status) {
        return CredentialFailure::new(
            CredentialErrorClass::Http5xx,
            "token_endpoint_unavailable",
            "the connector's token endpoint failed",
        )
        .with_retry_after(retry_after);
    }
    // RFC 6749 §5.2 puts the reason in the body. `invalid_client` is the one
    // that names the configured credential itself, and it is reported as the
    // permanent authentication failure it is rather than as a retryable one.
    let code = serde_json::from_slice::<TokenErrorBody>(response.body())
        .ok()
        .and_then(|body| body.error);
    if code.as_deref() == Some("invalid_client") {
        return CredentialFailure::permanent(
            CredentialErrorClass::Authentication,
            "token_endpoint_invalid_client",
            "the connector's token endpoint refused the configured client credentials",
        );
    }
    CredentialFailure::permanent(
        CredentialErrorClass::Authentication,
        "token_endpoint_refused",
        "the connector's token endpoint refused the request",
    )
}

/// The grant, or the contract failure of an answer that is not one.
fn grant(body: &[u8]) -> Result<AccessToken, ConnectorFailure> {
    let Ok(parsed) = serde_json::from_slice::<TokenResponseBody>(body) else {
        return Err(connector_failure(TOKEN_ENDPOINT_CONTRACT));
    };
    if parsed.access_token.is_empty() {
        return Err(connector_failure(TOKEN_ENDPOINT_CONTRACT));
    }
    // The plan applies RFC 6750's `Bearer`, so a grant of any other type is a
    // token this connector cannot send. Refusing is the fail-closed answer;
    // sending it as a bearer token would be a request the declaration does not
    // describe. RFC 6749 §5.1 makes `token_type` required and case-insensitive.
    if let Some(token_type) = &parsed.token_type
        && !token_type.eq_ignore_ascii_case(donat_connectors::sdk::BEARER_SCHEME)
    {
        return Err(connector_failure(TOKEN_ENDPOINT_CONTRACT));
    }
    // `expires_in` is read and deliberately unused: nothing outlives the
    // attempt, so there is no expiry to record and no row to write it to. It is
    // parsed so that a provider that answers with a *string* here is a contract
    // failure rather than a silent success.
    let _ = parsed.expires_in;
    Ok(AccessToken::new(parsed.access_token))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

    use donat_connectors::sdk::testing::{Expectation, ProviderStub};
    use donat_connectors::sdk::{
        Connector, ConnectorConfiguration, CredentialSpec, Effect, ErrorMap, Operation, Origin,
        OriginSpec, Required, ReqwestTransport, Secret,
    };
    use donat_ir::ValueScalar;
    use futures_util::future::BoxFuture;
    use reqwest::StatusCode;
    use serde_json::json;

    use super::super::provider::{
        DeclaredProvider, ProviderInstance, ProviderRuntime, admits_its_own_credential,
        bind_nothing, no_pagination,
    };
    use super::super::{ConnectorErrorClass, RegisteredConnector};
    use super::*;

    /// The configured client secret. It is what must never reach the provider
    /// request, a log, or a failure.
    const CLIENT_SECRET: &str = "donat-client-secret-sentinel-do-not-log";
    /// The minted access token. It is what must reach the provider request and
    /// nothing else — in particular no store, and no second attempt.
    const ISSUED_TOKEN: &str = "donat-issued-access-token-sentinel";
    const REISSUED_TOKEN: &str = "donat-reissued-access-token-sentinel";

    /// A resolver that answers loopback for any host, so the stub's own
    /// `127.0.0.1` origin and token origin both resolve to the peer the
    /// transport connects to.
    struct LoopbackResolver;

    impl HostResolver for LoopbackResolver {
        fn resolve<'a>(
            &'a self,
            _host: &'a str,
            _port: u16,
        ) -> BoxFuture<'a, Result<Vec<IpAddr>, donat_connectors::sdk::ResolveError>> {
            Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
        }
    }

    fn error_map() -> &'static ErrorMap {
        static MAP: std::sync::LazyLock<ErrorMap> = std::sync::LazyLock::new(|| {
            ErrorMap::builder(ConnectorErrorClass::Permanent)
                .on_status(401, ConnectorErrorClass::Authentication)
                .on_status(429, ConnectorErrorClass::Http429)
                .build()
                .expect("the test error map is a valid declaration")
        });
        &MAP
    }

    /// A connector whose whole credential is a client-credentials plan against
    /// the stub's own origin, so the executor's token exchange and provider
    /// request are both observable in one ordered expectation list.
    fn connector(origin: &str, token_origin: &str) -> Connector {
        Connector::declare("test_client_credentials", "1.0.0")
            .origin(OriginSpec::fixed(origin).expect("the stub origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_client_credentials(
                    Origin::parse(token_origin).expect("the stub token origin is valid"),
                    "/v1/oauth2/token",
                    &[],
                )
                .expect("a static token endpoint is valid"),
            ))
            .operations(vec![
                Operation::get("item.get", "/v1/items/{id}")
                    .version("1.0.0")
                    .path_param("id", ValueScalar::String)
                    .success_statuses([StatusCode::OK])
                    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
                    .effect(Effect::read_only())
                    .build()
                    .expect("the test declaration is valid"),
            ])
            .build()
            .expect("the test connector declaration is valid")
    }

    fn credential() -> Credential {
        Credential::from_fields([
            ("client_id", Secret::new("test-client-id")),
            ("client_secret", Secret::new(CLIENT_SECRET)),
        ])
    }

    fn instance(stub: &ProviderStub) -> ProviderInstance {
        let runtime = DeclaredProvider::compile(
            connector(stub.base_url(), stub.base_url()),
            credential(),
            ConnectorConfiguration::default(),
            error_map(),
            bind_nothing,
            no_pagination,
        )
        .expect("the test instance compiles");
        ProviderInstance::for_test(
            Box::new(runtime),
            ["item.get"],
            Arc::new(LoopbackResolver),
            Arc::new(ReqwestTransport::new()),
        )
    }

    fn deadline() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(10)
    }

    fn token_response(token: &str) -> serde_json::Value {
        json!({
            "scope": "https://uri.paypal.com/services/invoicing",
            "access_token": token,
            "token_type": "Bearer",
            "app_id": "APP-TEST",
            "expires_in": 32400,
            "nonce": "test-nonce"
        })
    }

    /// The token request the SDK's plan renders, as the stub must see it.
    fn token_expectation(token: &str) -> Expectation {
        Expectation::new("POST", "/v1/oauth2/token")
            .header(
                "authorization",
                &format!(
                    "Basic {}",
                    base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        format!("test-client-id:{CLIENT_SECRET}")
                    )
                ),
            )
            .header("content-type", "application/x-www-form-urlencoded")
            .respond_json(200, token_response(token))
    }

    /// `oauth_client_credentials_is_not_persisted` (spec 011 §8): the
    /// client-credentials plan writes no row and drops its token after the
    /// attempt.
    ///
    /// The proof has three parts, because "not persisted" is three claims.
    ///
    /// 1. **No store is reachable.** The executor that runs this attempt is
    ///    handed a resolver and a transport and nothing else — no pool, no
    ///    sealing key, no `CredentialRuntime` — so there is no value in scope
    ///    that could write `donat.connector_credential`. This is asserted the
    ///    only way a type can assert it: the whole attempt runs to success
    ///    against a stub while the process holds no database at all.
    /// 2. **No token survives the attempt.** A second execute mints a *second*
    ///    token, which the stub proves by answering the second exchange with a
    ///    different value and asserting that value on the second provider
    ///    request. A cached or stored token would have sent the first one twice.
    /// 3. **Nothing leaks.** Neither the client secret nor the minted token
    ///    appears in the connector success, its fingerprint, or the `Debug` of
    ///    anything this path produces.
    #[tokio::test]
    async fn oauth_client_credentials_is_not_persisted() {
        let stub = ProviderStub::start([
            token_expectation(ISSUED_TOKEN),
            Expectation::new("GET", "/v1/items/42")
                .header("authorization", &format!("Bearer {ISSUED_TOKEN}"))
                .respond_json(200, json!({ "id": "42" })),
            token_expectation(REISSUED_TOKEN),
            Expectation::new("GET", "/v1/items/42")
                .header("authorization", &format!("Bearer {REISSUED_TOKEN}"))
                .respond_json(200, json!({ "id": "42" })),
        ])
        .await;
        let instance = instance(&stub);

        let first = instance
            .execute("item.get", json!({ "id": "42" }), "activity-1", deadline())
            .await
            .expect("the attempt mints a token and sends the request");
        assert_eq!(first.output, json!({ "id": "42" }));

        // A second logical attempt mints a second token. Nothing was kept.
        let second = instance
            .execute("item.get", json!({ "id": "42" }), "activity-2", deadline())
            .await
            .expect("the second attempt mints its own token");
        assert_eq!(second.output, json!({ "id": "42" }));

        assert_eq!(stub.received(), 4, "one exchange and one request, twice");
        stub.assert_satisfied();

        let surface = format!("{first:?} {second:?}");
        assert!(
            !surface.contains(CLIENT_SECRET) && !surface.contains(ISSUED_TOKEN),
            "no credential reaches a printable surface: {surface}"
        );
    }

    /// The client secret buys the token and never travels beyond the token
    /// endpoint: the provider request carries the minted bearer token and no
    /// trace of the configured credential.
    #[tokio::test]
    async fn a_client_credentials_attempt_sends_the_minted_token_and_never_the_secret() {
        let stub = ProviderStub::start([
            token_expectation(ISSUED_TOKEN),
            Expectation::new("GET", "/v1/items/42")
                .header("authorization", &format!("Bearer {ISSUED_TOKEN}"))
                .respond_json(200, json!({ "id": "42" })),
        ])
        .await;
        let instance = instance(&stub);

        instance
            .execute("item.get", json!({ "id": "42" }), "activity-1", deadline())
            .await
            .expect("the attempt succeeds");
        stub.assert_satisfied();

        let recorded = stub.recorded();
        assert_eq!(recorded.len(), 2);
        assert_eq!(
            std::str::from_utf8(&recorded[0].body).expect("the token body is ASCII"),
            "grant_type=client_credentials",
        );
        let provider_request = format!("{:?}", recorded[1]);
        assert!(
            !provider_request.contains(CLIENT_SECRET),
            "the configured secret never reaches the provider: {provider_request}"
        );
    }

    /// A declared credential that cannot be obtained sends nothing at all
    /// ([[043-the-credential-seam-refuses-before-it-sends]]): the provider
    /// receives no request, and the failure is the token endpoint's, classified
    /// into the connector set.
    #[tokio::test]
    async fn a_client_credentials_connector_never_sends_an_unauthenticated_request() {
        for (status, body, expected_class, expected_code) in [
            (
                401,
                json!({ "error": "invalid_client" }),
                ConnectorErrorClass::Authentication,
                "token_endpoint_invalid_client",
            ),
            (
                403,
                json!({ "error": "unsupported_grant_type" }),
                ConnectorErrorClass::Authentication,
                "token_endpoint_refused",
            ),
            (
                429,
                json!({}),
                ConnectorErrorClass::Http429,
                "token_endpoint_throttled",
            ),
            (
                503,
                json!({}),
                ConnectorErrorClass::Http5xx,
                "token_endpoint_unavailable",
            ),
            (
                200,
                json!({ "token_type": "Bearer" }),
                ConnectorErrorClass::Permanent,
                "token_endpoint_contract",
            ),
            (
                200,
                json!({ "access_token": "a", "token_type": "MAC" }),
                ConnectorErrorClass::Permanent,
                "token_endpoint_contract",
            ),
        ] {
            let stub = ProviderStub::start([
                Expectation::new("POST", "/v1/oauth2/token").respond_json(status, body.clone())
            ])
            .await;
            let instance = instance(&stub);

            let failure = instance
                .execute("item.get", json!({ "id": "42" }), "activity-1", deadline())
                .await
                .expect_err("an attempt that cannot mint a token fails");
            assert_eq!(failure.class(), expected_class, "{status} {body}");
            assert_eq!(failure.code(), expected_code, "{status} {body}");
            assert_eq!(
                stub.received(),
                1,
                "only the token endpoint was contacted: {status} {body}"
            );
            stub.assert_satisfied();
            assert!(!failure.diagnostic().contains(CLIENT_SECRET));
        }
    }

    /// A `429` from the token endpoint carries the provider's own retry hint
    /// through the seam, so a Process declaring `retry_on: [http_429]` waits the
    /// interval the endpoint asked for.
    #[tokio::test]
    async fn a_throttled_token_endpoint_keeps_its_retry_hint() {
        let stub = ProviderStub::start([Expectation::new("POST", "/v1/oauth2/token")
            .respond_header("retry-after", "9")
            .respond_json(429, json!({}))])
        .await;

        let failure = instance(&stub)
            .execute("item.get", json!({ "id": "42" }), "activity-1", deadline())
            .await
            .expect_err("a throttled token endpoint fails the attempt");
        assert_eq!(failure.class(), ConnectorErrorClass::Http429);
        assert_eq!(failure.retry_after(), Some(Duration::from_secs(9)));
    }

    /// A `401` from the *provider* gets exactly one re-acquisition and one
    /// replay, and the replay's failure is the operation's own `error_map`
    /// verdict rather than a credential-shaped one
    /// ([[043-the-credential-seam-refuses-before-it-sends]]).
    #[tokio::test]
    async fn a_provider_401_is_reissued_once_and_replayed_once() {
        // The happy half: the replay under a fresh token succeeds.
        let stub = ProviderStub::start([
            token_expectation(ISSUED_TOKEN),
            Expectation::new("GET", "/v1/items/42")
                .header("authorization", &format!("Bearer {ISSUED_TOKEN}"))
                .respond_json(401, json!({ "name": "NOT_AUTHORIZED" })),
            token_expectation(REISSUED_TOKEN),
            Expectation::new("GET", "/v1/items/42")
                .header("authorization", &format!("Bearer {REISSUED_TOKEN}"))
                .respond_json(200, json!({ "id": "42" })),
        ])
        .await;
        let recovered = instance(&stub)
            .execute("item.get", json!({ "id": "42" }), "activity-1", deadline())
            .await
            .expect("the replay under a freshly minted token succeeds");
        assert_eq!(recovered.output, json!({ "id": "42" }));
        assert_eq!(stub.received(), 4);
        stub.assert_satisfied();

        // The exhausted half: a second `401` is *not* retried again, and the
        // failure returned is the one the operation's own error map produced.
        let stub = ProviderStub::start([
            token_expectation(ISSUED_TOKEN),
            Expectation::new("GET", "/v1/items/42").respond_json(401, json!({})),
            token_expectation(REISSUED_TOKEN),
            Expectation::new("GET", "/v1/items/42").respond_json(401, json!({})),
        ])
        .await;
        let failure = instance(&stub)
            .execute("item.get", json!({ "id": "42" }), "activity-1", deadline())
            .await
            .expect_err("a second 401 ends the attempt");
        assert_eq!(
            failure.class(),
            ConnectorErrorClass::Authentication,
            "the operation's own error map classified the 401"
        );
        assert_eq!(
            stub.received(),
            4,
            "exactly one re-acquisition and one replay"
        );
        stub.assert_satisfied();
    }

    /// A failure the operation's error map owns is not turned into a credential
    /// failure by proximity: a `429` from the provider is the operation's
    /// `http_429`, and no token is minted a second time for it.
    #[tokio::test]
    async fn a_provider_failure_that_is_not_a_401_mints_no_second_token() {
        let stub = ProviderStub::start([
            token_expectation(ISSUED_TOKEN),
            Expectation::new("GET", "/v1/items/42").respond_json(429, json!({})),
        ])
        .await;

        let failure = instance(&stub)
            .execute("item.get", json!({ "id": "42" }), "activity-1", deadline())
            .await
            .expect_err("a throttled provider fails the attempt");
        assert_eq!(failure.class(), ConnectorErrorClass::Http429);
        assert_eq!(stub.received(), 2, "one exchange, one request, no replay");
        stub.assert_satisfied();
    }

    /// The exchange is bounded in bytes as well as in time: a token endpoint
    /// that answers `200` and then floods is a contract failure, and the
    /// provider is never contacted.
    #[tokio::test]
    async fn the_token_exchange_is_bounded_in_bytes() {
        let flood = json!({
            "access_token": "a",
            "token_type": "Bearer",
            "padding": "x".repeat(MAX_TOKEN_RESPONSE_BYTES + 1)
        });
        let stub = ProviderStub::start([
            Expectation::new("POST", "/v1/oauth2/token").respond_json(200, flood)
        ])
        .await;

        let failure = instance(&stub)
            .execute("item.get", json!({ "id": "42" }), "activity-1", deadline())
            .await
            .expect_err("a token response past the ceiling is refused");
        assert_eq!(failure.class(), ConnectorErrorClass::Permanent);
        assert_eq!(failure.code(), "token_endpoint_contract");
        assert_eq!(stub.received(), 1);
        stub.assert_satisfied();
    }

    /// ...and in time: a token endpoint that holds the answer past the
    /// attempt's own deadline costs one bounded wait, not an unbounded one, and
    /// the provider is never contacted.
    #[tokio::test]
    async fn the_token_exchange_is_bounded_in_time() {
        let stub = ProviderStub::start([Expectation::new("POST", "/v1/oauth2/token")
            .delay(Duration::from_secs(30))
            .respond_json(200, token_response(ISSUED_TOKEN))])
        .await;

        let started = std::time::Instant::now();
        let failure = instance(&stub)
            .execute(
                "item.get",
                json!({ "id": "42" }),
                "activity-1",
                tokio::time::Instant::now() + Duration::from_millis(250),
            )
            .await
            .expect_err("a token endpoint that stops talking fails the attempt");
        assert_eq!(failure.class(), ConnectorErrorClass::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the exchange spent the attempt's budget, not the endpoint's"
        );
        assert_eq!(stub.received(), 1, "the provider was never contacted");
    }

    /// The exchange resolves and pins the token endpoint's own host: a peer the
    /// exchange did not resolve is refused, exactly as it is for a provider
    /// request.
    #[tokio::test]
    async fn the_token_endpoint_peer_is_pinned() {
        struct ElsewhereResolver;
        impl HostResolver for ElsewhereResolver {
            fn resolve<'a>(
                &'a self,
                _host: &'a str,
                _port: u16,
            ) -> BoxFuture<'a, Result<Vec<IpAddr>, donat_connectors::sdk::ResolveError>>
            {
                Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))]) })
            }
        }

        let stub = ProviderStub::start([token_expectation(ISSUED_TOKEN)]).await;
        let runtime = DeclaredProvider::compile(
            connector(stub.base_url(), stub.base_url()),
            credential(),
            ConnectorConfiguration::default(),
            error_map(),
            bind_nothing,
            no_pagination,
        )
        .expect("the test instance compiles");
        let instance = ProviderInstance::for_test(
            Box::new(runtime),
            ["item.get"],
            Arc::new(ElsewhereResolver),
            Arc::new(ReqwestTransport::new()),
        );

        let failure = instance
            .execute(
                "item.get",
                json!({ "id": "42" }),
                "activity-1",
                tokio::time::Instant::now() + Duration::from_millis(500),
            )
            .await
            .expect_err("a token endpoint that is not where we resolved it is refused");
        assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
        assert_eq!(
            failure.safe_message(),
            "connector token endpoint answered from an unresolved peer"
        );
        // The token endpoint answered — the URL is a loopback literal, which no
        // resolver override can re-aim — and the answer was thrown away rather
        // than spent. What matters is that the *provider* was never asked.
        assert_eq!(stub.received(), 1);
        stub.assert_satisfied();
    }

    /// A module that declares the client-credentials plan and cannot render its
    /// token exchange refuses at **deploy time**, not at the first attempt
    /// ([[043-the-credential-seam-refuses-before-it-sends]]).
    ///
    /// The mistake this catches is a module-authoring one: declaring the plan
    /// and forgetting to put the client id and secret on the credential the
    /// runtime hands back. Without this check that instance starts, serves, and
    /// fails every activity with `connector_credential_missing_field`.
    #[test]
    fn a_module_that_cannot_render_its_token_exchange_refuses_at_deploy_time() {
        struct BareRuntime {
            connector: Connector,
            origin: Origin,
            credential: Credential,
        }

        impl ProviderRuntime for BareRuntime {
            fn origin(&self) -> &Origin {
                &self.origin
            }
            fn auth_plan(&self) -> Option<&AuthPlan> {
                self.connector.credential().plan()
            }
            fn credential(&self) -> &Credential {
                &self.credential
            }
            fn admit_operation(
                &self,
                id: &str,
            ) -> Result<&Operation, donat_connectors::sdk::OperationRejection> {
                self.connector.admit_operation(id)
            }
            fn plan(
                &self,
                _id: &str,
                _input: &serde_json::Value,
                _idempotency_key: &str,
            ) -> Result<donat_connectors::sdk::RequestPlan, ConnectorFailure> {
                Err(ConnectorFailure::invariant("not used by this test"))
            }
            fn decode(
                &self,
                _id: &str,
                _status: u16,
                _headers: &reqwest::header::HeaderMap,
                _body: &[u8],
            ) -> Result<serde_json::Value, ConnectorFailure> {
                Err(ConnectorFailure::invariant("not used by this test"))
            }
        }

        let declared = connector("https://provider.example.test", "https://auth.example.test");
        let origin = Origin::parse("https://provider.example.test").expect("a static origin");

        let complete = BareRuntime {
            connector: declared.clone(),
            origin: origin.clone(),
            credential: credential(),
        };
        assert!(admits_its_own_credential(&complete).is_ok());

        let forgotten = BareRuntime {
            connector: declared,
            origin,
            credential: Credential::from_fields([]),
        };
        let refusal = admits_its_own_credential(&forgotten)
            .expect_err("a plan whose credential cannot render its exchange refuses at startup");
        assert!(
            refusal.contains("client-credentials"),
            "the refusal names the plan: {refusal}"
        );
        assert!(
            !refusal.contains(CLIENT_SECRET),
            "and never a resolved value: {refusal}"
        );
    }
}
