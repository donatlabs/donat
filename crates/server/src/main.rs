//! HTTP entry point. The serving surface is data-plane only:
//! `/v1/graphql` (+ws), `/v1alpha1/graphql`, `/v1/relay`, `/v1beta1/relay`,
//! `/v1/connectors/{instance}/webhooks`, `/healthz`, `/readyz`, `/v1/version`.
//! There is NO runtime admin/management API
//! (no `/v1/query` run_sql, no metadata mutation): schema is applied with
//! the `migrate` subcommand, metadata is loaded from YAML at boot.
//!
//! Launch forms:
//! - serve: `donat --database-url <url> [--metadata-dir <dir>] [--port N]`
//! - migrate (DDL): `donat migrate --migrations-dir <dir>`
//! - validate (metadata vs DB): `donat validate --metadata-dir <dir>`
//! - inspect (read-only): `donat process inspect --source <s> --instance <id>`
//! - authorize (deploy-time OAuth2): `donat connector authorize --source <s>
//!   --instance <i>`, with `donat connector credentials list|revoke` beside it.
//!   Obtaining a provider token is a command an operator runs, never a route
//!   the engine serves — see
//!   `knowledgebase/declarative-saas/decisions/041-*`.

// The binary builds its router from the library's module tree rather than
// declaring a second copy of it. A second copy compiled the same files again
// with only the binary's reachability, so everything the integration tests
// reach through the library facade was reported dead here.
use donat_server::{
    codegen, connector_webhook, connectors, cron, events, gql, jwt, mcp, migrate, processes, rest,
    state, validate, ws,
};

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
    /// Generate Go row structs from the catalog for the embedded SDK.
    Codegen(CodegenArgs),
    /// Dump `{metadata, catalog}` JSON for the embedded wasm-core host (core_init).
    DumpCoreConfig(DumpCoreConfigArgs),
    /// Read-only diagnostics for one durable Process instance.
    #[command(subcommand)]
    Process(ProcessCommand),
    /// Deploy-time connector credential lifecycle (OAuth2).
    #[command(subcommand)]
    Connector(ConnectorCommand),
}

/// The OAuth2 credential lifecycle, and the reason it is a CLI.
///
/// A refresh token is the one credential the engine has to write rather than
/// read, and obtaining the first one needs a human at a browser. Doing that
/// over HTTP would mean an endpoint that accepts a provider `code` and stores
/// a credential — a management API, which this engine does not have and will
/// not grow. So the operator runs it here, with the same database access every
/// other deploy-time step already requires.
#[derive(clap::Subcommand, Debug)]
enum ConnectorCommand {
    /// Obtain and store the first token for one connector instance.
    Authorize(ConnectorAuthorizeArgs),
    /// Read-only credential inventory, and revocation.
    #[command(subcommand)]
    Credentials(CredentialsCommand),
}

#[derive(clap::Subcommand, Debug)]
enum CredentialsCommand {
    /// List stored credentials for a source. No secrets; exits non-zero when a
    /// configured instance has none.
    List(CredentialsListArgs),
    /// Revoke at the provider (when it declares an endpoint) and delete.
    Revoke(CredentialsRevokeArgs),
}

