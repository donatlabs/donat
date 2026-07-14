//! Shared server state: per-source connection pools and the engine
//! snapshot (metadata + per-source catalogs) that metadata operations
//! mutate at runtime.

use std::collections::HashMap;
use std::sync::Arc;

use donat_backend::{AnyDialect, ClickhouseDialect, Dialect, MySqlDialect, SqliteDialect};
use donat_catalog::Catalog;
use donat_ir::RootField;
use donat_metadata::{DatabaseUrl, Metadata, Source, SourceKind};
use donat_schema::{CompiledMultiSourceSchema, PlanError};
use serde_json::Value as Json;
use tokio::sync::RwLock;

const CLICKHOUSE_MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
const CLICKHOUSE_MAX_DATA_BYTES: usize = 64 * 1024 * 1024;

pub struct AppState {
    pub engine: RwLock<EngineSnapshot>,
    /// The fallback/default database (also the metadata database in
    /// --hge-bin mode).
    pub default_url: String,
    pub admin_secret: Option<String>,
    /// DONAT_GRAPHQL_UNAUTHORIZED_ROLE: role for requests without one.
    pub unauthorized_role: Option<String>,
    /// --stringify-numeric-types
    pub stringify_numerics: bool,
    /// DONAT_GRAPHQL_INFER_FUNCTION_PERMISSIONS (default true).
    pub infer_function_permissions: bool,
    /// JWT authentication mode, when DONAT_GRAPHQL_JWT_SECRET is set.
    pub jwt: Option<crate::jwt::JwtConfig>,
    /// Webhook authentication mode: (url, "GET"|"POST").
    pub auth_hook: Option<(String, String)>,
    pub http: reqwest::Client,
    /// DONAT_GRAPHQL_ENABLE_ALLOWLIST: non-listed queries are rejected.
    pub allowlist_enabled: bool,
}

pub type SharedState = Arc<AppState>;
pub type EngineSnapshot = Arc<Engine>;

/// Failure of a backend read in `execute_query_json`. The caller maps each
/// variant to the existing GraphQL error body so the Postgres path keeps
/// byte-for-byte identical error shaping (`Postgres` carries the real
/// `tokio_postgres::Error` for `db_error_json`).
#[derive(Debug)]
pub enum QueryError {
    NoDefaultSource,
    Pool(String),
    Decode(String),
    Postgres(tokio_postgres::Error),
    Sqlite(String),
    Clickhouse(String),
}

/// Failure of a SQLite mutation in [`AppState::execute_sqlite_mutations`].
/// The caller maps each variant to the GraphQL error body; `CheckViolation`
/// reproduces the same `permission-error` shape the Postgres path emits.
pub enum SqliteMutationError {
    /// A row failed its insert/update permission check; the transaction was
    /// rolled back. `path` is the GraphQL error path for the body.
    CheckViolation {
        path: String,
    },
    Sqlite(String),
    Other(String),
}

/// Failure of a MySQL mutation in [`AppState::execute_mysql_mutations`]. Like
/// the SQLite variant, the caller maps each variant to the GraphQL error body;
/// `CheckViolation` reproduces the same `permission-error` shape Postgres emits.
pub enum MysqlMutationError {
    /// A row failed its insert/update permission check; the transaction was
    /// rolled back. `path` is the GraphQL error path for the body.
    CheckViolation {
        path: String,
    },
    /// A MySQL driver / SQL error (mapped to `data-exception`).
    Mysql(String),
    Other(String),
}

#[derive(Clone)]
pub enum SourceRuntime {
    Postgres {
        url: String,
        pool: deadpool_postgres::Pool,
    },
    Sqlite {
        path: String,
    },
    Mysql {
        url: String,
    },
    Clickhouse {
        url: String,
    },
}

impl SourceRuntime {
    fn kind(&self) -> SourceKind {
        match self {
            Self::Postgres { .. } => SourceKind::Postgres,
            Self::Sqlite { .. } => SourceKind::Sqlite,
            Self::Mysql { .. } => SourceKind::Mysql,
            Self::Clickhouse { .. } => SourceKind::Clickhouse,
        }
    }
}

pub struct Engine {
    pub metadata: Metadata,
    /// Catalog snapshot per source name.
    pub catalogs: HashMap<String, Catalog>,
    pub compiled: Option<Arc<CompiledMultiSourceSchema>>,
    pub runtimes: HashMap<String, SourceRuntime>,
}

impl Engine {
    pub fn bootstrap(metadata: Metadata) -> Self {
        Self {
            metadata,
            catalogs: HashMap::new(),
            compiled: None,
            runtimes: HashMap::new(),
        }
    }

    pub fn compiled(
        mut metadata: Metadata,
        catalogs: HashMap<String, Catalog>,
        runtimes: HashMap<String, SourceRuntime>,
        infer_function_permissions: bool,
    ) -> Result<Self, PlanError> {
        normalize_metadata_sources(&mut metadata);
        for source in &metadata.sources {
            let runtime = runtimes.get(&source.name).ok_or_else(|| {
                PlanError::new(
                    "$",
                    "not-found",
                    format!("runtime for source '{}' not found", source.name),
                )
            })?;
            if runtime.kind() != source.kind {
                return Err(PlanError::new(
                    "$",
                    "unexpected",
                    format!(
                        "runtime for source '{}' is {:?}, metadata requires {:?}",
                        source.name,
                        runtime.kind(),
                        source.kind
                    ),
                ));
            }
        }
        let compiled = Arc::new(CompiledMultiSourceSchema::compile(
            &metadata,
            &catalogs,
            infer_function_permissions,
        )?);
        Ok(Self {
            metadata,
            catalogs,
            compiled: Some(compiled),
            runtimes,
        })
    }

    /// The catalog the GraphQL schema is built against: the "default"
    /// source, or the first one.
    pub fn default_catalog(&self) -> Catalog {
        self.catalogs
            .get("default")
            .or_else(|| {
                self.metadata
                    .sources
                    .first()
                    .and_then(|s| self.catalogs.get(&s.name))
            })
            .cloned()
            .unwrap_or_default()
    }
}

