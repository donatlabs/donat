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
        Self::boot_with(cfg, provider_stub::spawn(), auth_hook::spawn())
    }

    /// Boot against a stub and hook the caller keeps between cases. A stable
    /// stub port lets every case of a worker share one template database,
    /// revisions included — the deploy's fingerprint covers the resolved
    /// provider configuration (see ADR declarative-saas/046), so the URL must
    /// not change between the deploy and the serve.
    pub fn boot_with(cfg: &StandConfig, providers: ProviderStub, auth: AuthHook) -> Result<Self> {
        let db_name = database_name(&cfg.name);
        let template = template_name(cfg, providers.base_url())?;
        let database = DropDatabase::create_from_template(
            &cfg.admin_database_url,
            &db_name,
            &template,
            cfg,
            providers.base_url(),
        )?;
        let db_url = database.url.clone();
        let env = child_environment(
            std::env::vars(),
            cfg.env
                .iter()
                .map(|(k, v)| (k.clone(), v.replace("${providers}", providers.base_url()))),
            [
                (
                    "DONAT_GRAPHQL_AUTH_HOOK".to_string(),
                    auth.url().to_string(),
                ),
                (
                    "DONAT_GRAPHQL_AUTH_HOOK_MODE".to_string(),
                    "POST".to_string(),
                ),
                ("DONAT_DATABASE_URL".to_string(), db_url.clone()),
                ("DONAT_GRAPHQL_DATABASE_URL".to_string(), db_url.clone()),
            ],
        );

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
            cmd.env_clear();
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

/// What the stand's children see. The parent's environment is not inherited:
/// `donat test` runs inside the image, where `DONAT_METADATA_DIR`,
/// `DONAT_GRAPHQL_JWT_SECRET` and the rest describe the deployment, and a
/// child that inherited them would migrate the wrong metadata and verify
/// tokens the test never issues. Only the variables that locate tools and
/// certificates pass through; `donat.test.yaml` says everything else, and the
/// stand's own connection and hook come last so nothing can override them.
fn child_environment(
    inherited: impl IntoIterator<Item = (String, String)>,
    app: impl IntoIterator<Item = (String, String)>,
    stand: impl IntoIterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    const PASS_THROUGH: &[&str] = &[
        "PATH",
        "HOME",
        "TMPDIR",
        "RUST_LOG",
        "RUST_BACKTRACE",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ];
    let mut env: Vec<(String, String)> = inherited
        .into_iter()
        .filter(|(k, _)| PASS_THROUGH.contains(&k.as_str()))
        .collect();
    for (k, v) in app.into_iter().chain(stand) {
        env.retain(|(existing, _)| existing != &k);
        env.push((k, v));
    }
    env
}

fn run_migrate(cfg: &StandConfig, env: &[(String, String)], extra: &[String]) -> Result<()> {
    let mut migrate = Command::new(&cfg.engine_binary);
    migrate.env_clear();
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

/// The migrated state every case shares — postgis, the engine's schema, the
/// application's migrations, the Process revisions — built once into a
/// template database and copied per case with `CREATE DATABASE … TEMPLATE`,
/// which is an order of magnitude cheaper than migrating each time. The
/// template's name carries a hash of everything that shaped it, so it is
/// reused across runs and rebuilt exactly when a migration, the metadata or
/// the engine changes. A Postgres advisory lock serializes concurrent
/// builders (parallel cases, and parallel test binaries).
fn template_name(cfg: &StandConfig, providers_url: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut feed_dir = |dir: &Path| -> Result<()> {
        let mut files = Vec::new();
        collect_files(dir, &mut files)?;
        files.sort();
        for file in files {
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // A test beside the metadata shapes no database state.
            if name.ends_with("_test.yaml") {
                continue;
            }
            hasher.update(
                file.strip_prefix(dir)
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .as_bytes(),
            );
            hasher.update(
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?,
            );
        }
        Ok(())
    };
    feed_dir(&cfg.engine_migrations_dir)?;
    if let Some(dir) = &cfg.app_migrations_dir {
        feed_dir(dir)?;
    }
    feed_dir(&cfg.metadata_dir)?;
    hasher.update(cfg.source.as_bytes());
    for (k, v) in &cfg.env {
        hasher.update(k.as_bytes());
        hasher.update(v.as_bytes());
    }
    // The engine deploys the revisions, so its build participates.
    let engine = std::fs::metadata(&cfg.engine_binary)
        .with_context(|| format!("reading {}", cfg.engine_binary.display()))?;
    hasher.update(engine.len().to_le_bytes());
    if let Ok(modified) = engine.modified()
        && let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH)
    {
        hasher.update(since.as_nanos().to_le_bytes());
    }
    let digest = hasher.finalize();
    // The resolved provider URL participates in the deployed revisions (ADR
    // declarative-saas/046), so the port is part of the template's identity —
    // as a suffix, not in the hash, so cleanup can tell "same content, another
    // worker's port" from "stale content".
    let port = providers_url.rsplit(':').next().unwrap_or("0");
    Ok(format!(
        "apptest_tpl_{}_p{port}",
        digest[..6]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

/// Build the template if this hash's database does not exist yet: create it,
/// with the worker's real provider URL in the environment — the deploy binds
/// to it, and the copies must serve under the same one.
/// install postgis, run the engine's migrations, the application's, and the
/// Process revision deploy — the same production order a real stand walks.
/// Old templates (a previous hash) are dropped opportunistically.
fn ensure_template(
    admin_url: &str,
    template: &str,
    cfg: &StandConfig,
    providers_url: &str,
) -> Result<()> {
    let mut admin = postgres::Client::connect(admin_url, postgres::NoTls)
        .with_context(|| format!("connecting to {admin_url} (is postgres up?)"))?;
    // One builder at a time, across processes.
    let key = i64::from_le_bytes(
        template.as_bytes()[template.len() - 8..]
            .try_into()
            .unwrap(),
    );
    admin.execute("SELECT pg_advisory_lock($1)", &[&key])?;
    let build = (|| -> Result<()> {
        let exists: bool = admin
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)",
                &[&template],
            )?
            .get(0);
        if exists {
            return Ok(());
        }
        // Same content prefix, another port: a sibling worker's template.
        // A different prefix is a stale build to reclaim.
        let prefix = &template[..template.rfind("_p").unwrap_or(template.len())];
        for stale in admin.query(
            "SELECT datname FROM pg_database WHERE datname LIKE 'apptest_tpl_%' \
             AND position($1 in datname) <> 1",
            &[&prefix],
        )? {
            let name: String = stale.get(0);
            let _ = admin.batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"));
        }
        let building = format!("{template}_building");
        admin.batch_execute(&format!("DROP DATABASE IF EXISTS {building} WITH (FORCE)"))?;
        admin.batch_execute(&format!("CREATE DATABASE {building}"))?;
        let db_url = with_db(admin_url, &building)?;
        create_postgis(&db_url)?;
        // The revision deploy resolves the application's environment (a
        // connector names its base-url variable); no provider answers during
        // a deploy, so `${providers}` points at a closed port.
        let env = child_environment(
            std::env::vars(),
            cfg.env
                .iter()
                .map(|(k, v)| (k.clone(), v.replace("${providers}", providers_url))),
            [
                ("DONAT_DATABASE_URL".to_string(), db_url.clone()),
                ("DONAT_GRAPHQL_DATABASE_URL".to_string(), db_url.clone()),
            ],
        );
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
        admin.batch_execute(&format!("ALTER DATABASE {building} RENAME TO {template}"))?;
        Ok(())
    })();
    let _ = admin.execute("SELECT pg_advisory_unlock($1)", &[&key]);
    build
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
    fn create_from_template(
        admin_url: &str,
        name: &str,
        template: &str,
        cfg: &StandConfig,
        providers_url: &str,
    ) -> Result<Self> {
        ensure_template(admin_url, template, cfg, providers_url)?;
        let mut client = postgres::Client::connect(admin_url, postgres::NoTls)
            .with_context(|| format!("connecting to {admin_url} (is postgres up?)"))?;
        client.batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))?;
        client
            .batch_execute(&format!("CREATE DATABASE {name} TEMPLATE {template}"))
            .with_context(|| format!("copying template {template}"))?;
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

#[cfg(test)]
mod tests {
    use super::child_environment;

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_child_does_not_inherit_the_deployments_donat_variables() {
        let env = child_environment(
            pairs(&[
                ("DONAT_METADATA_DIR", "/metadata"),
                ("DONAT_GRAPHQL_JWT_SECRET", "{}"),
                ("PATH", "/usr/bin"),
                ("PGPASSWORD", "secret"),
            ]),
            pairs(&[("APP_TOKEN", "t")]),
            pairs(&[("DONAT_DATABASE_URL", "postgresql://stand")]),
        );
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            ["PATH", "APP_TOKEN", "DONAT_DATABASE_URL"],
            "only tool-locating variables pass through: {env:?}"
        );
    }

    #[test]
    fn the_stands_own_variables_win_over_the_applications() {
        let env = child_environment(
            pairs(&[]),
            pairs(&[("DONAT_DATABASE_URL", "postgresql://app")]),
            pairs(&[("DONAT_DATABASE_URL", "postgresql://stand")]),
        );
        assert_eq!(env, pairs(&[("DONAT_DATABASE_URL", "postgresql://stand")]));
    }
}
