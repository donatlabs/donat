//! Spec 014 §3 acceptance tests for the Google Workspace batch: the two proofs
//! that are properties of a *deployment* rather than of a request.
//!
//! * `<name>_scope_shortfall_fails_closed` — a deployment whose declared scopes
//!   do not cover an operation it enabled, and one whose stored credential was
//!   granted less than it declares, are both refused at startup rather than at
//!   the first activity attempt.
//! * `<name>_refresh_happens_once_under_concurrency` — two concurrent
//!   activities on one credential row perform exactly one token exchange, which
//!   is what proves this batch uses the spec 011 path and keeps no cache of its
//!   own.
//!
//! Every provider in here is a stub bound to `127.0.0.1`. Nothing reaches
//! Google, and nothing needs network access beyond loopback and the test
//! Postgres.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use chrono::Utc;
use donat_metadata::Metadata;
use donat_server::connectors::{ConnectorErrorClass, ConnectorRegistry};
use donat_server::credentials::CredentialRuntime;
use donat_server::credentials::declaration::OauthDeclaration;
use donat_server::credentials::keys::{CredentialIdentity, SealingKey};
use donat_server::credentials::store;
use donat_server::migrate::run_migrate;
use donat_server::state::validate_connector_metadata;
use tokio_postgres::NoTls;

/// The one string that must never appear outside a sealed column.
const SENTINEL_REFRESH: &str = "sentinel-google-refresh-never-log-me";
const SENTINEL_ACCESS: &str = "sentinel-google-access-never-log-me";

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// The four connectors under test, and the scope sets they are exercised with.
// ---------------------------------------------------------------------------

/// One connector's read-only deployment: the operations a reader enables, the
/// scope those operations are authorized by, and one operation that needs more.
struct Batch {
    module: &'static str,
    reads: &'static [&'static str],
    read_scope: &'static str,
    write_operation: &'static str,
    write_scope: &'static str,
}

/// A scope from another Google product entirely: no operation of any connector
/// in this batch is authorized by it, so it is surplus wherever it is declared.
const FOREIGN_SCOPE: &str = "https://www.googleapis.com/auth/youtube.readonly";

const SHEETS: Batch = Batch {
    module: "google_sheets",
    reads: &["values.get", "spreadsheet.get"],
    read_scope: "https://www.googleapis.com/auth/spreadsheets.readonly",
    write_operation: "values.update",
    write_scope: "https://www.googleapis.com/auth/spreadsheets",
};

const DRIVE: Batch = Batch {
    module: "google_drive",
    reads: &["file.get", "file.list"],
    read_scope: "https://www.googleapis.com/auth/drive.metadata.readonly",
    write_operation: "file.delete",
    write_scope: "https://www.googleapis.com/auth/drive.file",
};

const GMAIL: Batch = Batch {
    module: "google_gmail",
    reads: &["message.get", "message.list"],
    read_scope: "https://www.googleapis.com/auth/gmail.readonly",
    write_operation: "label.delete",
    write_scope: "https://www.googleapis.com/auth/gmail.labels",
};

const CALENDAR: Batch = Batch {
    module: "google_calendar",
    reads: &["event.get", "event.list"],
    read_scope: "https://www.googleapis.com/auth/calendar.events.readonly",
    write_operation: "event.update",
    write_scope: "https://www.googleapis.com/auth/calendar.events",
};

// ---------------------------------------------------------------------------
// Deployment metadata
// ---------------------------------------------------------------------------

fn capacity() -> serde_json::Value {
    serde_json::json!({
        "max_in_flight": 1,
        "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
    })
}