pub fn make_pool(url: &str) -> anyhow::Result<deadpool_postgres::Pool> {
    let mut config = deadpool_postgres::Config::new();
    config.url = Some(url.to_string());
    Ok(config.create_pool(
        Some(deadpool_postgres::Runtime::Tokio1),
        tokio_postgres::NoTls,
    )?)
}

fn stage_postgres_runtime(
    url: &str,
    existing: Option<&SourceRuntime>,
) -> anyhow::Result<SourceRuntime> {
    if let Some(SourceRuntime::Postgres {
        url: existing_url,
        pool,
    }) = existing
        && existing_url == url
    {
        return Ok(SourceRuntime::Postgres {
            url: url.to_string(),
            pool: pool.clone(),
        });
    }
    Ok(SourceRuntime::Postgres {
        url: url.to_string(),
        pool: make_pool(url)?,
    })
}

fn normalize_metadata_sources(metadata: &mut Metadata) {
    let mut indexes = HashMap::<String, usize>::new();
    let mut normalized = Vec::with_capacity(metadata.sources.len());
    for source in std::mem::take(&mut metadata.sources) {
        if let Some(index) = indexes.get(&source.name).copied() {
            normalized[index] = source;
        } else {
            indexes.insert(source.name.clone(), normalized.len());
            normalized.push(source);
        }
    }
    metadata.sources = normalized;
}

fn resolve_source_url(source: &Source, default_url: &str) -> String {
    if let Some(connection_info) = &source.configuration.connection_info {
        return match &connection_info.database_url {
            DatabaseUrl::Url(url) => url.clone(),
            DatabaseUrl::FromEnv { from_env } => {
                std::env::var(from_env).unwrap_or_else(|_| default_url.to_string())
            }
        };
    }
    if source.kind == SourceKind::Clickhouse {
        if let Some(url) = resolve_hasura_clickhouse_template(source) {
            return url;
        }
    }
    default_url.to_string()
}

#[derive(serde::Deserialize)]
struct HasuraClickhouseConfiguration {
    url: String,
    username: Option<String>,
    password: Option<String>,
}

fn resolve_hasura_clickhouse_template(source: &Source) -> Option<String> {
    let template = source.configuration.extra.get("template")?.as_str()?;
    let rendered = render_hasura_environment_template(template)?;
    let configuration: HasuraClickhouseConfiguration = serde_json::from_value(rendered).ok()?;
    let mut url = reqwest::Url::parse(&configuration.url).ok()?;
    if let Some(username) = configuration.username {
        url.set_username(&username).ok()?;
    }
    if let Some(password) = configuration.password {
        url.set_password(Some(&password)).ok()?;
    }
    Some(url.to_string())
}

fn render_hasura_environment_template(template: &str) -> Option<serde_json::Value> {
    let mut rendered = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let expression_start = start + 2;
        let expression_end = remaining[expression_start..].find("}}")? + expression_start;
        let expression = remaining[expression_start..expression_end].trim();
        let argument = expression
            .strip_prefix("getEnvironmentVariable(")?
            .strip_suffix(')')?
            .trim();
        let variable: String = serde_json::from_str(argument).ok()?;
        let value = std::env::var(variable).ok()?;
        rendered.push_str(&serde_json::to_string(&value).ok()?);
        remaining = &remaining[expression_end + 2..];
    }
    rendered.push_str(remaining);
    serde_json::from_str(&rendered).ok()
}

impl AppState {
    pub async fn engine_snapshot(&self) -> EngineSnapshot {
        self.engine.read().await.clone()
    }

    async fn publish_candidate(
        &self,
        candidate: Result<Engine, PlanError>,
    ) -> Result<(), PlanError> {
        let candidate = Arc::new(candidate?);
        *self.engine.write().await = candidate;
        Ok(())
    }

    async fn default_source_name(&self) -> Option<String> {
        let engine = self.engine_snapshot().await;
        engine
            .metadata
            .sources
            .iter()
            .find(|source| source.name == "default")
            .or_else(|| engine.metadata.sources.first())
            .map(|source| source.name.clone())
    }

    pub async fn default_pool(&self) -> Option<deadpool_postgres::Pool> {
        let source = self.default_source_name().await?;
        self.source_pool(&source).await
    }

    pub async fn source_pool(&self, source_name: &str) -> Option<deadpool_postgres::Pool> {
        match self.source_runtime(source_name).await {
            Some(SourceRuntime::Postgres { pool, .. }) => Some(pool),
            _ => None,
        }
    }

    async fn source_runtime(&self, source_name: &str) -> Option<SourceRuntime> {
        self.engine_snapshot()
            .await
            .runtimes
            .get(source_name)
            .cloned()
    }

    pub async fn execute_source_query_json(
        &self,
        source_name: &str,
        roots: &[RootField],
    ) -> Result<Json, QueryError> {
        let runtime = self
            .source_runtime(source_name)
            .await
            .ok_or(QueryError::NoDefaultSource)?;
        self.execute_runtime_query_json(runtime, roots).await
    }

