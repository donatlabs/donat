//! Native conformance harness.
//!
//! Executes Donat-derived YAML fixtures (`crates/conformance/fixtures`)
//! against a freshly spawned `donat` instance, replicating the semantics
//! of tests-py `check_query_f`: same fixture format (`url`, `status`,
//! `headers`, `query`, `response`, list-of-steps files, `!include`), same
//! response comparison (key order enforced inside `data`, order-insensitive
//! elsewhere), same legacy-Apollo websocket protocol.
//!
//! Each suite runs against its own Postgres database (created from the
//! admin connection in `PG_URL`), so suites are hermetic and parallel-safe.

use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Once;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value as Json, json};

mod action_webhook;
pub mod cron_webhook;
pub mod idp_stub;
pub mod object_store;
mod remote_graphql;

// The leaf helpers an application test needs as much as a conformance suite
// does — stubs, fixture loading, response comparison, migrations — live in
// `donat-testkit` and are re-exported here under their historical names.
pub use donat_testkit::{
    SelMap, apply_sql_migration_dir, auth_hook, json_matches, load_fixture, provider_stub,
    response_matches, sel_tree_from_query, strip_mcp_content,
};

// ---------------------------------------------------------------- fixtures

pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

// ------------------------------------------------------------------ engine
//
// The harness sets up each suite WITHOUT the engine's runtime admin API
// (`/v1/query`, `/v2/query`, `/v1/metadata`). Instead it:
//
//  - creates the per-suite database and the postgis extension directly via
//    the `postgres` crate;
//  - parses every setup fixture and APPLIES its ops in-harness: schema
//    `run_sql` and seed `insert` ops run over the suite database via
//    `postgres`, while metadata ops (track_table, permissions,
//    relationships, inherited roles, query collections, ...) accumulate
//    into an in-memory `donat_metadata::Metadata`;
//  - spawns the engine lazily, on the first request, serializing the
//    accumulated metadata to a `version: 3` metadata directory and passing
//    it via `--metadata-dir`.
//
// The engine still ships the admin API for now; this harness simply never
// calls it, so that API can later be deleted.

use std::cell::RefCell;

use donat_metadata::{
    AllowlistEntry, ArrayRelationship, ComputedField, CronTrigger, DatabaseUrl, DeletePermission,
    EventTrigger, FunctionEntry, FunctionPermission, InheritedRole, InsertPermission, McpMetadata,
    Metadata, ObjectRelationship, PermissionEntry, QualifiedTable, QueryCollection,
    RemoteRelationship, RemoteSchema, RemoteSchemaPermission, RestEndpoint, SelectPermission,
    Source, SourceKind, TableConfiguration, TableEntry, UpdatePermission,
};

/// Datasource backends covered by the mandatory conformance matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendId {
    Postgres,
    Sqlite,
    Mysql,
    Clickhouse,
}

impl BackendId {
    pub const ALL: [Self; 4] = [Self::Postgres, Self::Sqlite, Self::Mysql, Self::Clickhouse];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Sqlite => "sqlite",
            Self::Mysql => "mysql",
            Self::Clickhouse => "clickhouse",
        }
    }

    pub const fn source_kind(self) -> SourceKind {
        match self {
            Self::Postgres => SourceKind::Postgres,
            Self::Sqlite => SourceKind::Sqlite,
            Self::Mysql => SourceKind::Mysql,
            Self::Clickhouse => SourceKind::Clickhouse,
        }
    }

    pub fn capabilities(self) -> donat_backend::Capabilities {
        match self {
            Self::Postgres => donat_backend::capabilities::postgres(),
            Self::Sqlite => donat_backend::capabilities::sqlite(),
            Self::Mysql => donat_backend::capabilities::mysql(),
            Self::Clickhouse => donat_backend::capabilities::clickhouse(),
        }
    }

    pub const fn required_url_env(self) -> Option<&'static str> {
        match self {
            Self::Mysql => Some("MYSQL_URL"),
            Self::Clickhouse => Some("CLICKHOUSE_URL"),
            Self::Postgres | Self::Sqlite => None,
        }
    }

    pub fn validate_configuration(
        self,
        get_env: impl FnOnce(&str) -> Option<String>,
    ) -> Result<()> {
        let Some(key) = self.required_url_env() else {
            return Ok(());
        };
        match get_env(key) {
            Some(value) if !value.trim().is_empty() => Ok(()),
            _ => Err(anyhow!(
                "CONF_BACKEND={} requires non-empty {key}",
                self.as_str()
            )),
        }
    }

    pub fn parse(value: Option<&str>) -> Result<Self> {
        let value = value
            .filter(|value| !value.is_empty())
            .unwrap_or("postgres");
        Self::ALL
            .into_iter()
            .find(|backend| backend.as_str() == value)
            .ok_or_else(|| {
                let supported = Self::ALL.map(Self::as_str).join(", ");
                anyhow!("unknown CONF_BACKEND '{value}'; expected one of: {supported}")
            })
    }

    pub fn selected() -> Result<Self> {
        let backend = Self::parse(std::env::var("CONF_BACKEND").ok().as_deref())?;
        backend.validate_configuration(|key| std::env::var(key).ok())?;
        Ok(backend)
    }
}

impl From<SourceKind> for BackendId {
    fn from(kind: SourceKind) -> Self {
        match kind {
            SourceKind::Postgres => Self::Postgres,
            SourceKind::Sqlite => Self::Sqlite,
            SourceKind::Mysql => Self::Mysql,
            SourceKind::Clickhouse => Self::Clickhouse,
        }
    }
}

/// Capabilities used to classify shared conformance cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseCapability {
    Reads,
    Transport,
    Mutations,
    Relationships,
    Aggregates,
    Json,
    Geo,
    Relay,
    Regex,
    Upsert,
    Returning,
    DistinctOn,
    Lateral,
    NestedInserts,
}

impl CaseCapability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reads => "reads",
            Self::Transport => "transport",
            Self::Mutations => "mutations",
            Self::Relationships => "relationships",
            Self::Aggregates => "aggregates",
            Self::Json => "json",
            Self::Geo => "geo",
            Self::Relay => "relay",
            Self::Regex => "regex",
            Self::Upsert => "upsert",
            Self::Returning => "returning",
            Self::DistinctOn => "distinct-on",
            Self::Lateral => "lateral",
            Self::NestedInserts => "nested-inserts",
        }
    }

    pub fn supported_by(self, backend: BackendId) -> bool {
        let capabilities = backend.capabilities();
        match self {
            Self::Reads | Self::Transport => true,
            Self::Mutations => capabilities.mutations,
            Self::Relationships => capabilities.relationships,
            Self::Aggregates => capabilities.aggregates,
            Self::Json => capabilities.json_ops != donat_backend::capabilities::JsonOps::None,
            Self::Geo => capabilities.geo,
            Self::Relay => capabilities.relay,
            Self::Regex => capabilities.regex_ops,
            Self::Upsert => capabilities.upsert != donat_backend::capabilities::UpsertKind::None,
            Self::Returning => capabilities.returning,
            Self::DistinctOn => capabilities.distinct_on,
            Self::Lateral => capabilities.lateral,
            Self::NestedInserts => capabilities.nested_inserts,
        }
    }
}

/// A backend-specific difference that remains visible in the shared matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownDifference {
    pub backend: BackendId,
    pub reason: &'static str,
    pub tracking: &'static str,
}

/// One single-sourced behavior case and the capabilities required to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConformanceCase {
    pub name: &'static str,
    pub requires: &'static [CaseCapability],
    pub known_differences: &'static [KnownDifference],
}

impl ConformanceCase {
    pub const fn new(name: &'static str, requires: &'static [CaseCapability]) -> Self {
        Self {
            name,
            requires,
            known_differences: &[],
        }
    }

    pub const fn with_known_differences(
        name: &'static str,
        requires: &'static [CaseCapability],
        known_differences: &'static [KnownDifference],
    ) -> Self {
        Self {
            name,
            requires,
            known_differences,
        }
    }
}

/// Deterministic outcome counts emitted by one shared conformance group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaseSummary {
    pub total: usize,
    pub passed: usize,
    pub unsupported: usize,
    pub known_differences: usize,
    pub failed: usize,
}

/// Run every declared case exactly once for one backend.
pub fn run_conformance_cases(
    group: &'static str,
    backend: BackendId,
    cases: &'static [ConformanceCase],
    mut run: impl FnMut(&'static str),
) -> CaseSummary {
    validate_case_manifest(group, cases);

    let mut summary = CaseSummary {
        total: cases.len(),
        passed: 0,
        unsupported: 0,
        known_differences: 0,
        failed: 0,
    };
    let mut first_failure: Option<Box<dyn std::any::Any + Send>> = None;

    for case in cases {
        let missing = case
            .requires
            .iter()
            .copied()
            .filter(|capability| !capability.supported_by(backend))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            summary.unsupported += 1;
            let capabilities = missing
                .iter()
                .map(|capability| capability.as_str())
                .collect::<Vec<_>>()
                .join(",");
            eprintln!(
                "conformance backend={} group={group} case={} outcome=unsupported-by-capability capabilities={capabilities}",
                backend.as_str(),
                case.name
            );
            continue;
        }

        if let Some(difference) = case
            .known_differences
            .iter()
            .find(|difference| difference.backend == backend)
        {
            summary.known_differences += 1;
            eprintln!(
                "conformance backend={} group={group} case={} outcome=known-diff reason={} tracking={}",
                backend.as_str(),
                case.name,
                difference.reason,
                difference.tracking
            );
            continue;
        }

        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(case.name))) {
            Ok(()) => {
                summary.passed += 1;
                eprintln!(
                    "conformance backend={} group={group} case={} outcome=passed",
                    backend.as_str(),
                    case.name
                );
            }
            Err(failure) => {
                summary.failed += 1;
                eprintln!(
                    "conformance backend={} group={group} case={} outcome=failed",
                    backend.as_str(),
                    case.name
                );
                if first_failure.is_none() {
                    first_failure = Some(failure);
                }
            }
        }
    }

    eprintln!(
        "conformance backend={} group={group} total={} passed={} unsupported={} known-diff={} failed={}",
        backend.as_str(),
        summary.total,
        summary.passed,
        summary.unsupported,
        summary.known_differences,
        summary.failed
    );

    if let Some(failure) = first_failure {
        std::panic::resume_unwind(failure);
    }
    summary
}

fn validate_case_manifest(group: &str, cases: &[ConformanceCase]) {
    assert!(!group.trim().is_empty(), "conformance group name is empty");
    assert!(
        !cases.is_empty(),
        "conformance group '{group}' has no cases"
    );

    for (case_index, case) in cases.iter().enumerate() {
        assert!(
            !case.name.trim().is_empty(),
            "conformance group '{group}' has an empty case name"
        );
        assert!(
            !cases[..case_index]
                .iter()
                .any(|existing| existing.name == case.name),
            "conformance group '{group}' has duplicate case '{}'",
            case.name
        );
        for (requirement_index, requirement) in case.requires.iter().enumerate() {
            assert!(
                !case.requires[..requirement_index].contains(requirement),
                "conformance case '{group}/{}' repeats capability '{}'",
                case.name,
                requirement.as_str()
            );
        }
        for (difference_index, difference) in case.known_differences.iter().enumerate() {
            assert!(
                !difference.reason.trim().is_empty(),
                "known difference '{group}/{}' has no reason",
                case.name
            );
            assert!(
                !difference.tracking.trim().is_empty(),
                "known difference '{group}/{}' has no tracking reference",
                case.name
            );
            assert!(
                !case.known_differences[..difference_index]
                    .iter()
                    .any(|existing| existing.backend == difference.backend),
                "conformance case '{group}/{}' repeats known difference for backend '{}'",
                case.name,
                difference.backend.as_str()
            );
            assert!(
                case.requires
                    .iter()
                    .all(|capability| capability.supported_by(difference.backend)),
                "known difference '{group}/{}' hides an unsupported capability for backend '{}'",
                case.name,
                difference.backend.as_str()
            );
        }
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

static BUILD_ENGINE: Once = Once::new();
const ENGINE_HEALTH_DEADLINE: Duration = Duration::from_secs(30);
const ENGINE_HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const ENGINE_START_ATTEMPTS: usize = 3;
const ENGINE_START_RETRY_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug)]
struct EngineStartFailure {
    attempt: usize,
    reason: String,
    log_path: PathBuf,
}

fn retry_engine_start<T>(
    attempts: usize,
    retry_delay: Duration,
    mut start: impl FnMut(usize) -> Result<T, EngineStartFailure>,
) -> Result<T, Vec<EngineStartFailure>> {
    assert!(attempts > 0, "engine startup needs at least one attempt");
    let mut failures = Vec::with_capacity(attempts);
    for attempt in 1..=attempts {
        match start(attempt) {
            Ok(value) => return Ok(value),
            Err(failure) => {
                failures.push(failure);
                if attempt < attempts {
                    std::thread::sleep(retry_delay);
                }
            }
        }
    }
    Err(failures)
}

