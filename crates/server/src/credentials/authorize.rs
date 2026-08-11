//! Obtaining the first token — a deploy-time command, never a request.
//!
//! An operator runs `donat connector authorize`, approves the grant in their
//! own browser, and pastes the address the provider redirected to back into
//! the terminal. The engine is not involved: it never serves an authorization
//! route, never accepts a `code` over HTTP, and never learns that this
//! happened until it reads the row.
//!
//! Everything that could make a stolen or mistaken redirect useful is checked
//! before anything is written:
//!
//! - `state` is random per run and must come back unchanged, so a redirect the
//!   operator did not start is refused;
//! - PKCE (`S256`) means a code intercepted in transit cannot be exchanged
//!   without the verifier that never left this process;
//! - the redirect must arrive at the declared redirect URI, so a link that
//!   points somewhere else is refused before the code is spent;
//! - the response must come from the declared token endpoint;
//! - the granted scopes must cover the declared ones, because half a
//!   credential fails later, inside an activity, where nobody is watching.
//!
//! Nothing is printed but the subject, the granted scopes, and the expiry.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use chrono::Utc;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};

use super::declaration::OauthDeclaration;
use super::keys::{KeyError, SealingKey};
use super::oauth::{self, CredentialFailure, GrantRequest, TokenExchange, TokenRequest};
use super::store;

/// Everything that can stop an authorization. Every one of these aborts before
/// a row is written.
#[derive(Debug)]
pub enum AuthorizeError {
    /// The pasted value is not an absolute URL. The operator is asked for the
    /// whole address bar precisely so the host can be checked.
    NotAbsoluteRedirect,
    /// The redirect did not arrive at the declared redirect URI.
    ForeignRedirectHost {
        expected: String,
        actual: String,
    },
    /// `state` did not come back unchanged.
    StateMismatch,
    /// The redirect carried no `code`.
    MissingCode,
    /// The provider redirected with an error instead of a code. The value is
    /// the RFC 6749 §4.1.2.1 code, sanitized to the grammar that specification
    /// allows, so nothing free-form can ride along.
    ProviderRefused {
        error: String,
    },
    /// The granted set does not cover the declared one.
    ScopeShortfall {
        missing: Vec<String>,
    },
    /// The token response did not come from the declared endpoint.
    ForeignTokenOrigin {
        expected: String,
        actual: String,
    },
    /// The provider named no account and the operator gave no `--subject`.
    UnknownSubject,
    /// The instance already holds a credential for a *different* provider
    /// account.
    ///
    /// Spec 011 §9 keeps one connector instance to one account, and the
    /// request path enforces it by refusing: `CredentialRuntime::subject`
    /// answers `credential_ambiguous` — permanently, for every activity on the
    /// instance — the moment it finds two. Writing the second row would report
    /// success at the one moment an operator is watching and break the instance
    /// from then on, so the authorization stops here instead.
    AnotherAccountAuthorized {
        instance: String,
        stored: Vec<String>,
        arrived: String,
    },
    Exchange(CredentialFailure),
    Key(KeyError),
    Store(String),
}

impl fmt::Display for AuthorizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAbsoluteRedirect => formatter.write_str(
                "paste the whole address the provider redirected to, including the scheme and \
                 host — the host is checked before the code is used",
            ),
            Self::ForeignRedirectHost { expected, actual } => write!(
                formatter,
                "the redirect arrived at `{actual}`, but this connector instance declares \
                 `{expected}`"
            ),
            Self::StateMismatch => formatter.write_str(
                "the redirect carried a different `state` than this run generated; it belongs to \
                 another authorization",
            ),
            Self::MissingCode => formatter.write_str("the redirect carried no `code`"),
            Self::ProviderRefused { error } => {
                write!(formatter, "the provider refused the authorization: {error}")
            }
            Self::ScopeShortfall { missing } => write!(
                formatter,
                "the provider granted fewer scopes than this instance declares; missing: {}",
                missing.join(", ")
            ),
            Self::ForeignTokenOrigin { expected, actual } => write!(
                formatter,
                "the token response came from `{actual}`, but this connector instance declares \
                 `{expected}`"
            ),
            Self::UnknownSubject => formatter.write_str(
                "the provider's token response named no account; pass `--subject <id>` with the \
                 provider's own identifier for it",
            ),
            Self::AnotherAccountAuthorized {
                instance,
                stored,
                arrived,
            } => write!(
                formatter,
                "connector instance `{instance}` already holds a credential for provider account \
                 `{}`, and this authorization came back as `{arrived}`; one instance holds one \
                 account, so approve as the account it holds, or revoke that one first with \
                 `donat connector credentials revoke --instance {instance} --subject <account>`",
                stored.join("`, `"),
            ),
            Self::Exchange(failure) => write!(formatter, "{failure}"),
            Self::Key(error) => write!(formatter, "{error}"),
            Self::Store(error) => write!(formatter, "the credential store refused: {error}"),
        }
    }
}