    pub(crate) async fn execute_runtime_query_json(
        &self,
        runtime: SourceRuntime,
        roots: &[RootField],
    ) -> Result<Json, QueryError> {
        match runtime {
            SourceRuntime::Postgres { pool, .. } => {
                let sql = donat_sqlgen::operation_to_sql_opts(roots, self.stringify_numerics);
                let client = pool
                    .get()
                    .await
                    .map_err(|e| QueryError::Pool(e.to_string()))?;
                let row = client
                    .query_one(&sql, &[])
                    .await
                    .map_err(QueryError::Postgres)?;
                row.try_get::<_, Json>(0)
                    .map_err(|e| QueryError::Decode(e.to_string()))
            }
            SourceRuntime::Sqlite { path } => {
                let sql = donat_sqlgen::operation_to_sql_opts_with(
                    roots,
                    self.stringify_numerics,
                    AnyDialect::Sqlite(SqliteDialect),
                );
                let text = tokio::task::spawn_blocking(move || -> Result<String, QueryError> {
                    let conn = rusqlite::Connection::open(&path)
                        .map_err(|e| QueryError::Sqlite(e.to_string()))?;
                    conn.query_row(&sql, [], |r| r.get::<_, String>(0))
                        .map_err(|e| QueryError::Sqlite(e.to_string()))
                })
                .await
                .map_err(|e| QueryError::Pool(format!("sqlite task panicked: {e}")))??;
                serde_json::from_str(&text).map_err(|e| QueryError::Decode(e.to_string()))
            }
            SourceRuntime::Mysql { url } => {
                use mysql::prelude::Queryable;

                let sql = donat_sqlgen::operation_to_sql_opts_with(
                    roots,
                    self.stringify_numerics,
                    AnyDialect::Mysql(MySqlDialect),
                );
                let text = tokio::task::spawn_blocking(move || -> Result<String, QueryError> {
                    let mut conn = mysql::Conn::new(url.as_str())
                        .map_err(|e| QueryError::Sqlite(e.to_string()))?;
                    // The engine emits Postgres-style `"ident"` quoting; MySQL
                    // reads double quotes as string literals unless ANSI_QUOTES
                    // is enabled for the session.
                    conn.query_drop("SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')")
                        .map_err(|e| QueryError::Sqlite(e.to_string()))?;
                    conn.query_drop("SET SESSION group_concat_max_len = 4294967295")
                        .map_err(|e| QueryError::Sqlite(e.to_string()))?;
                    let row: Option<String> = conn
                        .query_first(&sql)
                        .map_err(|e| QueryError::Sqlite(e.to_string()))?;
                    row.ok_or_else(|| QueryError::Sqlite("mysql returned no rows".to_string()))
                })
                .await
                .map_err(|e| QueryError::Pool(format!("mysql task panicked: {e}")))??;
                serde_json::from_str(&text).map_err(|e| QueryError::Decode(e.to_string()))
            }
            SourceRuntime::Clickhouse { url } => {
                let sql = donat_sqlgen::operation_to_sql_opts_with(
                    roots,
                    self.stringify_numerics,
                    AnyDialect::Clickhouse(ClickhouseDialect),
                );
                let text = clickhouse_post_data(
                    &self.http,
                    &url,
                    &format!("{sql} FORMAT TabSeparatedRaw"),
                )
                .await
                .map_err(QueryError::Clickhouse)?;
                serde_json::from_str(text.trim()).map_err(|e| QueryError::Decode(e.to_string()))
            }
        }
    }

    /// Execute a planned mutation against the default SQLite source.
    ///
    /// SQLite forbids DML in a CTE/subquery, so the Postgres "one statement
    /// assembles the whole response" shape is impossible (see ADR 003). Each
    /// root is one top-level DML with `RETURNING <node> AS node, <flag> AS
    /// violated`; this runs them in a transaction, folds the rows into the
    /// response in Rust, and ROLLs BACK if any row violated its permission
    /// check — returning [`SqliteMutationError::CheckViolation`] so the caller
    /// can render the exact permission-error body. The check is computed in
    /// the same DML and the rollback is in the same transaction, so the
    /// permission is still enforced atomically (no bypass).
    pub(crate) async fn execute_sqlite_mutations_at(
        &self,
        path: String,
        roots: &[donat_ir::MutationRoot],
    ) -> Result<Json, SqliteMutationError> {
        use donat_sqlgen::{MutationResponseSlot, SqliteMutationPlan};

        // Plan every root up front (alias + SQLite mutation plan), preserving
        // selection order for the response map.
        let planned: Vec<(String, SqliteMutationPlan)> = roots
            .iter()
            .map(|m| {
                let alias = match m {
                    donat_ir::MutationRoot::FunctionCall { alias, .. }
                    | donat_ir::MutationRoot::Insert { alias, .. }
                    | donat_ir::MutationRoot::Update { alias, .. }
                    | donat_ir::MutationRoot::Delete { alias, .. }
                    | donat_ir::MutationRoot::Typename { alias, .. } => alias.clone(),
                };
                (alias, donat_sqlgen::sqlite_mutation_plan(m))
            })
            .collect();

        tokio::task::spawn_blocking(move || -> Result<Json, SqliteMutationError> {
            let mut conn = rusqlite::Connection::open(&path)
                .map_err(|e| SqliteMutationError::Sqlite(e.to_string()))?;
            let tx = conn
                .transaction()
                .map_err(|e| SqliteMutationError::Sqlite(e.to_string()))?;

            let mut data = serde_json::Map::new();
            for (alias, plan) in &planned {
                // A `__typename`-only mutation root has no DML.
                if let Some((_, value)) = &plan.root_typename {
                    data.insert(alias.clone(), Json::String(value.clone()));
                    continue;
                }

                let mut returning: Vec<Json> = vec![];
                let mut affected_rows: i64 = 0;
                let mut violated = false;
                {
                    let mut stmt = tx
                        .prepare(&plan.dml_sql)
                        .map_err(|e| SqliteMutationError::Sqlite(e.to_string()))?;
                    let mut rows = stmt
                        .query([])
                        .map_err(|e| SqliteMutationError::Sqlite(e.to_string()))?;
                    while let Some(row) = rows
                        .next()
                        .map_err(|e| SqliteMutationError::Sqlite(e.to_string()))?
                    {
                        affected_rows += 1;
                        let node_text: String = row
                            .get("node")
                            .map_err(|e| SqliteMutationError::Sqlite(e.to_string()))?;
                        let flag: i64 = row
                            .get("violated")
                            .map_err(|e| SqliteMutationError::Sqlite(e.to_string()))?;
                        if flag != 0 {
                            violated = true;
                        }
                        let node: Json = serde_json::from_str(&node_text)
                            .map_err(|e| SqliteMutationError::Other(e.to_string()))?;
                        returning.push(node);
                    }
                }

                if violated {
                    // Drop without commit rolls the whole transaction back.
                    let _ = tx.rollback();
                    return Err(SqliteMutationError::CheckViolation {
                        path: plan.check_path.clone(),
                    });
                }

                if plan.single_row_output {
                    data.insert(
                        alias.clone(),
                        returning.into_iter().next().unwrap_or(Json::Null),
                    );
                    continue;
                }

                // Assemble this root's response object in GraphQL selection order.
                let mut obj = serde_json::Map::new();
                let returning_value = Json::Array(returning);
                for slot in &plan.response_slots {
                    match slot {
                        MutationResponseSlot::Returning { alias } => {
                            obj.insert(alias.clone(), returning_value.clone());
                        }
                        MutationResponseSlot::AffectedRows { alias } => {
                            obj.insert(alias.clone(), Json::from(affected_rows));
                        }
                        MutationResponseSlot::Typename { alias, value } => {
                            obj.insert(alias.clone(), Json::String(value.clone()));
                        }
                    }
                }
                data.insert(alias.clone(), Json::Object(obj));
            }

            tx.commit()
                .map_err(|e| SqliteMutationError::Sqlite(e.to_string()))?;
            Ok(Json::Object(data))
        })
        .await
        .map_err(|e| SqliteMutationError::Other(format!("sqlite task panicked: {e}")))?
    }

