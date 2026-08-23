//! A test stand: one fresh Postgres database, the engine's own and the
//! application's migrations, the application's metadata, a spawned engine,
//! and the two stubs a test talks through — an authentication hook that
//! turns `X-Donat-Role` / `X-Donat-User-Id` into a session, and a provider
//! stub that plays every HTTP provider the metadata's connectors name.
//!
//! One stand per test case. Two cases in one database would see each other's
//! rows, and the awaits an application test expresses ("this process reached
//! a terminal state") are keyed by process name, not by instance.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::Value as Json;

use crate::auth_hook::{self, AuthHook};
use crate::migrations::apply_sql_migration_dir;
use crate::provider_stub::{self, ProviderStub};

const ENGINE_HEALTH_DEADLINE: Duration = Duration::from_secs(30);
const ENGINE_HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const ENGINE_START_ATTEMPTS: usize = 3;
const ENGINE_START_RETRY_DELAY: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Everything that decides what a stand runs. Machine properties (where
/// Postgres is, which binary, where the engine's migrations are) come from the
/// caller; application properties (metadata, migrations, environment) come
/// from `donat.test.yaml`.
#[derive(Debug, Clone)]
pub struct StandConfig {
    /// Stem of the database name; sanitized and made unique per process.
    pub name: String,
    /// The `donat` binary that migrates and serves.
    pub engine_binary: PathBuf,
    /// The engine's own `donat.*` schema (shipped at `/usr/share/donat/migrations`).
    pub engine_migrations_dir: PathBuf,
    /// The application's schema, applied after the engine's.
    pub app_migrations_dir: Option<PathBuf>,
    /// The application's metadata directory.
    pub metadata_dir: PathBuf,
    /// The `default` source name whose Process revisions `migrate` deploys.
    pub source: String,
    /// Admin connection used to create and drop the stand database.
    pub admin_database_url: String,
    /// Where the engine's stdout/stderr go.
    pub log_dir: PathBuf,
    /// Environment for `migrate` and `serve`. The literal `${providers}` in a
    /// value is replaced with the provider stub's base URL.
    pub env: Vec<(String, String)>,
}

pub struct Stand {
    db_url: String,
    base_url: String,
    log_path: PathBuf,
    providers: ProviderStub,
    _auth: AuthHook,
    _engine: EngineProc,
    _database: DropDatabase,
    http: reqwest::blocking::Client,
}

impl Stand {
    pub fn boot(cfg: &StandConfig) -> Result<Self> {
        let providers = provider_stub::spawn();
        let auth = auth_hook::spawn();
        let mut env = cfg
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.replace("${providers}", providers.base_url())))
            .collect::<Vec<_>>();
        env.push(("DONAT_GRAPHQL_AUTH_HOOK".into(), auth.url().to_string()));
        env.push(("DONAT_GRAPHQL_AUTH_HOOK_MODE".into(), "POST".into()));

        let db_name = database_name(&cfg.name);
        let database = DropDatabase::create(&cfg.admin_database_url, &db_name)?;
        let db_url = database.url.clone();
        env.push(("DONAT_DATABASE_URL".into(), db_url.clone()));
        env.push(("DONAT_GRAPHQL_DATABASE_URL".into(), db_url.clone()));
        create_postgis(&db_url)?;

        // The production order: the engine's schema, then the application's,
        // then the Process revisions the metadata declares.
        run_migrate(cfg, &env, &[])?;
        if let Some(dir) = &cfg.app_migrations_dir {
            apply_sql_migration_dir(&db_url, dir)?;
        }
        run_migrate(
            cfg,
            &env,
            &[
                "--metadata-dir".into(),
                cfg.metadata_dir.display().to_string(),
                "--source".into(),
                cfg.source.clone(),
            ],
        )?;

        std::fs::create_dir_all(&cfg.log_dir)
            .with_context(|| format!("creating log directory {}", cfg.log_dir.display()))?;
        let http = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("building http client")?;

        let mut failures = Vec::new();
        for attempt in 1..=ENGINE_START_ATTEMPTS {
            let port = free_port()?;
            let log_path = if attempt == 1 {
                cfg.log_dir.join(format!("{db_name}.log"))
            } else {
                cfg.log_dir.join(format!("{db_name}.attempt-{attempt}.log"))
            };
            let log = std::fs::File::create(&log_path)
                .with_context(|| format!("creating {}", log_path.display()))?;
            let mut cmd = Command::new(&cfg.engine_binary);
            cmd.arg("--port")
                .arg(port.to_string())
                .arg("--metadata-dir")
                .arg(&cfg.metadata_dir)
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log));
            for (k, v) in &env {
                cmd.env(k, v);
            }
            let child = match cmd.spawn() {
                Ok(child) => child,
                Err(error) => {
                    failures.push(format!("attempt {attempt}: could not spawn donat: {error}"));
                    std::thread::sleep(ENGINE_START_RETRY_DELAY);
                    continue;
                }
            };
            let mut engine = EngineProc {
                child,
                base_url: format!("http://127.0.0.1:{port}"),
            };
            match wait_for_engine_health(&http, &mut engine) {
                None => {
                    return Ok(Stand {
                        db_url,
                        base_url: engine.base_url.clone(),
                        log_path,
                        providers,
                        _auth: auth,
                        _engine: engine,
                        _database: database,
                        http,
                    });
                }
                Some(reason) => {
                    drop(engine);
                    failures.push(format!(
                        "attempt {attempt}: {reason}; see {}",
                        log_path.display()
                    ));
                    std::thread::sleep(ENGINE_START_RETRY_DELAY);
                }
            }
        }
        Err(anyhow!(
            "engine failed to become healthy after {ENGINE_START_ATTEMPTS} attempts: {}",
            failures.join("; ")
        ))
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn db_url(&self) -> &str {
        &self.db_url
    }

    /// The engine's log for this stand — named in every failure message.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    pub fn providers(&self) -> &ProviderStub {
        &self.providers
    }

    pub fn pg(&self) -> Result<postgres::Client> {
        postgres::Client::connect(&self.db_url, postgres::NoTls)
            .with_context(|| format!("connecting to {}", self.db_url))
    }

    /// One HTTP request to the engine. A body that is not JSON comes back as
    /// a JSON string so the caller always has a value to compare.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: Option<&Json>,
    ) -> Result<(u16, Json)> {
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| anyhow!("bad HTTP method {method}"))?;
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base_url));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        let response = req.send().with_context(|| format!("requesting {path}"))?;
        let code = response.status().as_u16();
        let text = response.text().unwrap_or_default();
        Ok((
            code,
            serde_json::from_str(&text).unwrap_or(Json::String(text)),
        ))
    }
}