impl std::error::Error for AuthorizeError {}

/// One authorization run: the URL to open, and the two secrets that make the
/// returning redirect provable.
pub struct AuthorizationRequest {
    /// What the operator opens.
    pub url: String,
    /// Compared with what comes back.
    pub state: String,
    /// PKCE. Never leaves this process until the exchange.
    verifier: String,
}

impl fmt::Debug for AuthorizationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizationRequest")
            .field("url", &self.url)
            .field("state", &self.state)
            .field("verifier", &"redacted")
            .finish()
    }
}

/// The code the provider handed back, with the verifier that proves it is ours.
#[derive(Debug)]
pub struct RedirectedCode {
    code: String,
    verifier: String,
}

/// What an operator is told after a successful authorization. No token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedCredential {
    pub subject: String,
    pub scopes: Vec<String>,
    pub access_expires_at: chrono::DateTime<Utc>,
    pub has_refresh_token: bool,
}

fn random_urlsafe(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buffer)
        .expect("the system random source produces a nonce");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buffer)
}

/// Start one authorization run.
pub fn begin(declaration: &OauthDeclaration) -> AuthorizationRequest {
    // 32 bytes each: 43 base64url characters, inside RFC 7636's 43..=128.
    let state = random_urlsafe(32);
    let verifier = random_urlsafe(32);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(digest(&SHA256, verifier.as_bytes()).as_ref());

    let mut url = url::Url::parse(&declaration.authorization_endpoint)
        .expect("the declaration validated this endpoint");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", declaration.client_id())
        .append_pair("redirect_uri", &declaration.redirect_uri)
        .append_pair("scope", &declaration.scopes.join(" "))
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");

    AuthorizationRequest {
        url: url.to_string(),
        state,
        verifier,
    }
}

/// Check the redirect the operator pasted back, before the code is spent.
pub fn parse_redirect(
    declaration: &OauthDeclaration,
    request: &AuthorizationRequest,
    pasted: &str,
) -> Result<RedirectedCode, AuthorizeError> {
    let redirected =
        url::Url::parse(pasted.trim()).map_err(|_| AuthorizeError::NotAbsoluteRedirect)?;
    let declared = url::Url::parse(&declaration.redirect_uri)
        .expect("the declaration validated this redirect URI");

    let arrived_at = format!(
        "{}://{}{}",
        redirected.scheme(),
        redirected
            .host_str()
            .ok_or(AuthorizeError::NotAbsoluteRedirect)?,
        redirected.path()
    );
    if !oauth::same_endpoint(
        &declaration.redirect_uri,
        &redirected[..url::Position::AfterPath],
    ) {
        return Err(AuthorizeError::ForeignRedirectHost {
            expected: declared[..url::Position::AfterPath].to_owned(),
            actual: arrived_at,
        });
    }

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (name, value) in redirected.query_pairs() {
        match name.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }

    // `state` first: a redirect that is not ours is not ours whatever else it
    // says, and comparing it before reading the error keeps a foreign page from
    // choosing the message an operator sees.
    if state.as_deref() != Some(request.state.as_str()) {
        return Err(AuthorizeError::StateMismatch);
    }
    if let Some(error) = error {
        return Err(AuthorizeError::ProviderRefused {
            error: sanitize_error_code(&error),
        });
    }
    let code = code.ok_or(AuthorizeError::MissingCode)?;

    Ok(RedirectedCode {
        code,
        verifier: request.verifier.clone(),
    })
}

