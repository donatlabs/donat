//! Spec 015 §3 acceptance tests for the Microsoft 365 batch: the two proofs
//! that are properties of a *deployment* rather than of a request.
//!
//! * `<name>_rotation_survives_crash` — the batch's headline. The Microsoft
//!   identity platform returns a **new refresh token on every exchange**
//!   ("Refresh tokens replace themselves with a fresh token upon every use"),
//!   which is what makes an atomic write load-bearing: the value the engine will
//!   need next time is one the provider has already issued. This injects a crash
//!   between the provider exchange and the commit and proves the stored
//!   credential still works afterwards, through a real Microsoft connector
//!   instance rather than through the credential machinery alone.
//! * `<name>_permission_shortfall_fails_closed` — a deployment whose declared
//!   scopes do not authorize an operation it enabled, and one whose stored
//!   credential was granted less than it declares, are both refused at startup
//!   rather than at the first activity attempt.
//!
//! Every provider in here is a stub bound to `127.0.0.1`. Nothing reaches
//! Microsoft, and nothing needs network access beyond loopback and the test
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
use donat_server::credentials::declaration::OauthDeclaration;
use donat_server::credentials::keys::{CredentialIdentity, SealingKey};
use donat_server::credentials::oauth::HttpTokenExchange;
use donat_server::credentials::refresh::{self, RefreshOptions};
use donat_server::credentials::store;
use donat_server::migrate::run_migrate;
use donat_server::state::validate_connector_metadata;
use tokio_postgres::NoTls;

/// The two strings that must never appear outside a sealed column.
const SENTINEL_REFRESH: &str = "sentinel-microsoft-refresh-never-log-me";
const SENTINEL_ACCESS: &str = "sentinel-microsoft-access-never-log-me";

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// The four connectors under test, and the permission sets they are exercised
// with.
// ---------------------------------------------------------------------------

struct Batch {
    module: &'static str,
    reads: &'static [&'static str],
    read_scope: &'static str,
    write_operation: &'static str,
    write_scope: &'static str,
}

/// A permission from another Graph workload entirely: no operation of any
/// connector in this batch is authorized by it, so it is surplus wherever it is
/// declared.
const FOREIGN_SCOPE: &str = "Sites.Manage.All";

const OUTLOOK: Batch = Batch {
    module: "microsoft_outlook",
    reads: &["message.get", "message.list"],
    read_scope: "Mail.ReadBasic",
    write_operation: "message.delete",
    write_scope: "Mail.ReadWrite",
};

const TEAMS: Batch = Batch {
    module: "microsoft_teams",
    reads: &["channel.get", "channel.list"],
    read_scope: "Channel.ReadBasic.All",
    // Teams publishes no executable mutation, so the operation that widens the
    // grant is a read of a different resource.
    write_operation: "chat_message.list",
    write_scope: "Chat.Read",
};

const EXCEL: Batch = Batch {
    module: "microsoft_excel",
    reads: &["workbook.list"],
    read_scope: "Files.Read",
    write_operation: "worksheet.list",
    write_scope: "Files.ReadWrite",
};

const ONEDRIVE: Batch = Batch {
    module: "microsoft_onedrive",
    reads: &["file.get", "file.list_children"],
    read_scope: "Files.Read",
    write_operation: "file.delete",
    write_scope: "Files.ReadWrite",
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
                    "authorization_endpoint":
                        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
                    "token_endpoint": token_endpoint,
                    "redirect_uri": "http://127.0.0.1:8765/callback",
                    "client_id": { "value_from_env": "DONAT_MICROSOFT_TEST_CLIENT_ID" },
                    "client_secret": { "value_from_env": "DONAT_MICROSOFT_TEST_CLIENT_SECRET" },
                    "scopes": scopes,
                }
            },
            "operations": operations
                .iter()
                .map(|name| serde_json::json!({ "name": name, "capacity": capacity() }))
                .collect::<Vec<_>>(),
        }]
    }))
    .expect("the Microsoft connector metadata deserializes")
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
// `<name>_permission_shortfall_fails_closed`
// ---------------------------------------------------------------------------