    /// Execute a planned mutation against the default MySQL source.
    ///
    /// MySQL has no `RETURNING` and read-only CTEs, so the `returning` set is
    /// recovered with a COMPANION SELECT in the same transaction (ADR 004): for
    /// insert/update the SELECT runs AFTER the DML; for delete it runs BEFORE.
    /// `affected_rows` is the DML's row count. The companion SELECT also emits a
    /// `violated` flag (1 when a row fails its insert/update check); any set flag
    /// rolls the whole transaction back and yields
    /// [`MysqlMutationError::CheckViolation`] — the same atomic enforcement the
    /// Postgres/SQLite paths give.
    pub(crate) async fn execute_mysql_mutations_at(
        &self,
        catalog: Catalog,
        url: String,
        roots: &[donat_ir::MutationRoot],
    ) -> Result<Json, MysqlMutationError> {
        use donat_sqlgen::{MutationResponseSlot, MySqlMutationKind, MySqlMutationPlan};
        use mysql::prelude::Queryable;

        // Plan every root up front (alias + MySQL mutation plan), resolving each
        // table's primary key from the catalog (the IR mutation does not carry
        // it, but the insert companion SELECT needs it for last_insert_id()
        // recovery / the supplied-PK predicate). Preserve selection order.
        let pk_of = |t: &donat_ir::Table| -> Vec<String> {
            catalog
                .tables
                .get(&format!("{}.{}", t.schema, t.name))
                .map(|info| info.primary_key.clone())
                .unwrap_or_default()
        };
        let planned: Vec<(String, MySqlMutationPlan)> = roots
            .iter()
            .map(|m| {
                let (alias, pk) = match m {
                    donat_ir::MutationRoot::Insert { alias, insert } => {
                        (alias.clone(), pk_of(&insert.table))
                    }
                    donat_ir::MutationRoot::Update { alias, update } => {
                        (alias.clone(), pk_of(&update.table))
                    }
                    donat_ir::MutationRoot::Delete { alias, delete } => {
                        (alias.clone(), pk_of(&delete.table))
                    }
                    donat_ir::MutationRoot::FunctionCall { alias, .. }
                    | donat_ir::MutationRoot::Typename { alias, .. } => (alias.clone(), vec![]),
                };
                (alias, donat_sqlgen::mysql_mutation_plan(m, &pk))
            })
            .collect();

        tokio::task::spawn_blocking(move || -> Result<Json, MysqlMutationError> {
            let mut conn = mysql::Conn::new(url.as_str())
                .map_err(|e| MysqlMutationError::Mysql(e.to_string()))?;
            // The engine emits Postgres-style `"ident"` quoting in the few places
            // that bypass the dialect; MySQL needs ANSI_QUOTES to read those.
            // The mutation path itself renders backtick identifiers, but stay
            // consistent with the read path's session setup.
            conn.query_drop("SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')")
                .map_err(|e| MysqlMutationError::Mysql(e.to_string()))?;
            conn.query_drop("SET SESSION group_concat_max_len = 4294967295")
                .map_err(|e| MysqlMutationError::Mysql(e.to_string()))?;
            let mut tx = conn
                .start_transaction(mysql::TxOpts::default())
                .map_err(|e| MysqlMutationError::Mysql(e.to_string()))?;

            let mut data = serde_json::Map::new();
            for (alias, plan) in &planned {
                // A `__typename`-only mutation root has no DML.
                if let Some((_, value)) = &plan.root_typename {
                    data.insert(alias.clone(), Json::String(value.clone()));
                    continue;
                }

                // Build the companion-SELECT WHERE + ordering of DML vs SELECT
                // from the recovery strategy.
                let (companion_sql, affected_rows): (Option<String>, i64) = match &plan.kind {
                    MySqlMutationKind::Insert {
                        pk_col,
                        pk_in_predicate,
                    } => {
                        // INSERT first, then recover the new rows.
                        tx.query_drop(&plan.dml_sql)
                            .map_err(|e| MysqlMutationError::Mysql(e.to_string()))?;
                        let affected = tx.affected_rows() as i64;
                        let where_clause = match pk_in_predicate {
                            // Supplied PK: restrict by the exact values.
                            Some(pred) => pred.clone(),
                            None => {
                                // last_insert_id() recovery: requires a single
                                // AUTO_INCREMENT PK. The N rows occupy
                                // [last_id, last_id + affected - 1].
                                let col = pk_col.as_ref().ok_or_else(|| {
                                    MysqlMutationError::Other(
                                        "mysql insert returning needs a single \
                                         auto-increment primary key or supplied pk values"
                                            .to_string(),
                                    )
                                })?;
                                let last = tx.last_insert_id().unwrap_or(0) as i64;
                                if affected <= 0 {
                                    // Nothing inserted: an always-false restriction.
                                    format!("{col} IS NULL AND {col} IS NOT NULL")
                                } else {
                                    let hi = last + affected - 1;
                                    format!("{col} BETWEEN {last} AND {hi}")
                                }
                            }
                        };
                        (
                            Some(format!("{} WHERE {where_clause}", plan.companion_select)),
                            affected,
                        )
                    }
                    MySqlMutationKind::Update { where_clause } => {
                        // UPDATE first, then re-select by the same predicate.
                        tx.query_drop(&plan.dml_sql)
                            .map_err(|e| MysqlMutationError::Mysql(e.to_string()))?;
                        let affected = tx.affected_rows() as i64;
                        let sql = match where_clause {
                            Some(w) => format!("{} WHERE {w}", plan.companion_select),
                            None => plan.companion_select.clone(),
                        };
                        (Some(sql), affected)
                    }
                    MySqlMutationKind::Delete { where_clause } => {
                        // SELECT first (capture returning), then DELETE.
                        let sql = match where_clause {
                            Some(w) => format!("{} WHERE {w}", plan.companion_select),
                            None => plan.companion_select.clone(),
                        };
                        (Some(sql), 0) // affected filled after the DELETE below.
                    }
                    MySqlMutationKind::Typename => (None, 0),
                };

                // Run the companion SELECT, folding node rows + violated flags.
                let mut returning: Vec<Json> = vec![];
                let mut violated = false;
                let mut captured_rows: i64 = 0;
                if let Some(sql) = &companion_sql {
                    let rows: Vec<(String, i64)> = tx
                        .query(sql)
                        .map_err(|e| MysqlMutationError::Mysql(e.to_string()))?;
                    for (node_text, flag) in rows {
                        captured_rows += 1;
                        if flag != 0 {
                            violated = true;
                        }
                        let node: Json = serde_json::from_str(&node_text)
                            .map_err(|e| MysqlMutationError::Other(e.to_string()))?;
                        returning.push(node);
                    }
                }

                // For delete, the DML runs AFTER the capturing SELECT.
                let affected_rows = if let MySqlMutationKind::Delete { .. } = &plan.kind {
                    tx.query_drop(&plan.dml_sql)
                        .map_err(|e| MysqlMutationError::Mysql(e.to_string()))?;
                    tx.affected_rows() as i64
                } else {
                    affected_rows
                };
                let _ = captured_rows;

                if violated {
                    let _ = tx.rollback();
                    return Err(MysqlMutationError::CheckViolation {
                        path: plan.check_path.clone(),
                    });
                }

                if plan.single_row_output {
                    data.insert(
                        alias.clone(),
                        returning.into_iter().next().unwrap_or(Json::Null),
                    );
                    continue;
                }

                // Assemble this root's response object in GraphQL selection order.
                let mut obj = serde_json::Map::new();
                let returning_value = Json::Array(returning);
                for slot in &plan.response_slots {
                    match slot {
                        MutationResponseSlot::Returning { alias } => {
                            obj.insert(alias.clone(), returning_value.clone());
                        }
                        MutationResponseSlot::AffectedRows { alias } => {
                            obj.insert(alias.clone(), Json::from(affected_rows));
                        }
                        MutationResponseSlot::Typename { alias, value } => {
                            obj.insert(alias.clone(), Json::String(value.clone()));
                        }
                    }
                }
                data.insert(alias.clone(), Json::Object(obj));
            }

            tx.commit()
                .map_err(|e| MysqlMutationError::Mysql(e.to_string()))?;
            Ok(Json::Object(data))
        })
        .await
        .map_err(|e| MysqlMutationError::Other(format!("mysql task panicked: {e}")))?
    }

