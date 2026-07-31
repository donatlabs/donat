//! HTTP entry point. The serving surface is data-plane only:
//! `/v1/graphql` (+ws), `/v1alpha1/graphql`, `/v1/relay`, `/v1beta1/relay`,
//! `/v1/connectors/{instance}/webhooks`, `/healthz`, `/v1/version`. There is
//! NO runtime admin/management API
//! (no `/v1/query` run_sql, no metadata mutation): schema is applied with
//! the `migrate` subcommand, metadata is loaded from YAML at boot.
//!
//! Launch forms:
//! - serve: `donat --database-url <url> [--metadata-dir <dir>] [--port N]`
//! - migrate (DDL): `donat migrate --migrations-dir <dir>`
//! - validate (metadata vs DB): `donat validate --metadata-dir <dir>`

mod action;
mod connector_webhook;
// The binary has its own module tree for its historic entry point while the
// integration tests use the library facade. Connector activity dispatch lands
// in Task 3, so the binary currently uses only registry construction here.
mod commands;
#[allow(dead_code)]
mod connectors;
mod cron;
mod events;
mod gql;
mod jwt;
mod mcp;
mod migrate;
mod processes;
mod remote;
mod rest;
mod state;
mod validate;
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{any, get, post},
};
use clap::Parser;
use serde_json::{Value, json};

use state::{AppState, Engine, SharedState, ensure_default_source};

/// Which API surfaces are mounted in the router. Selected at deploy time by
/// `DONAT_GRAPHQL_ENABLED_APIS` / `--enabled-apis` (see ADR
/// `api-surfaces/decisions/003-enabled-apis-flag.md`). A disabled surface's
/// routes are simply not registered (so requests get a plain 404); there is
/// no per-request gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnabledApis {
    graphql: bool,
    rest: bool,
    mcp: bool,
}

/// Parse the enabled-apis list flag.
///
/// - `None` (flag absent: neither CLI nor env set) => default = all three on.
/// - `Some(s)` => exactly the recognized, comma-separated tokens listed
///   (case-insensitive, trimmed): `graphql`, `rest`, `mcp`. Unknown tokens are
///   warned about and ignored (not fatal). An explicitly empty value enables no
///   data API (warned about).
fn parse_enabled_apis(raw: Option<&str>) -> EnabledApis {
    let raw = match raw {
        None => {
            return EnabledApis {
                graphql: true,
                rest: true,
                mcp: true,
            };
        }
        Some(s) => s,
    };

    let mut apis = EnabledApis {
        graphql: false,
        rest: false,
        mcp: false,
    };
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        match token.to_ascii_lowercase().as_str() {
            "graphql" => apis.graphql = true,
            "rest" => apis.rest = true,
            "mcp" => apis.mcp = true,
            other => {
                tracing::warn!(token = %other, "ignoring unknown enabled-apis token");
            }
        }
    }
    if !apis.graphql && !apis.rest && !apis.mcp {
        tracing::warn!(
            "DONAT_GRAPHQL_ENABLED_APIS selects no data API; all data surfaces (graphql/rest/mcp) are disabled"
        );
    }
    apis
}

#[derive(Parser, Debug)]
#[command(
    name = "donat",
    about = "GraphQL engine over Postgres (Donat v2-compatible)"
)]
struct Args {
    /// Donat v2 metadata directory (version: 3 format). Optional.
    #[arg(long, env = "DONAT_METADATA_DIR")]
    metadata_dir: Option<PathBuf>,

    /// Postgres connection string.
    #[arg(long, env = "DONAT_DATABASE_URL")]
    database_url: Option<String>,

    /// Donat-compatible alias; also the default source's database.
    #[arg(long)]
    metadata_database_url: Option<String>,

    #[arg(long, env = "DONAT_PORT", default_value_t = 8080)]
    port: u16,

    /// If set, metadata endpoints require X-Donat-Admin-Secret.
    #[arg(long, env = "DONAT_GRAPHQL_ADMIN_SECRET")]
    admin_secret: Option<String>,

