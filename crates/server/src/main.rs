//! HTTP entry point. The serving surface is data-plane only:
//! `/v1/graphql` (+ws), `/v1alpha1/graphql`, `/v1/relay`, `/v1beta1/relay`,
//! `/healthz`, `/v1/version`. There is NO runtime admin/management API
//! (no `/v1/query` run_sql, no metadata mutation): schema is applied with
//! the `migrate` subcommand, metadata is loaded from YAML at boot.
//!
//! Launch forms:
//! - serve: `donat --database-url <url> [--metadata-dir <dir>] [--port N]`
//! - migrate (DDL): `donat migrate --migrations-dir <dir>`
//! - validate (metadata vs DB): `donat validate --metadata-dir <dir>`

mod action;
mod cron;
mod events;
mod gql;
mod jwt;
mod mcp;
mod migrate;
mod remote;
mod rest;
mod state;
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
}

#[derive(clap::Args, Debug)]
struct ValidateArgs {
    /// Metadata directory to validate (defaults to --metadata-dir).
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
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

    let database_url = args
        .database_url
        .clone()
        .or_else(|| args.metadata_database_url.clone())
        .or_else(|| std::env::var("DONAT_GRAPHQL_DATABASE_URL").ok())
        .ok_or_else(|| anyhow::anyhow!("--database-url or --metadata-database-url is required"))?;

    // Deploy-time subcommands: do their job and exit (no server, no
    // request-path mutation surface).
    match &args.command {
        Some(Command::Migrate(m)) => {
            migrate::run_migrate(&database_url, &m.migrations_dir).await?;
            // Optional deploy-time DDL: reconcile per-table event-trigger
            // Postgres triggers from the YAML metadata.
            let md_dir = m.metadata_dir.clone().or_else(|| args.metadata_dir.clone());
            if let Some(dir) = md_dir {
                let metadata = donat_metadata::load_metadata_dir(&dir)?;
                events::reconcile(&database_url, &metadata).await?;
                tracing::info!(dir = %dir.display(), "event triggers reconciled");
            }
            return Ok(());
        }
        Some(Command::Validate(v)) => {
            let dir = v
                .metadata_dir
                .clone()
                .or_else(|| args.metadata_dir.clone())
                .ok_or_else(|| anyhow::anyhow!("validate needs --metadata-dir"))?;
            let problems = migrate::check_consistency(&database_url, &dir).await?;
            if problems.is_empty() {
                tracing::info!("metadata is consistent");
                return Ok(());
            }
            for p in &problems {
                tracing::error!("inconsistency: {p}");
            }
            anyhow::bail!(
                "metadata validation failed: {} inconsistency(ies)",
                problems.len()
            );
        }
        _ => {}
    }
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
        },
    };
    ensure_default_source(&mut metadata);

    if let Some(jwt) = &jwt {
        jwt.spawn_refresher(reqwest::Client::new());
    }
    let state: SharedState = Arc::new(AppState {
        engine: tokio::sync::RwLock::new(Arc::new(Engine::bootstrap(metadata))),
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

    // Liveness/version are not data APIs — always mounted.
    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/version", get(version));
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
    use super::{EnabledApis, parse_enabled_apis};

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
}