#[derive(clap::Args, Debug)]
struct ConnectorAuthorizeArgs {
    /// Metadata source holding this connector's credentials.
    #[arg(long)]
    source: String,
    /// The connector instance name (`name:` in metadata).
    #[arg(long)]
    instance: String,
    /// The connector module, checked against the instance's declaration.
    #[arg(long)]
    connector: Option<String>,
    /// The provider's own account identifier, when its token response does not
    /// carry one.
    #[arg(long)]
    subject: Option<String>,
    /// Capture the redirect with a one-shot listener on `127.0.0.1:<port>`
    /// instead of pasting it. Never binds a public address.
    #[arg(long)]
    listen: Option<u16>,
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct CredentialsListArgs {
    #[arg(long)]
    source: String,
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct CredentialsRevokeArgs {
    #[arg(long)]
    source: String,
    #[arg(long)]
    instance: String,
    #[arg(long)]
    connector: Option<String>,
    /// Which provider account to revoke.
    #[arg(long)]
    subject: String,
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
}

/// The two read-only diagnostics
/// [[002-durable-process-operational-contracts]] permits.
///
/// There is deliberately nothing here that cancels, retries, replays or
/// otherwise changes an instance: doing that from a CLI would be the
/// permission bypass the whole engine is built to refuse. Recovery stays an
/// explicit declared command, called as any other caller calls one.
#[derive(clap::Subcommand, Debug)]
enum ProcessCommand {
    /// Print one instance's journal — state, events, activities, transitions.
    Inspect(ProcessInstanceArgs),
    /// Check that one instance's recorded history is internally consistent.
    /// Exits non-zero when it is not.
    VerifyHistory(ProcessInstanceArgs),
}

#[derive(clap::Args, Debug)]
struct ProcessInstanceArgs {
    /// Metadata source the instance belongs to.
    #[arg(long)]
    source: String,
    /// The instance id.
    #[arg(long)]
    instance: uuid::Uuid,
    /// Metadata directory used to resolve the source's database.
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
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
struct CodegenArgs {
    /// `go` is the only target today.
    #[arg(value_parser = ["go"])]
    target: String,
    /// Metadata directory (defaults to --metadata-dir).
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
    /// Output directory for the generated file.
    #[arg(long, default_value = "gen")]
    out: PathBuf,
    /// Go package name for the generated file.
    #[arg(long, default_value = "donat_gen")]
    package: String,
}

#[derive(clap::Args, Debug)]
struct DumpCoreConfigArgs {
    /// Metadata directory (defaults to --metadata-dir).
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
    /// Output file for the `{metadata, catalog}` JSON.
    #[arg(long, default_value = "core-config.json")]
    out: PathBuf,
    /// Do not write: compare the existing file with what would be written and
    /// exit non-zero if they differ. For CI, where a snapshot that has drifted
    /// from its metadata is a defect rather than a thing to fix silently.
    #[arg(long)]
    check: bool,
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

/// Which database holds the instance's journal.
///
/// A Process journal is source-local, so the source has to be named. With a
/// metadata directory the source's own URL is used, exactly as `validate`
/// resolves it; without one the global URL is the only candidate, and it is
/// only usable when the deployment has a single source anyway.
pub(crate) fn resolve_process_source_url(
    global: &Args,
    cli: &ProcessInstanceArgs,
) -> anyhow::Result<String> {
    if let Some(metadata_dir) = cli
        .metadata_dir
        .clone()
        .or_else(|| global.metadata_dir.clone())
    {
        let selection =
            resolve_metadata_source(global, metadata_dir, Some(&cli.source), &|name| {
                std::env::var(name)
            })?;
        return Ok(selection.database_url);
    }
    resolve_global_database_url(global, &|name| std::env::var(name))?.ok_or_else(|| {
        anyhow::anyhow!(
            "reading a Process journal needs --metadata-dir (to resolve the source's database) \
             or --database-url"
        )
    })
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
    init_logging(std::env::var("DONAT_LOG_FORMAT").ok().as_deref());

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
        Some(Command::Codegen(c)) => {
            let dir = c
                .metadata_dir
                .clone()
                .or_else(|| args.metadata_dir.clone())
                .ok_or_else(|| anyhow::anyhow!("codegen needs --metadata-dir"))?;
            let database_url = resolve_global_database_url(&args, &|name| std::env::var(name))?
                .ok_or_else(|| anyhow::anyhow!("codegen needs --database-url"))?;
            codegen::run_codegen(&database_url, &dir, &c.out, &c.package).await?;
            return Ok(());
        }
        Some(Command::DumpCoreConfig(d)) => {
            let dir = d
                .metadata_dir
                .clone()
                .or_else(|| args.metadata_dir.clone())
                .ok_or_else(|| anyhow::anyhow!("dump-core-config needs --metadata-dir"))?;
            let database_url = resolve_global_database_url(&args, &|name| std::env::var(name))?
                .ok_or_else(|| anyhow::anyhow!("dump-core-config needs --database-url"))?;
            if d.check {
                codegen::check_core_config(&database_url, &dir, &d.out).await?;
            } else {
                codegen::dump_core_config(&database_url, &dir, &d.out).await?;
            }
            return Ok(());
        }
        Some(Command::Process(command)) => {
            let (instance_args, verify) = match command {
                ProcessCommand::Inspect(args) => (args, false),
                ProcessCommand::VerifyHistory(args) => (args, true),
            };
            let database_url = resolve_process_source_url(&args, instance_args)?;
            let history = processes::diagnostics::inspect(
                &database_url,
                &instance_args.source,
                instance_args.instance,
            )
            .await?;
            if !verify {
                println!("{}", serde_json::to_string_pretty(&history)?);
                return Ok(());
            }
            let findings = processes::diagnostics::verify(&history);
            if findings.is_empty() {
                println!(
                    "instance {} in source {} has a consistent history",
                    instance_args.instance, instance_args.source
                );
                return Ok(());
            }
            for finding in &findings {
                eprintln!("{}: {}", finding.code, finding.detail);
            }
            anyhow::bail!(
                "instance {} has {} history inconsistency(ies)",
                instance_args.instance,
                findings.len()
            );
        }
        Some(Command::Connector(command)) => {
            return run_connector_command(&args, command).await;
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
    // A JWT configuration the engine cannot parse stops the boot. Dropping it
    // would disable token verification without saying so, and a deployment
    // with no admin secret would then honor whatever role a caller asks for.
    let jwt = std::env::var("DONAT_GRAPHQL_JWT_SECRET")
        .ok()
        .map(|raw| jwt::JwtConfig::from_env_value(&raw))
        .transpose()
        .map_err(|e| anyhow::anyhow!("DONAT_GRAPHQL_JWT_SECRET is unusable: {e}"))?;
    warn_when_unauthenticated(admin_secret.is_some(), jwt.is_some(), auth_hook.is_some());
    let infer_function_permissions = std::env::var("DONAT_GRAPHQL_INFER_FUNCTION_PERMISSIONS")
        .map(|v| !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true);

    let mut metadata = match &args.metadata_dir {
        Some(dir) if dir.exists() => {
            let md = donat_metadata::load_metadata_dir(dir)?;
            let in_process = donat_server::action::actions_without_a_handler(&md);
            if !in_process.is_empty() {
                anyhow::bail!(
                    "actions {:?} declare no handler. A handler-less action is resolved \
                     in-process by an embedded host that registers a function for it, and \
                     this server has no such registry — give each one a `handler`, or serve \
                     the metadata from an embedded host",
                    in_process
                );
            }
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
            mcp: Default::default(),
            storage: Default::default(),
            templates: vec![],
            media: Default::default(),
            ingest_schemas: vec![],
            recurrence: Default::default(),
        },
    };
    ensure_default_source(&mut metadata);

    // Connector configuration is fully validated before this process opens a
    // listener. The immutable registry retains runtime credentials privately;
    // errors contain static metadata or variable names only, never values.
    let connectors = {
        let mut registry = connectors::ConnectorRegistry::build(&metadata)?;
        // The OAuth2 half resolves here too: the sealing key, the client
        // identity behind each declared instance, and the proof that every one
        // of them already holds a stored credential (spec 011 §7). A deployment
        // that declares `config.oauth2` and cannot use it must not serve.
        registry
            .attach_credentials(&metadata, &database_url)
            .await?;
        Arc::new(registry)
    };

    // File attachments resolve their backends and secrets here, before the
    // listener binds: a missing credential must stop the boot, not surface as
    // a failed upload later.
    let storage = Arc::new(donat_storage::StorageRegistry::build(&metadata)?);

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
        storage,
        external_base_url: std::env::var("DONAT_EXTERNAL_URL")
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string(),
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
    // Stopping happens in two phases (readiness first, then the listener), and
    // one tracker says when the background work has finished. Both are handed
    // to each loop rather than kept in `AppState`: only this function starts
    // them, and only this function waits.
    let shutdown = donat_server::shutdown::Shutdown::new();
    let workers = tokio_util::task::TaskTracker::new();
    donat_server::shutdown::on_signal(shutdown.clone(), donat_server::shutdown::readiness_delay());

    cron::spawn(state.clone(), shutdown.stopping.clone(), &workers);
    // Background delivery of table event triggers. The per-table Postgres
    // triggers that capture events are created by `migrate --metadata-dir`.
    events::spawn(state.clone(), shutdown.stopping.clone(), &workers);
    // Durable Process workers retain the exact Engine snapshot published by
    // sync_sources; polling only wakes source-local journal transactions.
    processes::spawn(state.clone(), shutdown.stopping.clone(), &workers).await?;
    // Reclaiming storage is the only file I/O the engine does outside a
    // request: a mutation stores an id, and the bytes it orphans are collected
    // here, on the deployment's own schedule.
    donat_server::files::spawn(state.clone(), shutdown.stopping.clone(), &workers);
    workers.close();
    // Liveness/readiness/version are not data APIs — always mounted.
    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", {
            let shutdown = shutdown.clone();
            get(move || readyz(shutdown.clone()))
        })
        .route("/v1/version", get(version))
        // Connector ingress is a provider-facing deployment surface, not one
        // of the optional GraphQL/REST/MCP data APIs. It remains signed and
        // declaration-bound even when all data APIs are disabled — and it does
        // durable database work, so it carries the deadline the data surfaces
        // carry rather than being the one unbounded way in.
        .merge(bounded_router(connector_webhook::router()));
    // Data APIs are mounted only when enabled (deploy-time flag); a disabled
    // surface's routes are simply absent => plain 404.
    // Every request-response surface carries a deadline; the websocket
    // upgrades deliberately do not, because a subscription is supposed to
    // outlive it. `bounded` is applied before `.get(..)` adds the upgrade, so
    // only the POST handler registered so far is wrapped — the composition
    // `request_deadline_bounds_only_registered_methods` locks that down.
    if enabled_apis.graphql {
        app = app
            .route("/v1/graphql", bounded(post(graphql)).get(ws::upgrade))
            .route(
                "/v1alpha1/graphql",
                bounded(post(graphql_legacy)).get(ws::upgrade),
            )
            .route("/v1/relay", bounded(post(relay)).get(ws::upgrade_relay))
            .route(
                "/v1beta1/relay",
                bounded(post(relay)).get(ws::upgrade_relay),
            );
    }
    if enabled_apis.rest {
        app = app.route("/api/rest/{*path}", bounded(any(rest::dispatch)));
    }
    if enabled_apis.mcp {
        app = app.route(
            "/mcp",
            bounded(
                any(mcp::method_not_allowed)
                    .post(mcp::dispatch)
                    .get(mcp::get_not_allowed)
                    .delete(mcp::delete_not_allowed),
            )
            .layer(DefaultBodyLimit::max(mcp::MCP_MAX_REQUEST_BYTES)),
        );
    }
    // File attachments are a data-plane surface: the URLs these routes answer
    // are minted by GraphQL, so they follow the GraphQL flag.
    if enabled_apis.graphql
        && let Some(files) = donat_server::files::router(&state)
    {
        app = app.merge(files);
    }
    tracing::info!(
        graphql = enabled_apis.graphql,
        rest = enabled_apis.rest,
        mcp = enabled_apis.mcp,
        "enabled API surfaces"
    );
    // Outermost, so it also catches a panic raised inside the layers above,
    // and so every log line a request produces — including the panic — carries
    // the request's own span.
    let app = app
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(
            panic_response,
        ))
        .layer(axum::middleware::from_fn(with_request_id))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown({
            let stopping = shutdown.stopping.clone();
            async move { stopping.cancelled().await }
        })
        .await?;

    // The listener is closed and every request that was in flight has been
    // answered. The background workers were told at the same moment; give them
    // the rest of the grace period to finish the item they hold, then leave
    // regardless — a drain that never ends is a deploy that never finishes.
    let grace = donat_server::shutdown::drain_grace();
    match tokio::time::timeout(grace, workers.wait()).await {
        Ok(()) => tracing::info!(target: "donat::shutdown", "drained"),
        Err(_) => tracing::warn!(
            target: "donat::shutdown",
            grace_seconds = grace.as_secs(),
            "background workers did not finish within the grace period; exiting anyway"
        ),
    }
    Ok(())
}

/// Every connector-credential command needs the same two things: the metadata
/// directory that declares the instance, and the database of the source that
/// holds its credentials. Both are resolved exactly as `validate` resolves
/// them, so a deployment names its source once and every command agrees.
async fn run_connector_command(args: &Args, command: &ConnectorCommand) -> anyhow::Result<()> {
    use donat_server::credentials::cli::{self, ConnectorTarget};

    fn selection(
        args: &Args,
        metadata_dir: Option<&PathBuf>,
        source: &str,
    ) -> anyhow::Result<MetadataSourceSelection> {
        let metadata_dir = metadata_dir
            .cloned()
            .or_else(|| args.metadata_dir.clone())
            .ok_or_else(|| anyhow::anyhow!("connector credential commands need --metadata-dir"))?;
        resolve_metadata_source(args, metadata_dir, Some(source), &|name| {
            std::env::var(name)
        })
    }

    match command {
        ConnectorCommand::Authorize(authorize) => {
            let selected = selection(args, authorize.metadata_dir.as_ref(), &authorize.source)?;
            cli::authorize(
                &selected.database_url,
                &selected.metadata_dir,
                &ConnectorTarget {
                    source: selected.source_name,
                    instance: authorize.instance.clone(),
                    connector: authorize.connector.clone(),
                },
                authorize.subject.as_deref(),
                authorize.listen,
            )
            .await
        }
        ConnectorCommand::Credentials(CredentialsCommand::List(list)) => {
            let selected = selection(args, list.metadata_dir.as_ref(), &list.source)?;
            cli::list(
                &selected.database_url,
                Some(selected.metadata_dir.as_path()),
                &selected.source_name,
            )
            .await
        }
        ConnectorCommand::Credentials(CredentialsCommand::Revoke(revoke)) => {
            let selected = selection(args, revoke.metadata_dir.as_ref(), &revoke.source)?;
            cli::revoke(
                &selected.database_url,
                &selected.metadata_dir,
                &ConnectorTarget {
                    source: selected.source_name,
                    instance: revoke.instance.clone(),
                    connector: revoke.connector.clone(),
                },
                &revoke.subject,
            )
            .await
        }
    }
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

/// Whether this deployment wants its logs structured.
///
/// [[declarative-saas/decisions/002-durable-process-operational-contracts]]
/// gives the deployment the job of observing the internal journal, which means
/// the engine owes it something a collector can read. The default stays the
/// human format, because the default reader is a terminal.
fn wants_json_logs(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("json")
    )
}

fn init_logging(format: Option<&str>) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "donat=info".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if wants_json_logs(format) {
        // Span fields — the request id among them — become object fields
        // rather than a prefix a collector would have to parse back out.
        builder.json().with_current_span(true).init();
    } else {
        builder.init();
    }
}

/// The deadline every request-response surface carries, if any.
///
/// It is a backstop, not a latency budget: it sits above the per-source
/// statement timeout so a slow query surfaces as its own GraphQL error first,
/// and only a request that is stuck somewhere else hits this.
/// `DONAT_REQUEST_TIMEOUT_SECONDS=0` removes it.
const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 60;

fn parse_request_deadline(raw: Option<&str>) -> Option<std::time::Duration> {
    let seconds = match raw {
        Some(raw) => raw.trim().parse::<u64>().ok().filter(|value| *value > 0)?,
        None => DEFAULT_REQUEST_TIMEOUT_SECONDS,
    };
    Some(std::time::Duration::from_secs(seconds))
}

fn request_deadline() -> Option<std::time::Duration> {
    parse_request_deadline(
        std::env::var("DONAT_REQUEST_TIMEOUT_SECONDS")
            .ok()
            .as_deref(),
    )
}

/// Wrap the methods registered so far in the request deadline.
///
/// Methods added to the returned router afterwards are deliberately left
/// unwrapped — that is how the websocket upgrades escape the deadline while
/// sharing a path with a bounded POST.
fn bounded<S>(router: axum::routing::MethodRouter<S>) -> axum::routing::MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    match request_deadline() {
        Some(deadline) => router.layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            deadline,
        )),
        None => router,
    }
}

