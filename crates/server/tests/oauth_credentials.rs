//! Spec 011 acceptance tests: the OAuth2 credential lifecycle.
//!
//! Every provider in here is a stub bound to `127.0.0.1`. Nothing in this file
//! reaches a real provider, and nothing in it needs network access beyond
//! loopback and the test Postgres.

use std::collections::HashSet;
use std::io::Read;
use std::net::TcpListener as StdTcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use chrono::{DateTime, Utc};
use donat_metadata::Metadata;
use donat_server::credentials::authorize::{self, AuthorizeError};
use donat_server::credentials::declaration::OauthDeclaration;
use donat_server::credentials::keys::{CredentialIdentity, SealingKey};
use donat_server::credentials::oauth::{
    CredentialErrorClass, CredentialFailure, TokenExchange, TokenGrant, TokenRequest,
};
use donat_server::credentials::refresh::{self, Attempt, RefreshOptions};
use donat_server::credentials::store;
use donat_server::migrate::run_migrate;
use futures_util::future::BoxFuture;
use tokio_postgres::NoTls;

/// The one string that must never appear anywhere but inside a sealed column.
const SENTINEL_REFRESH: &str = "sentinel-refresh-6f21c0a9-never-log-me";
const SENTINEL_ACCESS: &str = "sentinel-access-3b7d51e4-never-log-me";

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

fn postgres_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

struct TestDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl TestDatabase {
    async fn create(label: &str) -> Self {
        let admin_url = postgres_admin_url();
        let name = format!(
            "donat_{label}_{}_{}",
            std::process::id(),
            NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
        );
        let (client, connection) = tokio_postgres::connect(&admin_url, NoTls)
            .await
            .expect("Postgres admin database is available");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE);"))
            .await
            .expect("a stale credential database drops");
        client
            .batch_execute(&format!("CREATE DATABASE {name};"))
            .await
            .expect("the credential database creates");
        connection.abort();

        let prefix = admin_url
            .rsplit_once('/')
            .expect("the Postgres URL has a database segment")
            .0
            .to_owned();
        let database = Self {
            admin_url,
            name: name.clone(),
            url: format!("{prefix}/{name}"),
        };
        run_migrate(&database.url, &migrations_dir())
            .await
            .expect("the bundled migrations apply");
        database
    }

    async fn client(&self) -> tokio_postgres::Client {
        let (client, connection) = tokio_postgres::connect(&self.url, NoTls)
            .await
            .expect("the credential database is available");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    }

    async fn drop_database(self) {
        let (client, connection) = tokio_postgres::connect(&self.admin_url, NoTls)
            .await
            .expect("Postgres admin database is available for cleanup");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE);", self.name))
            .await
            .expect("the credential database drops");
        connection.abort();
    }
}

// ---------------------------------------------------------------------------
// The local token endpoint. It is a provider only in the sense that it answers
// RFC 6749 §5 shapes; it exists so no test needs a real one.
// ---------------------------------------------------------------------------

struct StubInner {
    exchanges: AtomicUsize,
    /// Refresh tokens the provider still accepts. A rotating provider adds the
    /// new one and (unless `grace`) drops the old.
    accepted: Mutex<HashSet<String>>,
    latest_access: Mutex<String>,
    latest_refresh: Mutex<String>,
    rotate: bool,
    /// Keep the previous refresh token valid after a rotation, as providers
    /// with a rotation grace window do.
    grace: bool,
    /// Refuse everything with `invalid_grant`.
    refuse: bool,
    /// The scope set to report back, when different from what was asked for.
    granted_scope: Mutex<Option<String>>,
    expires_in: u64,
    issued: AtomicUsize,
}

#[derive(Clone)]
struct TokenStub {
    inner: Arc<StubInner>,
    base: String,
}

struct StubOptions {
    rotate: bool,
    grace: bool,
    refuse: bool,
    expires_in: u64,
    granted_scope: Option<String>,
}

impl Default for StubOptions {
    fn default() -> Self {
        Self {
            rotate: false,
            grace: false,
            refuse: false,
            expires_in: 3600,
            granted_scope: None,
        }
    }
}

impl TokenStub {
    async fn start(options: StubOptions, initial_refresh: &str) -> Self {
        let inner = Arc::new(StubInner {
            exchanges: AtomicUsize::new(0),
            accepted: Mutex::new(HashSet::from([initial_refresh.to_owned()])),
            latest_access: Mutex::new(String::new()),
            latest_refresh: Mutex::new(initial_refresh.to_owned()),
            rotate: options.rotate,
            grace: options.grace,
            refuse: options.refuse,
            granted_scope: Mutex::new(options.granted_scope),
            expires_in: options.expires_in,
            issued: AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/token", post(token_endpoint))
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("the token stub binds a loopback port");
        let port = listener
            .local_addr()
            .expect("the token stub has an address")
            .port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            inner,
            base: format!("http://127.0.0.1:{port}"),
        }
    }

    fn token_endpoint(&self) -> String {
        format!("{}/token", self.base)
    }

    fn exchanges(&self) -> usize {
        self.inner.exchanges.load(Ordering::SeqCst)
    }

    fn latest_access(&self) -> String {
        self.inner
            .latest_access
            .lock()
            .expect("the stub lock is intact")
            .clone()
    }

    /// The refresh token the provider most recently issued.
    fn latest_refresh(&self) -> String {
        self.inner
            .latest_refresh
            .lock()
            .expect("the stub lock is intact")
            .clone()
    }

    fn accepts(&self, token: &str) -> bool {
        self.inner
            .accepted
            .lock()
            .expect("the stub lock is intact")
            .contains(token)
    }
}

async fn token_endpoint(
    State(stub): State<Arc<StubInner>>,
    axum::extract::Form(form): axum::extract::Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    stub.exchanges.fetch_add(1, Ordering::SeqCst);
    let field = |name: &str| {
        form.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };

    if stub.refuse {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": "invalid_grant" })),
        );
    }

    if field("grant_type").as_deref() == Some("refresh_token") {
        let presented = field("refresh_token").unwrap_or_default();
        let accepted = stub
            .accepted
            .lock()
            .expect("the stub lock is intact")
            .contains(&presented);
        if !accepted {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": "invalid_grant" })),
            );
        }
    }

    let sequence = stub.issued.fetch_add(1, Ordering::SeqCst) + 1;
    let access = format!("{SENTINEL_ACCESS}-{sequence}");
    *stub.latest_access.lock().expect("the stub lock is intact") = access.clone();

    let mut body = serde_json::json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": stub.expires_in,
        "sub": "acct_stub_1",
    });
    if let Some(scope) = stub
        .granted_scope
        .lock()
        .expect("the stub lock is intact")
        .clone()
    {
        body["scope"] = serde_json::Value::String(scope);
    } else if let Some(scope) = field("scope") {
        body["scope"] = serde_json::Value::String(scope);
    }

    if stub.rotate {
        let rotated = format!("{SENTINEL_REFRESH}-{sequence}");
        let mut accepted = stub.accepted.lock().expect("the stub lock is intact");
        if !stub.grace {
            accepted.clear();
        }
        accepted.insert(rotated.clone());
        *stub.latest_refresh.lock().expect("the stub lock is intact") = rotated.clone();
        body["refresh_token"] = serde_json::Value::String(rotated);
    }

    (StatusCode::OK, axum::Json(body))
}

// ---------------------------------------------------------------------------
// Declarations and seeding
// ---------------------------------------------------------------------------