    /// Comma-separated list of API surfaces to expose: `graphql`, `rest`,
    /// `mcp` (case-insensitive). Absent => all three. Unknown tokens are
    /// ignored with a warning.
    #[arg(long, env = "DONAT_GRAPHQL_ENABLED_APIS")]
    enabled_apis: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Donat-compatible serve subcommand.
    Serve(ServeArgs),
    /// Apply versioned SQL schema migrations (DDL), then exit.
    Migrate(MigrateArgs),
    /// Validate YAML metadata against the database, then exit.
    Validate(ValidateArgs),
}

#[derive(clap::Args, Debug)]
struct MigrateArgs {
    /// Directory of `V{n}__name.sql` migration files.
    #[arg(long, default_value = "migrations")]
    migrations_dir: PathBuf,
    /// If given, also reconcile table event-trigger DDL (per-table Postgres
    /// triggers) from this metadata directory after applying SQL migrations.
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
    /// Metadata source to migrate. Valid only with a metadata directory.
    #[arg(long)]
    source: Option<String>,
}

#[derive(clap::Args, Debug)]
struct ValidateArgs {
    /// Metadata directory to validate (defaults to --metadata-dir).
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
    /// Metadata source to validate.
    #[arg(long)]
    source: Option<String>,
}

#[derive(clap::Args, Debug)]
struct ServeArgs {
    #[arg(long)]
    server_port: Option<u16>,
    /// Accepted for compatibility; ignored.
    #[arg(long)]
    enable_telemetry: Option<String>,
    #[arg(long, default_value_t = false)]
    stringify_numeric_types: bool,
    #[arg(long)]
    admin_secret: Option<String>,
    /// CLI override of `--enabled-apis` (wins over the global flag / env).
    #[arg(long)]
    enabled_apis: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentSelection {
    RefineryOnly {
        database_url: String,
        migrations_dir: PathBuf,
    },
    MetadataSource {
        metadata_dir: PathBuf,
        source_name: String,
        database_url: String,
        migrations_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataSourceSelection {
    pub metadata_dir: PathBuf,
    pub source_name: String,
    pub database_url: String,
}

pub(crate) fn resolve_migrate_selection(
    global: &Args,
    cli: &MigrateArgs,
    read_env: impl Fn(&str) -> Result<String, std::env::VarError>,
) -> anyhow::Result<DeploymentSelection> {
    let metadata_dir = cli
        .metadata_dir
        .clone()
        .or_else(|| global.metadata_dir.clone());

    let Some(metadata_dir) = metadata_dir else {
        if cli.source.is_some() {
            anyhow::bail!("--source requires --metadata-dir");
        }
        let database_url = resolve_global_database_url(global, &read_env)?.ok_or_else(|| {
            anyhow::anyhow!("--database-url or --metadata-database-url is required")
        })?;
        return Ok(DeploymentSelection::RefineryOnly {
            database_url,
            migrations_dir: cli.migrations_dir.clone(),
        });
    };

    let selected = resolve_metadata_source(global, metadata_dir, cli.source.as_deref(), &read_env)?;
    Ok(DeploymentSelection::MetadataSource {
        metadata_dir: selected.metadata_dir,
        source_name: selected.source_name,
        database_url: selected.database_url,
        migrations_dir: Some(cli.migrations_dir.clone()),
    })
}

pub(crate) fn resolve_validate_selection(
    global: &Args,
    cli: &ValidateArgs,
    read_env: impl Fn(&str) -> Result<String, std::env::VarError>,
) -> anyhow::Result<MetadataSourceSelection> {
    let metadata_dir = cli
        .metadata_dir
        .clone()
        .or_else(|| global.metadata_dir.clone())
        .ok_or_else(|| anyhow::anyhow!("validate needs --metadata-dir"))?;
    resolve_metadata_source(global, metadata_dir, cli.source.as_deref(), &read_env)
}

fn resolve_metadata_source(
    global: &Args,
    metadata_dir: PathBuf,
    requested_source: Option<&str>,
    read_env: &impl Fn(&str) -> Result<String, std::env::VarError>,
) -> anyhow::Result<MetadataSourceSelection> {
    use donat_metadata::{DatabaseUrl, SourceKind};

    let metadata = donat_metadata::load_metadata_dir(&metadata_dir).map_err(|error| {
        anyhow::anyhow!(
            "failed to load metadata from {}: {error}",
            metadata_dir.display()
        )
    })?;

    let source = match requested_source {
        Some(source_name) => metadata
            .sources
            .iter()
            .find(|source| source.name == source_name)
            .ok_or_else(|| anyhow::anyhow!("source `{source_name}` was not found in metadata"))?,
        None => {
            let mut postgres_sources = metadata
                .sources
                .iter()
                .filter(|source| source.kind == SourceKind::Postgres);
            let source = postgres_sources
                .next()
                .ok_or_else(|| anyhow::anyhow!("metadata contains no Postgres source"))?;
            if postgres_sources.next().is_some() {
                anyhow::bail!(
                    "metadata contains multiple Postgres sources; pass --source explicitly"
                );
            }
            source
        }
    };

    if source.kind != SourceKind::Postgres {
        anyhow::bail!("source `{}` is not Postgres", source.name);
    }

    let database_url = match source
        .configuration
        .connection_info
        .as_ref()
        .map(|connection| &connection.database_url)
    {
        Some(DatabaseUrl::Url(url)) => url.clone(),
        Some(DatabaseUrl::FromEnv { from_env }) => read_env(from_env)
            .map_err(|_| anyhow::anyhow!("source `{}` requires environment variable `{from_env}`", source.name))?,
        None => resolve_global_database_url(global, read_env)?.ok_or_else(|| {
            anyhow::anyhow!(
                "source `{}` has no connection URL and --database-url or --metadata-database-url is required",
                source.name
            )
        })?,
    };

    Ok(MetadataSourceSelection {
        metadata_dir,
        source_name: source.name.clone(),
        database_url,
    })
}

fn resolve_global_database_url(
    global: &Args,
    read_env: &impl Fn(&str) -> Result<String, std::env::VarError>,
) -> anyhow::Result<Option<String>> {
    if let Some(url) = &global.database_url {
        return Ok(Some(url.clone()));
    }
    if let Some(url) = &global.metadata_database_url {
        return Ok(Some(url.clone()));
    }
    if let Some(url) = read_optional_env("DONAT_DATABASE_URL", read_env)? {
        return Ok(Some(url));
    }
    read_optional_env("DONAT_GRAPHQL_DATABASE_URL", read_env)
}

fn read_optional_env(
    name: &str,
    read_env: &impl Fn(&str) -> Result<String, std::env::VarError>,
) -> anyhow::Result<Option<String>> {
    match read_env(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("environment variable `{name}` is not valid Unicode")
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "donat=info".into()),
        )
        .init();

    let args = Args::parse();
    let serve = match &args.command {
        Some(Command::Serve(serve)) => Some(serve),
        _ => None,
    };

    // Deploy-time subcommands: do their job and exit (no server, no
    // request-path mutation surface).
    match &args.command {
        Some(Command::Migrate(m)) => {
            match resolve_migrate_selection(&args, m, |name| std::env::var(name))? {
                DeploymentSelection::RefineryOnly {
                    database_url,
                    migrations_dir,
                } => {
                    migrate::run_migrate(&database_url, &migrations_dir).await?;
                }
                DeploymentSelection::MetadataSource {
                    metadata_dir,
                    source_name,
                    database_url,
                    migrations_dir,
                } => {
                    if let Some(migrations_dir) = migrations_dir {
                        migrate::run_migrate(&database_url, &migrations_dir).await?;
                    }

                    // Metadata compilation happens after schema migrations but
                    // before metadata-owned DDL. A rejected candidate cannot
                    // partially change event/process deployment state.
                    let problems = validate::check_source_consistency(
                        &database_url,
                        &metadata_dir,
                        &source_name,
                    )
                    .await?;
                    require_consistent_metadata(&problems)?;

                    let deployment = validate::compile_source_process_deployment(
                        &database_url,
                        &metadata_dir,
                        &source_name,
                    )
                    .await?;
                    processes::reconcile(
                        &source_name,
                        &database_url,
                        &deployment.source_catalog,
                        &deployment.processes,
                    )
                    .await?;

                    let mut metadata = donat_metadata::load_metadata_dir(&metadata_dir)?;
                    metadata.sources.retain(|source| source.name == source_name);
                    events::reconcile(&database_url, &metadata).await?;
                    tracing::info!(
                        dir = %metadata_dir.display(),
                        source = %source_name,
                        "source metadata reconciled"
                    );
                }
            }
            return Ok(());
        }
        Some(Command::Validate(v)) => {
            let selected = resolve_validate_selection(&args, v, |name| std::env::var(name))?;
            let problems = validate::check_source_consistency(
                &selected.database_url,
                &selected.metadata_dir,
                &selected.source_name,
            )
            .await?;
            require_consistent_metadata(&problems)?;
            return Ok(());
        }
        _ => {}
    }

    let database_url = resolve_global_database_url(&args, &|name| std::env::var(name))?
        .ok_or_else(|| anyhow::anyhow!("--database-url or --metadata-database-url is required"))?;
    let port = serve.and_then(|s| s.server_port).unwrap_or(args.port);
    let admin_secret = serve
        .and_then(|s| s.admin_secret.clone())
        .or(args.admin_secret);
    // CLI override (serve) wins over the global flag / env, mirroring
    // admin_secret. `None` (truly unset) => default all surfaces on.
    let enabled_apis = parse_enabled_apis(
        serve
            .and_then(|s| s.enabled_apis.clone())
            .or(args.enabled_apis)
            .as_deref(),
    );
    let stringify_numerics = serve.map(|s| s.stringify_numeric_types).unwrap_or(false);
    let unauthorized_role = std::env::var("DONAT_GRAPHQL_UNAUTHORIZED_ROLE").ok();
    let allowlist_enabled = std::env::var("DONAT_GRAPHQL_ENABLE_ALLOWLIST")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let auth_hook = std::env::var("DONAT_GRAPHQL_AUTH_HOOK").ok().map(|url| {
        let mode =
            std::env::var("DONAT_GRAPHQL_AUTH_HOOK_MODE").unwrap_or_else(|_| "GET".to_string());
        (url, mode)
    });
    let jwt = std::env::var("DONAT_GRAPHQL_JWT_SECRET")
        .ok()
        .and_then(|raw| jwt::JwtConfig::from_env_value(&raw));
    let infer_function_permissions = std::env::var("DONAT_GRAPHQL_INFER_FUNCTION_PERMISSIONS")
        .map(|v| !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true);

    let mut metadata = match &args.metadata_dir {
        Some(dir) if dir.exists() => {
            let md = donat_metadata::load_metadata_dir(dir)?;
            tracing::info!(dir = %dir.display(), "metadata loaded");
            md
        }
        _ => donat_metadata::Metadata {
            version: 3,
            sources: vec![],
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
        },
    };
    ensure_default_source(&mut metadata);

    // Connector configuration is fully validated before this process opens a
    // listener. The immutable registry retains runtime credentials privately;
    // errors contain static metadata or variable names only, never values.
    let connectors = Arc::new(connectors::ConnectorRegistry::build(&metadata)?);

    if let Some(jwt) = &jwt {
        jwt.spawn_refresher(reqwest::Client::new());
    }
    let state: SharedState = Arc::new(AppState {
        engine: tokio::sync::RwLock::new(Arc::new(Engine::bootstrap_checked(metadata)?)),
        connectors,
        default_url: database_url,
        admin_secret,
        unauthorized_role,
        stringify_numerics,
        infer_function_permissions,
        jwt,
        auth_hook,
        http: reqwest::Client::new(),
        allowlist_enabled,
        subscription_permits: Arc::new(tokio::sync::Semaphore::new(
            std::env::var("DONAT_GRAPHQL_MAX_ACTIVE_SUBSCRIPTIONS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1_000),
        )),
        subscription_poll_permits: Arc::new(tokio::sync::Semaphore::new(
            std::env::var("DONAT_GRAPHQL_MAX_CONCURRENT_SUBSCRIPTION_POLLS")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value: &usize| *value > 0)
                .unwrap_or(16),
        )),
    });

    // The database may still be starting; retry the first sync.
    {
        let mut attempt = 0;
        loop {
            match state.sync_sources().await {
                Ok(()) => break,
                Err(e) if attempt < 30 => {
                    attempt += 1;
                    tracing::warn!(attempt, error = %e, "database not ready, retrying");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => anyhow::bail!("cannot initialize sources: {e}"),
            }
        }
    }
    {
        let engine = state.engine.read().await;
        tracing::info!(
            sources = engine.metadata.sources.len(),
            tables = engine
                .catalogs
                .values()
                .map(|catalog| catalog.tables.len())
                .sum::<usize>(),
            schema_compiled = engine.compiled.is_some(),
            "initialized"
        );
    }

    // Background delivery of cron (scheduled) triggers. No-op unless the
    // metadata declares any (then the `donat` catalog must exist — apply
    // `migrate` before serving).
    cron::spawn(state.clone());
    // Background delivery of table event triggers. The per-table Postgres
    // triggers that capture events are created by `migrate --metadata-dir`.
    events::spawn(state.clone());
    // Durable Process workers retain the exact Engine snapshot published by
    // sync_sources; polling only wakes source-local journal transactions.
    processes::spawn(state.clone()).await?;
    // Liveness/version are not data APIs — always mounted.
    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/version", get(version))
        // Connector ingress is a provider-facing deployment surface, not one
        // of the optional GraphQL/REST/MCP data APIs. It remains signed and
        // declaration-bound even when all data APIs are disabled.
        .merge(connector_webhook::router());
    // Data APIs are mounted only when enabled (deploy-time flag); a disabled
    // surface's routes are simply absent => plain 404.
    if enabled_apis.graphql {
        app = app
            .route("/v1/graphql", post(graphql).get(ws::upgrade))
            .route("/v1alpha1/graphql", post(graphql_legacy).get(ws::upgrade))
            .route("/v1/relay", post(relay).get(ws::upgrade_relay))
            .route("/v1beta1/relay", post(relay).get(ws::upgrade_relay));
    }
    if enabled_apis.rest {
        app = app.route("/api/rest/{*path}", any(rest::dispatch));
    }
    if enabled_apis.mcp {
        app = app.route(
            "/mcp",
            any(mcp::method_not_allowed)
                .post(mcp::dispatch)
                .get(mcp::get_not_allowed)
                .delete(mcp::delete_not_allowed)
                .layer(DefaultBodyLimit::max(mcp::MCP_MAX_REQUEST_BYTES)),
        );
    }
    tracing::info!(
        graphql = enabled_apis.graphql,
        rest = enabled_apis.rest,
        mcp = enabled_apis.mcp,
        "enabled API surfaces"
    );
    let app = app.with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn require_consistent_metadata(problems: &[String]) -> anyhow::Result<()> {
    if problems.is_empty() {
        tracing::info!("metadata is consistent");
        return Ok(());
    }
    for problem in problems {
        tracing::error!("inconsistency: {problem}");
    }
    anyhow::bail!(
        "metadata validation failed: {} inconsistency(ies)",
        problems.len()
    );
}

async fn healthz() -> &'static str {
    "OK"
}

async fn version() -> Json<Value> {
    Json(json!({ "version": env!("CARGO_PKG_VERSION") }))
}

async fn graphql(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let session = match gql::resolve_session(&state, &headers).await {
        Ok(s) => s,
        Err((status, errors)) => return (status, Json(errors)),
    };
    let (status, response) = gql::execute_full(&state, &session, &body, false, &headers).await;
    (status, Json(response))
}

/// /v1alpha1/graphql keeps the legacy behavior: auth failures are 400.
async fn graphql_legacy(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let session = match gql::resolve_session(&state, &headers).await {
        Ok(s) => s,
        Err((_, errors)) => return (StatusCode::BAD_REQUEST, Json(errors)),
    };
    let (status, response) = gql::execute(&state, &session, &body).await;
    (status, Json(response))
}

async fn relay(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let session = match gql::resolve_session(&state, &headers).await {
        Ok(s) => s,
        Err((status, errors)) => return (status, Json(errors)),
    };
    let (status, response) = gql::execute_with(&state, &session, &body, true).await;
    (status, Json(response))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        Args, DeploymentSelection, EnabledApis, MetadataSourceSelection, MigrateArgs, ValidateArgs,
        parse_enabled_apis, resolve_migrate_selection, resolve_validate_selection,
    };

    static NEXT_METADATA_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestMetadataDir {
        path: PathBuf,
    }

    impl TestMetadataDir {
        fn new(sources: &str) -> Self {
            let sequence = NEXT_METADATA_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "donat-source-selection-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir_all(path.join("databases"))
                .expect("test metadata directory should be created");
            std::fs::write(path.join("version.yaml"), "version: 3\n")
                .expect("test metadata version should be written");
            std::fs::write(path.join("databases/databases.yaml"), sources)
                .expect("test source metadata should be written");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestMetadataDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn args(
        metadata_dir: Option<PathBuf>,
        database_url: Option<&str>,
        metadata_database_url: Option<&str>,
    ) -> Args {
        Args {
            metadata_dir,
            database_url: database_url.map(str::to_owned),
            metadata_database_url: metadata_database_url.map(str::to_owned),
            port: 8080,
            admin_secret: None,
            enabled_apis: None,
            command: None,
        }
    }

    fn migrate_args(metadata_dir: Option<PathBuf>, source: Option<&str>) -> MigrateArgs {
        MigrateArgs {
            migrations_dir: PathBuf::from("schema-migrations"),
            metadata_dir,
            source: source.map(str::to_owned),
        }
    }

    fn validate_args(metadata_dir: Option<PathBuf>, source: Option<&str>) -> ValidateArgs {
        ValidateArgs {
            metadata_dir,
            source: source.map(str::to_owned),
        }
    }

    fn expect_refinery_url(selection: DeploymentSelection, expected_url: &str) {
        match selection {
            DeploymentSelection::RefineryOnly {
                database_url,
                migrations_dir,
            } => {
                assert_eq!(database_url, expected_url);
                assert_eq!(migrations_dir, PathBuf::from("schema-migrations"));
            }
            DeploymentSelection::MetadataSource { .. } => {
                panic!("expected refinery-only deployment selection")
            }
        }
    }

    fn expect_metadata_source(
        selection: MetadataSourceSelection,
        expected_dir: &Path,
        expected_source: &str,
        expected_url: &str,
    ) {
        assert_eq!(selection.metadata_dir, expected_dir);
        assert_eq!(selection.source_name, expected_source);
        assert_eq!(selection.database_url, expected_url);
    }

    fn apis(graphql: bool, rest: bool, mcp: bool) -> EnabledApis {
        EnabledApis { graphql, rest, mcp }
    }

    #[test]
    fn absent_enables_all() {
        assert_eq!(parse_enabled_apis(None), apis(true, true, true));
    }

    #[test]
    fn single_token_enables_only_that() {
        assert_eq!(
            parse_enabled_apis(Some("graphql")),
            apis(true, false, false)
        );
    }

    #[test]
    fn two_tokens() {
        assert_eq!(
            parse_enabled_apis(Some("graphql,rest")),
            apis(true, true, false)
        );
    }

    #[test]
    fn case_and_space_tolerant() {
        assert_eq!(
            parse_enabled_apis(Some("GraphQL , MCP")),
            apis(true, false, true)
        );
    }

    #[test]
    fn unknown_token_ignored() {
        assert_eq!(
            parse_enabled_apis(Some("graphql,bogus")),
            apis(true, false, false)
        );
    }

    #[test]
    fn empty_enables_none() {
        assert_eq!(parse_enabled_apis(Some("")), apis(false, false, false));
    }

    #[test]
    fn source_selection_refinery_only_url_precedence_is_stable() {
        let cli = migrate_args(None, None);

        let selection = resolve_migrate_selection(
            &args(
                None,
                Some("postgres://explicit"),
                Some("postgres://metadata-alias"),
            ),
            &cli,
            |name| match name {
                "DONAT_DATABASE_URL" => Ok("postgres://donat-env".to_owned()),
                "DONAT_GRAPHQL_DATABASE_URL" => Ok("postgres://graphql-env".to_owned()),
                _ => Err(std::env::VarError::NotPresent),
            },
        )
        .expect("explicit database URL should resolve");
        expect_refinery_url(selection, "postgres://explicit");

        let selection = resolve_migrate_selection(
            &args(None, None, Some("postgres://metadata-alias")),
            &cli,
            |name| match name {
                "DONAT_DATABASE_URL" => Ok("postgres://donat-env".to_owned()),
                "DONAT_GRAPHQL_DATABASE_URL" => Ok("postgres://graphql-env".to_owned()),
                _ => Err(std::env::VarError::NotPresent),
            },
        )
        .expect("metadata database URL alias should resolve");
        expect_refinery_url(selection, "postgres://metadata-alias");

        let selection =
            resolve_migrate_selection(&args(None, None, None), &cli, |name| match name {
                "DONAT_DATABASE_URL" => Ok("postgres://donat-env".to_owned()),
                "DONAT_GRAPHQL_DATABASE_URL" => Ok("postgres://graphql-env".to_owned()),
                _ => Err(std::env::VarError::NotPresent),
            })
            .expect("DONAT_DATABASE_URL should resolve");
        expect_refinery_url(selection, "postgres://donat-env");

        let selection =
            resolve_migrate_selection(&args(None, None, None), &cli, |name| match name {
                "DONAT_GRAPHQL_DATABASE_URL" => Ok("postgres://graphql-env".to_owned()),
                _ => Err(std::env::VarError::NotPresent),
            })
            .expect("legacy GraphQL database URL alias should resolve");
        expect_refinery_url(selection, "postgres://graphql-env");
    }

    #[test]
    fn source_selection_refinery_only_requires_a_database_url() {
        let error =
            resolve_migrate_selection(&args(None, None, None), &migrate_args(None, None), |_| {
                Err(std::env::VarError::NotPresent)
            })
            .expect_err("refinery-only migration without a URL must fail");

        assert!(
            error
                .to_string()
                .contains("--database-url or --metadata-database-url is required"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn source_selection_refinery_only_rejects_source_without_loading_metadata() {
        let error = resolve_migrate_selection(
            &args(None, Some("postgres://explicit"), None),
            &migrate_args(None, Some("primary")),
            |_| panic!("refinery-only migration with an explicit URL must not read environment"),
        )
        .expect_err("--source without metadata must fail");

        assert!(
            error
                .to_string()
                .contains("--source requires --metadata-dir"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn source_selection_validate_requires_metadata() {
        let error = resolve_validate_selection(
            &args(None, Some("postgres://fallback"), None),
            &validate_args(None, None),
            |_| panic!("validation without metadata must not read environment"),
        )
        .expect_err("validation without metadata must fail");

        assert!(
            error.to_string().contains("validate needs --metadata-dir"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn source_selection_explicit_postgres_source_uses_its_literal_url() {
        let metadata = TestMetadataDir::new(
            r#"
- name: primary
  kind: postgres
  configuration:
    connection_info:
      database_url: postgres://primary
  tables: []
- name: secondary
  kind: postgres
  configuration:
    connection_info:
      database_url: postgres://secondary
  tables: []
"#,
        );
        let global = args(
            None,
            Some("postgres://must-not-win"),
            Some("postgres://must-not-win-either"),
        );
        let cli = validate_args(Some(metadata.path().to_path_buf()), Some("secondary"));

        let selection = resolve_validate_selection(&global, &cli, |_| {
            panic!("a literal source URL must not read environment")
        })
        .expect("explicit Postgres source should resolve");

        expect_metadata_source(
            selection,
            metadata.path(),
            "secondary",
            "postgres://secondary",
        );
    }

    #[test]
    fn source_selection_omission_accepts_one_unambiguous_postgres_source() {
        let metadata = TestMetadataDir::new(
            r#"
- name: local-cache
  kind: sqlite
  configuration: {}
  tables: []
- name: primary
  kind: postgres
  configuration:
    connection_info:
      database_url: postgres://primary
  tables: []
"#,
        );

        let selection = resolve_validate_selection(
            &args(Some(metadata.path().to_path_buf()), None, None),
            &validate_args(None, None),
            |_| panic!("a literal source URL must not read environment"),
        )
        .expect("one Postgres source should be selected when --source is omitted");

        expect_metadata_source(selection, metadata.path(), "primary", "postgres://primary");
    }

    #[test]
    fn source_selection_omission_rejects_zero_or_multiple_postgres_sources() {
        let no_postgres = TestMetadataDir::new(
            r#"
- name: local-cache
  kind: sqlite
  configuration: {}
  tables: []
"#,
        );
        let error = resolve_validate_selection(
            &args(None, None, None),
            &validate_args(Some(no_postgres.path().to_path_buf()), None),
            |_| panic!("ambiguous source selection must fail before reading environment"),
        )
        .expect_err("zero Postgres sources must fail");
        assert!(
            error
                .to_string()
                .contains("metadata contains no Postgres source"),
            "unexpected error: {error:#}"
        );

        let two_postgres = TestMetadataDir::new(
            r#"
- name: primary
  kind: postgres
  configuration: {}
  tables: []
- name: secondary
  kind: postgres
  configuration: {}
  tables: []
"#,
        );
        let error = resolve_validate_selection(
            &args(None, None, None),
            &validate_args(Some(two_postgres.path().to_path_buf()), None),
            |_| panic!("ambiguous source selection must fail before reading environment"),
        )
        .expect_err("multiple Postgres sources must fail");
        assert!(
            error
                .to_string()
                .contains("metadata contains multiple Postgres sources"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn source_selection_rejects_unknown_or_non_postgres_explicit_source() {
        let metadata = TestMetadataDir::new(
            r#"
- name: primary
  kind: postgres
  configuration:
    connection_info:
      database_url: postgres://primary
  tables: []
- name: local-cache
  kind: sqlite
  configuration: {}
  tables: []
"#,
        );

        let error = resolve_validate_selection(
            &args(None, None, None),
            &validate_args(Some(metadata.path().to_path_buf()), Some("missing")),
            |_| panic!("unknown source must fail before reading environment"),
        )
        .expect_err("unknown source must fail");
        assert!(
            error.to_string().contains("source `missing` was not found"),
            "unexpected error: {error:#}"
        );

        let error = resolve_validate_selection(
            &args(None, None, None),
            &validate_args(Some(metadata.path().to_path_buf()), Some("local-cache")),
            |_| panic!("non-Postgres source must fail before reading environment"),
        )
        .expect_err("non-Postgres source must fail");
        assert!(
            error
                .to_string()
                .contains("source `local-cache` is not Postgres"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn source_selection_resolves_selected_source_from_env() {
        let metadata = TestMetadataDir::new(
            r#"
- name: primary
  kind: postgres
  configuration:
    connection_info:
      database_url:
        from_env: PRIMARY_DATABASE_URL
  tables: []
"#,
        );

        let selection = resolve_validate_selection(
            &args(None, Some("postgres://fallback"), None),
            &validate_args(Some(metadata.path().to_path_buf()), None),
            |name| match name {
                "PRIMARY_DATABASE_URL" => Ok("postgres://from-source-env".to_owned()),
                other => panic!("unexpected environment read: {other}"),
            },
        )
        .expect("source from_env URL should resolve");

        expect_metadata_source(
            selection,
            metadata.path(),
            "primary",
            "postgres://from-source-env",
        );
    }

    #[test]
    fn source_selection_missing_source_env_fails_closed_without_global_fallback() {
        let metadata = TestMetadataDir::new(
            r#"
- name: primary
  kind: postgres
  configuration:
    connection_info:
      database_url:
        from_env: PRIMARY_DATABASE_URL
  tables: []
"#,
        );

        let error = resolve_validate_selection(
            &args(None, Some("postgres://must-not-fallback"), None),
            &validate_args(Some(metadata.path().to_path_buf()), None),
            |name| {
                assert_eq!(name, "PRIMARY_DATABASE_URL");
                Err(std::env::VarError::NotPresent)
            },
        )
        .expect_err("a missing source environment variable must fail closed");

        assert!(
            error.to_string().contains("PRIMARY_DATABASE_URL"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn source_selection_global_url_is_fallback_only_when_source_url_is_absent() {
        let metadata = TestMetadataDir::new(
            r#"
- name: primary
  kind: postgres
  configuration: {}
  tables: []
"#,
        );
        let migrate = migrate_args(Some(metadata.path().to_path_buf()), Some("primary"));

        let selection = resolve_migrate_selection(
            &args(None, Some("postgres://fallback"), None),
            &migrate,
            |_| panic!("explicit global fallback must not read environment"),
        )
        .expect("source without a connection URL should use the documented fallback");

        match selection {
            DeploymentSelection::MetadataSource {
                metadata_dir,
                source_name,
                database_url,
                migrations_dir,
            } => {
                assert_eq!(metadata_dir, metadata.path());
                assert_eq!(source_name, "primary");
                assert_eq!(database_url, "postgres://fallback");
                assert_eq!(migrations_dir, Some(PathBuf::from("schema-migrations")));
            }
            DeploymentSelection::RefineryOnly { .. } => {
                panic!("expected metadata-aware deployment selection")
            }
        }
    }
}