fn format_engine_start_failures(failures: &[EngineStartFailure]) -> String {
    failures
        .iter()
        .map(|failure| {
            format!(
                "attempt {}: {}; see {}",
                failure.attempt,
                failure.reason,
                failure.log_path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn engine_binary() -> PathBuf {
    if let Ok(p) = std::env::var("DONAT_BIN") {
        return PathBuf::from(p);
    }
    let bin = workspace_root().join("target/debug/donat");
    BUILD_ENGINE.call_once(|| {
        if !bin.exists() {
            let status = Command::new("cargo")
                .args(["build", "-p", "donat-server", "--bin", "donat"])
                .current_dir(workspace_root())
                .status()
                .expect("running cargo build");
            assert!(status.success(), "cargo build -p donat-server failed");
        }
    });
    bin
}

fn engine_is_healthy(client: &reqwest::blocking::Client, base_url: &str) -> bool {
    client
        .get(format!("{base_url}/healthz"))
        .timeout(ENGINE_HEALTH_PROBE_TIMEOUT)
        .send()
        .is_ok_and(|response| response.status().is_success())
}

fn wait_for_engine_health(
    client: &reqwest::blocking::Client,
    proc: &mut EngineProc,
) -> Option<String> {
    let deadline = Instant::now() + ENGINE_HEALTH_DEADLINE;
    loop {
        if engine_is_healthy(client, &proc.base_url) {
            return None;
        }
        match proc.child.try_wait() {
            Ok(Some(status)) => {
                return Some(format!("exited before becoming healthy with {status}"));
            }
            Ok(None) => {}
            Err(error) => {
                return Some(format!("could not check engine process status: {error}"));
            }
        }
        if Instant::now() >= deadline {
            return Some("did not become healthy before the startup deadline".to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn pg_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".into())
}

/// `postgresql://u:p@h:port/db` with the database swapped out.
fn with_db(admin_url: &str, db: &str) -> String {
    let (prefix, _) = admin_url
        .rsplit_once('/')
        .expect("PG_URL must contain a database path");
    format!("{prefix}/{db}")
}

fn create_suite_db(name: &str) -> Result<(String, String)> {
    let admin = pg_admin_url();
    let mut client = postgres::Client::connect(&admin, postgres::NoTls)
        .with_context(|| format!("connecting to {admin} (is the postgres container up?)"))?;
    client.batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))?;
    client.batch_execute(&format!("CREATE DATABASE {name}"))?;
    let database_url = with_db(&admin, name);
    Ok((admin, database_url))
}

fn suite_database_name(suite: &str) -> String {
    let sanitized = suite
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(24)
        .collect::<String>();
    format!(
        "conf_{}_{}_{}",
        sanitized,
        std::process::id(),
        NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
    )
}

struct SuiteDatabase {
    url: String,
    schema: String,
    cleanup: SuiteCleanup,
}

enum SuiteCleanup {
    Postgres { admin_url: String, name: String },
    Sqlite(PathBuf),
    Mysql { admin_url: String, name: String },
    Clickhouse { admin_url: String, name: String },
}

impl SuiteDatabase {
    fn create(backend: BackendId, name: &str) -> Result<Self> {
        match backend {
            BackendId::Postgres => Ok(Self {
                url: {
                    let (_, url) = create_suite_db(name)?;
                    url
                },
                schema: "public".to_string(),
                cleanup: SuiteCleanup::Postgres {
                    admin_url: pg_admin_url(),
                    name: name.to_string(),
                },
            }),
            BackendId::Sqlite => {
                let path = std::env::temp_dir().join(format!("donat_{name}.sqlite"));
                let _ = std::fs::remove_file(&path);
                rusqlite::Connection::open(&path)
                    .with_context(|| format!("creating SQLite database {}", path.display()))?;
                Ok(Self {
                    url: path.to_string_lossy().into_owned(),
                    schema: "main".to_string(),
                    cleanup: SuiteCleanup::Sqlite(path),
                })
            }
            BackendId::Mysql => {
                use mysql::prelude::Queryable;

                let admin_url = std::env::var("MYSQL_URL").context("MYSQL_URL is required")?;
                let mut client = mysql::Conn::new(admin_url.as_str())
                    .with_context(|| format!("connecting to MySQL at {admin_url}"))?;
                client.query_drop(format!("DROP DATABASE IF EXISTS `{name}`"))?;
                client.query_drop(format!("CREATE DATABASE `{name}`"))?;
                let mut url = reqwest::Url::parse(&admin_url).context("parsing MYSQL_URL")?;
                url.set_path(&format!("/{name}"));
                Ok(Self {
                    url: url.to_string(),
                    schema: name.to_string(),
                    cleanup: SuiteCleanup::Mysql {
                        admin_url,
                        name: name.to_string(),
                    },
                })
            }
            BackendId::Clickhouse => {
                let configured =
                    std::env::var("CLICKHOUSE_URL").context("CLICKHOUSE_URL is required")?;
                let mut admin =
                    reqwest::Url::parse(&configured).context("parsing CLICKHOUSE_URL")?;
                let retained = admin
                    .query_pairs()
                    .filter(|(key, _)| key != "database")
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect::<Vec<_>>();
                admin.set_query(None);
                admin.query_pairs_mut().extend_pairs(retained);
                let admin_url = admin.to_string();
                let http = reqwest::blocking::Client::new();
                http.post(&admin_url)
                    .body(format!("DROP DATABASE IF EXISTS `{name}`"))
                    .send()?
                    .error_for_status()?;
                http.post(&admin_url)
                    .body(format!("CREATE DATABASE `{name}`"))
                    .send()?
                    .error_for_status()?;
                let mut database = admin;
                database.query_pairs_mut().append_pair("database", name);
                Ok(Self {
                    url: database.to_string(),
                    schema: name.to_string(),
                    cleanup: SuiteCleanup::Clickhouse {
                        admin_url,
                        name: name.to_string(),
                    },
                })
            }
        }
    }
}

impl Drop for SuiteDatabase {
    fn drop(&mut self) {
        match &self.cleanup {
            SuiteCleanup::Postgres { admin_url, name } => {
                if let Ok(mut client) = postgres::Client::connect(admin_url, postgres::NoTls) {
                    let _ = client
                        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"));
                }
            }
            SuiteCleanup::Sqlite(path) => {
                let _ = std::fs::remove_file(path);
            }
            SuiteCleanup::Mysql { admin_url, name } => {
                use mysql::prelude::Queryable;
                if let Ok(mut client) = mysql::Conn::new(admin_url.as_str()) {
                    let _ = client.query_drop(format!("DROP DATABASE IF EXISTS `{name}`"));
                }
            }
            SuiteCleanup::Clickhouse { admin_url, name } => {
                let _ = reqwest::blocking::Client::new()
                    .post(admin_url)
                    .body(format!("DROP DATABASE IF EXISTS `{name}`"))
                    .send();
            }
        }
    }
}

static NEXT_DATABASE: AtomicU32 = AtomicU32::new(0);

fn free_port() -> u16 {
    static NEXT_PORT: AtomicU32 = AtomicU32::new(0);

    if NEXT_PORT.load(Ordering::Relaxed) == 0 {
        let _ = NEXT_PORT.compare_exchange(
            0,
            49152 + (std::process::id() % 10_000),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    for _ in 0..16_000 {
        let raw = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        let port = 49152 + (raw % 16_000);
        if TcpListener::bind(("0.0.0.0", port as u16)).is_ok() {
            return port as u16;
        }
    }
    panic!("could not find a free port for conformance engine");
}

/// A fresh `Metadata` with version 3 and a single empty "default" source
/// (so `track_table` & co. have somewhere to live). The source points at
/// `DONAT_DATABASE_URL`, which the engine resolves to the suite database.
fn empty_metadata() -> Metadata {
    default_metadata_with_configuration(
        BackendId::Postgres,
        serde_json::from_value(json!({
            "connection_info": { "database_url": { "from_env": "DONAT_DATABASE_URL" } }
        }))
        .expect("static source configuration"),
    )
}

fn default_metadata_for(backend: BackendId, database_url: &str) -> Metadata {
    default_metadata_with_configuration(
        backend,
        serde_json::from_value(json!({
            "connection_info": { "database_url": database_url }
        }))
        .expect("backend source configuration"),
    )
}

fn default_metadata_with_configuration(
    backend: BackendId,
    configuration: donat_metadata::SourceConfiguration,
) -> Metadata {
    Metadata {
        version: 3,
        permissions: Default::default(),
        limits: Default::default(),
        sources: vec![Source {
            name: "default".to_string(),
            kind: backend.source_kind(),
            configuration,
            tables: vec![],
            functions: vec![],
        }],
        inherited_roles: vec![],
        query_collections: vec![],
        allowlist: vec![],
        remote_schemas: vec![],
        actions: vec![],
        custom_types: Default::default(),
        cron_triggers: vec![],
        rest_endpoints: vec![],
        commands: vec![],
        rules: Default::default(),
        connectors: vec![],
        processes: vec![],
        mcp: Default::default(),
        storage: Default::default(),
        templates: vec![],
        media: Default::default(),
        ingest_schemas: vec![],
        recurrence: Default::default(),
        tenancy: None,
        iam: None,
        quotas: None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Http,
    Ws,
    Both,
}

pub struct Suite {
    name: String,
    backend: Option<BackendId>,
    env: Vec<(String, String)>,
    request_headers: Vec<(String, String)>,
    args: Vec<String>,
    /// False when the suite deliberately configures no way to authenticate a
    /// request; see `no_authentication`.
    authenticate: bool,
    webhook: Option<action_webhook::EngineHandle>,
    cron: Option<cron_webhook::CronWebhook>,
    event: Option<cron_webhook::CronWebhook>,
    run_migrations: bool,
    initial_metadata: Option<Metadata>,
}

impl Suite {
    pub fn new(name: &str) -> Self {
        Suite {
            name: name.to_string(),
            backend: None,
            env: vec![],
            request_headers: vec![],
            args: vec![],
            authenticate: true,
            webhook: None,
            cron: None,
            event: None,
            run_migrations: false,
            initial_metadata: None,
        }
    }

    pub fn backend(mut self, backend: BackendId) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn initial_metadata(mut self, metadata: Metadata) -> Self {
        self.initial_metadata = Some(metadata);
        self
    }

    /// Apply the `migrations/` DDL (the `donat` catalog) to the suite
    /// database before the engine spawns, mirroring the real deploy order
    /// (`migrate` then serve). Required for cron triggers.
    pub fn with_migrations(mut self) -> Self {
        self.run_migrations = true;
        self
    }

    /// Start the recording cron webhook stub and expose its base URL to the
    /// engine as `CRON_WEBHOOK_BASE` (cron metadata references it via
    /// `webhook: "{{CRON_WEBHOOK_BASE}}/ok"`). Implies `with_migrations` and
    /// sets a 1-second poll interval so tests observe delivery quickly.
    pub fn with_cron_webhook(mut self) -> Self {
        let stub = cron_webhook::spawn();
        self.env
            .push(("CRON_WEBHOOK_BASE".to_string(), stub.base_url().to_string()));
        self.env
            .push(("DONAT_CRON_POLL_SECONDS".to_string(), "1".to_string()));
        self.cron = Some(stub);
        self.run_migrations = true;
        self
    }

    /// Start the recording event webhook stub and expose its base URL to the
    /// engine as `EVENT_WEBHOOK_HANDLER` (table event triggers reference it via
    /// `webhook: "{{EVENT_WEBHOOK_HANDLER}}"`). Implies `with_migrations`
    /// (which also reconciles the per-table trigger DDL) and sets a 1-second
    /// poll interval so tests observe delivery quickly.
    pub fn with_event_webhook(mut self) -> Self {
        let stub = cron_webhook::spawn();
        self.env.push((
            "EVENT_WEBHOOK_HANDLER".to_string(),
            stub.base_url().to_string(),
        ));
        self.env
            .push(("DONAT_EVENTS_POLL_SECONDS".to_string(), "1".to_string()));
        self.event = Some(stub);
        self.run_migrations = true;
        self
    }

    /// Start the action-webhook stub and expose its base URL to the engine as
    /// `ACTION_WEBHOOK_HANDLER`, so action handler templates resolve to it.
    pub fn with_action_webhook(mut self) -> Self {
        let (base, handle) = action_webhook::spawn();
        self.env.push(("ACTION_WEBHOOK_HANDLER".to_string(), base));
        self.webhook = Some(handle);
        self
    }

    /// Start the upstream GraphQL stub and expose its base URL under the given
    /// env var (e.g. `GRAPHQL_SERVICE_1`), which remote-schema metadata
    /// references via `url: "{{GRAPHQL_SERVICE_1}}"`.
    pub fn with_remote_graphql(mut self, env_var: &str) -> Self {
        let base = remote_graphql::spawn();
        self.env.push((env_var.to_string(), base));
        self
    }

    /// Configure NO authentication mechanism: no JWT, and not even the
    /// harness's own hook. Every request is then whatever
    /// `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` names, whatever headers it carries —
    /// which is what a public deployment looks like, and what the
    /// unauthorized-role suites exist to prove.
    pub fn no_authentication(mut self) -> Self {
        self.authenticate = false;
        self
    }

    /// Add an HTTP/WebSocket request header to every fixture request in this
    /// suite unless a fixture provides the same header explicitly. This keeps
    /// suites that exercise one classic role explicit without rewriting each
    /// copied fixture.
    pub fn request_header(mut self, name: &str, value: &str) -> Self {
        self.request_headers
            .push((name.to_string(), value.to_string()));
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.to_string(), v.to_string()));
        self
    }

    pub fn arg(mut self, a: &str) -> Self {
        self.args.push(a.to_string());
        self
    }

    /// Create the suite database + postgis, but DO NOT spawn the engine yet.
    /// The engine starts lazily on the first request, once all setup ops
    /// have been accumulated into the in-memory metadata.
    pub fn start(mut self) -> Running {
        let backend = self
            .backend
            .map(Ok)
            .unwrap_or_else(BackendId::selected)
            .expect("selecting conformance backend");
        let database = SuiteDatabase::create(backend, &suite_database_name(&self.name))
            .expect("creating suite database");
        let db_url = database.url.clone();
        let schema = database.schema.clone();

        // Fresh database: postgis is used pervasively by fixtures. Concurrent
        // CREATE EXTENSION across databases races inside Postgres (shared
        // library/template locks) — serialize within this process and retry
        // to cover other test processes.
        static POSTGIS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        if backend == BackendId::Postgres {
            let _guard = POSTGIS_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut last_err = None;
            let mut ok = false;
            for _ in 0..10 {
                match postgres::Client::connect(&db_url, postgres::NoTls)
                    .and_then(|mut c| c.batch_execute("create extension if not exists postgis"))
                {
                    Ok(()) => {
                        ok = true;
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e);
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
            assert!(
                ok,
                "postgis init failed [{}] after retries: {:?}",
                self.name, last_err
            );
        }

        // A role reaches the engine through a verified JWT or an
        // authentication hook, and through nothing else — no header and no
        // shared secret can name one. Suites that configure a JWT bring their
        // own mechanism; every other suite gets the harness's hook, which
        // turns the role headers its fixtures carry into a session (see
        // `auth_hook`). Without this, every fixture in the crate would run as
        // whatever `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` happens to be.
        if self.authenticate
            && !self
                .env
                .iter()
                .any(|(key, _)| key == "DONAT_GRAPHQL_JWT_SECRET")
        {
            let hook = auth_hook::spawn();
            self.env.push((
                "DONAT_GRAPHQL_AUTH_HOOK".to_string(),
                hook.url().to_string(),
            ));
            self.env.push((
                "DONAT_GRAPHQL_AUTH_HOOK_MODE".to_string(),
                "POST".to_string(),
            ));
        }

        let metadata = self
            .initial_metadata
            .unwrap_or_else(|| default_metadata_for(backend, &db_url));

        Running {
            name: self.name,
            backend,
            env: self.env,
            request_headers: self.request_headers,
            args: self.args,
            webhook: self.webhook,
            cron: self.cron,
            event: self.event,
            // Every Postgres serving deployment requires the migration-owned
            // command/Process helpers. Non-Postgres suites keep their native
            // setup unless a test explicitly requests another migration path.
            run_migrations: self.run_migrations || backend == BackendId::Postgres,
            reconcile_metadata: self.run_migrations,
            db_url,
            schema,
            _database: database,
            metadata: RefCell::new(metadata),
            engine: RefCell::new(None),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
        }
    }
}

/// The spawned engine process and its endpoints.
struct EngineProc {
    child: Child,
    base_url: String,
    ws_base: String,
    // Keep the metadata dir alive for the engine's lifetime.
    _metadata_dir: PathBuf,
}

impl Drop for EngineProc {
    fn drop(&mut self) {
        let running = match self.child.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) | Err(_) => true,
        };
        if running {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

pub struct Running {
    pub name: String,
    pub backend: BackendId,
    env: Vec<(String, String)>,
    request_headers: Vec<(String, String)>,
    args: Vec<String>,
    webhook: Option<action_webhook::EngineHandle>,
    cron: Option<cron_webhook::CronWebhook>,
    event: Option<cron_webhook::CronWebhook>,
    run_migrations: bool,
    reconcile_metadata: bool,
    db_url: String,
    pub schema: String,
    _database: SuiteDatabase,
    /// Accumulated metadata, applied lazily when the engine is spawned.
    metadata: RefCell<Metadata>,
    /// The spawned engine, started on first request (`ensure_engine`).
    engine: RefCell<Option<EngineProc>>,
    http: reqwest::blocking::Client,
}

fn is_role_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("x-donat-role") || name.eq_ignore_ascii_case("x-hasura-role")
}

fn merge_request_headers(
    defaults: &[(String, String)],
    mut headers: Vec<(String, String)>,
) -> Vec<(String, String)> {
    for (name, value) in defaults {
        let overridden = headers.iter().any(|(existing, _)| {
            existing.eq_ignore_ascii_case(name)
                || (is_role_header(existing) && is_role_header(name))
        });
        if !overridden {
            headers.push((name.clone(), value.clone()));
        }
    }
    headers
}

#[derive(Debug, Clone, Copy)]
pub enum FixtureColumnType {
    BigInt,
    Boolean,
    Text,
    Json,
}

fn fixture_native_type(backend: BackendId, ty: FixtureColumnType) -> &'static str {
    match (backend, ty) {
        (BackendId::Clickhouse, FixtureColumnType::BigInt) => "UInt64",
        (BackendId::Clickhouse, FixtureColumnType::Boolean) => "Bool",
        (BackendId::Clickhouse, FixtureColumnType::Text) => "String",
        (BackendId::Clickhouse, FixtureColumnType::Json) => "JSON",
        (BackendId::Sqlite, FixtureColumnType::BigInt) => "BIGINT",
        (BackendId::Sqlite, FixtureColumnType::Boolean) => "BOOLEAN",
        (BackendId::Sqlite, FixtureColumnType::Text) => "TEXT",
        (BackendId::Sqlite, FixtureColumnType::Json) => "JSON",
        (BackendId::Mysql, FixtureColumnType::BigInt) => "BIGINT",
        (BackendId::Mysql, FixtureColumnType::Boolean) => "BOOLEAN",
        (BackendId::Mysql, FixtureColumnType::Text) => "TEXT",
        (BackendId::Mysql, FixtureColumnType::Json) => "JSON",
        (BackendId::Postgres, FixtureColumnType::BigInt) => "BIGINT",
        (BackendId::Postgres, FixtureColumnType::Boolean) => "BOOLEAN",
        (BackendId::Postgres, FixtureColumnType::Text) => "TEXT",
        (BackendId::Postgres, FixtureColumnType::Json) => "JSONB",
    }
}

pub struct FixtureColumn {
    pub name: &'static str,
    pub ty: FixtureColumnType,
    pub nullable: bool,
    pub primary_key: bool,
}

pub struct TableFixture {
    pub name: &'static str,
    pub columns: &'static [FixtureColumn],
    pub rows: Vec<Vec<Json>>,
    pub role: &'static str,
    pub allow_aggregations: bool,
    pub mutations: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.engine.borrow_mut().take();
    }
}

// --------------------------------------------------------------- the applier

/// A `postgres::Client` on the suite database (for run_sql / seed inserts).
fn pg_client(db_url: &str) -> postgres::Client {
    postgres::Client::connect(db_url, postgres::NoTls).expect("connecting to the suite database")
}

/// Render a JSON scalar as a SQL literal for seed `insert` ops.
fn sql_literal(v: &Json) -> String {
    match v {
        Json::Null => "NULL".to_string(),
        Json::Bool(b) => b.to_string(),
        Json::Number(n) => n.to_string(),
        Json::String(s) => format!("'{}'", s.replace('\'', "''")),
        // Objects/arrays (jsonb) — render as a quoted JSON string literal.
        other => format!("'{}'", other.to_string().replace('\'', "''")),
    }
}

/// Parse a `table`/`function` reference into a `QualifiedTable`: a bare name
/// string (schema defaults to public), or an object `{name, schema?}` /
/// `{schema, name}`. A bare-name object with no schema defaults to public.
fn qualified_from(v: &Json) -> QualifiedTable {
    match v {
        Json::String(s) => QualifiedTable::Name(s.clone()),
        Json::Object(map) => {
            let name = map
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or_else(|| panic!("qualified table/function object without name: {v}"))
                .to_string();
            match map.get("schema").and_then(Json::as_str) {
                Some(schema) => QualifiedTable::Qualified {
                    schema: schema.to_string(),
                    name,
                },
                None => QualifiedTable::Name(name),
            }
        }
        other => panic!("unexpected table/function arg: {other}"),
    }
}

fn from_value<T: serde::de::DeserializeOwned>(what: &str, v: &Json) -> T {
    serde_json::from_value(v.clone())
        .unwrap_or_else(|e| panic!("deserializing {what} from {v}: {e}"))
}

/// Two table/function references denote the same object when their resolved
/// (schema, name) match — `author` and `{schema: public, name: author}` are
/// the same table.
fn same_object(a: &QualifiedTable, b: &QualifiedTable) -> bool {
    a.schema() == b.schema() && a.name() == b.name()
}

impl Running {
    pub fn add_select_permission(
        &self,
        table_name: &str,
        role: &str,
        columns: Json,
        filter: Json,
        allow_aggregations: bool,
    ) {
        self.add_select_permission_document(
            table_name,
            role,
            json!({
                "columns": columns,
                "filter": filter,
                "allow_aggregations": allow_aggregations
            }),
        );
    }

    pub fn add_select_permission_document(&self, table_name: &str, role: &str, document: Json) {
        let table = QualifiedTable::Qualified {
            schema: self.schema.clone(),
            name: table_name.to_string(),
        };
        let permission: SelectPermission =
            serde_json::from_value(document).expect("fixture select permission");
        self.with_table(&table, |entry| {
            entry.select_permissions.push(PermissionEntry {
                role: role.to_string(),
                permission,
                comment: None,
            });
        });
    }

    pub fn add_insert_permission_document(&self, table_name: &str, role: &str, document: Json) {
        let table = QualifiedTable::Qualified {
            schema: self.schema.clone(),
            name: table_name.to_string(),
        };
        let permission: InsertPermission =
            serde_json::from_value(document).expect("fixture insert permission");
        self.with_table(&table, |entry| {
            entry.insert_permissions.push(PermissionEntry {
                role: role.to_string(),
                permission,
                comment: None,
            });
        });
    }

    pub fn add_relationship(
        &self,
        local_table: &str,
        name: &str,
        remote_table: &str,
        column_mapping: &[(&str, &str)],
        array: bool,
    ) {
        let local = QualifiedTable::Qualified {
            schema: self.schema.clone(),
            name: local_table.to_string(),
        };
        let remote = json!({ "schema": self.schema, "name": remote_table });
        let mapping = column_mapping
            .iter()
            .map(|(local, remote)| ((*local).to_string(), json!(remote)))
            .collect::<serde_json::Map<_, _>>();
        let relationship = json!({
            "name": name,
            "using": {
                "manual_configuration": {
                    "remote_table": remote,
                    "column_mapping": mapping
                }
            }
        });
        self.with_table(&local, |entry| {
            if array {
                entry.array_relationships.push(
                    serde_json::from_value(relationship).expect("fixture array relationship"),
                );
            } else {
                entry.object_relationships.push(
                    serde_json::from_value(relationship).expect("fixture object relationship"),
                );
            }
        });
    }

    pub fn install_table(&self, fixture: &TableFixture) {
        assert!(
            self.engine.borrow().is_none(),
            "fixtures must be installed before the engine starts"
        );
        let quote = |name: &str| match self.backend {
            BackendId::Mysql | BackendId::Clickhouse => {
                format!("`{}`", name.replace('`', "``"))
            }
            BackendId::Postgres | BackendId::Sqlite => {
                format!("\"{}\"", name.replace('"', "\"\""))
            }
        };
        let columns = fixture
            .columns
            .iter()
            .map(|column| {
                let base_type = fixture_native_type(self.backend, column.ty);
                let native_type = if self.backend == BackendId::Clickhouse && column.nullable {
                    format!("Nullable({base_type})")
                } else {
                    base_type.to_string()
                };
                let nullable = if column.nullable || self.backend == BackendId::Clickhouse {
                    ""
                } else {
                    " NOT NULL"
                };
                let primary = if column.primary_key && self.backend != BackendId::Clickhouse {
                    " PRIMARY KEY"
                } else {
                    ""
                };
                format!("{} {}{nullable}{primary}", quote(column.name), native_type)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let table = format!("{}.{}", quote(&self.schema), quote(fixture.name));
        let engine = if self.backend == BackendId::Clickhouse {
            let order = fixture
                .columns
                .iter()
                .find(|column| column.primary_key)
                .map(|column| quote(column.name))
                .unwrap_or_else(|| "tuple()".to_string());
            format!(" ENGINE = MergeTree ORDER BY {order}")
        } else {
            String::new()
        };
        self.execute_fixture_sql(&format!("CREATE TABLE {table} ({columns}){engine}"));

        if !fixture.rows.is_empty() {
            let column_names = fixture
                .columns
                .iter()
                .map(|column| quote(column.name))
                .collect::<Vec<_>>()
                .join(", ");
            let rows = fixture
                .rows
                .iter()
                .map(|row| {
                    assert_eq!(row.len(), fixture.columns.len());
                    format!(
                        "({})",
                        row.iter()
                            .zip(fixture.columns)
                            .map(|(value, column)| self.fixture_literal(value, column.ty))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            self.execute_fixture_sql(&format!(
                "INSERT INTO {table} ({column_names}) VALUES {rows}"
            ));
        }

        let table = QualifiedTable::Qualified {
            schema: self.schema.clone(),
            name: fixture.name.to_string(),
        };
        let permission: SelectPermission = serde_json::from_value(json!({
            "columns": "*",
            "filter": {},
            "allow_aggregations": fixture.allow_aggregations
        }))
        .expect("fixture select permission");
        self.with_table(&table, |entry| {
            entry.configuration = Some(
                serde_json::from_value(json!({ "custom_name": fixture.name }))
                    .expect("fixture table configuration"),
            );
            entry.select_permissions.push(PermissionEntry {
                role: fixture.role.to_string(),
                permission,
                comment: None,
            });
            if fixture.mutations {
                entry.insert_permissions.push(PermissionEntry {
                    role: fixture.role.to_string(),
                    permission: serde_json::from_value(json!({
                        "columns": "*",
                        "check": {}
                    }))
                    .expect("fixture insert permission"),
                    comment: None,
                });
                entry.update_permissions.push(PermissionEntry {
                    role: fixture.role.to_string(),
                    permission: serde_json::from_value(json!({
                        "columns": "*",
                        "filter": {},
                        "check": {}
                    }))
                    .expect("fixture update permission"),
                    comment: None,
                });
                entry.delete_permissions.push(PermissionEntry {
                    role: fixture.role.to_string(),
                    permission: serde_json::from_value(json!({ "filter": {} }))
                        .expect("fixture delete permission"),
                    comment: None,
                });
            }
        });
    }

    fn execute_fixture_sql(&self, sql: &str) {
        match self.backend {
            BackendId::Postgres => {
                pg_client(&self.db_url)
                    .batch_execute(sql)
                    .unwrap_or_else(|error| {
                        panic!("[{}] fixture SQL failed: {error}\n{sql}", self.name)
                    })
            }
            BackendId::Sqlite => rusqlite::Connection::open(&self.db_url)
                .and_then(|connection| connection.execute_batch(sql))
                .unwrap_or_else(|error| {
                    panic!("[{}] fixture SQL failed: {error}\n{sql}", self.name)
                }),
            BackendId::Mysql => {
                use mysql::prelude::Queryable;
                let mut connection =
                    mysql::Conn::new(self.db_url.as_str()).unwrap_or_else(|error| {
                        panic!("[{}] MySQL connect failed: {error}", self.name)
                    });
                connection.query_drop(sql).unwrap_or_else(|error| {
                    panic!("[{}] fixture SQL failed: {error}\n{sql}", self.name)
                });
            }
            BackendId::Clickhouse => {
                self.http
                    .post(&self.db_url)
                    .body(sql.to_string())
                    .send()
                    .and_then(reqwest::blocking::Response::error_for_status)
                    .unwrap_or_else(|error| {
                        panic!("[{}] fixture SQL failed: {error}\n{sql}", self.name)
                    });
            }
        }
    }

    fn fixture_literal(&self, value: &Json, ty: FixtureColumnType) -> String {
        use donat_backend::Dialect;

        if value.is_null() {
            return "NULL".to_string();
        }
        let dialect = match self.backend {
            BackendId::Postgres => {
                donat_backend::AnyDialect::Postgres(donat_backend::PostgresDialect)
            }
            BackendId::Sqlite => donat_backend::AnyDialect::Sqlite(donat_backend::SqliteDialect),
            BackendId::Mysql => donat_backend::AnyDialect::Mysql(donat_backend::MySqlDialect),
            BackendId::Clickhouse => {
                donat_backend::AnyDialect::Clickhouse(donat_backend::ClickhouseDialect)
            }
        };
        match value {
            Json::Bool(value) => value.to_string(),
            Json::Number(value) => value.to_string(),
            Json::String(value) => dialect.quote_literal(value),
            Json::Array(_) | Json::Object(_) => {
                let literal = dialect.quote_literal(&value.to_string());
                match (self.backend, ty) {
                    (BackendId::Postgres, FixtureColumnType::Json) => {
                        format!("{literal}::jsonb")
                    }
                    (BackendId::Mysql | BackendId::Clickhouse, FixtureColumnType::Json) => {
                        format!("CAST({literal} AS JSON)")
                    }
                    _ => literal,
                }
            }
            Json::Null => unreachable!(),
        }
    }

    /// Find (or create) the table entry for `args.table` in the default
    /// source and run `f` against it. Tables are matched by resolved
    /// (schema, name), so the bare-name and qualified forms unify.
    fn with_table<R>(&self, table: &QualifiedTable, f: impl FnOnce(&mut TableEntry) -> R) -> R {
        let mut md = self.metadata.borrow_mut();
        let source = md
            .sources
            .iter_mut()
            .find(|s| s.name == "default")
            .expect("default source");
        if !source.tables.iter().any(|t| same_object(&t.table, table)) {
            source.tables.push(TableEntry {
                table: table.clone(),
                configuration: None,
                is_enum: false,
                object_relationships: vec![],
                array_relationships: vec![],
                computed_fields: vec![],
                remote_relationships: vec![],
                insert_permissions: vec![],
                select_permissions: vec![],
                update_permissions: vec![],
                delete_permissions: vec![],
                command_insert_permissions: vec![],
                command_select_permissions: vec![],
                command_update_permissions: vec![],
                command_delete_permissions: vec![],
                event_triggers: vec![],
                attachments: vec![],
            });
        }
        let entry = source
            .tables
            .iter_mut()
            .find(|t| same_object(&t.table, table))
            .expect("table just inserted");
        f(entry)
    }

    /// Apply a single setup op into the accumulated metadata (or run it
    /// against the suite database, for run_sql/insert). Panics on an unknown
    /// op type so new fixture ops are noticed.
    fn apply_op(&self, op: &Json) {
        let raw = op
            .get("type")
            .and_then(Json::as_str)
            .unwrap_or_else(|| panic!("setup op has no type: {op}"));
        // mssql_* ops are out of scope — we never run the mssql backend.
        if raw.starts_with("mssql_") {
            return;
        }
        let kind = raw.strip_prefix("pg_").unwrap_or(raw);
        let args = op.get("args").cloned().unwrap_or(Json::Null);

        match kind {
            "bulk" => {
                let ops = args
                    .as_array()
                    .unwrap_or_else(|| panic!("bulk args must be a list: {op}"));
                for inner in ops {
                    self.apply_op(inner);
                }
            }

            "run_sql" => {
                let sql = args["sql"]
                    .as_str()
                    .unwrap_or_else(|| panic!("run_sql without sql: {op}"));
                pg_client(&self.db_url)
                    .batch_execute(sql)
                    .unwrap_or_else(|e| {
                        let detail = e
                            .as_db_error()
                            .map(|d| format!("{}: {}", d.code().code(), d.message()))
                            .unwrap_or_else(|| e.to_string());
                        panic!("[{}] run_sql failed: {detail}\nSQL:\n{sql}", self.name)
                    });
            }

            "insert" => {
                let table = qualified_from(&args["table"]);
                let objects = args["objects"]
                    .as_array()
                    .unwrap_or_else(|| panic!("insert without objects: {op}"));
                let mut client = pg_client(&self.db_url);
                for obj in objects {
                    let cols: Vec<&String> = obj
                        .as_object()
                        .unwrap_or_else(|| panic!("insert object must be a map: {obj}"))
                        .keys()
                        .collect();
                    let col_list = cols
                        .iter()
                        .map(|c| format!("\"{c}\""))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let vals = cols
                        .iter()
                        .map(|c| sql_literal(&obj[c.as_str()]))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let sql = format!(
                        "INSERT INTO \"{}\".\"{}\" ({col_list}) VALUES ({vals})",
                        table.schema(),
                        table.name()
                    );
                    client.batch_execute(&sql).unwrap_or_else(|e| {
                        panic!("[{}] seed insert failed: {e}\nSQL:\n{sql}", self.name)
                    });
                }
            }

            "track_table" => {
                // The arg is either `{table: <name|{schema,name}>}` or the
                // bare `{schema, name}` form. An optional `configuration`
                // (custom_name, custom_root_fields, column_config, ...) is
                // applied to the table entry.
                let table = if args.get("table").is_some() {
                    qualified_from(&args["table"])
                } else {
                    qualified_from(&args)
                };
                let configuration: Option<TableConfiguration> = args
                    .get("configuration")
                    .filter(|c| !c.is_null())
                    .map(|c| from_value("table configuration", c));
                self.with_table(&table, |t| {
                    if configuration.is_some() {
                        t.configuration = configuration;
                    }
                });
            }

            "create_select_permission" => {
                let table = qualified_from(&args["table"]);
                let role = args["role"].as_str().expect("role").to_string();
                let permission: SelectPermission =
                    from_value("select permission", &args["permission"]);
                self.with_table(&table, |t| {
                    t.select_permissions.push(PermissionEntry {
                        role,
                        permission,
                        comment: None,
                    });
                });
            }
            "create_insert_permission" => {
                let table = qualified_from(&args["table"]);
                let role = args["role"].as_str().expect("role").to_string();
                let permission: InsertPermission =
                    from_value("insert permission", &args["permission"]);
                self.with_table(&table, |t| {
                    t.insert_permissions.push(PermissionEntry {
                        role,
                        permission,
                        comment: None,
                    });
                });
            }
            "create_update_permission" => {
                let table = qualified_from(&args["table"]);
                let role = args["role"].as_str().expect("role").to_string();
                let permission: UpdatePermission =
                    from_value("update permission", &args["permission"]);
                self.with_table(&table, |t| {
                    t.update_permissions.push(PermissionEntry {
                        role,
                        permission,
                        comment: None,
                    });
                });
            }
            "create_delete_permission" => {
                let table = qualified_from(&args["table"]);
                let role = args["role"].as_str().expect("role").to_string();
                let permission: DeletePermission =
                    from_value("delete permission", &args["permission"]);
                self.with_table(&table, |t| {
                    t.delete_permissions.push(PermissionEntry {
                        role,
                        permission,
                        comment: None,
                    });
                });
            }

            "create_object_relationship" => {
                let table = qualified_from(&args["table"]);
                let rel: ObjectRelationship = from_value("object relationship", &args);
                self.with_table(&table, |t| t.object_relationships.push(rel));
            }
            "create_array_relationship" => {
                let table = qualified_from(&args["table"]);
                let rel: ArrayRelationship = from_value("array relationship", &args);
                self.with_table(&table, |t| t.array_relationships.push(rel));
            }

            "add_computed_field" => {
                let table = qualified_from(&args["table"]);
                let cf: ComputedField = from_value("computed field", &args);
                self.with_table(&table, |t| t.computed_fields.push(cf));
            }

            "create_remote_relationship" => {
                let table = qualified_from(&args["table"]);
                let rel: RemoteRelationship = from_value("remote relationship", &args);
                self.with_table(&table, |t| t.remote_relationships.push(rel));
            }

            "track_function" => {
                // Either `{function: <name|{schema,name}>}` or bare
                // `{name, schema}` (like track_table).
                let function = if args.get("function").is_some() {
                    qualified_from(&args["function"])
                } else {
                    qualified_from(&args)
                };
                let mut md = self.metadata.borrow_mut();
                let source = md.sources.iter_mut().find(|s| s.name == "default").unwrap();
                if !source
                    .functions
                    .iter()
                    .any(|f| same_object(&f.function, &function))
                {
                    source.functions.push(FunctionEntry {
                        function,
                        configuration: args
                            .get("configuration")
                            .filter(|c| !c.is_null())
                            .map(|c| from_value("function configuration", c)),
                        permissions: vec![],
                    });
                }
            }
            "create_function_permission" | "add_function_permission" => {
                let function = qualified_from(&args["function"]);
                let role = args["role"].as_str().expect("role").to_string();
                let mut md = self.metadata.borrow_mut();
                let source = md.sources.iter_mut().find(|s| s.name == "default").unwrap();
                let entry = source
                    .functions
                    .iter_mut()
                    .find(|f| same_object(&f.function, &function))
                    .unwrap_or_else(|| panic!("function {function} not tracked before permission"));
                entry.permissions.push(FunctionPermission { role });
            }

            "add_inherited_role" => {
                let role: InheritedRole = from_value("inherited role", &args);
                self.metadata.borrow_mut().inherited_roles.push(role);
            }
            "drop_inherited_role" => {
                let name = args["role_name"].as_str().expect("role_name").to_string();
                self.metadata
                    .borrow_mut()
                    .inherited_roles
                    .retain(|r| r.role_name != name);
            }

            "add_remote_schema" => {
                let schema: RemoteSchema = from_value("remote schema", &args);
                self.metadata.borrow_mut().remote_schemas.push(schema);
            }
            "remove_remote_schema" | "drop_remote_schema" => {
                let name = args["name"].as_str().expect("name").to_string();
                self.metadata
                    .borrow_mut()
                    .remote_schemas
                    .retain(|r| r.name != name);
            }
            "update_remote_schema" => {
                let schema: RemoteSchema = from_value("remote schema", &args);
                let mut md = self.metadata.borrow_mut();
                if let Some(existing) = md.remote_schemas.iter_mut().find(|r| r.name == schema.name)
                {
                    // Keep accumulated permissions across an update.
                    let perms = std::mem::take(&mut existing.permissions);
                    *existing = schema;
                    existing.permissions = perms;
                } else {
                    md.remote_schemas.push(schema);
                }
            }
            "add_remote_schema_permissions" => {
                let name = args["remote_schema"]
                    .as_str()
                    .expect("remote_schema")
                    .to_string();
                let perm = RemoteSchemaPermission {
                    role: args["role"].as_str().expect("role").to_string(),
                    definition: from_value("remote schema permission", &args["definition"]),
                };
                let mut md = self.metadata.borrow_mut();
                let schema = md
                    .remote_schemas
                    .iter_mut()
                    .find(|r| r.name == name)
                    .unwrap_or_else(|| panic!("remote schema {name} not added before permission"));
                schema.permissions.push(perm);
            }
            "drop_remote_schema_permissions" => {
                let name = args["remote_schema"]
                    .as_str()
                    .expect("remote_schema")
                    .to_string();
                let role = args["role"].as_str().expect("role").to_string();
                let mut md = self.metadata.borrow_mut();
                if let Some(schema) = md.remote_schemas.iter_mut().find(|r| r.name == name) {
                    schema.permissions.retain(|p| p.role != role);
                }
            }

            "create_query_collection" => {
                let collection: QueryCollection = from_value("query collection", &args);
                self.metadata
                    .borrow_mut()
                    .query_collections
                    .push(collection);
            }
            "drop_query_collection" => {
                let name = args["collection"]
                    .as_str()
                    .or_else(|| args["name"].as_str())
                    .expect("collection name")
                    .to_string();
                self.metadata
                    .borrow_mut()
                    .query_collections
                    .retain(|c| c.name != name);
            }
            "add_query_to_collection" => {
                let coll = args["collection_name"]
                    .as_str()
                    .expect("collection_name")
                    .to_string();
                let query = donat_metadata::CollectionQuery {
                    name: args["query_name"].as_str().expect("query_name").to_string(),
                    query: args["query"].as_str().expect("query").to_string(),
                };
                let mut md = self.metadata.borrow_mut();
                let collection = md
                    .query_collections
                    .iter_mut()
                    .find(|c| c.name == coll)
                    .unwrap_or_else(|| panic!("collection {coll} not created before add_query"));
                collection.definition.queries.push(query);
            }
            "drop_query_from_collection" => {
                let coll = args["collection_name"]
                    .as_str()
                    .expect("collection_name")
                    .to_string();
                let qname = args["query_name"].as_str().expect("query_name").to_string();
                let mut md = self.metadata.borrow_mut();
                if let Some(collection) = md.query_collections.iter_mut().find(|c| c.name == coll) {
                    collection.definition.queries.retain(|q| q.name != qname);
                }
            }
            "create_rest_endpoint" => {
                let endpoint: RestEndpoint = from_value("rest endpoint", &args);
                self.metadata.borrow_mut().rest_endpoints.push(endpoint);
            }
            "drop_rest_endpoint" => {
                let name = args["name"]
                    .as_str()
                    .expect("rest endpoint name")
                    .to_string();
                self.metadata
                    .borrow_mut()
                    .rest_endpoints
                    .retain(|e| e.name != name);
            }
            "add_collection_to_allowlist" => {
                let entry: AllowlistEntry = from_value("allowlist entry", &args);
                self.metadata.borrow_mut().allowlist.push(entry);
            }
            "drop_collection_from_allowlist" => {
                let coll = args["collection"].as_str().expect("collection").to_string();
                self.metadata
                    .borrow_mut()
                    .allowlist
                    .retain(|a| a.collection != coll);
            }

            "untrack_table" => {
                let table = if args.get("table").is_some() {
                    qualified_from(&args["table"])
                } else {
                    qualified_from(&args)
                };
                let mut md = self.metadata.borrow_mut();
                let source = md.sources.iter_mut().find(|s| s.name == "default").unwrap();
                source.tables.retain(|t| !same_object(&t.table, &table));
            }
            "untrack_function" => {
                let function = if args.get("function").is_some() {
                    qualified_from(&args["function"])
                } else {
                    qualified_from(&args)
                };
                let mut md = self.metadata.borrow_mut();
                let source = md.sources.iter_mut().find(|s| s.name == "default").unwrap();
                source
                    .functions
                    .retain(|f| !same_object(&f.function, &function));
            }
            "drop_relationship" => {
                let table = qualified_from(&args["table"]);
                let name = args["relationship"]
                    .as_str()
                    .expect("relationship")
                    .to_string();
                self.with_table(&table, |t| {
                    t.object_relationships.retain(|r| r.name != name);
                    t.array_relationships.retain(|r| r.name != name);
                });
            }
            "drop_computed_field" => {
                let table = qualified_from(&args["table"]);
                let name = args["name"].as_str().expect("name").to_string();
                self.with_table(&table, |t| t.computed_fields.retain(|c| c.name != name));
            }
            "drop_remote_relationship" => {
                let table = qualified_from(&args["table"]);
                let name = args["name"].as_str().expect("name").to_string();
                self.with_table(&table, |t| {
                    t.remote_relationships.retain(|r| r.name != name)
                });
            }
            "drop_select_permission" => {
                let table = qualified_from(&args["table"]);
                let role = args["role"].as_str().expect("role").to_string();
                self.with_table(&table, |t| t.select_permissions.retain(|p| p.role != role));
            }
            "drop_insert_permission" => {
                let table = qualified_from(&args["table"]);
                let role = args["role"].as_str().expect("role").to_string();
                self.with_table(&table, |t| t.insert_permissions.retain(|p| p.role != role));
            }
            "drop_update_permission" => {
                let table = qualified_from(&args["table"]);
                let role = args["role"].as_str().expect("role").to_string();
                self.with_table(&table, |t| t.update_permissions.retain(|p| p.role != role));
            }
            "drop_delete_permission" => {
                let table = qualified_from(&args["table"]);
                let role = args["role"].as_str().expect("role").to_string();
                self.with_table(&table, |t| t.delete_permissions.retain(|p| p.role != role));
            }
            "drop_function_permission" => {
                let function = if args.get("function").is_some() {
                    qualified_from(&args["function"])
                } else {
                    qualified_from(&args)
                };
                let role = args["role"].as_str().expect("role").to_string();
                let mut md = self.metadata.borrow_mut();
                let source = md.sources.iter_mut().find(|s| s.name == "default").unwrap();
                if let Some(f) = source
                    .functions
                    .iter_mut()
                    .find(|f| same_object(&f.function, &function))
                {
                    f.permissions.retain(|p| p.role != role);
                }
            }

            "set_custom_types" => {
                let custom_types: donat_metadata::CustomTypes =
                    serde_json::from_value(args.clone()).unwrap_or_else(|e| {
                        panic!("[{}] bad set_custom_types: {e}\n{op}", self.name)
                    });
                self.metadata.borrow_mut().custom_types = custom_types;
            }

            "create_action" => {
                let entry: donat_metadata::ActionEntry = serde_json::from_value(args.clone())
                    .unwrap_or_else(|e| panic!("[{}] bad create_action: {e}\n{op}", self.name));
                let mut md = self.metadata.borrow_mut();
                md.actions.retain(|a| a.name != entry.name);
                md.actions.push(entry);
            }

            "update_action" => {
                let entry: donat_metadata::ActionEntry = serde_json::from_value(args.clone())
                    .unwrap_or_else(|e| panic!("[{}] bad update_action: {e}\n{op}", self.name));
                let mut md = self.metadata.borrow_mut();
                // Preserve existing permissions across a definition update.
                let permissions = md
                    .actions
                    .iter()
                    .find(|a| a.name == entry.name)
                    .map(|a| a.permissions.clone())
                    .unwrap_or_default();
                md.actions.retain(|a| a.name != entry.name);
                md.actions.push(donat_metadata::ActionEntry {
                    permissions,
                    ..entry
                });
            }

            "drop_action" => {
                let name = args["name"].as_str().expect("action name").to_string();
                self.metadata
                    .borrow_mut()
                    .actions
                    .retain(|a| a.name != name);
            }

            "create_action_permission" => {
                let action = args["action"].as_str().expect("action").to_string();
                let role = args["role"].as_str().expect("role").to_string();
                let mut md = self.metadata.borrow_mut();
                if let Some(a) = md.actions.iter_mut().find(|a| a.name == action)
                    && !a.permissions.iter().any(|p| p.role == role)
                {
                    a.permissions
                        .push(donat_metadata::ActionPermission { role });
                }
            }

            "drop_action_permission" => {
                let action = args["action"].as_str().expect("action").to_string();
                let role = args["role"].as_str().expect("role").to_string();
                let mut md = self.metadata.borrow_mut();
                if let Some(a) = md.actions.iter_mut().find(|a| a.name == action) {
                    a.permissions.retain(|p| p.role != role);
                }
            }

            "clear_metadata" => {
                *self.metadata.borrow_mut() = empty_metadata();
            }

            other => panic!(
                "[{}] unsupported setup op `{other}` (raw `{raw}`): {op}",
                self.name
            ),
        }
    }

    /// Apply a list-or-single setup document into the accumulated metadata.
    fn apply_doc(&self, doc: &Json) {
        match doc {
            Json::Array(ops) => {
                for op in ops {
                    self.apply_op(op);
                }
            }
            obj => self.apply_op(obj),
        }
    }

    // ----------------------------------------------------- lazy engine spawn

    /// Serialize the accumulated metadata to a temp `version: 3` directory.
    fn write_metadata_dir(&self) -> PathBuf {
        let md = self.metadata.borrow();
        Self::write_metadata_snapshot(&self.name, &md)
    }

    fn write_metadata_snapshot(name: &str, md: &Metadata) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dist_conf_md_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("databases")).unwrap();

        std::fs::write(dir.join("version.yaml"), "version: 3\n").unwrap();
        std::fs::write(
            dir.join("databases").join("databases.yaml"),
            serde_yaml::to_string(&md.sources).expect("serialize sources"),
        )
        .unwrap();
        if !md.inherited_roles.is_empty() {
            std::fs::write(
                dir.join("inherited_roles.yaml"),
                serde_yaml::to_string(&md.inherited_roles).unwrap(),
            )
            .unwrap();
        }
        if !md.query_collections.is_empty() {
            std::fs::write(
                dir.join("query_collections.yaml"),
                serde_yaml::to_string(&md.query_collections).unwrap(),
            )
            .unwrap();
        }
        if !md.allowlist.is_empty() {
            std::fs::write(
                dir.join("allow_list.yaml"),
                serde_yaml::to_string(&md.allowlist).unwrap(),
            )
            .unwrap();
        }
        if !md.remote_schemas.is_empty() {
            std::fs::write(
                dir.join("remote_schemas.yaml"),
                serde_yaml::to_string(&md.remote_schemas).unwrap(),
            )
            .unwrap();
        }
        if !md.cron_triggers.is_empty() {
            std::fs::write(
                dir.join("cron_triggers.yaml"),
                serde_yaml::to_string(&md.cron_triggers).unwrap(),
            )
            .unwrap();
        }
        if !md.rest_endpoints.is_empty() {
            std::fs::write(
                dir.join("rest_endpoints.yaml"),
                serde_yaml::to_string(&md.rest_endpoints).unwrap(),
            )
            .unwrap();
        }
        if !md.commands.is_empty() {
            std::fs::write(
                dir.join("commands.yaml"),
                serde_yaml::to_string(&md.commands).expect("serialize commands"),
            )
            .unwrap();
        }
        if !md.processes.is_empty() {
            std::fs::write(
                dir.join("flows.yaml"),
                serde_yaml::to_string(&md.processes).expect("serialize processes"),
            )
            .unwrap();
        }
        if !md.rules.is_empty() {
            std::fs::write(
                dir.join("rules.yaml"),
                serde_yaml::to_string(&md.rules).expect("serialize rules wrapper"),
            )
            .unwrap();
        }
        if !md.connectors.is_empty() {
            std::fs::write(
                dir.join("connectors.yaml"),
                serde_yaml::to_string(&md.connectors).expect("serialize connectors"),
            )
            .unwrap();
        }
        if !md.storage.is_empty() {
            std::fs::write(
                dir.join("storage.yaml"),
                serde_yaml::to_string(&md.storage).expect("serialize storage"),
            )
            .unwrap();
        }
        if let Some(tenancy) = &md.tenancy {
            std::fs::write(
                dir.join("tenancy.yaml"),
                serde_yaml::to_string(tenancy).expect("serialize tenancy"),
            )
            .unwrap();
        }
        if let Some(iam) = &md.iam {
            std::fs::write(
                dir.join("iam.yaml"),
                serde_yaml::to_string(iam).expect("serialize iam"),
            )
            .unwrap();
        }
        if let Some(quotas) = &md.quotas {
            std::fs::write(
                dir.join("quotas.yaml"),
                serde_yaml::to_string(quotas).expect("serialize quotas"),
            )
            .unwrap();
        }
        if md.mcp.is_configured() {
            std::fs::write(
                dir.join("mcp.yaml"),
                serde_yaml::to_string(&md.mcp).unwrap(),
            )
            .unwrap();
        }
        if !md.actions.is_empty() || !md.custom_types.is_empty() {
            // Both live together in actions.yaml, the donat-cli export layout.
            let doc = json!({
                "actions": md.actions,
                "custom_types": md.custom_types,
            });
            std::fs::write(
                dir.join("actions.yaml"),
                serde_yaml::to_string(&doc).unwrap(),
            )
            .unwrap();
        }
        dir
    }

    /// Spawn the engine (once) with the accumulated metadata.
    fn ensure_engine(&self) {
        if self.engine.borrow().is_some() {
            return;
        }
        let metadata_dir = self.write_metadata_dir();
        // Install the migration-owned runtime contract in every Postgres
        // source before serving. Suites that explicitly request migrations
        // also reconcile their command, Process, and trigger metadata.
        if self.run_migrations {
            let migrations = workspace_root().join("migrations");
            let postgres_sources = self
                .metadata
                .borrow()
                .sources
                .iter()
                .filter(|source| source.kind == SourceKind::Postgres)
                .cloned()
                .collect::<Vec<_>>();
            let resolve_source_url = |source: &Source| {
                let connection = source
                    .configuration
                    .connection_info
                    .as_ref()
                    .unwrap_or_else(|| {
                        panic!(
                            "Postgres source {} has no connection_info in suite {}",
                            source.name, self.name
                        )
                    });
                match &connection.database_url {
                    DatabaseUrl::Url(url) => url.clone(),
                    DatabaseUrl::FromEnv { from_env } => self
                        .env
                        .iter()
                        .rev()
                        .find(|(key, _)| key == from_env)
                        .map(|(_, value)| value.clone())
                        .or_else(|| {
                            matches!(
                                from_env.as_str(),
                                "DONAT_DATABASE_URL" | "DONAT_GRAPHQL_DATABASE_URL"
                            )
                            .then(|| self.db_url.clone())
                        })
                        .or_else(|| std::env::var(from_env).ok())
                        .unwrap_or_else(|| {
                            panic!(
                                "Postgres source {} requires missing environment variable {} \
                                 in suite {}",
                                source.name, from_env, self.name
                            )
                        }),
                }
            };
            let migrate_source = |source: Option<&Source>| {
                let mut migrate = Command::new(engine_binary());
                migrate
                    .arg("migrate")
                    .arg("--migrations-dir")
                    .arg(&migrations);
                if self.reconcile_metadata
                    && let Some(source) = source
                {
                    migrate
                        .arg("--metadata-dir")
                        .arg(&metadata_dir)
                        .arg("--source")
                        .arg(&source.name);
                }
                let database_url = source
                    .map(&resolve_source_url)
                    .unwrap_or_else(|| self.db_url.clone());
                let output = migrate
                    .env("DONAT_DATABASE_URL", database_url)
                    .env("DONAT_GRAPHQL_DATABASE_URL", &self.db_url)
                    .envs(self.env.iter().map(|(key, value)| (key, value)))
                    .output()
                    .expect("running donat migrate");
                assert!(
                    output.status.success(),
                    "donat migrate failed for suite {} source {}:\n{}{}",
                    self.name,
                    source.map_or("<direct>", |source| source.name.as_str()),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            };
            if postgres_sources.is_empty() {
                // Connector-only metadata still serves through the suite's
                // implicit default Postgres source.
                migrate_source(None);
            } else {
                // Source initialization verifies the migration-owned runtime
                // contract independently for every Postgres database.
                for source in &postgres_sources {
                    migrate_source(Some(source));
                }
            }
        }
        let log_dir = workspace_root().join("target/conformance-logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        // `free_port` probes a port and then releases it before the child can
        // bind. Under parallel conformance, another process may claim that
        // port in this small window. Retry the whole startup so a transient
        // bind or database initialization failure does not fail an unrelated
        // suite. Failed children are always reaped by `EngineProc::drop`
        // before the next attempt.
        let start_result =
            retry_engine_start(ENGINE_START_ATTEMPTS, ENGINE_START_RETRY_DELAY, |attempt| {
                let port = free_port();
                let log_path = if attempt == ENGINE_START_ATTEMPTS {
                    log_dir.join(format!("{}.log", self.name))
                } else {
                    log_dir.join(format!("{}.attempt-{attempt}.log", self.name))
                };
                let log = std::fs::File::create(&log_path).unwrap();

                let mut cmd = Command::new(engine_binary());
                cmd.arg("--port")
                    .arg(port.to_string())
                    .arg("--metadata-dir")
                    .arg(&metadata_dir)
                    .env("DONAT_DATABASE_URL", &self.db_url)
                    .env("DONAT_GRAPHQL_DATABASE_URL", &self.db_url)
                    .stdout(Stdio::from(log.try_clone().unwrap()))
                    .stderr(Stdio::from(log));
                for a in &self.args {
                    cmd.arg(a);
                }
                for (k, v) in &self.env {
                    cmd.env(k, v);
                }

                let child = match cmd.spawn() {
                    Ok(child) => child,
                    Err(error) => {
                        return Err(EngineStartFailure {
                            attempt,
                            reason: format!("could not spawn donat: {error}"),
                            log_path,
                        });
                    }
                };

                let mut proc = EngineProc {
                    child,
                    base_url: format!("http://127.0.0.1:{port}"),
                    ws_base: format!("ws://127.0.0.1:{port}"),
                    _metadata_dir: metadata_dir.clone(),
                };

                if let Some(reason) = wait_for_engine_health(&self.http, &mut proc) {
                    drop(proc);
                    Err(EngineStartFailure {
                        attempt,
                        reason,
                        log_path,
                    })
                } else {
                    Ok(proc)
                }
            });

        let proc = match start_result {
            Ok(proc) => proc,
            Err(failures) => panic!(
                "engine for suite {} failed to become healthy after {ENGINE_START_ATTEMPTS} attempts: {}",
                self.name,
                format_engine_start_failures(&failures)
            ),
        };

        // Let webhook callback endpoints reach the now-running engine.
        if let Some(handle) = &self.webhook {
            let role = self
                .request_headers
                .iter()
                .find(|(name, _)| is_role_header(name))
                .map(|(_, value)| value.clone());
            handle.set(&proc.base_url, role);
        }
        *self.engine.borrow_mut() = Some(proc);
    }

    /// The engine's HTTP base URL, spawning it lazily if needed.
    pub fn base_url(&self) -> String {
        self.ensure_engine();
        self.engine.borrow().as_ref().unwrap().base_url.clone()
    }

    /// The engine's WebSocket base URL, spawning it lazily if needed.
    pub fn ws_base(&self) -> String {
        self.ensure_engine();
        self.engine.borrow().as_ref().unwrap().ws_base.clone()
    }

    /// The suite database URL (for cron tests that seed/inspect the
    /// `donat` catalog directly).
    pub fn db_url(&self) -> &str {
        &self.db_url
    }

    /// Replace one child-process environment value before a deliberate
    /// restart. This is a conformance-harness control, not an engine runtime
    /// configuration API.
    pub fn set_engine_env_for_restart(&mut self, key: &str, value: &str) {
        self.env.retain(|(existing, _)| existing != key);
        self.env.push((key.to_owned(), value.to_owned()));
    }

    /// Stop the current child and stage a new immutable metadata deployment
    /// against the same suite database. The next request/base URL lookup runs
    /// the normal migrate-then-serve path again.
    pub fn restart_with_metadata(&mut self, metadata: Metadata) {
        let process = self
            .engine
            .get_mut()
            .take()
            .expect("restart_with_metadata requires a running engine");
        drop(process);
        *self.metadata.get_mut() = metadata;
    }

    /// The recording cron webhook stub (only present after
    /// [`Suite::with_cron_webhook`]).
    pub fn cron_webhook(&self) -> &cron_webhook::CronWebhook {
        self.cron
            .as_ref()
            .expect("with_cron_webhook() was not called on this suite")
    }

    /// Register a cron trigger in the metadata before the engine spawns.
    /// Panics if the engine has already started (metadata is read at boot).
    pub fn add_cron_trigger(&self, trigger: CronTrigger) {
        assert!(
            self.engine.borrow().is_none(),
            "add_cron_trigger must be called before the engine spawns"
        );
        self.metadata.borrow_mut().cron_triggers.push(trigger);
    }

    /// Configure the deployment's storage backends before the engine starts.
    pub fn set_storage(&self, storage: donat_metadata::StorageMetadata) {
        assert!(
            self.engine.borrow().is_none(),
            "set_storage must be called before the engine spawns"
        );
        self.metadata.borrow_mut().storage = storage;
    }

    /// Declare engine-wide tenancy before the engine starts.
    ///
    /// Suites build their tables in their own schema, so the declaration is
    /// written with `{schema}` placeholders and filled in here rather than
    /// hard-coding `public`.
    pub fn set_tenancy(&self, tenancy: Json) {
        assert!(
            self.engine.borrow().is_none(),
            "set_tenancy must be called before the engine spawns"
        );
        let filled: Json = serde_json::from_str(
            &serde_json::to_string(&tenancy)
                .expect("tenancy declaration serializes")
                .replace("{schema}", &self.schema),
        )
        .expect("tenancy declaration parses");
        let tenancy: donat_metadata::TenancyMetadata =
            serde_json::from_value(filled).expect("fixture tenancy");
        self.metadata.borrow_mut().tenancy = Some(tenancy);
    }

    /// Declare in-tenant grants before the engine starts. `{schema}` is filled
    /// in the same way `set_tenancy` fills it.
    pub fn set_iam(&self, iam: Json) {
        assert!(
            self.engine.borrow().is_none(),
            "set_iam must be called before the engine spawns"
        );
        let filled: Json = serde_json::from_str(
            &serde_json::to_string(&iam)
                .expect("iam declaration serializes")
                .replace("{schema}", &self.schema),
        )
        .expect("iam declaration parses");
        let iam: donat_metadata::IamMetadata = serde_json::from_value(filled).expect("fixture iam");
        self.metadata.borrow_mut().iam = Some(iam);
    }

    /// Declare plan entitlements before the engine starts.
    pub fn set_quotas(&self, quotas: Json) {
        assert!(
            self.engine.borrow().is_none(),
            "set_quotas must be called before the engine spawns"
        );
        let filled: Json = serde_json::from_str(
            &serde_json::to_string(&quotas)
                .expect("quota declaration serializes")
                .replace("{schema}", &self.schema),
        )
        .expect("quota declaration parses");
        let quotas: donat_metadata::QuotaMetadata =
            serde_json::from_value(filled).expect("fixture quotas");
        self.metadata.borrow_mut().quotas = Some(quotas);
    }

    /// Declare a file column on a tracked table before the engine starts.
    pub fn add_attachment(&self, table_name: &str, attachment: Json) {
        assert!(
            self.engine.borrow().is_none(),
            "add_attachment must be called before the engine spawns"
        );
        let table = QualifiedTable::Qualified {
            schema: self.schema.clone(),
            name: table_name.to_string(),
        };
        let attachment: donat_metadata::Attachment =
            serde_json::from_value(attachment).expect("fixture attachment");
        self.with_table(&table, |entry| entry.attachments.push(attachment));
    }

    /// Configure the explicit MCP publication metadata before the engine
    /// starts. This always writes an `mcp.yaml`, including for an empty
    /// deny-all publication list.
    pub fn set_mcp_metadata(&self, mut mcp: McpMetadata) {
        assert!(
            self.engine.borrow().is_none(),
            "set_mcp_metadata must be called before the engine spawns"
        );
        mcp.mark_configured();
        self.metadata.borrow_mut().mcp = mcp;
    }

    /// The recording event webhook stub (only present after
    /// [`Suite::with_event_webhook`]).
    pub fn event_webhook(&self) -> &cron_webhook::CronWebhook {
        self.event
            .as_ref()
            .expect("with_event_webhook() was not called on this suite")
    }

    /// Attach a table event trigger to a tracked table before the engine
    /// spawns (so `migrate --metadata-dir` reconciles its Postgres triggers).
    pub fn add_event_trigger(&self, table: &QualifiedTable, trigger: EventTrigger) {
        assert!(
            self.engine.borrow().is_none(),
            "add_event_trigger must be called before the engine spawns"
        );
        self.with_table(table, |t| t.event_triggers.push(trigger));
    }

    /// Issue an HTTP request against the (lazily spawned) engine. The
    /// well-known admin-API paths are intercepted: requests to `/v1/query`,
    /// `/v2/query` and `/v1/metadata` are applied in-harness as metadata/SQL
    /// ops (returning a `success` body) rather than hitting the engine, so
    /// the harness never depends on the runtime admin API. All other paths
    /// (graphql, relay, ...) reach the engine.
    pub fn post(&self, path: &str, body: &Json, headers: &[(String, String)]) -> (u16, Json) {
        self.post_inner(path, body, headers, true)
    }

    pub fn post_without_mcp_protocol(
        &self,
        path: &str,
        body: &Json,
        headers: &[(String, String)],
    ) -> (u16, Json) {
        self.post_inner(path, body, headers, false)
    }

    /// Issue a raw-byte HTTP request against the spawned engine and preserve
    /// the exact response bytes. This is intentionally a harness helper for
    /// provider ingress tests; it does not add a new engine API surface.
    pub fn post_bytes(
        &self,
        path: &str,
        body: &[u8],
        headers: &[(String, String)],
    ) -> (u16, Vec<u8>) {
        self.ensure_engine();
        let headers = merge_request_headers(&self.request_headers, headers.to_vec());
        let base = self.engine.borrow().as_ref().unwrap().base_url.clone();
        let mut request = self.http.post(format!("{base}{path}")).body(body.to_vec());
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        let response = request.send().expect("raw HTTP request failed");
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .expect("read raw HTTP response body")
            .to_vec();
        (status, body)
    }

    /// Issue an arbitrary-method request at a URL the engine handed out.
    ///
    /// File attachments give the caller a URL rather than an endpoint to
    /// construct, so a test has to follow it exactly as a client would —
    /// including a URL that points at the object store instead of the engine.
    /// A path (rather than an absolute URL) is resolved against the engine.
    pub fn request_url(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
        headers: &[(&str, &str)],
    ) -> (u16, Vec<u8>) {
        self.ensure_engine();
        let url = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            let base = self.engine.borrow().as_ref().unwrap().base_url.clone();
            format!("{base}{url}")
        };
        let method = reqwest::Method::from_bytes(method.as_bytes()).expect("valid HTTP method");
        let mut request = self.http.request(method, url).body(body.to_vec());
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().expect("raw HTTP request failed");
        let status = response.status().as_u16();
        let body = response.bytes().expect("read response body").to_vec();
        (status, body)
    }

    /// Like [`Self::request_url`], but also returning the response headers
    /// (lower-cased names) — a download's headers are part of its contract.
    pub fn request_url_with_headers(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
        headers: &[(&str, &str)],
    ) -> (u16, Vec<(String, String)>) {
        self.ensure_engine();
        let url = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            let base = self.engine.borrow().as_ref().unwrap().base_url.clone();
            format!("{base}{url}")
        };
        let method = reqwest::Method::from_bytes(method.as_bytes()).expect("valid HTTP method");
        let mut request = self.http.request(method, url).body(body.to_vec());
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().expect("raw HTTP request failed");
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        (status, headers)
    }

    fn post_inner(
        &self,
        path: &str,
        body: &Json,
        headers: &[(String, String)],
        add_mcp_protocol: bool,
    ) -> (u16, Json) {
        if path == "/v1/query" || path == "/v2/query" || path == "/v1/metadata" {
            // Admin-API paths are applied in-harness rather than POSTed.
            // Before the engine starts they accumulate into the boot
            // metadata; a few fixtures embed a metadata mutation as a test
            // STEP (after the engine is up) — for those the equivalent state
            // is pre-loaded at boot, so we still apply it to the in-harness
            // metadata (a no-op against the running engine) and return the
            // success body the fixture asserts.
            self.apply_doc(body);
            return (200, json!({"message": "success"}));
        }
        self.ensure_engine();
        let headers = merge_request_headers(&self.request_headers, headers.to_vec());
        let base = self.engine.borrow().as_ref().unwrap().base_url.clone();
        let mut req = self.http.post(format!("{base}{path}")).json(body);
        let has_accept = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept"));
        if path == "/mcp" && !has_accept {
            req = req.header("Accept", "application/json, text/event-stream");
        }
        let has_protocol = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("MCP-Protocol-Version"));
        if path == "/mcp"
            && add_mcp_protocol
            && !has_protocol
            && body.get("method").and_then(Json::as_str) != Some("initialize")
        {
            req = req.header("MCP-Protocol-Version", "2025-06-18");
        }
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        let resp = req.send().expect("http request failed");
        let code = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        let body = serde_json::from_str(&text).unwrap_or(Json::String(text));
        (code, body)
    }

    /// A fixture's request headers, plus whatever the suite adds to every
    /// request. Nothing authenticates a request here: the role headers the
    /// fixtures carry become a session only because the suite's
    /// authentication hook says so (see `auth_hook`).
    fn auth_headers(&self, headers: Vec<(String, String)>) -> Vec<(String, String)> {
        merge_request_headers(&self.request_headers, headers)
    }

    /// Apply a setup fixture: parse the document and accumulate its ops into
    /// the in-harness metadata (or run its SQL). `endpoint` is accepted for
    /// API compatibility but ignored — nothing is POSTed to the engine.
    pub fn apply(&self, rel: &str, _endpoint: &str) {
        let path = fixture_root().join(rel);
        let body = load_fixture(&path).expect("loading setup fixture");
        self.apply_doc(&body);
    }

    /// tests-py applies v2-style setup files only when they exist.
    pub fn apply_if_exists(&self, rel: &str, endpoint: &str) -> bool {
        if fixture_root().join(rel).exists() {
            self.apply(rel, endpoint);
            true
        } else {
            false
        }
    }

    pub fn setup_v1q(&self, rel: &str) {
        self.apply(rel, "/v1/query");
    }

    /// Apply a teardown fixture. Suite-level metadata teardown is a no-op —
    /// every suite has its own database and a fresh metadata directory — but
    /// per-method DATA teardown (run_sql / insert that reset rows between
    /// mutation cases) DOES run against the live suite database. Metadata
    /// teardown ops (untrack, drop permission) are harmless no-ops once the
    /// engine has booted from the accumulated metadata, so applying the whole
    /// document is correct and faithful: the data resets happen, the metadata
    /// drops are inert.
    pub fn teardown_v1q(&self, rel: &str) {
        let path = fixture_root().join(rel);
        if let Ok(body) = load_fixture(&path) {
            self.apply_doc(&body);
        }
    }

    /// Replicates tests-py `check_query_f` for one fixture file.
    pub fn check_query_f(&self, rel: &str, transport: Transport) {
        self.ensure_engine();
        let path = fixture_root().join(rel);
        let conf = load_fixture(&path).expect("loading test fixture");
        match conf {
            Json::Array(steps) => {
                for (i, step) in steps.iter().enumerate() {
                    self.run_conf(step, transport, &format!("{rel}[{i}]"));
                }
            }
            other => self.run_conf(&other, transport, rel),
        }
    }

    fn run_conf(&self, conf: &Json, transport: Transport, label: &str) {
        let url = conf["url"].as_str().expect("conf.url");
        let is_gql = url.ends_with("/graphql") || url.ends_with("/relay");
        match transport {
            Transport::Http => self.http_case(conf, label),
            Transport::Ws => {
                assert!(is_gql, "ws transport on non-graphql url in {label}");
                self.ws_case(conf, label);
            }
            Transport::Both => {
                self.http_case(conf, label);
                if is_gql {
                    self.ws_case(conf, label);
                }
            }
        }
    }

    fn conf_headers(conf: &Json) -> Vec<(String, String)> {
        // One upstream fixture spells the key `header:` (singular) — see
        // `queries/graphql_mutation/insert/permissions/
        // article_on_conflict_constraint_on_user_role_error.yaml`. Reading
        // only `headers:` silently dropped its role, so the case ran with no
        // session at all rather than as `user`. Accepting both spellings
        // keeps the fixture unedited and makes it test what it says.
        conf.get("headers")
            .or_else(|| conf.get("header"))
            .and_then(|h| h.as_object())
            .map(|h| {
                h.iter()
                    .map(|(k, v)| {
                        let val = match v {
                            Json::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (k.clone(), val)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn http_case(&self, conf: &Json, label: &str) {
        let url = conf["url"].as_str().unwrap();
        let headers = self.auth_headers(Self::conf_headers(conf));
        let exp_status = conf.get("status").and_then(Json::as_u64).unwrap_or(200) as u16;
        let method = conf.get("method").and_then(Json::as_str).unwrap_or("POST");

        let (code, resp) = match method {
            "GET" => {
                let mut req = self.http.get(format!("{}{url}", self.base_url()));
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                let r = req.send().expect("http GET failed");
                let code = r.status().as_u16();
                let text = r.text().unwrap_or_default();
                (
                    code,
                    serde_json::from_str(&text).unwrap_or(Json::String(text)),
                )
            }
            "POST" => {
                let body = conf.get("query").or_else(|| conf.get("body")).cloned();
                self.post(url, &body.unwrap_or(Json::Null), &headers)
            }
            // Other verbs (PUT/PATCH/DELETE) are used by REST endpoint
            // fixtures; issue the real method against the engine. The
            // admin-API interception only applies to POST paths, so these
            // always reach the engine.
            other => {
                let m = reqwest::Method::from_bytes(other.as_bytes())
                    .unwrap_or_else(|_| panic!("[{label}] bad method {other}"));
                let mut req = self.http.request(m, format!("{}{url}", self.base_url()));
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                if let Some(body) = conf.get("body") {
                    req = req.json(body);
                }
                let r = req.send().expect("http request failed");
                let code = r.status().as_u16();
                let text = r.text().unwrap_or_default();
                (
                    code,
                    serde_json::from_str(&text).unwrap_or(Json::String(text)),
                )
            }
        };

        assert_eq!(
            code,
            exp_status,
            "[{}] {label}: status mismatch (got {code}, want {exp_status})\nresponse:\n{}",
            self.name,
            pretty(&resp)
        );

        // MCP (`/mcp`) responses are JSON-RPC: the `result.content` field is a
        // human/text duplicate of `result.structuredContent` and is NOT part
        // of the contract. Strip it from both expected and actual before
        // comparing, so fixtures assert only the structured payload (plus
        // protocolVersion / serverInfo / tools / isError / ...). GraphQL and
        // REST comparison is unchanged.
        let resp = if url == "/mcp" {
            strip_mcp_content(&resp)
        } else {
            resp
        };

        let query_text = conf_query_text(conf);
        if let Some(allowed) = conf.get("allowed_responses").and_then(Json::as_array) {
            let ok = allowed.iter().any(|a| {
                a.get("response")
                    .map(|exp| {
                        if url == "/mcp" {
                            strip_mcp_content(exp)
                        } else {
                            exp.clone()
                        }
                    })
                    .is_some_and(|exp| response_matches(&exp, &resp, query_text))
            });
            assert!(
                ok,
                "[{}] {label}: response matched none of allowed_responses\nactual:\n{}",
                self.name,
                pretty(&resp)
            );
        } else if let Some(exp) = conf.get("response") {
            let exp = if url == "/mcp" {
                strip_mcp_content(exp)
            } else {
                exp.clone()
            };
            self.assert_response(&exp, &resp, query_text, label);
        }
    }

    fn assert_response(&self, exp: &Json, act: &Json, query_text: Option<&str>, label: &str) {
        assert!(
            response_matches(exp, act, query_text),
            "[{}] {label}: response mismatch\nexpected:\n{}\nactual:\n{}",
            self.name,
            pretty(exp),
            pretty(act)
        );
    }

    /// Legacy Apollo graphql-ws: init({headers}) -> ack, start -> data|error
    /// (payload compared against the full expected HTTP response), then
    /// complete.
    fn ws_case(&self, conf: &Json, label: &str) {
        use tungstenite::Message;
        use tungstenite::client::IntoClientRequest;

        let url = conf["url"].as_str().unwrap();
        let exp = conf
            .get("response")
            .unwrap_or_else(|| panic!("[{label}] ws case without response"));
        let headers = self.auth_headers(Self::conf_headers(conf));
        let query = conf["query"].clone();

        let mut req = format!("{}{url}", self.ws_base())
            .into_client_request()
            .expect("ws request");
        req.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            "graphql-ws".parse().expect("protocol header"),
        );
        let (mut sock, _) = tungstenite::connect(req).expect("ws connect");

        let mut init_payload = Map::new();
        if !headers.is_empty() {
            init_payload.insert(
                "headers".into(),
                Json::Object(headers.into_iter().map(|(k, v)| (k, json!(v))).collect()),
            );
        }
        sock.send(Message::text(
            json!({"type":"connection_init","payload": init_payload}).to_string(),
        ))
        .unwrap();

        let frame = next_frame(&mut sock, &["connection_ack", "connection_error"], label);
        assert_eq!(
            frame["type"],
            "connection_ack",
            "[{label}] ws init failed: {}",
            pretty(&frame)
        );

        sock.send(Message::text(
            json!({"id":"hge_test","type":"start","payload": query}).to_string(),
        ))
        .unwrap();

        let frame = next_frame(&mut sock, &["data", "error"], label);
        let payload = &frame["payload"];
        let payload = if frame["type"] == "error" {
            // Legacy protocol error frames carry the bare error object.
            &json!({ "errors": [payload.clone()] })
        } else {
            payload
        };
        self.assert_response(
            exp,
            payload,
            conf_query_text(conf),
            &format!("{label} (ws)"),
        );

        let has_errors = exp.get("errors").is_some() || exp.get("error").is_some();
        if !has_errors {
            let done = next_frame(&mut sock, &["complete"], label);
            assert_eq!(done["type"], "complete", "[{label}] expected complete");
        }
        let _ = sock.close(None);
    }
}

fn next_frame<S>(sock: &mut tungstenite::WebSocket<S>, wanted: &[&str], label: &str) -> Json
where
    S: Read + std::io::Write,
{
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "[{label}] timed out waiting for ws frame {wanted:?}"
        );
        let msg = sock.read().expect("ws read");
        if !msg.is_text() {
            continue;
        }
        let v: Json = serde_json::from_str(msg.to_text().unwrap()).expect("ws frame json");
        let t = v["type"].as_str().unwrap_or_default().to_string();
        if t == "ka" {
            continue;
        }
        if wanted.contains(&t.as_str()) || t == "error" || t == "connection_error" {
            return v;
        }
    }
}

fn conf_query_text(conf: &Json) -> Option<&str> {
    conf.get("query")?.get("query")?.as_str()
}

fn pretty(v: &Json) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

// -------------------------------------------------------------------- tests

/// Unit tests for the pure parts of the harness (backend registry, engine
/// start/retry, suite naming, metadata writer). They need neither Postgres
/// nor a running engine, so they live in the lib target (the `tests/`
/// binaries require a database). Fixture loading and response comparison
/// are tested where they live now, in `donat-testkit`.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn backend_registry_has_stable_ci_ids() {
        assert_eq!(
            BackendId::ALL.map(BackendId::as_str),
            ["postgres", "sqlite", "mysql", "clickhouse"]
        );
    }

    #[test]
    fn engine_health_probe_timeout_is_bounded_per_attempt() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("binding health test server");
        let address = listener.local_addr().expect("health test address");
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("accepting health probe");
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("health probe did not time out promptly");
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        let started = Instant::now();
        assert!(!engine_is_healthy(&client, &format!("http://{address}")));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "one health probe consumed the whole startup deadline"
        );

        release_tx.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn suite_request_headers_are_case_insensitive_and_never_override_fixture_roles() {
        let defaults = vec![
            ("X-Donat-Role".to_string(), "tester".to_string()),
            ("X-Trace-Mode".to_string(), "suite".to_string()),
        ];
        let fixture = vec![
            ("x-hasura-role".to_string(), "fixture-role".to_string()),
            ("x-trace-mode".to_string(), "fixture".to_string()),
        ];

        assert_eq!(
            merge_request_headers(&defaults, fixture),
            vec![
                ("x-hasura-role".to_string(), "fixture-role".to_string()),
                ("x-trace-mode".to_string(), "fixture".to_string()),
            ]
        );
    }

    #[test]
    fn engine_start_retry_stops_after_success() {
        let mut calls = 0;
        let result = retry_engine_start(3, Duration::ZERO, |attempt| {
            calls += 1;
            if attempt < 3 {
                Err(EngineStartFailure {
                    attempt,
                    reason: format!("transient failure {attempt}"),
                    log_path: PathBuf::from(format!("attempt-{attempt}.log")),
                })
            } else {
                Ok(attempt)
            }
        });

        assert_eq!(calls, 3);
        assert_eq!(result.expect("third startup attempt succeeds"), 3);
    }

    #[test]
    fn engine_start_failure_diagnostics_include_every_attempt_log() {
        let failures = retry_engine_start::<()>(3, Duration::ZERO, |attempt| {
            Err(EngineStartFailure {
                attempt,
                reason: format!("startup failure {attempt}"),
                log_path: PathBuf::from(format!("attempt-{attempt}.log")),
            })
        })
        .expect_err("all startup attempts fail");

        assert_eq!(failures.len(), 3);
        let diagnostics = format_engine_start_failures(&failures);
        for attempt in 1..=3 {
            assert!(diagnostics.contains(&format!("attempt {attempt}")));
            assert!(diagnostics.contains(&format!("attempt-{attempt}.log")));
        }
    }

    #[test]
    fn free_ports_are_unique_when_allocated_in_parallel() {
        let handles: Vec<_> = (0..8).map(|_| std::thread::spawn(free_port)).collect();
        let mut ports: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("port allocator thread succeeds"))
            .collect();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(ports.len(), 8);
    }

    #[cfg(unix)]
    #[test]
    fn engine_proc_drop_kills_and_reaps_child() {
        let child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn cleanup test child");
        let pid = child.id().to_string();
        let proc = EngineProc {
            child,
            base_url: "http://127.0.0.1:1".to_string(),
            ws_base: "ws://127.0.0.1:1".to_string(),
            _metadata_dir: PathBuf::new(),
        };

        drop(proc);

        let status = Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .expect("probe cleanup test child");
        assert!(!status.success(), "child {pid} is still running");
    }

    #[test]
    fn neutral_boolean_fixture_type_is_rendered_for_every_backend() {
        assert_eq!(
            fixture_native_type(BackendId::Postgres, FixtureColumnType::Boolean),
            "BOOLEAN"
        );
        assert_eq!(
            fixture_native_type(BackendId::Sqlite, FixtureColumnType::Boolean),
            "BOOLEAN"
        );
        assert_eq!(
            fixture_native_type(BackendId::Mysql, FixtureColumnType::Boolean),
            "BOOLEAN"
        );
        assert_eq!(
            fixture_native_type(BackendId::Clickhouse, FixtureColumnType::Boolean),
            "Bool"
        );
    }

    #[test]
    fn neutral_bigint_fixture_keeps_bigint_affinity_on_sqlite() {
        assert_eq!(
            fixture_native_type(BackendId::Sqlite, FixtureColumnType::BigInt),
            "BIGINT"
        );
    }

    #[test]
    fn backend_selection_defaults_to_postgres() {
        assert_eq!(BackendId::parse(None).unwrap(), BackendId::Postgres);
        assert_eq!(BackendId::parse(Some("")).unwrap(), BackendId::Postgres);
    }

    #[test]
    fn backend_selection_parses_every_registered_backend() {
        for backend in BackendId::ALL {
            assert_eq!(BackendId::parse(Some(backend.as_str())).unwrap(), backend);
        }
    }

    #[test]
    fn backend_selection_rejects_unknown_values() {
        let err = BackendId::parse(Some("oracle")).unwrap_err();
        assert!(err.to_string().contains("oracle"), "{err}");
        for backend in BackendId::ALL {
            assert!(err.to_string().contains(backend.as_str()), "{err}");
        }
    }

    #[test]
    fn backend_registry_covers_every_source_kind() {
        for backend in BackendId::ALL {
            assert_eq!(BackendId::from(backend.source_kind()), backend);
        }
    }

    #[test]
    fn backend_registry_uses_engine_capabilities() {
        assert_eq!(
            BackendId::Postgres.capabilities(),
            donat_backend::capabilities::postgres()
        );
        assert_eq!(
            BackendId::Sqlite.capabilities(),
            donat_backend::capabilities::sqlite()
        );
        assert_eq!(
            BackendId::Mysql.capabilities(),
            donat_backend::capabilities::mysql()
        );
        assert_eq!(
            BackendId::Clickhouse.capabilities(),
            donat_backend::capabilities::clickhouse()
        );
    }

    #[test]
    fn case_capabilities_follow_backend_registry() {
        for backend in BackendId::ALL {
            assert!(CaseCapability::Reads.supported_by(backend));
            assert_eq!(
                CaseCapability::Mutations.supported_by(backend),
                backend.capabilities().mutations
            );
            assert_eq!(
                CaseCapability::Relationships.supported_by(backend),
                backend.capabilities().relationships
            );
            assert_eq!(
                CaseCapability::Json.supported_by(backend),
                backend.capabilities().json_ops != donat_backend::capabilities::JsonOps::None
            );
        }
    }

    #[test]
    fn case_runner_counts_passed_unsupported_and_known_differences() {
        const KNOWN_DIFFERENCES: &[KnownDifference] = &[KnownDifference {
            backend: BackendId::Clickhouse,
            reason: "tracked output difference",
            tracking: "knowledgebase/multi-backend/decisions/006-mandatory-conformance-backend-matrix.md",
        }];
        const CASES: &[ConformanceCase] = &[
            ConformanceCase::new("read", &[CaseCapability::Reads]),
            ConformanceCase::new("write", &[CaseCapability::Mutations]),
            ConformanceCase::with_known_differences(
                "known-difference",
                &[CaseCapability::Reads],
                KNOWN_DIFFERENCES,
            ),
        ];

        let mut called = Vec::new();
        let summary = run_conformance_cases("runner-unit", BackendId::Clickhouse, CASES, |name| {
            called.push(name)
        });

        assert_eq!(called, ["read"]);
        assert_eq!(summary.total, 3);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.unsupported, 1);
        assert_eq!(summary.known_differences, 1);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn case_runner_rejects_an_invalid_manifest() {
        const DUPLICATE_CASES: &[ConformanceCase] = &[
            ConformanceCase::new("duplicate", &[CaseCapability::Reads]),
            ConformanceCase::new("duplicate", &[CaseCapability::Reads]),
        ];
        assert!(
            std::panic::catch_unwind(|| run_conformance_cases(
                "duplicates",
                BackendId::Postgres,
                DUPLICATE_CASES,
                |_| {}
            ))
            .is_err()
        );

        const UNTRACKED: &[KnownDifference] = &[KnownDifference {
            backend: BackendId::Postgres,
            reason: "",
            tracking: "",
        }];
        const UNTRACKED_CASE: &[ConformanceCase] = &[ConformanceCase::with_known_differences(
            "untracked",
            &[CaseCapability::Reads],
            UNTRACKED,
        )];
        assert!(
            std::panic::catch_unwind(|| run_conformance_cases(
                "untracked",
                BackendId::Postgres,
                UNTRACKED_CASE,
                |_| {}
            ))
            .is_err()
        );
    }

    #[test]
    fn case_runner_records_all_failures_before_failing_the_test() {
        const CASES: &[ConformanceCase] = &[
            ConformanceCase::new("first", &[CaseCapability::Reads]),
            ConformanceCase::new("second", &[CaseCapability::Reads]),
        ];
        let mut called = Vec::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_conformance_cases("failures", BackendId::Postgres, CASES, |name| {
                called.push(name);
                panic!("failure in {name}");
            });
        }));

        assert!(result.is_err());
        assert_eq!(called, ["first", "second"]);
    }

    #[test]
    fn in_process_and_default_backends_need_no_explicit_url() {
        BackendId::Postgres
            .validate_configuration(|_| None)
            .unwrap();
        BackendId::Sqlite.validate_configuration(|_| None).unwrap();
    }

    #[test]
    fn service_backends_require_explicit_urls() {
        let mysql = BackendId::Mysql
            .validate_configuration(|_| None)
            .unwrap_err();
        assert!(mysql.to_string().contains("MYSQL_URL"), "{mysql}");
        let clickhouse = BackendId::Clickhouse
            .validate_configuration(|_| None)
            .unwrap_err();
        assert!(
            clickhouse.to_string().contains("CLICKHOUSE_URL"),
            "{clickhouse}"
        );

        BackendId::Mysql
            .validate_configuration(|key| (key == "MYSQL_URL").then(|| "mysql://db".into()))
            .unwrap();
        BackendId::Clickhouse
            .validate_configuration(|key| {
                (key == "CLICKHOUSE_URL").then(|| "http://clickhouse".into())
            })
            .unwrap();
    }

    #[test]
    fn default_metadata_tracks_selected_backend_and_url() {
        for backend in BackendId::ALL {
            let url = format!("{}://suite", backend.as_str());
            let metadata = default_metadata_for(backend, &url);
            let source = metadata.sources.first().unwrap();
            assert_eq!(source.name, "default");
            assert_eq!(source.kind, backend.source_kind());
            let encoded = serde_json::to_value(&source.configuration).unwrap();
            assert_eq!(
                encoded.pointer("/connection_info/database_url"),
                Some(&json!(url))
            );
        }
    }

    #[test]
    fn sqlite_suite_owns_file_and_default_source() {
        let path = {
            let suite = Suite::new("sqlite_lifecycle")
                .backend(BackendId::Sqlite)
                .start();
            assert_eq!(suite.backend, BackendId::Sqlite);
            let path = PathBuf::from(&suite.db_url);
            assert!(path.exists(), "SQLite database was not created");
            let metadata = suite.metadata.borrow();
            assert_eq!(metadata.sources[0].kind, SourceKind::Sqlite);
            path
        };
        assert!(!path.exists(), "SQLite database was not cleaned up");
    }

    #[test]
    fn suite_database_names_are_unique_and_safe() {
        let first = suite_database_name("Reads / weird name");
        let second = suite_database_name("Reads / weird name");
        assert_ne!(first, second);
        for name in [first, second] {
            assert!(name.len() <= 63, "database name too long: {name}");
            assert!(
                name.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
                "unsafe database name: {name}"
            );
        }
    }

    #[test]
    fn ci_matrix_covers_backend_registry() {
        let workflow = std::fs::read_to_string(workspace_root().join(".github/workflows/ci.yml"))
            .expect("reading CI workflow");
        let workflow: serde_yaml::Value =
            serde_yaml::from_str(&workflow).expect("parsing CI workflow");
        let include = workflow["jobs"]["backend-core"]["strategy"]["matrix"]["include"]
            .as_sequence()
            .expect("conformance matrix.include");
        let actual = include
            .iter()
            .map(|entry| entry["backend"].as_str().expect("matrix backend string"))
            .collect::<Vec<_>>();
        assert_eq!(actual, BackendId::ALL.map(BackendId::as_str));

        assert_eq!(
            workflow["jobs"]["backend-core"]["strategy"]["fail-fast"].as_bool(),
            Some(false)
        );
        let steps = workflow["jobs"]["backend-core"]["steps"]
            .as_sequence()
            .expect("backend matrix steps");
        let backend_cache_key = steps
            .iter()
            .find(|step| step["uses"].as_str() == Some("Swatinem/rust-cache@v2"))
            .and_then(|step| step["with"]["key"].as_str())
            .expect("backend matrix cache key");
        assert_eq!(
            backend_cache_key, "backend-${{ matrix.backend }}",
            "backend matrix caches must be isolated by backend"
        );
        let shared_command = steps
            .iter()
            .find(|step| step["name"].as_str() == Some("Shared backend contract"))
            .and_then(|step| step["run"].as_str())
            .expect("shared backend command");
        assert!(
            shared_command.contains("--test-threads=4 --nocapture"),
            "shared backend suites must use the reviewed parallelism: {shared_command}"
        );
        let postgres_command = steps
            .iter()
            .find(|step| step["name"].as_str() == Some("Full Postgres reference conformance"))
            .and_then(|step| step["run"].as_str())
            .expect("full Postgres conformance command");
        assert!(
            postgres_command.contains("--test-threads=4"),
            "full Postgres conformance must use the reviewed parallelism: {postgres_command}"
        );
        assert!(
            postgres_command.contains("env -u MYSQL_URL -u CLICKHOUSE_URL"),
            "full Postgres conformance must not enter unrelated backend binaries: {postgres_command}"
        );
        assert_eq!(
            workflow["jobs"]["backend-core-gate"]["name"].as_str(),
            Some("Conformance matrix")
        );

        let mixed_steps = workflow["jobs"]["mixed-backend-conformance"]["steps"]
            .as_sequence()
            .expect("mixed backend steps");
        let start_mixed = mixed_steps
            .iter()
            .find(|step| step["name"].as_str() == Some("Start Postgres and ClickHouse"))
            .and_then(|step| step["run"].as_str())
            .expect("mixed backend service command");
        assert!(start_mixed.contains("--wait postgres clickhouse"));
        let mixed_contract = mixed_steps
            .iter()
            .find(|step| step["name"].as_str() == Some("Multi-database and multi-source contracts"))
            .and_then(|step| step["run"].as_str())
            .expect("mixed backend contract command");
        for binary in [
            "clickhouse_multi_database",
            "multi_source",
            "tandt_clickhouse_contract",
        ] {
            assert!(
                mixed_contract.contains(&format!("--test {binary}")),
                "mixed backend job misses {binary}: {mixed_contract}"
            );
        }
        assert!(mixed_contract.contains("--test-threads=1 --nocapture"));

        let artifact_needs = workflow["jobs"]["artifacts"]["needs"]
            .as_sequence()
            .expect("artifact job needs");
        assert!(
            artifact_needs
                .iter()
                .any(|need| { need.as_str() == Some("mixed-backend-conformance") })
        );
    }

    #[test]
    fn every_conformance_binary_is_classified() {
        const SHARED: &[&str] = &["backend_matrix"];
        const BACKEND_SPECIFIC: &[&str] = &[
            "clickhouse_multi_database",
            "multi_source",
            "tandt_clickhouse_contract",
        ];
        const POSTGRES_REFERENCE: &[&str] = &[
            "actions",
            "agg_relay_introspection",
            "auth_env",
            "commands",
            "connectors",
            "cron_triggers",
            "enabled_apis",
            "event_triggers",
            "file_attachments",
            "graphql_mutations",
            "graphql_queries",
            "introspection_descriptions",
            "jwk",
            "jwt",
            "jwt_claims_map",
            "mcp_tools",
            "migrate",
            "oidc_login",
            "petshop_yaml",
            "pethub",
            "process_activity",
            "process_inbound",
            "processes",
            "remote_schemas",
            "rest_endpoints",
            "roles_inheritance",
            "rules",
            "security",
            "subscriptions",
            "tenancy",
        ];

        let test_dir = workspace_root().join("crates/conformance/tests");
        let test_files = std::fs::read_dir(&test_dir)
            .expect("reading conformance tests")
            .map(|entry| entry.expect("test entry").path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
            .collect::<Vec<_>>();
        for path in &test_files {
            let source = std::fs::read_to_string(path).expect("reading conformance test source");
            assert!(
                !source.contains("#[ignore"),
                "ignored conformance cases are forbidden: {}",
                path.strip_prefix(&test_dir).unwrap_or(path).display()
            );
        }

        let mut actual = test_files
            .into_iter()
            .map(|path| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .expect("UTF-8 test filename")
                    .to_string()
            })
            .collect::<Vec<_>>();
        actual.sort();
        let mut classified = SHARED
            .iter()
            .chain(BACKEND_SPECIFIC)
            .chain(POSTGRES_REFERENCE)
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        classified.sort();
        assert_eq!(actual, classified, "unclassified conformance test binary");
        eprintln!(
            "conformance manifest: {} shared / {} backend-specific / {} postgres-reference",
            SHARED.len(),
            BACKEND_SPECIFIC.len(),
            POSTGRES_REFERENCE.len()
        );
    }

    #[test]
    fn metadata_writer_emits_only_the_nonempty_rules_wrapper() {
        let mut with_rules = empty_metadata();
        with_rules.rules = serde_json::from_value(json!({
            "rules": [{
                "name": "is_ready",
                "result": "bool!",
                "expression": "true"
            }],
            "decision_tables": [{
                "name": "route",
                "inputs": {"amount": "int!"},
                "output": {"route": "string!"},
                "hit_policy": "first",
                "rows": [{
                    "id": "default",
                    "when": {"amount": "true"},
                    "output": {"route": "manual"}
                }]
            }]
        }))
        .expect("rule metadata deserializes");
        let with_rules_dir = Running::write_metadata_snapshot("rules_wrapper", &with_rules);
        let rules = std::fs::read_to_string(with_rules_dir.join("rules.yaml"))
            .expect("nonempty wrapper is serialized");
        assert!(rules.contains("is_ready"));
        assert!(rules.contains("decision_tables"));

        let mut with_types_only = empty_metadata();
        with_types_only.rules = serde_json::from_value(json!({
            "types": [{
                "name": "OrderStatus",
                "enum": ["draft", "submitted"]
            }]
        }))
        .expect("types-only rule metadata deserializes");
        let with_types_only_dir =
            Running::write_metadata_snapshot("rules_types_only_wrapper", &with_types_only);
        let types_only = std::fs::read_to_string(with_types_only_dir.join("rules.yaml"))
            .expect("a types-only wrapper is serialized");
        assert!(types_only.contains("OrderStatus"));
        assert!(!types_only.contains("rules:"));
        assert!(!types_only.contains("decision_tables:"));

        let without_rules_dir =
            Running::write_metadata_snapshot("empty_rules_wrapper", &empty_metadata());
        assert!(
            !without_rules_dir.join("rules.yaml").exists(),
            "an empty wrapper must not create rules.yaml"
        );

        std::fs::remove_dir_all(with_rules_dir).expect("remove rules metadata directory");
        std::fs::remove_dir_all(with_types_only_dir)
            .expect("remove types-only rules metadata directory");
        std::fs::remove_dir_all(without_rules_dir).expect("remove empty metadata directory");
    }

    #[test]
    fn metadata_writer_emits_nonempty_commands_section() {
        let mut metadata = empty_metadata();
        metadata.commands = serde_json::from_value(json!([{
            "name": "create_order",
            "source": "default",
            "permissions": [{"role": "customer"}],
            "steps": [{
                "name": "order",
                "insert": {
                    "table": {"schema": "public", "name": "orders"},
                    "object": {"status": {"literal": "draft"}},
                    "returning": ["id"]
                }
            }],
            "result": {"order_id": {"step": "order", "column": "id"}}
            ,"effects": [{
                "start_process": {
                    "process": "checkout_order",
                    "idempotency_key": {"argument": "request_id"}
                }
            }]
        }]))
        .expect("command metadata deserializes");

        let dir = Running::write_metadata_snapshot("commands_section", &metadata);
        let commands = std::fs::read_to_string(dir.join("commands.yaml"))
            .expect("nonempty commands section is serialized");
        assert!(commands.contains("create_order"));
        assert!(commands.contains("argument: request_id"));

        let reloaded = donat_metadata::load_metadata_dir(&dir)
            .expect("serialized command metadata reloads through the directory loader");
        assert_eq!(reloaded.commands[0].name, "create_order");
        std::fs::remove_dir_all(dir).expect("remove commands metadata directory");
    }

    #[test]
    fn metadata_writer_emits_only_the_nonempty_connectors_section() {
        let mut with_connectors = empty_metadata();
        with_connectors.connectors = serde_json::from_value(json!([{
            "name": "logistics_api",
            "module": "http",
            "config": {
                "endpoint_identity": "logistics_prod_eu_2026_07",
                "credential_identity": "logistics_primary",
                "base_url": "https://logistics.example.test"
            },
            "operations": [{
                "name": "create_shipment",
                "capacity": {
                    "max_in_flight": 8,
                    "rate_limit": { "permits": 20, "per": "1s", "burst": 8 }
                }
            }]
        }]))
        .expect("connector metadata deserializes");

        let with_connectors_dir =
            Running::write_metadata_snapshot("connectors_section", &with_connectors);
        let connectors = std::fs::read_to_string(with_connectors_dir.join("connectors.yaml"))
            .expect("nonempty connectors section is serialized");
        assert!(connectors.contains("logistics_api"));
        assert_eq!(
            donat_metadata::load_metadata_dir(&with_connectors_dir)
                .expect("serialized connector metadata reloads")
                .connectors[0]
                .name,
            "logistics_api"
        );

        let without_connectors_dir =
            Running::write_metadata_snapshot("empty_connectors_section", &empty_metadata());
        assert!(
            !without_connectors_dir.join("connectors.yaml").exists(),
            "an empty connector list must not create connectors.yaml"
        );

        std::fs::remove_dir_all(with_connectors_dir).expect("remove connector metadata directory");
        std::fs::remove_dir_all(without_connectors_dir)
            .expect("remove empty connector metadata directory");
    }

    #[test]
    fn metadata_writer_emits_only_the_nonempty_process_section() {
        let mut with_processes = empty_metadata();
        with_processes.processes = serde_json::from_value(json!([{
            "name": "checkout",
            "kind": "process",
            "version": 1,
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "output": [{ "name": "status", "type": "string!" }],
            "start_at": "done",
            "states": [{
                "id": "done",
                "output": {
                    "values": { "status": { "literal": "ready" } }
                }
            }]
        }]))
        .expect("process metadata deserializes");

        let with_processes_dir =
            Running::write_metadata_snapshot("processes_section", &with_processes);
        let flows = std::fs::read_to_string(with_processes_dir.join("flows.yaml"))
            .expect("nonempty process section is serialized");
        assert!(flows.contains("checkout"));
        assert_eq!(
            donat_metadata::load_metadata_dir(&with_processes_dir)
                .expect("serialized process metadata reloads")
                .processes[0]
                .name,
            "checkout"
        );

        let without_processes_dir =
            Running::write_metadata_snapshot("empty_processes_section", &empty_metadata());
        assert!(
            !without_processes_dir.join("flows.yaml").exists(),
            "an empty process list must not create flows.yaml"
        );

        std::fs::remove_dir_all(with_processes_dir).expect("remove process metadata directory");
        std::fs::remove_dir_all(without_processes_dir)
            .expect("remove empty process metadata directory");
    }
}