fn metadata(batch: &Batch, token_endpoint: &str, operations: &[&str], scopes: &[&str]) -> Metadata {
    serde_json::from_value(serde_json::json!({
        "version": 3,
        "sources": [{ "name": "default", "kind": "postgres", "configuration": {} }],
        "connectors": [{
            "name": instance(batch),
            "module": batch.module,
            "config": {
                "endpoint_identity": batch.module,
                "credential_identity": format!("{}-oauth", batch.module),
                "oauth2": {
                    "authorization_endpoint": "https://accounts.google.com/o/oauth2/v2/auth",
                    "token_endpoint": token_endpoint,
                    "redirect_uri": "http://127.0.0.1:8765/callback",
                    "client_id": { "value_from_env": "DONAT_GOOGLE_TEST_CLIENT_ID" },
                    "client_secret": { "value_from_env": "DONAT_GOOGLE_TEST_CLIENT_SECRET" },
                    "scopes": scopes,
                }
            },
            "operations": operations
                .iter()
                .map(|name| serde_json::json!({ "name": name, "capacity": capacity() }))
                .collect::<Vec<_>>(),
        }]
    }))
    .expect("the Google connector metadata deserializes")
}

fn instance(batch: &Batch) -> String {
    format!("{}-main", batch.module)
}