/// RFC 6749 §4.1.2.1 restricts an error code to printable ASCII minus quote and
/// backslash. Anything else in the parameter is a page trying to write the
/// operator's terminal, so it is dropped rather than escaped.
fn sanitize_error_code(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "unspecified".to_owned()
    } else {
        cleaned
    }
}

/// Exchange the code and write the sealed row in one transaction.
///
/// Every check that can fail happens before the transaction opens, so an
/// aborted authorization leaves the database exactly as it found it.
pub async fn complete(
    client: &mut tokio_postgres::Client,
    key: &SealingKey,
    declaration: &OauthDeclaration,
    exchange: &dyn TokenExchange,
    redirected: &RedirectedCode,
    subject_override: Option<&str>,
    budget: Duration,
) -> Result<AuthorizedCredential, AuthorizeError> {
    let grant = exchange
        .exchange(TokenRequest {
            token_endpoint: &declaration.token_endpoint,
            client_id: declaration.client_id(),
            client_secret: declaration.client_secret(),
            grant: GrantRequest::AuthorizationCode {
                code: &redirected.code,
                code_verifier: &redirected.verifier,
                redirect_uri: &declaration.redirect_uri,
            },
            scopes: &declaration.scopes,
            budget,
        })
        .await
        .map_err(AuthorizeError::Exchange)?;

    if !oauth::same_endpoint(&declaration.token_endpoint, &grant.issued_by) {
        return Err(AuthorizeError::ForeignTokenOrigin {
            expected: declaration.token_endpoint.clone(),
            actual: grant.issued_by.clone(),
        });
    }

    let missing = oauth::scope_shortfall(&declaration.scopes, grant.granted_scopes.as_deref());
    if !missing.is_empty() {
        return Err(AuthorizeError::ScopeShortfall { missing });
    }

    let subject = subject_override
        .map(str::to_owned)
        .or_else(|| grant.subject.clone())
        .ok_or(AuthorizeError::UnknownSubject)?;
    let identity = declaration.identity(&subject);

    let scopes = grant
        .granted_scopes
        .clone()
        .unwrap_or_else(|| declaration.scopes.clone());
    let access_expires_at = Utc::now()
        + chrono::Duration::from_std(grant.expires_in.unwrap_or(Duration::from_secs(3600)))
            .unwrap_or_else(|_| chrono::Duration::seconds(3600));
    let sealed_access = key.seal(&identity, grant.access_token.expose());
    let sealed_refresh = grant
        .refresh_token
        .as_ref()
        .map(|token| key.seal(&identity, token.expose()));

    let transaction = client
        .transaction()
        .await
        .map_err(|error| AuthorizeError::Store(error.to_string()))?;

    // The last check, and the only one that needs the database: this instance
    // must not already belong to another provider account. `store::upsert`
    // keys on the subject, so approving as a different account would insert
    // rather than replace, print `authorized`, and leave every activity on the
    // instance failing `credential_ambiguous` from then on. Nothing has been
    // written at this point, so the refusal still leaves the database as it
    // found it. (Two `authorize` runs racing each other would both read an
    // empty table; this is a deploy-time command an operator runs at a
    // terminal, and the request path refuses the outcome either way.)
    let stored = store::subjects(
        &transaction,
        &declaration.source,
        &declaration.connector,
        &declaration.instance,
    )
    .await
    .map_err(|error| AuthorizeError::Store(error.to_string()))?;
    if stored.iter().any(|held| held != &subject) {
        return Err(AuthorizeError::AnotherAccountAuthorized {
            instance: declaration.instance.clone(),
            stored,
            arrived: subject,
        });
    }

    store::upsert(
        &transaction,
        &identity,
        &sealed_access,
        access_expires_at,
        sealed_refresh.as_deref(),
        &scopes,
    )
    .await
    .map_err(|error| AuthorizeError::Store(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AuthorizeError::Store(error.to_string()))?;

    Ok(AuthorizedCredential {
        subject,
        scopes,
        access_expires_at,
        has_refresh_token: sealed_refresh.is_some(),
    })
}

/// Call the provider's revocation endpoint, when it declares one.
///
/// A failure here does not stop the deletion: an operator revoking a
/// credential wants it gone locally whatever the provider says, and a token
/// that outlives our row is the provider's to expire.
pub async fn revoke_at_provider(
    declaration: &OauthDeclaration,
    token: &str,
    budget: Duration,
) -> Result<(), CredentialFailure> {
    let Some(endpoint) = &declaration.revocation_endpoint else {
        return Ok(());
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("the revocation HTTP client builds");
    let mut form: Vec<(&str, &str)> = vec![
        ("token", token),
        ("token_type_hint", "refresh_token"),
        ("client_id", declaration.client_id()),
    ];
    if let Some(secret) = declaration.client_secret() {
        form.push(("client_secret", secret));
    }
    match tokio::time::timeout(budget, client.post(endpoint).form(&form).send()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(oauth::TOKEN_ENDPOINT_TRANSPORT),
    }
}

/// A one-shot `127.0.0.1` listener that captures the redirect for the operator.
///
/// Explicitly requested with `--listen <port>`, and explicitly loopback: this
/// binds the interface no other machine can reach, because a public bind would
/// be an authorization endpoint on the deployment, which is the one thing spec
/// 011 forbids. It answers exactly one request and closes.
pub async fn capture_redirect(
    declaration: &OauthDeclaration,
    port: u16,
    timeout: Duration,
) -> Result<String, AuthorizeError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let declared = url::Url::parse(&declaration.redirect_uri)
        .expect("the declaration validated this redirect URI");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| {
            AuthorizeError::Store(format!("cannot listen on 127.0.0.1:{port}: {error}"))
        })?;

    let accepted = tokio::time::timeout(timeout, listener.accept())
        .await
        .map_err(|_| AuthorizeError::Store("no redirect arrived before the timeout".to_owned()))?
        .map_err(|error| AuthorizeError::Store(error.to_string()))?;
    let (mut socket, _peer) = accepted;

    let mut buffer = vec![0u8; 8 * 1024];
    let read = socket
        .read(&mut buffer)
        .await
        .map_err(|error| AuthorizeError::Store(error.to_string()))?;
    let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or(AuthorizeError::NotAbsoluteRedirect)?
        .to_owned();

    let _ = socket
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\
              Connection: close\r\n\r\nAuthorization received. You can close this window.\n",
        )
        .await;
    let _ = socket.shutdown().await;

    // The listener saw a path, not a URL. It is resolved against the declared
    // redirect URI so the host check downstream has something real to compare,
    // and so a redirect captured here is held to the same rule as one pasted.
    declared
        .join(&target)
        .map(|url| url.to_string())
        .map_err(|_| AuthorizeError::NotAbsoluteRedirect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration() -> OauthDeclaration {
        let metadata: donat_metadata::Metadata = serde_yaml::from_str(
            r#"
version: 3
connectors:
  - name: acme-main
    module: acme
    config:
      endpoint_identity: acme
      credential_identity: acme-oauth
      oauth2:
        authorization_endpoint: https://provider.example/oauth/authorize
        token_endpoint: https://provider.example/oauth/token
        redirect_uri: https://deploy.example/callback
        client_id:
          value_from_env: ACME_CLIENT_ID
        scopes: [read, write]
"#,
        )
        .expect("connector metadata parses");
        OauthDeclaration::resolve(&metadata, "default", "acme-main", &|name| {
            (name == "ACME_CLIENT_ID").then(|| "client-id".to_owned())
        })
        .expect("the declaration resolves")
    }

    #[test]
    fn the_authorization_url_carries_pkce_and_a_fresh_state() {
        let declaration = declaration();
        let first = begin(&declaration);
        let second = begin(&declaration);
        assert_ne!(first.state, second.state, "state must be per run");
        assert_ne!(first.verifier, second.verifier);

        let url = url::Url::parse(&first.url).expect("the authorization URL parses");
        let pairs: std::collections::HashMap<_, _> = url
            .query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["client_id"], "client-id");
        assert_eq!(pairs["redirect_uri"], "https://deploy.example/callback");
        assert_eq!(pairs["scope"], "read write");
        assert_eq!(pairs["state"], first.state);
        assert_eq!(pairs["code_challenge_method"], "S256");

        // The challenge is the SHA-256 of the verifier, and the verifier is
        // not in the URL.
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(digest(&SHA256, first.verifier.as_bytes()).as_ref());
        assert_eq!(pairs["code_challenge"], expected);
        assert!(!first.url.contains(&first.verifier));
        assert!(!format!("{first:?}").contains(&first.verifier));
    }

    #[test]
    fn a_redirect_must_be_absolute_and_arrive_where_it_was_sent() {
        let declaration = declaration();
        let request = begin(&declaration);

        assert!(matches!(
            parse_redirect(&declaration, &request, "code=abc&state=xyz"),
            Err(AuthorizeError::NotAbsoluteRedirect)
        ));

        let foreign = format!(
            "https://attacker.example/callback?code=abc&state={}",
            request.state
        );
        assert!(matches!(
            parse_redirect(&declaration, &request, &foreign),
            Err(AuthorizeError::ForeignRedirectHost { .. })
        ));

        let wrong_path = format!(
            "https://deploy.example/somewhere-else?code=abc&state={}",
            request.state
        );
        assert!(matches!(
            parse_redirect(&declaration, &request, &wrong_path),
            Err(AuthorizeError::ForeignRedirectHost { .. })
        ));
    }

    #[test]
    fn a_redirect_with_another_runs_state_is_refused() {
        let declaration = declaration();
        let request = begin(&declaration);
        let other = begin(&declaration);

        assert!(matches!(
            parse_redirect(
                &declaration,
                &request,
                &format!(
                    "https://deploy.example/callback?code=abc&state={}",
                    other.state
                ),
            ),
            Err(AuthorizeError::StateMismatch)
        ));
        assert!(matches!(
            parse_redirect(
                &declaration,
                &request,
                "https://deploy.example/callback?code=abc",
            ),
            Err(AuthorizeError::StateMismatch)
        ));
    }

    #[test]
    fn a_provider_error_is_reported_as_a_code_and_nothing_else() {
        let declaration = declaration();
        let request = begin(&declaration);
        let error = parse_redirect(
            &declaration,
            &request,
            &format!(
                "https://deploy.example/callback?error=access_denied%20%3Cscript%3E&state={}",
                request.state
            ),
        )
        .expect_err("a provider error must abort");
        match error {
            AuthorizeError::ProviderRefused { error } => {
                // The markup is gone, not escaped: what reaches an operator's
                // terminal is a code, or nothing.
                assert_eq!(error, "access_deniedscript");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_good_redirect_yields_the_code_and_this_runs_verifier() {
        let declaration = declaration();
        let request = begin(&declaration);
        let redirected = parse_redirect(
            &declaration,
            &request,
            &format!(
                "https://deploy.example/callback?code=the-code&state={}",
                request.state
            ),
        )
        .expect("a matching redirect parses");
        assert_eq!(redirected.code, "the-code");
        assert_eq!(redirected.verifier, request.verifier);
    }

    #[test]
    fn a_shortfall_is_what_is_missing_and_silence_is_not_a_shortfall() {
        let declared = vec!["read".to_owned(), "write".to_owned()];
        assert!(oauth::scope_shortfall(&declared, None).is_empty());
        assert!(
            oauth::scope_shortfall(&declared, Some(&["read".to_owned(), "write".to_owned()]))
                .is_empty()
        );
        assert_eq!(
            oauth::scope_shortfall(&declared, Some(&["read".to_owned()])),
            vec!["write".to_owned()]
        );
    }
}