/// The metadata half: an enabled operation no declared permission authorizes,
/// and a declared permission no enabled operation needs, are both refused
/// before a listener opens, each naming `config.oauth2.scopes`.
fn assert_permission_shortfall_fails_closed(batch: &Batch) {
    let token_endpoint = "https://login.microsoftonline.com/common/oauth2/v2.0/token";
    let mut enabled = batch.reads.to_vec();

    // 1. The narrow deployment is accepted with its own permission alone.
    assert_eq!(
        errors(&metadata(
            batch,
            token_endpoint,
            &enabled,
            &[batch.read_scope]
        )),
        "",
        "a read-only `{}` deployment holds only its read permission",
        batch.module
    );

    // 2. Enabling one more operation without widening the grant is refused, and
    //    the message names the operation and the least privileged permission
    //    that would work.
    enabled.push(batch.write_operation);
    let rendered = errors(&metadata(
        batch,
        token_endpoint,
        &enabled,
        &[batch.read_scope],
    ));
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
        "the refusal names the least privileged permission that would satisfy it: {rendered}"
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
        "the same deployment with the documented permission is accepted"
    );

    // 4. Microsoft documents `scope=User.Read` and
    //    `https://graph.microsoft.com/User.Read` as the same grant, and writes
    //    both `Mail.Read` and `mail.read` on its own pages. Either spelling of
    //    either case is the permission.
    for spelling in [
        format!("https://graph.microsoft.com/{}", batch.read_scope),
        batch.read_scope.to_ascii_lowercase(),
    ] {
        assert_eq!(
            errors(&metadata(batch, token_endpoint, batch.reads, &[&spelling])),
            "",
            "`{spelling}` is the same grant as `{}`",
            batch.read_scope
        );
    }

    // 5. A permission no enabled operation is authorized by is refused rather
    //    than granted quietly: least privilege is enforced in both directions.
    let surplus = errors(&metadata(
        batch,
        token_endpoint,
        batch.reads,
        &[batch.read_scope, FOREIGN_SCOPE],
    ));
    assert!(
        surplus.contains("connectors.yaml[0].config.oauth2.scopes"),
        "a surplus permission is refused with its metadata path: {surplus}"
    );
    assert!(surplus.contains(FOREIGN_SCOPE), "{surplus}");

    // 6. `offline_access` is a protocol scope and never surplus: it is what
    //    makes the token response carry a refresh token at all, and no API
    //    operation is ever authorized *by* it.
    assert_eq!(
        errors(&metadata(
            batch,
            token_endpoint,
            batch.reads,
            &[
                batch.read_scope,
                "offline_access",
                "openid",
                "profile",
                "email"
            ],
        )),
        "",
        "a protocol scope is not an API permission"
    );

    // 7. A `microsoft_*` instance configured like a key-based connector is
    //    refused: there is no way to deploy one of these without `config.oauth2`.
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
fn microsoft_outlook_permission_shortfall_fails_closed() {
    assert_permission_shortfall_fails_closed(&OUTLOOK);
}

#[test]
fn microsoft_teams_permission_shortfall_fails_closed() {
    assert_permission_shortfall_fails_closed(&TEAMS);
}

#[test]
fn microsoft_excel_permission_shortfall_fails_closed() {
    assert_permission_shortfall_fails_closed(&EXCEL);
}

#[test]
fn microsoft_onedrive_permission_shortfall_fails_closed() {
    assert_permission_shortfall_fails_closed(&ONEDRIVE);
}