fn errors(metadata: &Metadata) -> String {
    validate_connector_metadata(metadata)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// `<name>_scope_shortfall_fails_closed`
// ---------------------------------------------------------------------------

/// The metadata half: an enabled operation no declared scope authorizes, and a
/// declared scope no enabled operation needs, are both refused before a
/// listener opens, each naming `config.oauth2.scopes`.
fn assert_scope_shortfall_fails_closed(batch: &Batch) {
    let token_endpoint = "https://oauth2.googleapis.com/token";
    let mut enabled = batch.reads.to_vec();

    // 1. The read-only deployment is accepted with the read scope alone. This
    //    is the property spec 014 §1 asks for: enabling reads must not force a
    //    write grant.
    let read_only = metadata(batch, token_endpoint, &enabled, &[batch.read_scope]);
    assert_eq!(
        errors(&read_only),
        "",
        "a read-only `{}` deployment holds only its read scope",
        batch.module
    );

    // 2. Enabling one write without widening the grant is refused, and the
    //    message names the operation and the least scope that would work.
    enabled.push(batch.write_operation);
    let shortfall = metadata(batch, token_endpoint, &enabled, &[batch.read_scope]);
    let rendered = errors(&shortfall);
    assert!(
        rendered.contains(&format!(
            "connectors.yaml[0].config.oauth2.scopes: connector operation `{}` on module `{}` is \
             not authorized by any declared scope",
            batch.write_operation, batch.module
        )),
        "the refusal names the operation and its metadata path: {rendered}"
    );
    assert!(
        rendered.contains(batch.write_scope),
        "the refusal names the least scope that would satisfy it: {rendered}"
    );

    // 3. ...and widening the grant accepts it.
    assert_eq!(
        errors(&metadata(
            batch,
            token_endpoint,
            &enabled,
            &[batch.read_scope, batch.write_scope],
        )),
        "",
        "the same deployment with the documented scope is accepted"
    );

    // 4. A scope no enabled operation is authorized by is refused rather than
    //    granted quietly: least privilege is enforced in both directions.
    //
    //    The surplus scope has to come from another Google product, because
    //    Google's own scope lists are nested: `spreadsheets` authorizes the
    //    Sheets reads as well as its writes, so declaring it alongside
    //    `spreadsheets.readonly` is a broader grant rather than an unused one,
    //    and only the deployment can say which it meant.
    let surplus = errors(&metadata(
        batch,
        token_endpoint,
        batch.reads,
        &[batch.read_scope, FOREIGN_SCOPE],
    ));
    assert!(
        surplus.contains("connectors.yaml[0].config.oauth2.scopes"),
        "a surplus scope is refused with its metadata path: {surplus}"
    );
    assert!(surplus.contains(FOREIGN_SCOPE), "{surplus}");

    // 5. Google's OpenID Connect scopes are never surplus: they grant no
    //    Workspace API access and are how a token response names the account.
    assert_eq!(
        errors(&metadata(
            batch,
            token_endpoint,
            batch.reads,
            &[batch.read_scope, "openid", "email"],
        )),
        "",
        "an identity scope is not an API grant"
    );

    // 6. A `google_*` instance configured like a key-based connector is refused
    //    too: there is no way to deploy one of these without `config.oauth2`.
    let mut without: serde_json::Value = serde_json::to_value(metadata(
        batch,
        token_endpoint,
        batch.reads,
        &[batch.read_scope],
    ))
    .expect("metadata serializes");
    without["connectors"][0]["config"]
        .as_object_mut()
        .expect("a config object")
        .remove("oauth2");
    let without: Metadata = serde_json::from_value(without).expect("metadata deserializes");
    let rendered = errors(&without);
    assert!(
        rendered.contains("connectors.yaml[0].config.oauth2"),
        "an instance with no OAuth2 declaration is refused: {rendered}"
    );
}

#[test]
fn google_sheets_scope_shortfall_fails_closed() {
    assert_scope_shortfall_fails_closed(&SHEETS);
}

#[test]
fn google_drive_scope_shortfall_fails_closed() {
    assert_scope_shortfall_fails_closed(&DRIVE);
}

#[test]
fn google_gmail_scope_shortfall_fails_closed() {
    assert_scope_shortfall_fails_closed(&GMAIL);
}

#[test]
fn google_calendar_scope_shortfall_fails_closed() {
    assert_scope_shortfall_fails_closed(&CALENDAR);
}

/// The stored-grant half of the same property: a credential the provider
/// granted fewer scopes than the instance declares stops startup, naming the
/// missing scopes and no token.
#[tokio::test]
async fn a_google_credential_granted_fewer_scopes_than_declared_fails_startup() {
    let key_material = deploy_time_environment().to_owned();
    let key = SealingKey::from_base64(&key_material).expect("the shared key is well formed");
    let database = TestDatabase::create("google_scope_grant").await;
    let stub = TokenStub::start().await;

    let batch = &SHEETS;
    let enabled = [batch.reads, &[batch.write_operation]].concat();
    let metadata = metadata(
        batch,
        &stub.token_endpoint(),
        &enabled,
        &[batch.read_scope, batch.write_scope],
    );
    let identity = identity(batch, &stub.token_endpoint());
    let client = database.client().await;

    // The stored grant covers the read scope only — a token authorized before
    // this deployment enabled its write.
    seed(&client, &key, &identity, &[batch.read_scope]).await;

    let mut registry =
        ConnectorRegistry::build(&metadata).expect("the Google instance compiles from metadata");
    let error = registry
        .attach_credentials(&metadata, &database.url)
        .await
        .expect_err("a grant that falls short of the declaration must not serve");
    let rendered = error.to_string();
    assert!(rendered.contains(&instance(batch)), "{rendered}");
    assert!(rendered.contains("fewer scopes"), "{rendered}");
    assert!(rendered.contains(batch.write_scope), "{rendered}");
    assert!(
        !rendered.contains(SENTINEL_ACCESS) && !rendered.contains(SENTINEL_REFRESH),
        "a startup failure carries no token: {rendered}"
    );

    // The same deployment with the grant it declares does start.
    seed(
        &client,
        &key,
        &identity,
        &[batch.read_scope, batch.write_scope],
    )
    .await;
    registry
        .attach_credentials(&metadata, &database.url)
        .await
        .expect("a credential that covers the declaration serves");

    database.drop_database().await;
}

// ---------------------------------------------------------------------------
// `<name>_refresh_happens_once_under_concurrency`
// ---------------------------------------------------------------------------

/// Two concurrent activities on one connector instance's credential row perform
/// exactly one token exchange.
///
/// The attempt deliberately stops after the header is applied: it names an
/// operation this deployment did not enable, so the registry refuses it inside
/// `execute_authorized` — after the credential path has run and before any
/// socket to Google is opened. No test in this workspace reaches a provider,
/// and a hand-written connector's origin *is* its provider's, so the credential
/// property is proven at the last point before the wire.
///
/// What it proves is the whole of spec 011 §6: the refresh is single-flighted
/// by the row lock rather than by anything in this batch, so a second binary
/// during a rolling deploy behaves the same way. If any of these four
/// connectors had a token cache of its own, the two attempts would take
/// different paths and the exchange count would not be one.
async fn assert_refresh_happens_once_under_concurrency(batch: &Batch, label: &str) {
    let key_material = deploy_time_environment().to_owned();
    let key = SealingKey::from_base64(&key_material).expect("the shared key is well formed");
    let database = TestDatabase::create(label).await;
    let stub = TokenStub::start().await;

    let metadata = metadata(
        batch,
        &stub.token_endpoint(),
        batch.reads,
        &[batch.read_scope],
    );
    let identity = identity(batch, &stub.token_endpoint());
    let client = database.client().await;
    // An access token that is already expired, so the first claimer must
    // refresh and the second must observe its committed result.
    seed_expired(&client, &key, &identity, &[batch.read_scope]).await;

    let mut registry =
        ConnectorRegistry::build(&metadata).expect("the Google instance compiles from metadata");
    registry
        .attach_credentials(&metadata, &database.url)
        .await
        .expect("an authorized instance starts");

    let attempt = async || {
        registry
            .execute(
                &instance(batch),
                "an.operation.this.deployment.did.not.enable",
                serde_json::json!({}),
                "activity-1",
                tokio::time::Instant::now() + Duration::from_secs(20),
            )
            .await
            .expect_err("an operation the deployment did not enable is never dispatched")
    };
    let (left, right) = tokio::join!(attempt(), attempt());

    assert_eq!(
        stub.exchanges(),
        1,
        "two concurrent activities on one credential row must produce one token exchange"
    );
    for failure in [&left, &right] {
        assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
        assert_eq!(
            failure.code(),
            "connector_invariant",
            "both attempts got as far as the module and stopped there"
        );
    }

    let row = store::read(&client, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    assert_eq!(
        row.rotation_count, 1,
        "one exchange, one committed rotation"
    );

    database.drop_database().await;
}

#[tokio::test]
async fn google_sheets_refresh_happens_once_under_concurrency() {
    assert_refresh_happens_once_under_concurrency(&SHEETS, "google_sheets_flight").await;
}

#[tokio::test]
async fn google_drive_refresh_happens_once_under_concurrency() {
    assert_refresh_happens_once_under_concurrency(&DRIVE, "google_drive_flight").await;
}

#[tokio::test]
async fn google_gmail_refresh_happens_once_under_concurrency() {
    assert_refresh_happens_once_under_concurrency(&GMAIL, "google_gmail_flight").await;
}

#[tokio::test]
async fn google_calendar_refresh_happens_once_under_concurrency() {
    assert_refresh_happens_once_under_concurrency(&CALENDAR, "google_calendar_flight").await;
}

/// A Google instance never sends an unauthenticated request: with no credential
/// runtime resolved the attempt fails before a socket opens, and the connector
/// itself has no auth plan that could render one without a stored token
/// ([[043-the-credential-seam-refuses-before-it-sends]]).
#[tokio::test]
async fn a_google_connector_never_sends_an_unauthenticated_request() {
    let _ = deploy_time_environment();
    let metadata = metadata(
        &SHEETS,
        "https://oauth2.googleapis.com/token",
        SHEETS.reads,
        &[SHEETS.read_scope],
    );
    let registry = ConnectorRegistry::build(&metadata).expect("the Google instance compiles");
    let failure = registry
        .execute(
            &instance(&SHEETS),
            "spreadsheet.get",
            serde_json::json!({ "spreadsheet_id": "1" }),
            "activity-1",
            tokio::time::Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect_err("a declared credential that cannot be applied fails the attempt");
    assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
    assert_eq!(failure.code(), "connector_credential_runtime_absent");
}

// ---------------------------------------------------------------------------
// Support: the deploy-time environment, the token stub, and the test database.
// ---------------------------------------------------------------------------

/// The deploy-time environment these tests share. The registry resolves the
/// OAuth2 client identity through the same startup check every other connector
/// secret goes through, and that check reads the process environment.
fn deploy_time_environment() -> &'static str {
    static ENVIRONMENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ENVIRONMENT.get_or_init(|| {
        let key = SealingKey::generate_base64_for_tests();
        // SAFETY: these variable names are owned by this test binary, are set
        // once, and are never unset.
        unsafe {
            std::env::set_var("DONAT_GOOGLE_TEST_CLIENT_ID", "google-client-id");
            std::env::set_var("DONAT_GOOGLE_TEST_CLIENT_SECRET", "google-client-secret");
            std::env::set_var("DONAT_CREDENTIAL_KEY", &key);
        }
        key
    })
}

fn identity(batch: &Batch, token_endpoint: &str) -> CredentialIdentity {
    CredentialIdentity {
        source: "default".to_owned(),
        connector: batch.module.to_owned(),
        instance: instance(batch),
        subject: "google-account-1".to_owned(),
        token_origin: token_endpoint.to_owned(),
    }
}

async fn seed(
    client: &tokio_postgres::Client,
    key: &SealingKey,
    identity: &CredentialIdentity,
    scopes: &[&str],
) {
    write_credential(
        client,
        key,
        identity,
        scopes,
        Utc::now() + chrono::Duration::hours(1),
    )
    .await;
}

async fn seed_expired(
    client: &tokio_postgres::Client,
    key: &SealingKey,
    identity: &CredentialIdentity,
    scopes: &[&str],
) {
    write_credential(
        client,
        key,
        identity,
        scopes,
        Utc::now() - chrono::Duration::minutes(5),
    )
    .await;
}

async fn write_credential(
    client: &tokio_postgres::Client,
    key: &SealingKey,
    identity: &CredentialIdentity,
    scopes: &[&str],
    expires_at: chrono::DateTime<Utc>,
) {
    let scopes = scopes
        .iter()
        .map(|scope| (*scope).to_owned())
        .collect::<Vec<_>>();
    let sealed_access = key.seal(identity, format!("{SENTINEL_ACCESS}-0").as_bytes());
    let sealed_refresh = key.seal(identity, SENTINEL_REFRESH.as_bytes());
    store::upsert(
        client,
        identity,
        &sealed_access,
        expires_at,
        Some(&sealed_refresh),
        &scopes,
    )
    .await
    .expect("the seeded credential writes");
}

/// A local RFC 6749 §5 token endpoint that counts its exchanges.
struct StubInner {
    exchanges: AtomicUsize,
    accepted: Mutex<HashSet<String>>,
    issued: AtomicUsize,
}

struct TokenStub {
    inner: Arc<StubInner>,
    base: String,
}

impl TokenStub {
    async fn start() -> Self {
        let inner = Arc::new(StubInner {
            exchanges: AtomicUsize::new(0),
            accepted: Mutex::new(HashSet::from([SENTINEL_REFRESH.to_owned()])),
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
    if field("grant_type").as_deref() == Some("refresh_token") {
        let presented = field("refresh_token").unwrap_or_default();
        if !stub
            .accepted
            .lock()
            .expect("the stub lock is intact")
            .contains(&presented)
        {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": "invalid_grant" })),
            );
        }
    }
    let sequence = stub.issued.fetch_add(1, Ordering::SeqCst) + 1;
    // Google rotates its refresh token only for some client types; this stub
    // rotates so the committed row is observable.
    let rotated = format!("{SENTINEL_REFRESH}-{sequence}");
    let mut accepted = stub.accepted.lock().expect("the stub lock is intact");
    accepted.clear();
    accepted.insert(rotated.clone());
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({
            "access_token": format!("{SENTINEL_ACCESS}-{sequence}"),
            "refresh_token": rotated,
            "token_type": "Bearer",
            "expires_in": 3599,
            "scope": field("scope").unwrap_or_default(),
        })),
    )
}

struct TestDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl TestDatabase {
    async fn create(label: &str) -> Self {
        let admin_url = std::env::var("PG_URL").unwrap_or_else(|_| {
            "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned()
        });
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

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

/// Silence the unused-import warning when the credential runtime is only
/// reached through `ConnectorRegistry::attach_credentials`.
#[allow(dead_code)]
fn _credential_runtime_is_the_one_path(runtime: &CredentialRuntime) -> bool {
    runtime.declares("google_sheets-main")
}

#[allow(dead_code)]
fn _declaration_is_resolved_from_metadata(metadata: &Metadata) -> bool {
    OauthDeclaration::resolve_all(metadata, "default", &|name| std::env::var(name).ok()).is_ok()
}