fn declaration_for(token_endpoint: &str, scopes: &str) -> OauthDeclaration {
    let yaml = format!(
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
        token_endpoint: {token_endpoint}
        redirect_uri: https://deploy.example/callback
        client_id:
          value_from_env: ACME_CLIENT_ID
        client_secret:
          value_from_env: ACME_CLIENT_SECRET
        scopes: [{scopes}]
"#
    );
    let metadata: Metadata = serde_yaml::from_str(&yaml).expect("connector metadata parses");
    OauthDeclaration::resolve(&metadata, "default", "acme-main", &|name| match name {
        "ACME_CLIENT_ID" => Some("client-id".to_owned()),
        "ACME_CLIENT_SECRET" => Some("client-secret".to_owned()),
        _ => None,
    })
    .expect("the declaration resolves")
}

fn key() -> SealingKey {
    SealingKey::from_base64(&SealingKey::generate_base64_for_tests())
        .expect("a generated key is well formed")
}

async fn seed(
    client: &tokio_postgres::Client,
    key: &SealingKey,
    identity: &CredentialIdentity,
    refresh_token: &str,
    access_expires_at: DateTime<Utc>,
) {
    let scopes = vec!["read".to_owned(), "write".to_owned()];
    let sealed_access = key.seal(identity, format!("{SENTINEL_ACCESS}-0").as_bytes());
    let sealed_refresh = key.seal(identity, refresh_token.as_bytes());
    store::upsert(
        client,
        identity,
        &sealed_access,
        access_expires_at,
        Some(&sealed_refresh),
        &scopes,
    )
    .await
    .expect("the seeded credential writes");
}