    /// Reconcile pools and catalogs with the current metadata sources,
    /// pruning metadata that refers to dropped objects (run_sql untracks
    /// dropped tables/functions, like Donat).
    pub async fn sync_sources(&self) -> anyhow::Result<()> {
        let metadata = self.engine_snapshot().await.metadata.clone();
        self.sync_candidate(metadata).await
    }

    async fn sync_candidate(&self, mut metadata: Metadata) -> anyhow::Result<()> {
        normalize_metadata_sources(&mut metadata);
        let existing_runtimes = self.engine_snapshot().await.runtimes.clone();
        let sources: Vec<(String, SourceKind, String, Vec<String>)> = metadata
            .sources
            .iter()
            .map(|source| {
                let url = resolve_source_url(source, &self.default_url);
                let mut tracked_databases = Vec::new();
                for table in &source.tables {
                    let schema = table.table.schema().to_string();
                    if !tracked_databases.contains(&schema) {
                        tracked_databases.push(schema);
                    }
                }
                (source.name.clone(), source.kind, url, tracked_databases)
            })
            .collect();

        let mut new_catalogs = HashMap::new();
        let mut new_runtimes = HashMap::new();
        for (name, kind, url, tracked_databases) in &sources {
            let (catalog, runtime) = match kind {
                SourceKind::Postgres => {
                    let runtime = stage_postgres_runtime(url, existing_runtimes.get(name))?;
                    let SourceRuntime::Postgres { pool, .. } = &runtime else {
                        unreachable!("PostgreSQL staging returned a non-PostgreSQL runtime")
                    };
                    let client = pool.get().await?;
                    ensure_check_violation_helper(&client).await?;
                    (donat_catalog::introspect(&client).await?, runtime)
                }
                SourceKind::Sqlite => {
                    // SQLite uses no pool and no PL/pgSQL helper: introspect
                    // once at boot via a blocking connection, then remember
                    // the path for per-query connections in
                    // `execute_query_json`.
                    let path = url.clone();
                    let catalog =
                        tokio::task::spawn_blocking(move || -> anyhow::Result<Catalog> {
                            let conn = rusqlite::Connection::open(&path)?;
                            Ok(donat_catalog::sqlite_introspect(&conn)?)
                        })
                        .await??;
                    (catalog, SourceRuntime::Sqlite { path: url.clone() })
                }
                SourceKind::Mysql => {
                    // MySQL uses no pool and no PL/pgSQL helper: introspect once
                    // at boot via a blocking connection, then remember the url
                    // for per-query connections in `execute_query_json`. The
                    // tracked schema is the database name from the url.
                    let conn_url = url.clone();
                    let catalog =
                        tokio::task::spawn_blocking(move || -> anyhow::Result<Catalog> {
                            let opts = mysql::Opts::from_url(&conn_url)?;
                            let db =
                                opts.get_db_name().map(|s| s.to_string()).ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "mysql source url has no database name: {conn_url}"
                                    )
                                })?;
                            let mut conn = mysql::Conn::new(opts)?;
                            Ok(donat_catalog::mysql_introspect(&mut conn, &db)?)
                        })
                        .await??;
                    (catalog, SourceRuntime::Mysql { url: url.clone() })
                }
                SourceKind::Clickhouse => {
                    let fallback_database = clickhouse_database(url)?;
                    let databases = if tracked_databases.is_empty() {
                        vec![fallback_database.clone()]
                    } else {
                        tracked_databases.clone()
                    };
                    let sql = "SELECT database, table, name, type, default_kind, is_in_primary_key \
                               FROM system.columns \
                               WHERE database IN {databases:Array(String)} \
                               ORDER BY database, table, position \
                               FORMAT JSONEachRow";
                    let text =
                        clickhouse_post_with_databases_param(&self.http, url, sql, &databases)
                            .await
                            .map_err(anyhow::Error::msg)?;
                    (
                        donat_catalog::clickhouse_catalog_from_json_each_row(
                            &text,
                            &fallback_database,
                        )?,
                        SourceRuntime::Clickhouse { url: url.clone() },
                    )
                }
            };
            new_catalogs.insert(name.clone(), catalog);
            new_runtimes.insert(name.clone(), runtime);
        }

        for source in &mut metadata.sources {
            let Some(catalog) = new_catalogs.get(&source.name) else {
                continue;
            };
            source
                .tables
                .retain(|t| catalog.table(t.table.schema(), t.table.name()).is_some());
            source.functions.retain(|f| {
                catalog
                    .function(f.function.schema(), f.function.name())
                    .is_some()
            });
            for table in &mut source.tables {
                table.computed_fields.retain(|cf| {
                    catalog
                        .function(
                            cf.definition.function.schema(),
                            cf.definition.function.name(),
                        )
                        .is_some()
                });
            }
        }
        let candidate = Engine::compiled(
            metadata,
            new_catalogs,
            new_runtimes,
            self.infer_function_permissions,
        )?;
        self.publish_candidate(Ok(candidate)).await?;
        Ok(())
    }
}

