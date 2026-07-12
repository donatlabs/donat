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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value as Json, json};

mod action_webhook;
pub mod cron_webhook;
mod remote_graphql;

// ---------------------------------------------------------------- fixtures

pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Load a fixture YAML into JSON, resolving `!include <file>` (both the real
/// YAML tag and the quoted-string spelling donat-cli produces) relative to
/// the including file.
pub fn load_fixture(path: &Path) -> Result<Json> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading fixture {}", path.display()))?;
    let v: serde_yaml::Value = serde_yaml::from_str(&text)
        .with_context(|| format!("parsing fixture {}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    yaml_to_json(&v, dir)
}

fn yaml_to_json(v: &serde_yaml::Value, dir: &Path) -> Result<Json> {
    use serde_yaml::Value as Y;
    Ok(match v {
        Y::Null => Json::Null,
        Y::Bool(b) => Json::Bool(*b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!(i)
            } else if let Some(u) = n.as_u64() {
                json!(u)
            } else {
                json!(n.as_f64().unwrap())
            }
        }
        Y::String(s) => {
            if let Some(rest) = s.strip_prefix("!include ") {
                load_fixture(&dir.join(rest.trim()))?
            } else {
                Json::String(s.clone())
            }
        }
        Y::Sequence(xs) => Json::Array(
            xs.iter()
                .map(|x| yaml_to_json(x, dir))
                .collect::<Result<_>>()?,
        ),
        Y::Mapping(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                let key = match k {
                    Y::String(s) => s.clone(),
                    other => serde_yaml::to_string(other)?.trim().to_string(),
                };
                out.insert(key, yaml_to_json(val, dir)?);
            }
            Json::Object(out)
        }
        Y::Tagged(t) => {
            if t.tag.to_string().trim_start_matches('!') == "include" {
                let f = t
                    .value
                    .as_str()
                    .ok_or_else(|| anyhow!("!include expects a string"))?;
                load_fixture(&dir.join(f))?
            } else {
                yaml_to_json(&t.value, dir)?
            }
        }
    })
}

// -------------------------------------------------------------- comparison

/// Selection tree extracted from the fixture's GraphQL query: response-alias
/// -> nested selections (None for leaf fields). Used to replicate tests-py
/// `collapse_order_not_selset`: key order is enforced only among keys that
/// are part of the selection set; everything else (errors, jsonb column
/// values, ...) compares order-insensitively.
#[derive(Default)]
pub struct SelMap(std::collections::HashMap<String, Option<SelMap>>);

impl SelMap {
    fn contains_key(&self, k: &str) -> bool {
        self.0.contains_key(k)
    }
    fn get(&self, k: &str) -> Option<&Option<SelMap>> {
        self.0.get(k)
    }
}

pub fn sel_tree_from_query(query: &str) -> Option<SelMap> {
    use graphql_parser::query::{Definition, OperationDefinition, Selection, SelectionSet};

    let doc = graphql_parser::parse_query::<String>(query).ok()?;
    let mut frags = std::collections::HashMap::new();
    for def in &doc.definitions {
        if let Definition::Fragment(f) = def {
            frags.insert(f.name.clone(), &f.selection_set);
        }
    }
    fn build<'a>(
        ss: &SelectionSet<'a, String>,
        frags: &std::collections::HashMap<String, &SelectionSet<'a, String>>,
    ) -> SelMap {
        let mut out = SelMap::default();
        for item in &ss.items {
            match item {
                Selection::Field(f) => {
                    let key = f.alias.clone().unwrap_or_else(|| f.name.clone());
                    let child = if f.selection_set.items.is_empty() {
                        None
                    } else {
                        Some(build(&f.selection_set, frags))
                    };
                    out.0.insert(key, child);
                }
                Selection::FragmentSpread(fs) => {
                    if let Some(inner) = frags.get(&fs.fragment_name) {
                        out.0.extend(build(inner, frags).0);
                    }
                }
                Selection::InlineFragment(inf) => {
                    out.0.extend(build(&inf.selection_set, frags).0);
                }
            }
        }
        out
    }
    for def in &doc.definitions {
        let ss = match def {
            Definition::Operation(OperationDefinition::Query(q)) => &q.selection_set,
            Definition::Operation(OperationDefinition::Mutation(m)) => &m.selection_set,
            Definition::Operation(OperationDefinition::Subscription(s)) => &s.selection_set,
            Definition::Operation(OperationDefinition::SelectionSet(ss)) => ss,
            Definition::Fragment(_) => continue,
        };
        return Some(build(ss, &frags));
    }
    None
}