async fn stored_refresh(
    client: &tokio_postgres::Client,
    key: &SealingKey,
    identity: &CredentialIdentity,
) -> String {
    let row = store::read(client, identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    let sealed = row
        .refresh_token_sealed
        .expect("the credential has a refresh token");
    key.open(identity, &sealed)
        .expect("the stored refresh token opens")
        .expose_str()
        .expect("the stored refresh token is UTF-8")
        .to_owned()
}

async fn row_count(client: &tokio_postgres::Client) -> i64 {
    client
        .query_one("SELECT count(*) FROM donat.connector_credential", &[])
        .await
        .expect("the credential table is countable")
        .get(0)
}

// ---------------------------------------------------------------------------
// oauth_tokens_are_sealed_at_rest
// ---------------------------------------------------------------------------

/// A shared buffer that stands in for the deployment's log collector.
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("the log lock is intact")).into_owned()
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("the log lock is intact")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn oauth_tokens_are_sealed_at_rest() {
    let database = TestDatabase::create("oauth_sealed").await;
    let client = database.client().await;
    let stub = TokenStub::start(StubOptions::default(), SENTINEL_REFRESH).await;
    let declaration = declaration_for(&stub.token_endpoint(), "read, write");
    let key = key();
    let identity = declaration.identity("acct_stub_1");

    let logs = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    // Two writes of the same plaintext under the same key and identity.
    seed(
        &client,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;
    let first: Vec<u8> = client
        .query_one("SELECT refresh_token FROM donat.connector_credential", &[])
        .await
        .expect("the sealed column reads")
        .get(0);
    seed(
        &client,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;
    let second: Vec<u8> = client
        .query_one("SELECT refresh_token FROM donat.connector_credential", &[])
        .await
        .expect("the sealed column reads")
        .get(0);

    assert_ne!(
        first, second,
        "a fresh nonce per write is what keeps AES-GCM safe under repetition"
    );
    for sealed in [&first, &second] {
        assert!(
            !sealed
                .windows(SENTINEL_REFRESH.len())
                .any(|window| window == SENTINEL_REFRESH.as_bytes()),
            "the plaintext must not survive into the column"
        );
    }

    // The AAD is the row's identity: change any part of it and the bytes stop
    // being a credential.
    for foreign in [
        CredentialIdentity {
            source: "other".to_owned(),
            ..identity.clone()
        },
        CredentialIdentity {
            connector: "other".to_owned(),
            ..identity.clone()
        },
        CredentialIdentity {
            instance: "other".to_owned(),
            ..identity.clone()
        },
        CredentialIdentity {
            subject: "other".to_owned(),
            ..identity.clone()
        },
        CredentialIdentity {
            token_origin: "https://attacker.example/token".to_owned(),
            ..identity.clone()
        },
    ] {
        assert!(
            key.open(&foreign, &second).is_err(),
            "a sealed row must not open under {foreign:?}"
        );
    }

    // Now exercise the diagnostic surfaces with the sentinel in play: a
    // refresh (which opens and re-seals), a listing, and a deliberate failure.
    let mut refreshing = database.client().await;
    let token = refresh::access_token(
        &mut refreshing,
        &key,
        &declaration,
        "acct_stub_1",
        &StubExchange::real(),
        RefreshOptions {
            force: true,
            ..RefreshOptions::default()
        },
    )
    .await
    .expect("the refresh succeeds");
    let header = token
        .authorization_header(donat_connectors::sdk::BEARER_SCHEME)
        .expect("the header builds");

    let row = store::read(&client, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    let listed = store::list(&client, "default")
        .await
        .expect("the listing reads");

    let failure = key
        .open(
            &CredentialIdentity {
                subject: "other".to_owned(),
                ..identity.clone()
            },
            &second,
        )
        .expect_err("a foreign identity does not open the row");

    drop(guard);

    let surfaces = vec![
        format!("{row:?}"),
        format!("{listed:?}"),
        format!("{token:?}"),
        format!("{header:?}"),
        format!("{failure} / {failure:?}"),
        format!("{declaration:?}"),
        logs.contents(),
    ];
    for surface in &surfaces {
        assert!(
            !surface.contains(SENTINEL_REFRESH),
            "a refresh token reached a diagnostic surface:\n{surface}"
        );
        assert!(
            !surface.contains(SENTINEL_ACCESS),
            "an access token reached a diagnostic surface:\n{surface}"
        );
    }
    assert!(
        format!("{row:?}").contains("redacted"),
        "the sealed columns must render as redacted"
    );
    // The header itself is the only thing that ever holds the token, and only
    // for the attempt.
    assert!(header.expose().starts_with("Bearer "));

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// A `TokenExchange` that talks to the local stub, plus one that lies about
// where its answer came from.
// ---------------------------------------------------------------------------

struct StubExchange {
    inner: donat_server::credentials::oauth::HttpTokenExchange,
    /// When set, report this as the origin the answer came from.
    pretend_origin: Option<String>,
}

impl StubExchange {
    fn real() -> Self {
        Self {
            inner: donat_server::credentials::oauth::HttpTokenExchange::new(),
            pretend_origin: None,
        }
    }

    fn from(origin: &str) -> Self {
        Self {
            inner: donat_server::credentials::oauth::HttpTokenExchange::new(),
            pretend_origin: Some(origin.to_owned()),
        }
    }
}

impl TokenExchange for StubExchange {
    fn exchange<'a>(
        &'a self,
        request: TokenRequest<'a>,
    ) -> BoxFuture<'a, Result<TokenGrant, CredentialFailure>> {
        Box::pin(async move {
            let mut grant = self.inner.exchange(request).await?;
            if let Some(origin) = &self.pretend_origin {
                grant.issued_by = origin.clone();
            }
            Ok(grant)
        })
    }
}

// ---------------------------------------------------------------------------
// oauth_refresh_is_single_flight
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oauth_refresh_is_single_flight() {
    let database = TestDatabase::create("oauth_singleflight").await;
    let stub = TokenStub::start(
        StubOptions {
            rotate: true,
            ..StubOptions::default()
        },
        SENTINEL_REFRESH,
    )
    .await;
    let declaration = declaration_for(&stub.token_endpoint(), "read, write");
    let key = key();
    let identity = declaration.identity("acct_stub_1");

    let seeder = database.client().await;
    seed(
        &seeder,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() - chrono::Duration::minutes(5),
    )
    .await;

    // Two claimers, two connections, one row.
    let mut left = database.client().await;
    let mut right = database.client().await;
    let left_exchange = StubExchange::real();
    let right_exchange = StubExchange::real();
    let (first, second) = tokio::join!(
        refresh::access_token(
            &mut left,
            &key,
            &declaration,
            "acct_stub_1",
            &left_exchange,
            RefreshOptions::default(),
        ),
        refresh::access_token(
            &mut right,
            &key,
            &declaration,
            "acct_stub_1",
            &right_exchange,
            RefreshOptions::default(),
        ),
    );

    let first = first.expect("the first claimer gets a token");
    let second = second.expect("the second claimer gets a token");
    assert_eq!(
        stub.exchanges(),
        1,
        "the row lock is the single-flight mechanism; two claimers must produce one exchange"
    );
    assert_eq!(
        first
            .authorization_header(donat_connectors::sdk::BEARER_SCHEME)
            .expect("the header builds")
            .expose(),
        second
            .authorization_header(donat_connectors::sdk::BEARER_SCHEME)
            .expect("the header builds")
            .expose(),
        "both claimers must end up with the token the one exchange produced"
    );
    assert!(
        first.refreshed ^ second.refreshed,
        "exactly one of the two performed the refresh"
    );

    let row = store::read(&seeder, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    assert_eq!(row.rotation_count, 1);

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// oauth_rotation_is_atomic
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oauth_rotation_is_atomic() {
    let database = TestDatabase::create("oauth_rotation").await;
    // A rotating provider with a grace window: the previous refresh token stays
    // valid for one more exchange, which is what makes the crash case
    // observable rather than merely asserted.
    let stub = TokenStub::start(
        StubOptions {
            rotate: true,
            grace: true,
            ..StubOptions::default()
        },
        SENTINEL_REFRESH,
    )
    .await;
    let declaration = declaration_for(&stub.token_endpoint(), "read, write");
    let key = key();
    let identity = declaration.identity("acct_stub_1");

    let observer = database.client().await;
    seed(
        &observer,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() - chrono::Duration::minutes(5),
    )
    .await;

    // 1. The rotation itself: the new refresh token is in the row, and the
    //    access token handed out is the one the row holds.
    let mut client = database.client().await;
    let token = refresh::access_token(
        &mut client,
        &key,
        &declaration,
        "acct_stub_1",
        &StubExchange::real(),
        RefreshOptions::default(),
    )
    .await
    .expect("the first refresh succeeds");
    assert!(token.refreshed);

    let rotated = stored_refresh(&observer, &key, &identity).await;
    assert_ne!(rotated, SENTINEL_REFRESH, "the provider rotated");
    assert!(
        stub.accepts(&rotated),
        "the committed refresh token is the one the provider now expects"
    );
    let row = store::read(&observer, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    assert_eq!(row.rotation_count, 1);
    let stored_access = key
        .open(&identity, &row.access_token_sealed)
        .expect("the stored access token opens")
        .expose_str()
        .expect("the stored access token is UTF-8")
        .to_owned();
    assert_eq!(
        token
            .authorization_header(donat_connectors::sdk::BEARER_SCHEME)
            .expect("the header builds")
            .expose(),
        format!("Bearer {stored_access}"),
        "the value handed out must be the value the database committed"
    );
    assert_eq!(stored_access, stub.latest_access());

    // 2. A crash between the exchange and the commit. The provider rotated;
    //    we lost the answer.
    expire(&observer, &identity).await;
    let before = row_snapshot(&observer, &identity).await;
    let failure = refresh::access_token(
        &mut client,
        &key,
        &declaration,
        "acct_stub_1",
        &StubExchange::real(),
        RefreshOptions {
            abort_before_commit: true,
            ..RefreshOptions::default()
        },
    )
    .await
    .expect_err("an interrupted refresh must fail");
    assert_eq!(failure.class, CredentialErrorClass::Transport);
    assert!(
        !failure.permanent,
        "a lost attempt is not a dead credential"
    );

    let after = row_snapshot(&observer, &identity).await;
    assert_eq!(
        before, after,
        "a crash before the commit must leave the row byte-identical"
    );
    let unchanged = store::read(&observer, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    assert!(
        unchanged.unusable_reason.is_none(),
        "an interrupted refresh must never mark the credential unusable"
    );

    // 3. The stored credential still works: the very next attempt refreshes
    //    with the token that is in the row and succeeds.
    let recovered = refresh::access_token(
        &mut client,
        &key,
        &declaration,
        "acct_stub_1",
        &StubExchange::real(),
        RefreshOptions::default(),
    )
    .await
    .expect("the credential is still usable after the interrupted attempt");
    assert!(recovered.refreshed);
    assert_eq!(
        stored_refresh(&observer, &key, &identity).await,
        stub.latest_refresh(),
    );

    database.drop_database().await;
}

async fn expire(client: &tokio_postgres::Client, identity: &CredentialIdentity) {
    client
        .execute(
            "UPDATE donat.connector_credential SET access_expires_at = now() - interval '5 minutes'
             WHERE source = $1 AND connector = $2 AND instance = $3 AND subject = $4",
            &[
                &identity.source,
                &identity.connector,
                &identity.instance,
                &identity.subject,
            ],
        )
        .await
        .expect("the access token expires");
}

/// The exact bytes and counters of one row, so "unchanged" means unchanged.
async fn row_snapshot(
    client: &tokio_postgres::Client,
    identity: &CredentialIdentity,
) -> (Vec<u8>, Option<Vec<u8>>, i64, Option<String>) {
    let row = client
        .query_one(
            "SELECT access_token, refresh_token, rotation_count, unusable_reason
             FROM donat.connector_credential
             WHERE source = $1 AND connector = $2 AND instance = $3 AND subject = $4",
            &[
                &identity.source,
                &identity.connector,
                &identity.instance,
                &identity.subject,
            ],
        )
        .await
        .expect("the credential row reads");
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

// ---------------------------------------------------------------------------
// oauth_authorize_never_writes_on_mismatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oauth_authorize_never_writes_on_mismatch() {
    let database = TestDatabase::create("oauth_authorize").await;
    let mut client = database.client().await;
    let key = key();

    // A stub that grants strictly fewer scopes than the instance declares.
    let short = TokenStub::start(
        StubOptions {
            granted_scope: Some("read".to_owned()),
            ..StubOptions::default()
        },
        SENTINEL_REFRESH,
    )
    .await;
    let declaration = declaration_for(&short.token_endpoint(), "read, write");

    // 1. `state` mismatch — a redirect from another run.
    let request = authorize::begin(&declaration);
    let other = authorize::begin(&declaration);
    let error = authorize::parse_redirect(
        &declaration,
        &request,
        &format!(
            "https://deploy.example/callback?code=abc&state={}",
            other.state
        ),
    )
    .expect_err("a foreign state must abort");
    assert!(matches!(error, AuthorizeError::StateMismatch));

    // 2. A redirect that arrived somewhere else entirely.
    let error = authorize::parse_redirect(
        &declaration,
        &request,
        &format!(
            "https://attacker.example/callback?code=abc&state={}",
            request.state
        ),
    )
    .expect_err("a foreign redirect host must abort");
    assert!(matches!(error, AuthorizeError::ForeignRedirectHost { .. }));

    // 3. Scope shortfall — checked after the exchange, before the write.
    let redirected = authorize::parse_redirect(
        &declaration,
        &request,
        &format!(
            "https://deploy.example/callback?code=the-code&state={}",
            request.state
        ),
    )
    .expect("a matching redirect parses");
    let error = authorize::complete(
        &mut client,
        &key,
        &declaration,
        &StubExchange::real(),
        &redirected,
        None,
        Duration::from_secs(10),
    )
    .await
    .expect_err("a scope shortfall must abort");
    match error {
        AuthorizeError::ScopeShortfall { missing } => assert_eq!(missing, vec!["write".to_owned()]),
        other => panic!("unexpected error: {other:?}"),
    }

    // 4. A token response that came from an origin the instance never declared.
    let full = TokenStub::start(StubOptions::default(), SENTINEL_REFRESH).await;
    let declaration = declaration_for(&full.token_endpoint(), "read, write");
    let request = authorize::begin(&declaration);
    let redirected = authorize::parse_redirect(
        &declaration,
        &request,
        &format!(
            "https://deploy.example/callback?code=the-code&state={}",
            request.state
        ),
    )
    .expect("a matching redirect parses");
    let error = authorize::complete(
        &mut client,
        &key,
        &declaration,
        &StubExchange::from("https://attacker.example/token"),
        &redirected,
        None,
        Duration::from_secs(10),
    )
    .await
    .expect_err("a foreign token origin must abort");
    assert!(matches!(error, AuthorizeError::ForeignTokenOrigin { .. }));

    assert_eq!(
        row_count(&client).await,
        0,
        "not one of the four mismatches may write a row"
    );

    // And the happy path does write exactly one, so the assertion above is
    // about the mismatches rather than about a command that cannot write.
    let request = authorize::begin(&declaration);
    let redirected = authorize::parse_redirect(
        &declaration,
        &request,
        &format!(
            "https://deploy.example/callback?code=the-code&state={}",
            request.state
        ),
    )
    .expect("a matching redirect parses");
    let written = authorize::complete(
        &mut client,
        &key,
        &declaration,
        &StubExchange::real(),
        &redirected,
        None,
        Duration::from_secs(10),
    )
    .await
    .expect("a matching authorization writes");
    assert_eq!(written.subject, "acct_stub_1");
    assert_eq!(row_count(&client).await, 1);
    assert!(
        !format!("{written:?}").contains(SENTINEL_ACCESS),
        "what an operator is shown carries no token"
    );

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// oauth_invalid_grant_is_permanent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oauth_invalid_grant_is_permanent() {
    let database = TestDatabase::create("oauth_invalid_grant").await;
    let stub = TokenStub::start(
        StubOptions {
            refuse: true,
            ..StubOptions::default()
        },
        SENTINEL_REFRESH,
    )
    .await;
    let declaration = declaration_for(&stub.token_endpoint(), "read, write");
    let key = key();
    let identity = declaration.identity("acct_stub_1");

    let observer = database.client().await;
    seed(
        &observer,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() - chrono::Duration::minutes(5),
    )
    .await;

    let mut client = database.client().await;
    let failure = refresh::access_token(
        &mut client,
        &key,
        &declaration,
        "acct_stub_1",
        &StubExchange::real(),
        RefreshOptions::default(),
    )
    .await
    .expect_err("a refused refresh token must fail");
    assert_eq!(failure.class, CredentialErrorClass::Authentication);
    assert!(failure.permanent);
    assert!(failure.is_invalid_grant());
    assert_eq!(stub.exchanges(), 1);

    let row = store::read(&observer, &identity)
        .await
        .expect("the credential reads")
        .expect("the row is retained so an operator can see what happened");
    assert_eq!(row.unusable_reason.as_deref(), Some("invalid_grant"));

    // Every later attempt fails the same way without touching the provider:
    // no retry loop, and the row is still there to be re-authorized.
    for _ in 0..3 {
        let failure = refresh::access_token(
            &mut client,
            &key,
            &declaration,
            "acct_stub_1",
            &StubExchange::real(),
            RefreshOptions::default(),
        )
        .await
        .expect_err("an unusable credential keeps failing");
        assert_eq!(failure.class, CredentialErrorClass::Authentication);
        assert!(failure.permanent);
        assert_eq!(failure.code, "credential_unusable");
    }
    assert_eq!(
        stub.exchanges(),
        1,
        "an unusable credential must never reach the provider again"
    );
    assert_eq!(row_count(&observer).await, 1);

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// oauth_expired_access_token_refreshes_once
// ---------------------------------------------------------------------------

/// An [`refresh::AccessTokenSource`] over one connection this test owns, which
/// is what a caller with no pool to answer to looks like.
struct OneConnectionTokens<'a> {
    client: tokio::sync::Mutex<tokio_postgres::Client>,
    key: &'a SealingKey,
    declaration: &'a OauthDeclaration,
    subject: &'a str,
    exchange: StubExchange,
}

impl refresh::AccessTokenSource for OneConnectionTokens<'_> {
    fn acquire(
        &self,
        force: bool,
    ) -> BoxFuture<'_, Result<refresh::AccessToken, CredentialFailure>> {
        Box::pin(async move {
            let mut client = self.client.lock().await;
            refresh::access_token(
                &mut client,
                self.key,
                self.declaration,
                self.subject,
                &self.exchange,
                RefreshOptions {
                    force,
                    ..RefreshOptions::default()
                },
            )
            .await
        })
    }
}

#[tokio::test]
async fn oauth_expired_access_token_refreshes_once() {
    let database = TestDatabase::create("oauth_replay").await;
    let stub = TokenStub::start(
        StubOptions {
            rotate: true,
            ..StubOptions::default()
        },
        SENTINEL_REFRESH,
    )
    .await;
    let declaration = declaration_for(&stub.token_endpoint(), "read, write");
    let key = key();
    let identity = declaration.identity("acct_stub_1");

    let observer = database.client().await;
    // Deliberately *not* expired as far as we know: the provider is the one
    // that disagrees, mid-attempt, with a 401.
    seed(
        &observer,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;

    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let tokens = OneConnectionTokens {
        client: tokio::sync::Mutex::new(database.client().await),
        key: &key,
        declaration: &declaration,
        subject: "acct_stub_1",
        exchange: StubExchange::real(),
    };

    let outcome: &'static str = {
        let attempts = attempts.clone();
        let seen = seen.clone();
        refresh::with_access_token(
            &tokens,
            donat_connectors::sdk::BEARER_SCHEME,
            move |header| {
                let attempts = attempts.clone();
                let seen = seen.clone();
                let presented = header.expose().to_owned();
                Box::pin(async move {
                    seen.lock()
                        .expect("the attempt lock is intact")
                        .push(presented.clone());
                    // The stale token is the one that was seeded; the provider
                    // rejects it exactly once.
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(Attempt::Unauthorized)
                    } else {
                        Ok(Attempt::Done("provider answered"))
                    }
                })
            },
        )
        .await
        .expect("the replay succeeds")
    };

    assert_eq!(outcome, "provider answered");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "one attempt, one replay — never a loop"
    );
    assert_eq!(
        stub.exchanges(),
        1,
        "a 401 mid-attempt triggers exactly one refresh"
    );

    let seen = seen.lock().expect("the attempt lock is intact").clone();
    assert_ne!(seen[0], seen[1], "the replay carries the refreshed token");
    assert_eq!(seen[1], format!("Bearer {}", stub.latest_access()));

    let row = store::read(&observer, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    assert_eq!(row.rotation_count, 1);

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// oauth_engine_accepts_no_credential_over_http
// ---------------------------------------------------------------------------

struct MetadataDir {
    path: PathBuf,
}

impl MetadataDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "donat-oauth-http-{}-{}",
            std::process::id(),
            NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("databases")).expect("the metadata directory creates");
        std::fs::write(path.join("version.yaml"), "version: 3\n").expect("the version writes");
        std::fs::write(
            path.join("databases/databases.yaml"),
            "- name: default\n  kind: postgres\n  configuration: {}\n  tables: []\n",
        )
        .expect("the source metadata writes");
        Self { path }
    }
}

impl Drop for MetadataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn free_port() -> u16 {
    StdTcpListener::bind(("127.0.0.1", 0))
        .expect("an ephemeral port binds")
        .local_addr()
        .expect("the ephemeral address exists")
        .port()
}

async fn wait_for_health(child: &mut Child, port: u16) -> Result<(), String> {
    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{port}/healthz");
    for _ in 0..200 {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("checking the server status: {error}"))?
        {
            let mut stderr = String::new();
            if let Some(mut stream) = child.stderr.take() {
                let _ = stream.read_to_string(&mut stderr);
            }
            return Err(format!(
                "the server exited before the health check ({status}):\n{stderr}"
            ));
        }
        match client.get(&health).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    Err("the server did not become healthy".to_owned())
}

/// The engine has no credential surface at all: not a route that starts an
/// authorization, not one that stores a code, not one that lists or returns a
/// token. Authorization exists only in the CLI.
#[tokio::test]
async fn oauth_engine_accepts_no_credential_over_http() {
    let database = TestDatabase::create("oauth_http").await;
    let metadata = MetadataDir::new();
    let key = key();

    // A real, seeded credential is sitting in this database while the server
    // serves it, so "no route returns a token" is a claim about a token that
    // actually exists.
    let seeded = database.client().await;
    let identity = CredentialIdentity {
        source: "default".to_owned(),
        connector: "acme".to_owned(),
        instance: "acme-main".to_owned(),
        subject: "acct_stub_1".to_owned(),
        token_origin: "https://provider.example/oauth/token".to_owned(),
    };
    seed(
        &seeded,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;

    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_donat"))
        .env("DONAT_DATABASE_URL", &database.url)
        .env("DONAT_METADATA_DIR", &metadata.path)
        .env("DONAT_PORT", port.to_string())
        .env(
            "DONAT_CREDENTIAL_KEY",
            SealingKey::generate_base64_for_tests(),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the donat binary starts");
    if let Err(error) = wait_for_health(&mut child, port).await {
        let _ = child.kill();
        panic!("{error}");
    }

    let http = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let mut bodies = Vec::new();

    // Every shape a credential API would take, if one existed.
    for path in [
        "/v1/credentials",
        "/v1/connectors/acme-main/credentials",
        "/v1/connectors/acme-main/authorize",
        "/v1/connectors/acme-main/oauth/callback",
        "/v1/oauth/authorize",
        "/v1/oauth/callback",
        "/oauth/callback",
        "/api/rest/credentials",
        "/api/rest/oauth/callback",
        "/v1/query",
        "/v1/metadata",
    ] {
        for method in ["GET", "POST"] {
            let request = match method {
                "GET" => http.get(format!("{base}{path}")),
                _ => http
                    .post(format!("{base}{path}"))
                    .json(&serde_json::json!({ "code": "abc", "state": "xyz" })),
            };
            let response = request
                .header("x-donat-role", "anonymous")
                .send()
                .await
                .expect("the engine answers");
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{method} {path} must not exist"
            );
            bodies.push(response.text().await.unwrap_or_default());
        }
    }

    // The data surfaces do exist, and none of them knows the word.
    let introspection = http
        .post(format!("{base}/v1/graphql"))
        .header("x-donat-role", "anonymous")
        .json(&serde_json::json!({
            "query": "{ __schema { queryType { name } types { name fields { name } } } }"
        }))
        .send()
        .await
        .expect("the GraphQL surface answers")
        .text()
        .await
        .expect("the GraphQL body reads");
    bodies.push(introspection.clone());

    let tools = http
        .post(format!("{base}/mcp"))
        .header("x-donat-role", "anonymous")
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}
        }))
        .send()
        .await
        .expect("the MCP surface answers")
        .text()
        .await
        .expect("the MCP body reads");
    bodies.push(tools.clone());

    let _ = child.kill();
    let _ = child.wait();

    for body in &bodies {
        assert!(
            !body.contains(SENTINEL_REFRESH) && !body.contains(SENTINEL_ACCESS),
            "an HTTP response carried a stored token:\n{body}"
        );
    }
    for (surface, body) in [("graphql", &introspection), ("mcp", &tools)] {
        let lowered = body.to_ascii_lowercase();
        assert!(
            !lowered.contains("credential") && !lowered.contains("oauth"),
            "the {surface} surface publishes a credential-shaped name:\n{body}"
        );
    }

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// The read-only companions, run as the operator runs them: through the binary.
// ---------------------------------------------------------------------------