fn clickhouse_database(url: &str) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(url)?;
    Ok(url
        .query_pairs()
        .find(|(key, _)| key == "database")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| "default".to_string()))
}

async fn clickhouse_post(
    client: &reqwest::Client,
    url: &str,
    sql: &str,
    max_bytes: usize,
) -> Result<String, String> {
    use futures_util::StreamExt;

    let response = client
        .post(url)
        .body(sql.to_string())
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        append_clickhouse_chunk(&mut body, &chunk, max_bytes)?;
    }
    let body = String::from_utf8(body)
        .map_err(|error| format!("ClickHouse returned non-UTF-8 data: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "ClickHouse returned {status}: {}",
            body.chars().take(4096).collect::<String>()
        ));
    }
    Ok(body)
}

fn append_clickhouse_chunk(
    body: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
) -> Result<(), String> {
    if body.len().saturating_add(chunk.len()) > max_bytes {
        return Err(format!("ClickHouse response exceeds {max_bytes} bytes"));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

async fn clickhouse_post_with_databases_param(
    client: &reqwest::Client,
    url: &str,
    sql: &str,
    databases: &[String],
) -> Result<String, String> {
    let mut url = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    let databases = format!(
        "[{}]",
        databases
            .iter()
            .map(|database| ClickhouseDialect.quote_literal(database))
            .collect::<Vec<_>>()
            .join(",")
    );
    url.query_pairs_mut()
        .append_pair("param_databases", &databases);
    clickhouse_post(client, url.as_str(), sql, CLICKHOUSE_MAX_CATALOG_BYTES).await
}

async fn clickhouse_post_data(
    client: &reqwest::Client,
    url: &str,
    sql: &str,
) -> Result<String, String> {
    let mut url = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("enable_named_columns_in_function_tuple", "1")
        .append_pair("allow_experimental_json_type", "1")
        // Keep GraphQL numeric values numeric in the JSON assembled by
        // toJSONString. ClickHouse quotes 64-bit integers by default.
        .append_pair("output_format_json_quote_64bit_integers", "0");
    clickhouse_post(client, url.as_str(), sql, CLICKHOUSE_MAX_DATA_BYTES).await
}

#[cfg(test)]
mod snapshot_tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    use donat_catalog::{Catalog, ColumnInfo, TableInfo};
    use donat_metadata::{Metadata, SourceKind};
    use donat_schema::{MultiSourcePlan, MultiSourcePlanner, Session};
    use serde_json::{Map as JsonMap, json};
    use tokio::sync::RwLock;

    use super::{AppState, Engine, SourceRuntime, stage_postgres_runtime};

    fn candidate(
        root: &str,
        path: &str,
    ) -> (
        Metadata,
        HashMap<String, Catalog>,
        HashMap<String, SourceRuntime>,
    ) {
        let metadata = serde_json::from_value(json!({
            "version": 3,
            "sources": [{
                "name": "default",
                "kind": "sqlite",
                "configuration": {
                    "connection_info": { "database_url": path }
                },
                "tables": [{
                    "table": { "schema": "public", "name": "item" },
                    "configuration": { "custom_name": root },
                    "select_permissions": [{
                        "role": "user",
                        "permission": { "columns": ["id"], "filter": {} }
                    }],
                    "insert_permissions": [{
                        "role": "user",
                        "permission": { "columns": ["id"], "check": {} }
                    }]
                }]
            }]
        }))
        .expect("metadata deserializes");
        let catalog = Catalog {
            tables: BTreeMap::from([(
                "public.item".to_string(),
                TableInfo {
                    schema: "public".to_string(),
                    name: "item".to_string(),
                    columns: vec![ColumnInfo {
                        name: "id".to_string(),
                        pg_type: "int8".to_string(),
                        native_type: None,
                        nullable: false,
                        has_default: false,
                    }],
                    primary_key: vec!["id".to_string()],
                    foreign_keys: vec![],
                },
            )]),
            functions: BTreeMap::new(),
        };
        (
            metadata,
            HashMap::from([("default".to_string(), catalog)]),
            HashMap::from([(
                "default".to_string(),
                SourceRuntime::Sqlite {
                    path: path.to_string(),
                },
            )]),
        )
    }

    fn state(engine: Engine) -> AppState {
        AppState {
            engine: RwLock::new(Arc::new(engine)),
            default_url: "sqlite::memory:".to_string(),
            admin_secret: None,
            unauthorized_role: None,
            stringify_numerics: false,
            infer_function_permissions: true,
            jwt: None,
            auth_hook: None,
            http: reqwest::Client::new(),
            allowlist_enabled: false,
        }
    }

    fn user_session() -> Session {
        Session {
            role: "user".to_string(),
            vars: HashMap::new(),
            backend_request: false,
        }
    }

    #[test]
    fn duplicate_source_uses_last_definition_for_query_and_mutation() {
        let (first, catalogs, _) = candidate("old_item", "/tmp/old.sqlite");
        let (last, _, runtimes) = candidate("new_item", "/tmp/new.sqlite");
        let mut metadata = first;
        metadata.sources[0].kind = SourceKind::Postgres;
        metadata.sources.extend(last.sources);

        let engine = Engine::compiled(metadata, catalogs, runtimes, true)
            .expect("the last same-named source wins");
        assert_eq!(engine.metadata.sources.len(), 1);
        assert_eq!(engine.metadata.sources[0].kind, SourceKind::Sqlite);
        let compiled = engine.compiled.as_deref().expect("compiled snapshot");
        let planner =
            MultiSourcePlanner::from_compiled(&engine.metadata, &engine.catalogs, compiled)
                .expect("planner constructs from normalized snapshot");

        let query = graphql_parser::parse_query::<String>("{ new_item { id } }")
            .expect("query parses")
            .into_static();
        let MultiSourcePlan::Query { sources, .. } = planner
            .plan(&query, None, &JsonMap::new(), &user_session())
            .expect("last source query plans")
        else {
            panic!("query plan expected");
        };
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source, "default");

        let mutation = graphql_parser::parse_query::<String>(
            "mutation { insert_new_item_one(object: { id: 1 }) { id } }",
        )
        .expect("mutation parses")
        .into_static();
        let MultiSourcePlan::Mutation { source, .. } = planner
            .plan(&mutation, None, &JsonMap::new(), &user_session())
            .expect("last source mutation plans")
        else {
            panic!("mutation plan expected");
        };
        assert_eq!(source.as_deref(), Some("default"));
    }

    #[test]
    fn compiled_engine_rejects_missing_or_mismatched_runtime() {
        let (metadata, catalogs, _) = candidate("item", "/tmp/item.sqlite");
        let missing = Engine::compiled(metadata.clone(), catalogs.clone(), HashMap::new(), true)
            .err()
            .expect("a source without a runtime is rejected");
        assert!(
            missing
                .message
                .contains("runtime for source 'default' not found")
        );

        let runtimes = HashMap::from([(
            "default".to_string(),
            SourceRuntime::Mysql {
                url: "mysql://unused/item".to_string(),
            },
        )]);
        let mismatched = Engine::compiled(metadata, catalogs, runtimes, true)
            .err()
            .expect("a runtime with the wrong backend kind is rejected");
        assert!(mismatched.message.contains("metadata requires Sqlite"));
    }

    #[test]
    fn postgres_runtime_reuses_pool_only_for_the_same_url() {
        let first = stage_postgres_runtime("postgres://localhost/first", None)
            .expect("first runtime stages");
        let same = stage_postgres_runtime("postgres://localhost/first", Some(&first))
            .expect("same-url runtime stages");
        let changed = stage_postgres_runtime("postgres://localhost/second", Some(&same))
            .expect("changed-url runtime stages");

        let SourceRuntime::Postgres {
            pool: first_pool, ..
        } = &first
        else {
            panic!("postgres runtime expected");
        };
        let SourceRuntime::Postgres {
            pool: same_pool, ..
        } = &same
        else {
            panic!("postgres runtime expected");
        };
        let SourceRuntime::Postgres {
            pool: changed_pool, ..
        } = &changed
        else {
            panic!("postgres runtime expected");
        };
        assert!(std::ptr::eq(first_pool.manager(), same_pool.manager()));
        assert!(!std::ptr::eq(same_pool.manager(), changed_pool.manager()));
    }

    #[tokio::test]
    async fn failed_sync_preserves_the_published_engine_snapshot() {
        let (metadata, catalogs, runtimes) = candidate("old_item", "/tmp/old.sqlite");
        let engine =
            Engine::compiled(metadata, catalogs, runtimes, true).expect("old engine compiles");
        let old_compiled = engine.compiled.as_ref().expect("compiled snapshot").clone();
        let state = state(engine);
        let (candidate, _, _) =
            candidate("new_item", "/definitely/missing/donat-parent/new.sqlite");

        state
            .sync_candidate(candidate)
            .await
            .expect_err("candidate sync fails during SQLite introspection");

        let engine = state.engine_snapshot().await;
        assert_eq!(
            engine.metadata.sources[0].tables[0]
                .configuration
                .as_ref()
                .and_then(|configuration| configuration.custom_name.as_deref()),
            Some("old_item")
        );
        assert!(Arc::ptr_eq(
            engine.compiled.as_ref().expect("compiled snapshot"),
            &old_compiled
        ));
        assert!(matches!(
            engine.runtimes.get("default"),
            Some(SourceRuntime::Sqlite { path }) if path == "/tmp/old.sqlite"
        ));
    }

    #[tokio::test]
    async fn failed_candidate_preserves_entire_engine_snapshot() {
        let (metadata, catalogs, runtimes) = candidate("old_item", "/tmp/old.sqlite");
        let engine =
            Engine::compiled(metadata, catalogs, runtimes, true).expect("old engine compiles");
        let old_compiled = engine.compiled.as_ref().expect("compiled snapshot").clone();
        let state = state(engine);

        let (metadata, _, runtimes) = candidate("new_item", "/tmp/new.sqlite");
        let invalid = Engine::compiled(metadata, HashMap::new(), runtimes, true);
        state
            .publish_candidate(invalid)
            .await
            .expect_err("missing candidate catalog is rejected");
        let engine = state.engine_snapshot().await;

        assert_eq!(
            engine.metadata.sources[0].tables[0]
                .configuration
                .as_ref()
                .and_then(|configuration| configuration.custom_name.as_deref()),
            Some("old_item")
        );
        assert!(engine.catalogs.contains_key("default"));
        assert!(Arc::ptr_eq(
            engine.compiled.as_ref().expect("compiled snapshot"),
            &old_compiled
        ));
        assert!(matches!(
            engine.runtimes.get("default"),
            Some(SourceRuntime::Sqlite { path }) if path == "/tmp/old.sqlite"
        ));
    }

    #[tokio::test]
    async fn valid_candidate_publishes_entire_engine_snapshot() {
        let (metadata, catalogs, runtimes) = candidate("old_item", "/tmp/old.sqlite");
        let engine =
            Engine::compiled(metadata, catalogs, runtimes, true).expect("old engine compiles");
        let old_compiled = engine.compiled.as_ref().expect("compiled snapshot").clone();
        let state = state(engine);
        let (metadata, catalogs, runtimes) = candidate("new_item", "/tmp/new.sqlite");
        let replacement = Engine::compiled(metadata, catalogs, runtimes, true);

        state
            .publish_candidate(replacement)
            .await
            .expect("candidate publishes");
        let engine = state.engine_snapshot().await;

        assert_eq!(
            engine.metadata.sources[0].tables[0]
                .configuration
                .as_ref()
                .and_then(|configuration| configuration.custom_name.as_deref()),
            Some("new_item")
        );
        assert!(engine.catalogs.contains_key("default"));
        assert!(!Arc::ptr_eq(
            engine.compiled.as_ref().expect("compiled snapshot"),
            &old_compiled
        ));
        assert!(matches!(
            engine.runtimes.get("default"),
            Some(SourceRuntime::Sqlite { path }) if path == "/tmp/new.sqlite"
        ));
    }
}