/// Deep comparison. `sel` carries the selection tree for the current level;
/// among keys present in the tree, the relative order in expected and actual
/// must match, and their children recurse with their sub-tree. Keys outside
/// the tree (and everything once `sel` is None) compare order-insensitively.
/// Numbers compare by value (1 == 1.0), like Python.
pub fn json_matches(exp: &Json, act: &Json, sel: Option<&SelMap>) -> bool {
    match (exp, act) {
        (Json::Object(e), Json::Object(a)) => {
            if e.len() != a.len() || !e.keys().all(|k| a.contains_key(k)) {
                return false;
            }
            if let Some(tree) = sel {
                let eseq: Vec<&String> = e.keys().filter(|k| tree.contains_key(*k)).collect();
                let aseq: Vec<&String> = a.keys().filter(|k| tree.contains_key(*k)).collect();
                if eseq != aseq {
                    return false;
                }
            }
            e.iter().all(|(k, ve)| {
                let child = sel.and_then(|t| t.get(k)).and_then(|c| c.as_ref());
                json_matches(ve, &a[k], child)
            })
        }
        (Json::Array(e), Json::Array(a)) => {
            e.len() == a.len() && e.iter().zip(a.iter()).all(|(x, y)| json_matches(x, y, sel))
        }
        (Json::Number(e), Json::Number(a)) => {
            e == a || (e.as_f64().zip(a.as_f64()).is_some_and(|(x, y)| x == y))
        }
        _ => exp == act,
    }
}