/// The stored-grant half of the same property: a credential the provider
/// granted fewer permissions than the instance declares stops startup, naming
/// the missing permission and no token.
#[tokio::test]
async fn a_microsoft_credential_granted_fewer_permissions_than_declared_fails_startup() {
    let key =
        SealingKey::from_base64(deploy_time_environment()).expect("the shared key is well formed");
    let database = TestDatabase::create("microsoft_grant").await;
    let stub = TokenStub::start().await;

    let batch = &OUTLOOK;
    let enabled = [batch.reads, &[batch.write_operation]].concat();
    let metadata = metadata(
        batch,
        &stub.token_endpoint(),
        &enabled,
        &[batch.read_scope, batch.write_scope],
    );
    let identity = identity(batch, &stub.token_endpoint());
    let client = database.client().await;

    // The stored grant covers the read permission only — a token authorized
    // before this deployment enabled its delete.
    seed(&client, &key, &identity, &[batch.read_scope]).await;

    let mut registry =
        ConnectorRegistry::build(&metadata).expect("the Microsoft instance compiles from metadata");
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
// `<name>_rotation_survives_crash`
// ---------------------------------------------------------------------------

/// A refresh that rotates the refresh token, with a crash injected between the
/// provider exchange and the commit, leaves a credential that still works.
///
/// The stub models what Microsoft publishes, and only that. It issues a new
/// refresh token on every exchange — "Refresh tokens replace themselves with a
/// fresh token upon every use" — and it does **not** revoke the presented one,
/// because Microsoft says it doesn't: "The Microsoft identity platform doesn't
/// revoke old refresh tokens when used to fetch new access tokens. Securely
/// delete the old refresh token after acquiring a new one." That published
/// pairing is exactly why the crash is survivable rather than fatal, and it is
/// what this case pins: if the rotation were *also* an invalidation, the same
/// crash would destroy the account, and the only thing standing between the two
/// worlds is that the engine commits before it uses.
///
/// Three things are asserted, in the order they matter:
///
/// 1. the crash happens **after** the exchange — the provider really did rotate
///    and the answer really was lost;
/// 2. the row is byte-identical afterwards and is not marked unusable, so no
///    half-written credential exists and no retry loop is armed;
/// 3. the next attempt, made through the serving registry with this
///    deployment's own metadata, refreshes with the token still in the row,
///    commits one rotation, and ends up holding the value the provider now
///    expects.
async fn assert_rotation_survives_crash(batch: &Batch, label: &str) {
    let key =
        SealingKey::from_base64(deploy_time_environment()).expect("the shared key is well formed");
    let database = TestDatabase::create(label).await;
    let stub = TokenStub::start().await;

    let metadata = metadata(
        batch,
        &stub.token_endpoint(),
        batch.reads,
        &[batch.read_scope],
    );
    let declaration = OauthDeclaration::resolve(&metadata, "default", &instance(batch), &|name| {
        std::env::var(name).ok()
    })
    .expect("the declared OAuth2 block resolves");
    let identity = identity(batch, &stub.token_endpoint());
    let observer = database.client().await;
    // An access token that is already expired, so every attempt below must
    // refresh rather than take the fast path.
    seed_expired(&observer, &key, &identity, &[batch.read_scope]).await;
    let before = row_snapshot(&observer, &identity).await;

    // 1. The crash. `abort_before_commit` drops the transaction after the
    //    provider exchange and before the commit, which is precisely what a
    //    worker dying at that instant does.
    let mut client = database.client().await;
    let failure = refresh::access_token(
        &mut client,
        &key,
        &declaration,
        "microsoft-account-1",
        &HttpTokenExchange::new(),
        RefreshOptions {
            abort_before_commit: true,
            ..RefreshOptions::default()
        },
    )
    .await
    .expect_err("an interrupted refresh must fail");
    assert_eq!(
        failure.code, "credential_refresh_interrupted",
        "the attempt is lost, and says so"
    );
    assert!(
        !failure.permanent,
        "a lost attempt is not a dead credential"
    );
    assert_eq!(
        stub.exchanges(),
        1,
        "the crash is injected after the provider exchange, not before it"
    );
    assert_ne!(
        stub.latest_refresh(),
        SENTINEL_REFRESH,
        "the provider really did rotate, and this engine lost the answer"
    );

    // 2. Nothing was written, and nothing was marked.
    assert_eq!(
        row_snapshot(&observer, &identity).await,
        before,
        "a crash before the commit must leave the row byte-identical"
    );
    let row = store::read(&observer, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    assert!(
        row.unusable_reason.is_none(),
        "an interrupted refresh must never mark the credential unusable"
    );
    assert_eq!(row.rotation_count, 0);

    // 3. The credential still works, through the serving registry: the attempt
    //    stops at the module — it names an operation this deployment did not
    //    enable — which is after the credential seam has run and before any
    //    socket to Microsoft is opened.
    let mut registry =
        ConnectorRegistry::build(&metadata).expect("the Microsoft instance compiles from metadata");
    registry
        .attach_credentials(&metadata, &database.url)
        .await
        .expect("an authorized instance starts");
    let attempt = registry
        .execute(
            &instance(batch),
            "an.operation.this.deployment.did.not.enable",
            serde_json::json!({}),
            "activity-1",
            tokio::time::Instant::now() + Duration::from_secs(20),
        )
        .await
        .expect_err("an operation the deployment did not enable is never dispatched");
    assert_eq!(attempt.class(), ConnectorErrorClass::Invariant);
    assert_eq!(
        attempt.code(),
        "connector_invariant",
        "the attempt got as far as the module, so the credential path ran"
    );

    assert_eq!(
        stub.exchanges(),
        2,
        "the recovery performed exactly one further exchange"
    );
    assert_eq!(
        stub.presented(),
        vec![SENTINEL_REFRESH.to_owned(), SENTINEL_REFRESH.to_owned()],
        "both exchanges presented the refresh token that was actually in the row"
    );
    let row = store::read(&observer, &identity)
        .await
        .expect("the credential reads")
        .expect("the credential exists");
    assert_eq!(row.rotation_count, 1, "one committed rotation, not two");
    let stored = stored_refresh(&observer, &key, &identity).await;
    assert_eq!(
        stored,
        stub.latest_refresh(),
        "the committed refresh token is the one the provider issued last"
    );
    assert_ne!(stored, SENTINEL_REFRESH, "and it is a rotated one");

    database.drop_database().await;
}

#[tokio::test]
async fn microsoft_outlook_rotation_survives_crash() {
    assert_rotation_survives_crash(&OUTLOOK, "microsoft_outlook_rotation").await;
}

#[tokio::test]
async fn microsoft_teams_rotation_survives_crash() {
    assert_rotation_survives_crash(&TEAMS, "microsoft_teams_rotation").await;
}

#[tokio::test]
async fn microsoft_excel_rotation_survives_crash() {
    assert_rotation_survives_crash(&EXCEL, "microsoft_excel_rotation").await;
}

#[tokio::test]
async fn microsoft_onedrive_rotation_survives_crash() {
    assert_rotation_survives_crash(&ONEDRIVE, "microsoft_onedrive_rotation").await;
}

/// A Microsoft instance never sends an unauthenticated request: with no
/// credential runtime resolved the attempt fails before a socket opens, and the
/// connector itself has no auth plan that could render one without a stored
/// token ([[043-the-credential-seam-refuses-before-it-sends]]).
#[tokio::test]
async fn a_microsoft_connector_never_sends_an_unauthenticated_request() {
    let _ = deploy_time_environment();
    let metadata = metadata(
        &ONEDRIVE,
        "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        ONEDRIVE.reads,
        &[ONEDRIVE.read_scope],
    );
    let registry = ConnectorRegistry::build(&metadata).expect("the Microsoft instance compiles");
    let failure = registry
        .execute(
            &instance(&ONEDRIVE),
            "file.get",
            serde_json::json!({ "item_id": "1" }),
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

fn deploy_time_environment() -> &'static str {
    static ENVIRONMENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ENVIRONMENT.get_or_init(|| {
        let key = SealingKey::generate_base64_for_tests();
        // SAFETY: these variable names are owned by this test binary, are set
        // once, and are never unset.
        unsafe {
            std::env::set_var("DONAT_MICROSOFT_TEST_CLIENT_ID", "microsoft-client-id");
            std::env::set_var(
                "DONAT_MICROSOFT_TEST_CLIENT_SECRET",
                "microsoft-client-secret",
            );
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
        subject: "microsoft-account-1".to_owned(),
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
        .expect("the credential holds a refresh token");
    key.open(identity, &sealed)
        .expect("the stored refresh token opens")
        .expose_str()
        .expect("the stored refresh token is UTF-8")
        .to_owned()
}

/// A local RFC 6749 §6 token endpoint that behaves the way Microsoft documents.
struct StubInner {
    exchanges: AtomicUsize,
    /// Every refresh token that has been *presented* to this endpoint, in order.
    presented: Mutex<Vec<String>>,
    /// Every refresh token this endpoint has ever issued, and the seed. The old
    /// ones stay valid: "The Microsoft identity platform doesn't revoke old
    /// refresh tokens when used to fetch new access tokens."
    accepted: Mutex<HashSet<String>>,
    latest_refresh: Mutex<String>,
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
            presented: Mutex::new(Vec::new()),
            accepted: Mutex::new(HashSet::from([SENTINEL_REFRESH.to_owned()])),
            latest_refresh: Mutex::new(SENTINEL_REFRESH.to_owned()),
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

    fn presented(&self) -> Vec<String> {
        self.inner
            .presented
            .lock()
            .expect("the stub lock is intact")
            .clone()
    }

    fn latest_refresh(&self) -> String {
        self.inner
            .latest_refresh
            .lock()
            .expect("the stub lock is intact")
            .clone()
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
        let candidate = field("refresh_token").unwrap_or_default();
        stub.presented
            .lock()
            .expect("the stub lock is intact")
            .push(candidate.clone());
        if !stub
            .accepted
            .lock()
            .expect("the stub lock is intact")
            .contains(&candidate)
        {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": "invalid_grant" })),
            );
        }
    }
    // "Refresh tokens replace themselves with a fresh token upon every use",
    // and the presented one is not revoked.
    let sequence = stub.issued.fetch_add(1, Ordering::SeqCst) + 1;
    let rotated = format!("{SENTINEL_REFRESH}-{sequence}");
    stub.accepted
        .lock()
        .expect("the stub lock is intact")
        .insert(rotated.clone());
    *stub.latest_refresh.lock().expect("the stub lock is intact") = rotated.clone();
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