/// The same deadline, for a whole router rather than one path's methods.
///
/// Used where the surface has no websocket sibling to keep out of it.
fn bounded_router<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    match request_deadline() {
        Some(deadline) => router.layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            deadline,
        )),
        None => router,
    }
}

/// Give every request a name in the log, and hand the caller's own name back.
///
/// Without this, two concurrent requests interleave their log lines with
/// nothing to tell them apart, which is exactly the situation an operator is
/// in when something goes wrong. A caller that already labels its requests
/// gets that label echoed, so a report ("request abc123 failed") can be
/// traced. We do not invent a header for callers that did not ask for one:
/// the wire contract stays what the fixtures say it is.
async fn with_request_id(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use tracing::Instrument;

    let caller_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let span = tracing::info_span!(
        "request",
        method = %request.method(),
        path = %request.uri().path(),
        request_id = caller_id.as_deref().unwrap_or("-"),
    );

    let echo = caller_id.clone();
    let mut response = next.run(request).instrument(span).await;
    if let Some(id) = echo
        && let Ok(value) = axum::http::HeaderValue::from_str(&id)
    {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

/// Turn a panicking handler into a response.
///
/// Without this the connection is dropped and the caller sees a transport
/// error it cannot distinguish from a network fault. The panic itself is
/// already logged by the default hook; the payload never reaches the caller.
fn panic_response(panic: Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    let detail = panic
        .downcast_ref::<&'static str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string());
    tracing::error!(target: "donat::panic", detail, "a request handler panicked");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "errors": [{
                "extensions": { "path": "$", "code": "unexpected" },
                "message": "internal server error",
            }]
        })),
    )
        .into_response()
}