fn run_migrate(cfg: &StandConfig, env: &[(String, String)], extra: &[String]) -> Result<()> {
    let mut migrate = Command::new(&cfg.engine_binary);
    migrate
        .arg("migrate")
        .arg("--migrations-dir")
        .arg(&cfg.engine_migrations_dir)
        .args(extra);
    for (k, v) in env {
        migrate.env(k, v);
    }
    let output = migrate
        .output()
        .with_context(|| format!("running {} migrate", cfg.engine_binary.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "donat migrate {} failed:\n{}{}",
            extra.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// Concurrent `CREATE EXTENSION` across databases races inside Postgres
/// (shared library/template locks): serialize within this process and retry
/// to cover other test processes.
fn create_postgis(db_url: &str) -> Result<()> {
    static POSTGIS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = POSTGIS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut last = None;
    for _ in 0..10 {
        match postgres::Client::connect(db_url, postgres::NoTls)
            .and_then(|mut c| c.batch_execute("create extension if not exists postgis"))
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    Err(anyhow!("postgis init failed after retries: {last:?}"))
}

fn database_name(stem: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let sanitized = stem
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
        "apptest_{sanitized}_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// `postgresql://u:p@h:port/db` with the database swapped out.
fn with_db(admin_url: &str, db: &str) -> Result<String> {
    let (prefix, _) = admin_url
        .rsplit_once('/')
        .ok_or_else(|| anyhow!("database url must contain a database path"))?;
    Ok(format!("{prefix}/{db}"))
}

struct DropDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl DropDatabase {
    fn create(admin_url: &str, name: &str) -> Result<Self> {
        let mut client = postgres::Client::connect(admin_url, postgres::NoTls)
            .with_context(|| format!("connecting to {admin_url} (is postgres up?)"))?;
        client.batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))?;
        client.batch_execute(&format!("CREATE DATABASE {name}"))?;
        Ok(Self {
            admin_url: admin_url.to_string(),
            name: name.to_string(),
            url: with_db(admin_url, name)?,
        })
    }
}

impl Drop for DropDatabase {
    fn drop(&mut self) {
        if std::env::var_os("DONAT_TEST_KEEP_DATABASES").is_some() {
            return;
        }
        if let Ok(mut client) = postgres::Client::connect(&self.admin_url, postgres::NoTls) {
            let _ = client.batch_execute(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.name
            ));
        }
    }
}

struct EngineProc {
    child: Child,
    base_url: String,
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

fn free_port() -> Result<u16> {
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
            return Ok(port as u16);
        }
    }
    Err(anyhow!("could not find a free port for the engine"))
}