#[cfg(test)]
mod clickhouse_transport_tests {
    use super::*;
    use axum::Router;
    use axum::extract::{Query, State};
    use axum::routing::post;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn resolves_hasura_clickhouse_template_configuration() {
        let source: Source = serde_json::from_value(serde_json::json!({
            "name": "clickhouse",
            "kind": "clickhouse",
            "configuration": {
                "template": r#"{
                    "url": {{getEnvironmentVariable("DONAT_TEST_CLICKHOUSE_URL")}},
                    "username": {{getEnvironmentVariable("DONAT_TEST_CLICKHOUSE_USERNAME")}},
                    "password": {{getEnvironmentVariable("DONAT_TEST_CLICKHOUSE_PASSWORD")}}
                }"#,
                "timeout": null,
                "value": {}
            },
            "tables": []
        }))
        .expect("Hasura ClickHouse source should deserialize");

        unsafe {
            std::env::set_var(
                "DONAT_TEST_CLICKHOUSE_URL",
                "http://clickhouse:8123?database=logs",
            );
            std::env::set_var("DONAT_TEST_CLICKHOUSE_USERNAME", "clickhouse");
            std::env::set_var("DONAT_TEST_CLICKHOUSE_PASSWORD", "secret");
        }

        let resolved = resolve_source_url(&source, "postgres://postgres:5432/tandt");