/// A metadata directory that declares one OAuth2 connector instance, so
/// `credentials list` has something to say is missing.
fn oauth_metadata_dir(token_endpoint: &str) -> MetadataDir {
    let directory = MetadataDir::new();
    std::fs::write(
        directory.path.join("connectors.yaml"),
        format!(
            r#"
- name: acme-main
  module: acme
  config:
    endpoint_identity: acme
    credential_identity: acme-oauth
    oauth2:
      authorization_endpoint: https://provider.example/oauth/authorize
      token_endpoint: {token_endpoint}
      redirect_uri: https://deploy.example/callback
      client_id:
        value_from_env: ACME_CLIENT_ID
      client_secret:
        value_from_env: ACME_CLIENT_SECRET
      scopes: [read, write]
"#
        ),
    )
    .expect("the connector metadata writes");
    directory
}

#[tokio::test]
async fn connector_credentials_list_reports_a_missing_credential_and_revoke_removes_one() {
    let database = TestDatabase::create("oauth_cli").await;
    let stub = TokenStub::start(StubOptions::default(), SENTINEL_REFRESH).await;
    let metadata = oauth_metadata_dir(&stub.token_endpoint());
    let key_material = SealingKey::generate_base64_for_tests();
    let key = SealingKey::from_base64(&key_material).expect("the generated key is well formed");

    let donat = |arguments: Vec<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_donat"));
        command
            .env_remove("DONAT_METADATA_DIR")
            .env("DONAT_DATABASE_URL", &database.url)
            .env("DONAT_CREDENTIAL_KEY", &key_material)
            .env("ACME_CLIENT_ID", "client-id")
            .env("ACME_CLIENT_SECRET", "client-secret")
            .args(arguments)
            .args([
                "--metadata-dir",
                metadata.path.to_str().expect("a UTF-8 path"),
            ]);
        command.output().expect("the donat binary runs")
    };

    // Nothing stored yet: the command has to fail, because a deployment in this
    // state cannot run the activities that need the credential.
    let output = donat(vec![
        "connector",
        "credentials",
        "list",
        "--source",
        "default",
    ]);
    assert!(
        !output.status.success(),
        "a configured instance with no credential must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("acme-main") && stderr.contains("no credential"),
        "unexpected stderr:\n{stderr}"
    );

    let client = database.client().await;
    let identity = CredentialIdentity {
        source: "default".to_owned(),
        connector: "acme".to_owned(),
        instance: "acme-main".to_owned(),
        subject: "acct_stub_1".to_owned(),
        token_origin: stub.token_endpoint(),
    };
    seed(
        &client,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;

    let output = donat(vec![
        "connector",
        "credentials",
        "list",
        "--source",
        "default",
    ]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("acct_stub_1"),
        "unexpected stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("read write"),
        "unexpected stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains(SENTINEL_REFRESH) && !stdout.contains(SENTINEL_ACCESS),
        "the listing must carry no secret:\n{stdout}"
    );

    let output = donat(vec![
        "connector",
        "credentials",
        "revoke",
        "--source",
        "default",
        "--instance",
        "acme-main",
        "--subject",
        "acct_stub_1",
    ]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(row_count(&client).await, 0);

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// The seam: a declared credential on a real provider request.
//
// Spec 011 §2 says a connector module "receives an applied `Authorization`
// header for one attempt". These two tests are what makes that a fact about the
// wire rather than a sentence: the provider stub asserts the exact header it
// received, and refuses to be satisfied by a request it did not expect.
// ---------------------------------------------------------------------------

/// The deploy-time environment these tests share.
///
/// The registry resolves the OAuth2 client identity through the same startup
/// check every other connector secret goes through, and that check reads the
/// process environment. One key, set once, for the whole file.
fn deploy_time_environment() -> &'static str {
    static ENVIRONMENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ENVIRONMENT.get_or_init(|| {
        let key = SealingKey::generate_base64_for_tests();
        // SAFETY: these variable names are owned by this test binary, are set
        // once, and are never unset. The child processes this file spawns pass
        // their own values explicitly.
        unsafe {
            std::env::set_var("ACME_CLIENT_ID", "client-id");
            std::env::set_var("ACME_CLIENT_SECRET", "client-secret");
            std::env::set_var("DONAT_CREDENTIAL_KEY", &key);
        }
        key
    })
}

fn oauth_http_metadata(provider_base_url: &str, token_endpoint: &str) -> Metadata {
    serde_json::from_value(serde_json::json!({
        "version": 3,
        "sources": [{ "name": "default", "kind": "postgres", "configuration": {} }],
        "connectors": [{
            "name": "acme-main",
            "module": "http",
            "config": {
                "endpoint_identity": "acme",
                "credential_identity": "acme-oauth",
                "base_url": provider_base_url,
                "oauth2": {
                    "authorization_endpoint": "https://provider.example/oauth/authorize",
                    "token_endpoint": token_endpoint,
                    "redirect_uri": "https://deploy.example/callback",
                    "client_id": { "value_from_env": "ACME_CLIENT_ID" },
                    "client_secret": { "value_from_env": "ACME_CLIENT_SECRET" },
                    "scopes": ["read", "write"]
                }
            },
            "operations": [{
                "name": "thing.get",
                "version": "v1",
                "method": "GET",
                "path": "/v1/things/{input.thing_id}",
                "success_statuses": [200],
                "response": { "id": { "json_pointer": "/id", "type": "String!" } },
                "capacity": {
                    "max_in_flight": 1,
                    "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
                }
            }]
        }]
    }))
    .expect("the OAuth2 http connector metadata deserializes")
}

/// The identity `acme-main` credentials are stored under. `connector` is the
/// module name, which is what the declaration resolves it to.
fn http_identity(token_endpoint: &str) -> CredentialIdentity {
    CredentialIdentity {
        source: "default".to_owned(),
        connector: "http".to_owned(),
        instance: "acme-main".to_owned(),
        subject: "acct_stub_1".to_owned(),
        token_origin: token_endpoint.to_owned(),
    }
}

fn credential_runtime(
    metadata: &Metadata,
    database_url: &str,
    key_material: &str,
) -> donat_server::credentials::CredentialRuntime {
    let pool = donat_server::state::make_pool(database_url).expect("the credential pool builds");
    donat_server::credentials::CredentialRuntime::resolve_with(metadata, "default", pool, &|name| {
        match name {
            "DONAT_CREDENTIAL_KEY" => Some(key_material.to_owned()),
            "ACME_CLIENT_ID" => Some("client-id".to_owned()),
            "ACME_CLIENT_SECRET" => Some("client-secret".to_owned()),
            _ => None,
        }
    })
    .expect("the credential runtime resolves")
    .expect("this deployment declares OAuth2")
    .with_exchange(Arc::new(StubExchange::real()))
}

/// The stored credential reaches the provider request as an `Authorization`
/// header, and a provider `401` costs exactly one refresh and one replay — with
/// the *refreshed* token on the wire the second time.
#[tokio::test]
async fn oauth_credential_reaches_the_provider_request() {
    use donat_connectors::sdk::testing::{Expectation, ProviderStub};

    let key_material = deploy_time_environment().to_owned();
    let key = SealingKey::from_base64(&key_material).expect("the shared key is well formed");
    let database = TestDatabase::create("oauth_wire").await;
    let token_stub = TokenStub::start(
        StubOptions {
            rotate: true,
            ..StubOptions::default()
        },
        SENTINEL_REFRESH,
    )
    .await;

    // The provider answers the seeded token with a 401 — an access token can
    // expire while we still believe it is fresh — and the replay with a 200.
    let provider = ProviderStub::start([
        Expectation::new("GET", "/v1/things/42")
            .header("authorization", &format!("Bearer {SENTINEL_ACCESS}-0"))
            .respond_json(401, serde_json::json!({ "error": "expired" })),
        Expectation::new("GET", "/v1/things/42")
            .header("authorization", &format!("Bearer {SENTINEL_ACCESS}-1"))
            .respond_json(200, serde_json::json!({ "id": "thing_1" })),
    ])
    .await;

    let metadata = oauth_http_metadata(provider.base_url(), &token_stub.token_endpoint());
    let identity = http_identity(&token_stub.token_endpoint());
    let observer = database.client().await;
    seed(
        &observer,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;

    let mut registry = donat_server::connectors::ConnectorRegistry::build(&metadata)
        .expect("the OAuth2 http instance compiles");
    registry.with_credential_runtime(Arc::new(credential_runtime(
        &metadata,
        &database.url,
        &key_material,
    )));

    let success = registry
        .execute(
            "acme-main",
            "thing.get",
            serde_json::json!({ "thing_id": "42" }),
            "activity-1",
            tokio::time::Instant::now() + Duration::from_secs(10),
        )
        .await
        .expect("the authorized operation succeeds");
    assert_eq!(success.output, serde_json::json!({ "id": "thing_1" }));

    // The stub asserted both `Authorization` values itself; this is the
    // assertion that it saw exactly those two requests and no others.
    provider.assert_satisfied();
    assert_eq!(
        token_stub.exchanges(),
        1,
        "a 401 mid-attempt triggers exactly one refresh"
    );
    let row = store::read(&observer, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    assert_eq!(row.rotation_count, 1, "one rotation, committed before use");

    database.drop_database().await;
}

/// [[034-a-declaration-the-runtime-ignores-is-a-defect]]: an instance that
/// declares `config.oauth2` never reaches the provider without the header.
///
/// The two ways it could — a deployment whose credential runtime was never
/// resolved, and one whose stored credential is gone — both fail the attempt
/// before a socket is opened. The provider stub expects nothing at all, so any
/// request that did leave is a mismatch.
#[tokio::test]
async fn a_declared_oauth2_connector_never_sends_an_unauthenticated_request() {
    use donat_connectors::sdk::testing::ProviderStub;

    let key_material = deploy_time_environment().to_owned();
    let key = SealingKey::from_base64(&key_material).expect("the shared key is well formed");
    let database = TestDatabase::create("oauth_never_bare").await;
    let token_stub = TokenStub::start(StubOptions::default(), SENTINEL_REFRESH).await;
    let provider = ProviderStub::start([]).await;

    let metadata = oauth_http_metadata(provider.base_url(), &token_stub.token_endpoint());
    let identity = http_identity(&token_stub.token_endpoint());
    let observer = database.client().await;

    let call = async |registry: &donat_server::connectors::ConnectorRegistry| {
        registry
            .execute(
                "acme-main",
                "thing.get",
                serde_json::json!({ "thing_id": "42" }),
                "activity-1",
                tokio::time::Instant::now() + Duration::from_secs(10),
            )
            .await
            .expect_err("a declared credential that cannot be applied fails the attempt")
    };

    // 1. The declaration is there and nothing resolved it. The old behaviour was
    //    to send the request anyway, with no header at all.
    let unresolved = donat_server::connectors::ConnectorRegistry::build(&metadata)
        .expect("the OAuth2 http instance compiles");
    let failure = call(&unresolved).await;
    assert_eq!(
        failure.class(),
        donat_server::connectors::ConnectorErrorClass::Invariant
    );
    assert_eq!(failure.code(), "connector_credential_runtime_absent");

    // 2. The runtime is resolved and the credential is not there — revoked, or
    //    never authorized. Still no request.
    let mut resolved = donat_server::connectors::ConnectorRegistry::build(&metadata)
        .expect("the OAuth2 http instance compiles");
    resolved.with_credential_runtime(Arc::new(credential_runtime(
        &metadata,
        &database.url,
        &key_material,
    )));
    let failure = call(&resolved).await;
    assert_eq!(
        failure.class(),
        donat_server::connectors::ConnectorErrorClass::Authentication
    );
    assert_eq!(failure.code(), "credential_missing");

    // 3. ...and the same registry, once the credential exists, does reach the
    //    provider — so steps 1 and 2 refused for the reason claimed rather than
    //    because nothing could ever work.
    seed(
        &observer,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;
    let failure = call(&resolved).await;
    assert_eq!(
        failure.code(),
        "connector_unsupported_http_status",
        "the request now leaves and the stub, which expects nothing, refuses it"
    );

    let mismatches = provider.mismatches();
    assert_eq!(
        mismatches.len(),
        1,
        "exactly one request reached the provider, and only after a credential existed: \
         {mismatches:?}"
    );
    assert!(
        mismatches[0].contains("unexpected request 0"),
        "{mismatches:?}"
    );

    database.drop_database().await;
}

/// Spec 011 §7: a configured instance with no credential is a startup failure,
/// not something an activity discovers.
#[tokio::test]
async fn a_declared_oauth2_instance_without_a_credential_fails_startup() {
    use donat_connectors::sdk::testing::ProviderStub;

    let key_material = deploy_time_environment().to_owned();
    let key = SealingKey::from_base64(&key_material).expect("the shared key is well formed");
    let database = TestDatabase::create("oauth_startup").await;
    let token_stub = TokenStub::start(StubOptions::default(), SENTINEL_REFRESH).await;
    let provider = ProviderStub::start([]).await;
    let metadata = oauth_http_metadata(provider.base_url(), &token_stub.token_endpoint());

    let mut registry = donat_server::connectors::ConnectorRegistry::build(&metadata)
        .expect("the OAuth2 http instance compiles");
    let error = registry
        .attach_credentials(&metadata, &database.url)
        .await
        .expect_err("an instance with no stored credential must not serve");
    let rendered = error.to_string();
    assert!(rendered.contains("acme-main"), "{rendered}");
    assert!(rendered.contains("no stored credential"), "{rendered}");
    assert!(
        !rendered.contains(SENTINEL_REFRESH) && !rendered.contains(SENTINEL_ACCESS),
        "a startup failure carries no secret: {rendered}"
    );

    let observer = database.client().await;
    seed(
        &observer,
        &key,
        &http_identity(&token_stub.token_endpoint()),
        SENTINEL_REFRESH,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;
    registry
        .attach_credentials(&metadata, &database.url)
        .await
        .expect("an authorized instance starts");

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// A token endpoint that answers and then misbehaves.
//
// `refresh_locked` runs the exchange inside the transaction that holds
// `SELECT … FOR UPDATE` on the credential row, because that lock is the whole
// single-flight mechanism (spec 011 §6). Everything the exchange does is
// therefore something a credential row is held through, so the exchange must be
// bounded in time *and* in bytes — a provider that answers `200` and then
// trickles would otherwise pin the row and the pooled connection until someone
// notices.
// ---------------------------------------------------------------------------

/// A provider that answers headers immediately and then misbehaves: `/trickle`
/// dribbles a body for ever, `/flood` sends megabytes of it.
struct MisbehavingProvider {
    base: String,
    flooded: Arc<AtomicUsize>,
}

/// What `/flood` offers, in total. A token response is a few hundred bytes.
const FLOOD_BYTES: usize = 64 * 1024 * 1024;
const FLOOD_CHUNK: usize = 64 * 1024;

impl MisbehavingProvider {
    async fn start() -> Self {
        let flooded = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/trickle", post(trickle_endpoint))
            .route("/flood", post(flood_endpoint))
            .with_state(flooded.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("the misbehaving provider binds a loopback port");
        let port = listener
            .local_addr()
            .expect("the misbehaving provider has an address")
            .port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base: format!("http://127.0.0.1:{port}"),
            flooded,
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// How many bytes of the flood the provider actually got to send.
    fn flooded(&self) -> usize {
        self.flooded.load(Ordering::SeqCst)
    }
}

async fn trickle_endpoint() -> axum::response::Response {
    let stream = futures_util::stream::unfold((), |()| async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Some((
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b" ")),
            (),
        ))
    });
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(axum::body::Body::from_stream(stream))
        .expect("the trickling response builds")
}

async fn flood_endpoint(State(sent): State<Arc<AtomicUsize>>) -> axum::response::Response {
    let stream = futures_util::stream::unfold(0usize, move |offset| {
        let sent = sent.clone();
        async move {
            if offset >= FLOOD_BYTES {
                return None;
            }
            sent.fetch_add(FLOOD_CHUNK, Ordering::SeqCst);
            Some((
                Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![b'x'; FLOOD_CHUNK])),
                offset + FLOOD_CHUNK,
            ))
        }
    });
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(axum::body::Body::from_stream(stream))
        .expect("the flooding response builds")
}

/// The exchange is bounded by its own budget, headers *and* body — so the row
/// lock it is held under is bounded too, and the next claimer waits for the
/// budget rather than for the statement timeout.
#[tokio::test]
async fn a_slow_token_endpoint_cannot_pin_the_credential_row() {
    let database = TestDatabase::create("oauth_slow_endpoint").await;
    let provider = MisbehavingProvider::start().await;
    let declaration = declaration_for(&provider.endpoint("/trickle"), "read, write");
    let key = key();
    let identity = declaration.identity("acct_stub_1");

    let observer = database.client().await;
    seed(
        &observer,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() - chrono::Duration::minutes(5),
    )
    .await;

    // A second claimer, waiting on the same row the exchange is holding.
    let waiting_identity = identity.clone();
    let mut waiting = database.client().await;
    let waiter = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = std::time::Instant::now();
        let transaction = waiting
            .transaction()
            .await
            .expect("the waiting transaction opens");
        let row = store::lock(&transaction, &waiting_identity)
            .await
            .expect("the waiting claimer reads the row once it has the lock");
        (started.elapsed(), row.is_some())
    });

    let budget = Duration::from_secs(2);
    let mut client = database.client().await;
    let started = std::time::Instant::now();
    let failure = tokio::time::timeout(
        Duration::from_secs(20),
        refresh::access_token(
            &mut client,
            &key,
            &declaration,
            "acct_stub_1",
            &StubExchange::real(),
            RefreshOptions {
                budget,
                ..RefreshOptions::default()
            },
        ),
    )
    .await
    .expect("the exchange must end on its own budget, not when the provider stops talking")
    .expect_err("a token endpoint that never finishes its body is a transport failure");
    let elapsed = started.elapsed();

    assert_eq!(failure.class, CredentialErrorClass::Transport);
    assert!(
        !failure.permanent,
        "a provider that stopped talking is not a dead credential"
    );
    assert!(
        elapsed < budget * 4,
        "the whole exchange is bounded by the budget; it took {elapsed:?}"
    );

    let (waited, found) = tokio::time::timeout(Duration::from_secs(20), waiter)
        .await
        .expect("the second claimer must not wait for ever on the row lock")
        .expect("the waiting task does not panic");
    assert!(found, "the row is still there for the next claimer");
    assert!(
        waited < budget * 4,
        "the next claimer waits for the exchange budget, not for the statement timeout; \
         it waited {waited:?}"
    );

    let row = store::read(&observer, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    assert_eq!(row.rotation_count, 0, "nothing was written");
    assert!(row.unusable_reason.is_none());

    database.drop_database().await;
}

/// A token response is a few hundred bytes. A provider that answers with
/// megabytes is refused without being buffered — the bytes are bounded, not
/// merely the time.
#[tokio::test]
async fn a_flooded_token_response_is_refused_without_being_buffered() {
    let database = TestDatabase::create("oauth_flood").await;
    let provider = MisbehavingProvider::start().await;
    let declaration = declaration_for(&provider.endpoint("/flood"), "read, write");
    let key = key();
    let identity = declaration.identity("acct_stub_1");

    let observer = database.client().await;
    seed(
        &observer,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() - chrono::Duration::minutes(5),
    )
    .await;

    let mut client = database.client().await;
    let failure = tokio::time::timeout(
        Duration::from_secs(20),
        refresh::access_token(
            &mut client,
            &key,
            &declaration,
            "acct_stub_1",
            &StubExchange::real(),
            RefreshOptions::default(),
        ),
    )
    .await
    .expect("the exchange ends")
    .expect_err("a body that big is not a token response");
    assert_eq!(failure.class, CredentialErrorClass::Contract);

    // The reader stops at its ceiling and drops the connection, so the provider
    // never gets to push the rest: what it managed to send is the ceiling plus
    // whatever the sockets between here and there were already holding, and not
    // the whole answer. A reader with no ceiling drains all of it.
    assert!(
        provider.flooded() < FLOOD_BYTES / 4,
        "the read stops at the ceiling instead of buffering the whole answer; the provider \
         managed to send {} of {FLOOD_BYTES} bytes",
        provider.flooded()
    );

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// A refresh may not narrow the grant.
// ---------------------------------------------------------------------------

/// `authorize::complete` refuses a grant that does not cover the declared
/// scopes. A refresh is the same grant arriving again, so it is held to the
/// same rule: a provider that narrows on refresh must not have that written
/// over the stored row, because the deployment would keep dispatching
/// operations the narrower grant no longer authorizes.
#[tokio::test]
async fn a_refresh_that_narrows_the_grant_is_refused_and_recorded() {
    let database = TestDatabase::create("oauth_narrowed").await;
    let stub = TokenStub::start(
        StubOptions {
            rotate: true,
            granted_scope: Some("read".to_owned()),
            ..StubOptions::default()
        },
        SENTINEL_REFRESH,
    )
    .await;
    let declaration = declaration_for(&stub.token_endpoint(), "read, write");
    let key = key();
    let identity = declaration.identity("acct_stub_1");

    let observer = database.client().await;
    seed(
        &observer,
        &key,
        &identity,
        SENTINEL_REFRESH,
        Utc::now() - chrono::Duration::minutes(5),
    )
    .await;

    let mut client = database.client().await;
    let failure = refresh::access_token(
        &mut client,
        &key,
        &declaration,
        "acct_stub_1",
        &StubExchange::real(),
        RefreshOptions::default(),
    )
    .await
    .expect_err("a narrowed grant must not be written");
    assert_eq!(failure.class, CredentialErrorClass::Authentication);
    assert_eq!(failure.code, "credential_scope_shortfall");
    assert!(
        failure.permanent,
        "a provider that narrowed the grant answers the same way next time"
    );

    let row = store::read(&observer, &identity)
        .await
        .expect("the credential reads")
        .expect("the row is kept so an operator can see what happened");
    assert_eq!(
        row.scopes,
        vec!["read".to_owned(), "write".to_owned()],
        "the stored grant is not overwritten with the narrower one"
    );
    assert_eq!(row.rotation_count, 0, "nothing rotated");
    assert_eq!(
        row.unusable_reason.as_deref(),
        Some("scope_shortfall"),
        "the reason is recorded, so `credentials list` says why re-authorization is needed"
    );

    // And it is over for this row: no further attempt reaches the provider.
    for _ in 0..2 {
        let failure = refresh::access_token(
            &mut client,
            &key,
            &declaration,
            "acct_stub_1",
            &StubExchange::real(),
            RefreshOptions::default(),
        )
        .await
        .expect_err("a credential that lost a scope keeps failing");
        assert_eq!(failure.code, "credential_unusable");
    }
    assert_eq!(
        stub.exchanges(),
        1,
        "a narrowed grant is not asked for again and again"
    );

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// One instance holds one provider account.
// ---------------------------------------------------------------------------

/// Spec 011 §9: one connector instance holds one provider account, and
/// `CredentialRuntime::subject` fails every activity permanently when it finds
/// two. An operator who re-runs `donat connector authorize` and approves as a
/// different account must therefore be refused at the authorization, not
/// congratulated and left with an instance that cannot run.
#[tokio::test]
async fn authorizing_a_second_provider_account_on_one_instance_is_refused() {
    let database = TestDatabase::create("oauth_second_account").await;
    let mut client = database.client().await;
    let key = key();
    let stub = TokenStub::start(StubOptions::default(), SENTINEL_REFRESH).await;
    let declaration = declaration_for(&stub.token_endpoint(), "read, write");

    let complete = async |client: &mut tokio_postgres::Client, subject: Option<&str>| {
        let request = authorize::begin(&declaration);
        let redirected = authorize::parse_redirect(
            &declaration,
            &request,
            &format!(
                "https://deploy.example/callback?code=the-code&state={}",
                request.state
            ),
        )
        .expect("a matching redirect parses");
        authorize::complete(
            client,
            &key,
            &declaration,
            &StubExchange::real(),
            &redirected,
            subject,
            Duration::from_secs(10),
        )
        .await
    };

    let first = complete(&mut client, None)
        .await
        .expect("the first authorization writes");
    assert_eq!(first.subject, "acct_stub_1");
    assert_eq!(row_count(&client).await, 1);

    // The operator approves as a different provider account. Today that writes
    // a second row and prints "authorized"; from then on every activity on the
    // instance fails `credential_ambiguous`.
    let error = complete(&mut client, Some("acct_someone_else"))
        .await
        .expect_err("a second provider account on one instance must be refused");
    let rendered = error.to_string();
    assert!(
        rendered.contains("acct_stub_1") && rendered.contains("acct_someone_else"),
        "the operator is told which account is stored and which one arrived: {rendered}"
    );
    assert!(
        rendered.contains("revoke"),
        "and how to switch accounts on purpose: {rendered}"
    );
    assert_eq!(
        row_count(&client).await,
        1,
        "the refused authorization writes nothing"
    );
    assert_eq!(
        store::subjects(&client, "default", "acme", "acme-main")
            .await
            .expect("the subjects read"),
        vec!["acct_stub_1".to_owned()]
    );

    // Re-authorizing the account the instance already holds is still the
    // ordinary thing to do, and still replaces the row.
    let again = complete(&mut client, None)
        .await
        .expect("re-authorizing the same account is not a conflict");
    assert_eq!(again.subject, "acct_stub_1");
    assert_eq!(row_count(&client).await, 1);

    database.drop_database().await;
}