/// Compare a full HTTP-level response: top-level object unordered, the
/// `data` subtree governed by the query's selection tree.
pub fn response_matches(exp: &Json, act: &Json, query_text: Option<&str>) -> bool {
    let tree = query_text.and_then(sel_tree_from_query);
    match (exp, act) {
        (Json::Object(e), Json::Object(a)) => {
            if e.len() != a.len() || !e.keys().all(|k| a.contains_key(k)) {
                return false;
            }
            e.iter().all(|(k, ve)| {
                let sel = if k == "data" { tree.as_ref() } else { None };
                json_matches(ve, &a[k], sel)
            })
        }
        _ => json_matches(exp, act, None),
    }
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
    AllowlistEntry, ArrayRelationship, ComputedField, CronTrigger, DeletePermission, EventTrigger,
    FunctionEntry, FunctionPermission, InheritedRole, InsertPermission, Metadata, ObjectRelationship,
    PermissionEntry, QualifiedTable, QueryCollection, RemoteRelationship, RemoteSchema,
    RemoteSchemaPermission, RestEndpoint, SelectPermission, Source, SourceKind, TableConfiguration,
    TableEntry, UpdatePermission,
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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

static BUILD_ENGINE: Once = Once::new();

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
                let path = std::env::temp_dir().join(format!(
                    "donat_{name}.sqlite"
                ));
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
                let configured = std::env::var("CLICKHOUSE_URL")
                    .context("CLICKHOUSE_URL is required")?;
                let mut admin = reqwest::Url::parse(&configured).context("parsing CLICKHOUSE_URL")?;
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
                    let _ = client.batch_execute(&format!(
                        "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
                    ));
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
    args: Vec<String>,
    admin_secret: Option<String>,
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
            args: vec![],
            admin_secret: None,
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
        self.env
            .push(("EVENT_WEBHOOK_HANDLER".to_string(), stub.base_url().to_string()));
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

    /// Classes marked `@pytest.mark.admin_secret`: the engine gets
    /// DONAT_GRAPHQL_ADMIN_SECRET and every request carries the secret
    /// header (mirroring tests-py `add_auth`).
    pub fn admin_secret(mut self, secret: &str) -> Self {
        self.admin_secret = Some(secret.to_string());
        self.env.push((
            "DONAT_GRAPHQL_ADMIN_SECRET".to_string(),
            secret.to_string(),
        ));
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
    pub fn start(self) -> Running {
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

        let metadata = self
            .initial_metadata
            .unwrap_or_else(|| default_metadata_for(backend, &db_url));

        Running {
            name: self.name,
            backend,
            env: self.env,
            args: self.args,
            admin_secret: self.admin_secret,
            webhook: self.webhook,
            cron: self.cron,
            event: self.event,
            run_migrations: self.run_migrations,
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

pub struct Running {
    pub name: String,
    pub backend: BackendId,
    env: Vec<(String, String)>,
    args: Vec<String>,
    admin_secret: Option<String>,
    webhook: Option<action_webhook::EngineHandle>,
    cron: Option<cron_webhook::CronWebhook>,
    event: Option<cron_webhook::CronWebhook>,
    run_migrations: bool,
    db_url: String,
    pub schema: String,
    _database: SuiteDatabase,
    /// Accumulated metadata, applied lazily when the engine is spawned.
    metadata: RefCell<Metadata>,
    /// The spawned engine, started on first request (`ensure_engine`).
    engine: RefCell<Option<EngineProc>>,
    http: reqwest::blocking::Client,
}

#[derive(Debug, Clone, Copy)]
pub enum FixtureColumnType {
    BigInt,
    Text,
    Json,
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
        if let Some(mut proc) = self.engine.borrow_mut().take() {
            let _ = proc.child.kill();
            let _ = proc.child.wait();
        }
    }
}

// --------------------------------------------------------------- the applier

/// A `postgres::Client` on the suite database (for run_sql / seed inserts).
fn pg_client(db_url: &str) -> postgres::Client {
    postgres::Client::connect(db_url, postgres::NoTls)
        .expect("connecting to the suite database")
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
        let column_type = |column: &FixtureColumn| match (self.backend, column.ty) {
            (BackendId::Clickhouse, FixtureColumnType::BigInt) => "UInt64",
            (BackendId::Clickhouse, FixtureColumnType::Text) => "String",
            (BackendId::Clickhouse, FixtureColumnType::Json) => "JSON",
            (BackendId::Sqlite, FixtureColumnType::BigInt) => "INTEGER",
            (BackendId::Sqlite, FixtureColumnType::Text) => "TEXT",
            (BackendId::Sqlite, FixtureColumnType::Json) => "JSON",
            (BackendId::Mysql, FixtureColumnType::BigInt) => "BIGINT",
            (BackendId::Mysql, FixtureColumnType::Text) => "TEXT",
            (BackendId::Mysql, FixtureColumnType::Json) => "JSON",
            (BackendId::Postgres, FixtureColumnType::BigInt) => "BIGINT",
            (BackendId::Postgres, FixtureColumnType::Text) => "TEXT",
            (BackendId::Postgres, FixtureColumnType::Json) => "JSONB",
        };
        let columns = fixture
            .columns
            .iter()
            .map(|column| {
                let base_type = column_type(column);
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
                format!(
                    "{} {}{nullable}{primary}",
                    quote(column.name),
                    native_type
                )
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
            BackendId::Postgres => pg_client(&self.db_url)
                .batch_execute(sql)
                .unwrap_or_else(|error| panic!("[{}] fixture SQL failed: {error}\n{sql}", self.name)),
            BackendId::Sqlite => rusqlite::Connection::open(&self.db_url)
                .and_then(|connection| connection.execute_batch(sql))
                .unwrap_or_else(|error| panic!("[{}] fixture SQL failed: {error}\n{sql}", self.name)),
            BackendId::Mysql => {
                use mysql::prelude::Queryable;
                let mut connection = mysql::Conn::new(self.db_url.as_str())
                    .unwrap_or_else(|error| panic!("[{}] MySQL connect failed: {error}", self.name));
                connection
                    .query_drop(sql)
                    .unwrap_or_else(|error| panic!("[{}] fixture SQL failed: {error}\n{sql}", self.name));
            }
            BackendId::Clickhouse => {
                self.http
                    .post(&self.db_url)
                    .body(sql.to_string())
                    .send()
                    .and_then(reqwest::blocking::Response::error_for_status)
                    .unwrap_or_else(|error| panic!("[{}] fixture SQL failed: {error}\n{sql}", self.name));
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
            BackendId::Sqlite => {
                donat_backend::AnyDialect::Sqlite(donat_backend::SqliteDialect)
            }
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
                event_triggers: vec![],
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
                pg_client(&self.db_url).batch_execute(sql).unwrap_or_else(|e| {
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
                let permission: SelectPermission = from_value("select permission", &args["permission"]);
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
                let permission: InsertPermission = from_value("insert permission", &args["permission"]);
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
                let permission: UpdatePermission = from_value("update permission", &args["permission"]);
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
                let permission: DeletePermission = from_value("delete permission", &args["permission"]);
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
                if !source.functions.iter().any(|f| same_object(&f.function, &function)) {
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
                self.metadata.borrow_mut().query_collections.push(collection);
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
                let name = args["name"].as_str().expect("rest endpoint name").to_string();
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
                source.functions.retain(|f| !same_object(&f.function, &function));
            }
            "drop_relationship" => {
                let table = qualified_from(&args["table"]);
                let name = args["relationship"].as_str().expect("relationship").to_string();
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
                self.with_table(&table, |t| t.remote_relationships.retain(|r| r.name != name));
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
                if let Some(f) = source.functions.iter_mut().find(|f| same_object(&f.function, &function)) {
                    f.permissions.retain(|p| p.role != role);
                }
            }

            "set_custom_types" => {
                let custom_types: donat_metadata::CustomTypes =
                    serde_json::from_value(args.clone())
                        .unwrap_or_else(|e| panic!("[{}] bad set_custom_types: {e}\n{op}", self.name));
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
                md.actions.push(donat_metadata::ActionEntry { permissions, ..entry });
            }

            "drop_action" => {
                let name = args["name"].as_str().expect("action name").to_string();
                self.metadata.borrow_mut().actions.retain(|a| a.name != name);
            }

            "create_action_permission" => {
                let action = args["action"].as_str().expect("action").to_string();
                let role = args["role"].as_str().expect("role").to_string();
                let mut md = self.metadata.borrow_mut();
                if let Some(a) = md.actions.iter_mut().find(|a| a.name == action) {
                    if !a.permissions.iter().any(|p| p.role == role) {
                        a.permissions.push(donat_metadata::ActionPermission { role });
                    }
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
        let dir = std::env::temp_dir().join(format!(
            "dist_conf_md_{}_{}",
            self.name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("databases")).unwrap();

        let md = self.metadata.borrow();
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
        // Apply DDL before serving, like a real deploy: the donat catalog
        // (migrations) plus per-table event-trigger reconciliation from the
        // metadata we are about to serve.
        if self.run_migrations {
            let migrations = workspace_root().join("migrations");
            let status = Command::new(engine_binary())
                .arg("migrate")
                .arg("--migrations-dir")
                .arg(&migrations)
                .arg("--metadata-dir")
                .arg(&metadata_dir)
                .env("DONAT_DATABASE_URL", &self.db_url)
                .status()
                .expect("running donat migrate");
            assert!(status.success(), "donat migrate failed for suite {}", self.name);
        }
        let port = free_port();
        let log_dir = workspace_root().join("target/conformance-logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log = std::fs::File::create(log_dir.join(format!("{}.log", self.name))).unwrap();

        let mut cmd = Command::new(engine_binary());
        cmd.arg("--port")
            .arg(port.to_string())
            .arg("--metadata-dir")
            .arg(&metadata_dir)
            .env("DONAT_DATABASE_URL", &self.db_url)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        for a in &self.args {
            cmd.arg(a);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawning donat");

        let proc = EngineProc {
            child,
            base_url: format!("http://127.0.0.1:{port}"),
            ws_base: format!("ws://127.0.0.1:{port}"),
            _metadata_dir: metadata_dir,
        };

        // Wait healthy.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(r) = self.http.get(format!("{}/healthz", proc.base_url)).send()
                && r.status().is_success()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "engine for suite {} did not become healthy; see target/conformance-logs/{}.log",
                self.name,
                self.name
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        // Let webhook callback endpoints reach the now-running engine.
        if let Some(handle) = &self.webhook {
            handle.set(&proc.base_url, self.admin_secret.clone());
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
        let base = self.engine.borrow().as_ref().unwrap().base_url.clone();
        let mut req = self.http.post(format!("{base}{path}")).json(body);
        let has_accept = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept"));
        if path == "/mcp" && !has_accept {
            req = req.header("Accept", "application/json, text/event-stream");
        }
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.send().expect("http request failed");
        let code = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        let body = serde_json::from_str(&text).unwrap_or(Json::String(text));
        (code, body)
    }

    fn auth_headers(&self, mut headers: Vec<(String, String)>) -> Vec<(String, String)> {
        if let Some(secret) = &self.admin_secret {
            headers.push(("X-Donat-Admin-Secret".to_string(), secret.clone()));
        }
        headers
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
        conf.get("headers")
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
                let mut req = self
                    .http
                    .request(m, format!("{}{url}", self.base_url()));
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
                    .map(|exp| if url == "/mcp" { strip_mcp_content(exp) } else { exp.clone() })
                    .is_some_and(|exp| response_matches(&exp, &resp, query_text))
            });
            assert!(
                ok,
                "[{}] {label}: response matched none of allowed_responses\nactual:\n{}",
                self.name,
                pretty(&resp)
            );
        } else if let Some(exp) = conf.get("response") {
            let exp = if url == "/mcp" { strip_mcp_content(exp) } else { exp.clone() };
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

/// Normalize a JSON-RPC `result` for MCP comparison by dropping fields that
/// are not part of the conformance contract:
///
/// - `content` (always): a text duplicate of the structured data.
/// - `structuredContent` *only when* `isError` is true: an error tool result's
///   structured payload carries engine-dependent GraphQL error details, so the
///   contract for a failure is just `isError: true`. On success,
///   `structuredContent` (the real data) is kept and asserted.
///
/// Everything else (`isError`, `tools`, `protocolVersion`, `serverInfo`,
/// `capabilities`, ...) is asserted as-is. GraphQL/REST comparison never calls
/// this.
fn strip_mcp_content(v: &Json) -> Json {
    let mut out = v.clone();
    if let Some(result) = out.get_mut("result").and_then(Json::as_object_mut) {
        result.remove("content");
        if result.get("isError") == Some(&Json::Bool(true)) {
            result.remove("structuredContent");
        }
    }
    out
}

// -------------------------------------------------------------------- tests

/// Unit tests for the pure parts of the harness: fixture loading
/// (`!include`) and the tests-py-faithful response comparison. They need
/// neither Postgres nor a running engine, so they live in the lib target
/// (the `tests/` binaries require a database).
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn backend_registry_has_stable_ci_ids() {
        assert_eq!(
            BackendId::ALL.map(BackendId::as_str),
            ["postgres", "sqlite", "mysql", "clickhouse"]
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
        assert_eq!(BackendId::Postgres.capabilities(), donat_backend::capabilities::postgres());
        assert_eq!(BackendId::Sqlite.capabilities(), donat_backend::capabilities::sqlite());
        assert_eq!(BackendId::Mysql.capabilities(), donat_backend::capabilities::mysql());
        assert_eq!(BackendId::Clickhouse.capabilities(), donat_backend::capabilities::clickhouse());
    }

    #[test]
    fn in_process_and_default_backends_need_no_explicit_url() {
        BackendId::Postgres.validate_configuration(|_| None).unwrap();
        BackendId::Sqlite.validate_configuration(|_| None).unwrap();
    }

    #[test]
    fn service_backends_require_explicit_urls() {
        let mysql = BackendId::Mysql.validate_configuration(|_| None).unwrap_err();
        assert!(mysql.to_string().contains("MYSQL_URL"), "{mysql}");
        let clickhouse = BackendId::Clickhouse.validate_configuration(|_| None).unwrap_err();
        assert!(clickhouse.to_string().contains("CLICKHOUSE_URL"), "{clickhouse}");

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
            .map(|entry| {
                entry["backend"]
                    .as_str()
                    .expect("matrix backend string")
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, BackendId::ALL.map(BackendId::as_str));
    }

    #[test]
    fn every_conformance_binary_is_classified() {
        const SHARED: &[&str] = &["backend_matrix"];
        const BACKEND_SPECIFIC: &[&str] = &["clickhouse"];
        const POSTGRES_REFERENCE: &[&str] = &[
            "actions",
            "agg_relay_introspection",
            "auth_env",
            "cron_triggers",
            "enabled_apis",
            "event_triggers",
            "graphql_mutations",
            "graphql_queries",
            "introspection_descriptions",
            "jwk",
            "jwt",
            "mcp_tools",
            "migrate",
            "remote_schemas",
            "rest_endpoints",
            "roles_inheritance",
            "security",
            "subscriptions",
        ];

        let mut actual = std::fs::read_dir(workspace_root().join("crates/conformance/tests"))
            .expect("reading conformance tests")
            .map(|entry| entry.expect("test entry").path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
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

    // ---------------------------------------------------- load_fixture

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "donat_conformance_fixture_{tag}_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn load_fixture_resolves_string_include_relative_to_file() {
        // The quoted-string spelling, resolved against the *including*
        // file's directory — including transitively from a subdirectory.
        let dir = tempdir("string");
        write(
            &dir,
            "suite/case.yaml",
            "setup: \"!include sub/inner.yaml\"\nname: top\n",
        );
        write(
            &dir,
            "suite/sub/inner.yaml",
            "deep: \"!include leaf.yaml\"\n",
        );
        write(&dir, "suite/sub/leaf.yaml", "- 1\n- two\n");

        let v = load_fixture(&dir.join("suite/case.yaml")).unwrap();
        assert_eq!(v["name"], json!("top"));
        assert_eq!(v["setup"]["deep"], json!([1, "two"]));
    }

    #[test]
    fn load_fixture_resolves_real_yaml_tag_include() {
        let dir = tempdir("tag");
        write(&dir, "case.yaml", "steps: !include steps.yaml\n");
        write(&dir, "steps.yaml", "- url: /v1/graphql\n  status: 200\n");

        let v = load_fixture(&dir.join("case.yaml")).unwrap();
        assert_eq!(v["steps"][0]["url"], json!("/v1/graphql"));
        assert_eq!(v["steps"][0]["status"], json!(200));
    }

    #[test]
    fn load_fixture_missing_include_target_errors() {
        let dir = tempdir("missing");
        write(&dir, "case.yaml", "setup: \"!include nope.yaml\"\n");
        let err = load_fixture(&dir.join("case.yaml")).unwrap_err();
        assert!(format!("{err:#}").contains("nope.yaml"), "got: {err:#}");
    }

    #[test]
    fn load_fixture_preserves_numbers_and_non_string_keys() {
        let dir = tempdir("scalars");
        write(
            &dir,
            "case.yaml",
            "int: 5\nbig: 18446744073709551615\nfloat: 1.5\nmap:\n  1: one\n",
        );
        let v = load_fixture(&dir.join("case.yaml")).unwrap();
        assert_eq!(v["int"], json!(5));
        assert_eq!(v["big"], json!(18446744073709551615u64));
        assert_eq!(v["float"], json!(1.5));
        // Non-string YAML keys are stringified.
        assert_eq!(v["map"]["1"], json!("one"));
    }

    // ----------------------------------------- json/response matching

    #[test]
    fn numbers_coerce_across_int_and_float() {
        assert!(json_matches(&json!(1), &json!(1.0), None));
        assert!(json_matches(&json!(1.0), &json!(1), None));
        assert!(!json_matches(&json!(1), &json!(2.0), None));
        assert!(json_matches(&json!({"n": 1}), &json!({"n": 1.0}), None));
    }

    #[test]
    fn objects_without_selection_tree_compare_unordered() {
        let exp = json!({"a": 1, "b": 2});
        let act = json!({"b": 2, "a": 1});
        assert!(json_matches(&exp, &act, None));
    }

    #[test]
    fn object_key_set_mismatch_fails() {
        // Missing, extra, and renamed keys all fail even order-insensitively.
        assert!(!json_matches(
            &json!({"a": 1}),
            &json!({"a": 1, "b": 2}),
            None
        ));
        assert!(!json_matches(
            &json!({"a": 1, "b": 2}),
            &json!({"a": 1}),
            None
        ));
        assert!(!json_matches(&json!({"a": 1}), &json!({"b": 1}), None));
    }

    #[test]
    fn arrays_require_equal_length_and_order() {
        assert!(json_matches(&json!([1, 2]), &json!([1, 2]), None));
        assert!(!json_matches(&json!([1, 2]), &json!([2, 1]), None));
        assert!(!json_matches(&json!([1, 2]), &json!([1, 2, 3]), None));
    }

    #[test]
    fn data_key_order_is_enforced_per_selection_tree() {
        let query = "query { a b }";
        let exp = json!({"data": {"a": 1, "b": 2}});
        let in_order = json!({"data": {"a": 1, "b": 2}});
        let reordered = json!({"data": {"b": 2, "a": 1}});
        assert!(response_matches(&exp, &in_order, Some(query)));
        assert!(!response_matches(&exp, &reordered, Some(query)));
    }

    #[test]
    fn nested_selection_order_is_enforced_per_level() {
        let query = "query { items { x y } }";
        let exp = json!({"data": {"items": [{"x": 1, "y": 2}]}});
        let good = json!({"data": {"items": [{"x": 1, "y": 2}]}});
        let bad = json!({"data": {"items": [{"y": 2, "x": 1}]}});
        assert!(response_matches(&exp, &good, Some(query)));
        assert!(!response_matches(&exp, &bad, Some(query)));
    }

    #[test]
    fn aliases_key_the_selection_tree() {
        // The response key is the alias; ordering is enforced on aliases.
        let query = "query { first: item { v } second: item { v } }";
        let exp = json!({"data": {"first": {"v": 1}, "second": {"v": 2}}});
        let good = json!({"data": {"first": {"v": 1}, "second": {"v": 2}}});
        let swapped = json!({"data": {"second": {"v": 2}, "first": {"v": 1}}});
        assert!(response_matches(&exp, &good, Some(query)));
        assert!(!response_matches(&exp, &swapped, Some(query)));
    }

    #[test]
    fn fragment_spread_fields_join_the_selection_tree() {
        let query = "
            query { item { ...F } }
            fragment F on Item { p q }
        ";
        let exp = json!({"data": {"item": {"p": 1, "q": 2}}});
        let good = json!({"data": {"item": {"p": 1, "q": 2}}});
        let bad = json!({"data": {"item": {"q": 2, "p": 1}}});
        assert!(response_matches(&exp, &good, Some(query)));
        assert!(
            !response_matches(&exp, &bad, Some(query)),
            "fragment fields must take part in order enforcement"
        );
    }

    #[test]
    fn inline_fragment_fields_join_the_selection_tree() {
        let query = "query { item { ... on Item { p q } } }";
        let exp = json!({"data": {"item": {"p": 1, "q": 2}}});
        let bad = json!({"data": {"item": {"q": 2, "p": 1}}});
        assert!(!response_matches(&exp, &bad, Some(query)));
    }

    #[test]
    fn jsonb_value_under_data_leaf_is_not_order_enforced() {
        // `payload` is a leaf field (no sub-selection): its object value is
        // a jsonb column, where Postgres does not guarantee key order.
        let query = "query { item { payload } }";
        let exp = json!({"data": {"item": {"payload": {"x": 1, "y": 2}}}});
        let act = json!({"data": {"item": {"payload": {"y": 2, "x": 1}}}});
        assert!(response_matches(&exp, &act, Some(query)));
    }

    #[test]
    fn keys_outside_the_selection_tree_are_not_order_enforced() {
        // Only keys present in the selection tree participate in the
        // relative-order check (collapse_order_not_selset semantics).
        let query = "query { a }";
        let exp = json!({"data": {"extra": 0, "a": 1}});
        let act = json!({"data": {"a": 1, "extra": 0}});
        assert!(response_matches(&exp, &act, Some(query)));
    }

    #[test]
    fn errors_compare_unordered() {
        // `errors` is outside `data`: key order inside error objects is free.
        let query = "query { a }";
        let exp = json!({"errors": [{
            "message": "boom",
            "extensions": {"code": "x", "path": "$"}
        }]});
        let act = json!({"errors": [{
            "extensions": {"path": "$", "code": "x"},
            "message": "boom"
        }]});
        assert!(response_matches(&exp, &act, Some(query)));
        // ...but error values still have to match.
        let wrong = json!({"errors": [{
            "message": "other",
            "extensions": {"code": "x", "path": "$"}
        }]});
        assert!(!response_matches(&exp, &wrong, Some(query)));
    }

    #[test]
    fn top_level_response_keys_compare_unordered() {
        let query = "query { a }";
        let exp = json!({"data": {"a": 1}, "errors": [{"message": "partial"}]});
        let act = json!({"errors": [{"message": "partial"}], "data": {"a": 1}});
        assert!(response_matches(&exp, &act, Some(query)));
    }

    #[test]
    fn unparsable_query_disables_order_enforcement() {
        assert!(sel_tree_from_query("not a graphql query {{{").is_none());
        let exp = json!({"data": {"a": 1, "b": 2}});
        let act = json!({"data": {"b": 2, "a": 1}});
        assert!(response_matches(
            &exp,
            &act,
            Some("not a graphql query {{{")
        ));
        assert!(response_matches(&exp, &act, None));
    }

    #[test]
    fn sel_tree_covers_operations_and_marks_leaves() {
        let tree = sel_tree_from_query("mutation { insert_x { affected_rows } }").unwrap();
        assert!(tree.contains_key("insert_x"));
        let child = tree.get("insert_x").unwrap().as_ref().unwrap();
        assert!(child.contains_key("affected_rows"));
        // Leaf fields carry no sub-tree.
        assert!(child.get("affected_rows").unwrap().is_none());
    }
}