/// The warning a deployment with no request authentication deserves, if any.
///
/// With no admin secret, no JWT configuration and no auth hook every request is
/// trusted: the caller names its own role in `X-Donat-Role` and gets exactly
/// that role's permissions. That is the documented compatible behaviour and it
/// stays the default — the fixture mode and the conformance harness rely on it
/// — but it is a deploy-time decision nobody should make by accident, so the
/// boot says so out loud.
fn unauthenticated_warning(admin_secret: bool, jwt: bool, auth_hook: bool) -> Option<&'static str> {
    (!admin_secret && !jwt && !auth_hook).then_some(
        "no request authentication is configured: no admin secret, no \
         DONAT_GRAPHQL_JWT_SECRET and no DONAT_GRAPHQL_AUTH_HOOK. Every caller \
         chooses its own role with the x-donat-role header and receives that \
         role's permissions. Serve this only on a trusted network.",
    )
}

fn warn_when_unauthenticated(admin_secret: bool, jwt: bool, auth_hook: bool) {
    if let Some(message) = unauthenticated_warning(admin_secret, jwt, auth_hook) {
        tracing::warn!(target: "donat::auth", "{message}");
    }
}

/// Liveness: is this process alive at all.
///
/// Deliberately static, and deliberately not a database check. A liveness
/// probe that fails when the database is unreachable asks the orchestrator to
/// restart a process whose restart cannot help, turning one outage into a
/// crash loop across every replica.
async fn healthz() -> &'static str {
    "OK"
}