        unsafe {
            std::env::remove_var("DONAT_TEST_CLICKHOUSE_URL");
            std::env::remove_var("DONAT_TEST_CLICKHOUSE_USERNAME");
            std::env::remove_var("DONAT_TEST_CLICKHOUSE_PASSWORD");
        }
        assert_eq!(
            resolved,
            "http://clickhouse:secret@clickhouse:8123/?database=logs"
        );
    }

    #[derive(Clone, Default)]
    struct QueryState(Arc<Mutex<Option<HashMap<String, String>>>>);

    async fn capture_query(
        State(state): State<QueryState>,
        Query(query): Query<HashMap<String, String>>,
    ) -> &'static str {
        *state.0.lock().await = Some(query);
        "{}"
    }

    #[test]
    fn clickhouse_response_limit_rejects_the_chunk_that_crosses_it() {
        let mut body = Vec::new();
        append_clickhouse_chunk(&mut body, b"1234", 5).unwrap();
        let error = append_clickhouse_chunk(&mut body, b"56", 5).unwrap_err();
        assert_eq!(error, "ClickHouse response exceeds 5 bytes");
        assert_eq!(body, b"1234");
    }

    #[tokio::test]
    async fn clickhouse_data_request_keeps_64_bit_json_numbers_unquoted() {
        let state = QueryState::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", post(capture_query))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("query capture server");
        });

        clickhouse_post_data(
            &reqwest::Client::new(),
            &format!("http://{address}/"),
            "SELECT 1",
        )
        .await
        .expect("ClickHouse request succeeds");

        let query = state
            .0
            .lock()
            .await
            .clone()
            .expect("query parameters captured");
        assert_eq!(
            query.get("output_format_json_quote_64bit_integers"),
            Some(&"0".to_string())
        );
        server.abort();
    }
}

/// The helper raised by generated mutation SQL on permission-check
/// violations (SQLSTATE 23514 with a JSON payload).
pub async fn ensure_check_violation_helper(
    client: &deadpool_postgres::Client,
) -> anyhow::Result<()> {
    client
        .batch_execute(
            r#"
            CREATE SCHEMA IF NOT EXISTS donat;
            CREATE OR REPLACE FUNCTION donat.check_violation(msg text)
            RETURNS json AS $$
            BEGIN
                RAISE EXCEPTION USING message = msg, errcode = '23514';
            END;
            $$ LANGUAGE plpgsql;
            "#,
        )
        .await?;
    Ok(())
}

/// Make sure the metadata has at least one (default) source so that
/// track_table & co. have somewhere to live.
pub fn ensure_default_source(metadata: &mut Metadata) {
    if metadata.sources.is_empty() {
        metadata.sources.push(Source {
            name: "default".to_string(),
            kind: SourceKind::Postgres,
            configuration: serde_json::from_value(serde_json::json!({
                "connection_info": { "database_url": { "from_env": "DONAT_DATABASE_URL" } }
            }))
            .expect("static source configuration"),
            tables: vec![],
            functions: vec![],
        });
    }
}