/// Readiness: does this process still want traffic.
///
/// It answers `503` from the moment a stop signal arrives, while the listener
/// is deliberately still open. That gap is the whole point — a balancer needs
/// to be told before it is true, or it routes into a socket that is already
/// refusing.
///
/// Like liveness, it does not probe the database. Readiness that follows a
/// transient database blip removes every replica at once, which is worse than
/// the blip; a source that is genuinely gone surfaces as an ordinary error on
/// the request that needed it.
async fn readyz(shutdown: donat_server::shutdown::Shutdown) -> impl IntoResponse {
    if shutdown.is_ready() {
        (StatusCode::OK, "READY")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "DRAINING")
    }
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
        unauthenticated_warning,
    };

    static NEXT_METADATA_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    /// Structured logs are opt-in and spelled one way. A deployment that mis-
    /// spells the value gets the human format rather than silence.
    #[test]
    fn json_logs_are_requested_explicitly() {
        use super::wants_json_logs;
        assert!(wants_json_logs(Some("json")));
        assert!(wants_json_logs(Some(" JSON ")));
        assert!(!wants_json_logs(None));
        assert!(!wants_json_logs(Some("text")));
        assert!(!wants_json_logs(Some("jsonl")));
    }

    /// A deadline by default, removable on purpose, never accidentally.
    #[test]
    fn request_deadline_is_configurable_and_defaults_on() {
        use super::{DEFAULT_REQUEST_TIMEOUT_SECONDS, parse_request_deadline};
        assert_eq!(
            parse_request_deadline(None),
            Some(std::time::Duration::from_secs(
                DEFAULT_REQUEST_TIMEOUT_SECONDS
            ))
        );
        assert_eq!(
            parse_request_deadline(Some(" 5 ")),
            Some(std::time::Duration::from_secs(5))
        );
        assert_eq!(parse_request_deadline(Some("0")), None);
        // A value nobody can read as a duration falls back to no deadline
        // rather than to a guess: the operator meant to say something.
        assert_eq!(parse_request_deadline(Some("later")), None);
    }

    /// The deadline must wrap the methods registered before `bounded`, and
    /// leave anything added after it alone. That is what lets a websocket
    /// upgrade share `/v1/graphql` with a bounded POST, so it is asserted
    /// rather than assumed.
    #[tokio::test]
    async fn request_deadline_bounds_only_registered_methods() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::routing::post;
        use tower::ServiceExt;

        async fn never_answers() -> &'static str {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            "unreachable"
        }

        let app: axum::Router<()> = axum::Router::new().route(
            "/",
            super::bounded(post(never_answers).layer(
                tower_http::timeout::TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    std::time::Duration::from_millis(50),
                ),
            ))
            .get(never_answers),
        );

        let timed_out = app
            .clone()
            .oneshot(
                Request::post("/")
                    .body(Body::empty())
                    .expect("a POST request builds"),
            )
            .await
            .expect("the bounded route answers");
        assert_eq!(timed_out.status(), StatusCode::REQUEST_TIMEOUT);

        // The unbounded sibling is still running when the deadline would have
        // fired, which is exactly what a subscription needs.
        let upgrade = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            app.oneshot(
                Request::get("/")
                    .body(Body::empty())
                    .expect("a GET request builds"),
            ),
        )
        .await;
        assert!(
            upgrade.is_err(),
            "the method added after `bounded` must not inherit the deadline"
        );
    }

    /// A caller's own request label comes back; a caller that sent none gets
    /// no invented header, because the response shape is part of the contract
    /// the conformance fixtures pin down.
    #[tokio::test]
    async fn a_callers_request_id_is_echoed_and_never_invented() {
        use axum::body::Body;
        use axum::http::Request;
        use axum::routing::get;
        use tower::ServiceExt;

        let app: axum::Router<()> = axum::Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(super::with_request_id));

        let labelled = app
            .clone()
            .oneshot(
                Request::get("/")
                    .header("x-request-id", "abc123")
                    .body(Body::empty())
                    .expect("a labelled request builds"),
            )
            .await
            .expect("the request is answered");
        assert_eq!(
            labelled
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("abc123")
        );

        let unlabelled = app
            .oneshot(
                Request::get("/")
                    .body(Body::empty())
                    .expect("an unlabelled request builds"),
            )
            .await
            .expect("the request is answered");
        assert!(
            unlabelled.headers().get("x-request-id").is_none(),
            "a header the caller did not send must not appear in the response"
        );
    }

    /// A panicking handler must become a response, not a dropped connection.
    #[tokio::test]
    async fn a_panicking_handler_answers_with_the_donat_error_shape() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::routing::get;
        use tower::ServiceExt;

        async fn panics() -> &'static str {
            panic!("handler exploded");
        }

        let app: axum::Router<()> = axum::Router::new().route("/", get(panics)).layer(
            tower_http::catch_panic::CatchPanicLayer::custom(super::panic_response),
        );

        let response = app
            .oneshot(
                Request::get("/")
                    .body(Body::empty())
                    .expect("a GET request builds"),
            )
            .await
            .expect("the panic became a response");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("the error body is readable");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("the error body is JSON");
        assert_eq!(body["errors"][0]["extensions"]["code"], "unexpected");
        // The panic message is for the log, never for the caller.
        assert_eq!(body["errors"][0]["message"], "internal server error");
        assert!(!body.to_string().contains("handler exploded"));
    }

    /// Only a deployment where nothing authenticates the caller is warned
    /// about; any one of the three mechanisms is enough to silence it.
    #[test]
    fn only_a_wholly_unauthenticated_deployment_is_warned() {
        let warning = unauthenticated_warning(false, false, false)
            .expect("a deployment with no authentication is warned");
        assert!(warning.contains("x-donat-role"));
        for (admin_secret, jwt, auth_hook) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
            (true, true, true),
        ] {
            assert!(
                unauthenticated_warning(admin_secret, jwt, auth_hook).is_none(),
                "admin_secret={admin_secret} jwt={jwt} auth_hook={auth_hook} must not warn"
            );
        }
    }

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
