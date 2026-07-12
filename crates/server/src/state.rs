//! Shared server state: per-source connection pools and the engine
//! snapshot (metadata + per-source catalogs) that metadata operations
//! mutate at runtime.

use std::collections::HashMap;
use std::sync::Arc;

use donat_backend::{AnyDialect, ClickhouseDialect, MySqlDialect, SqliteDialect};
use donat_catalog::Catalog;
use donat_ir::RootField;
use donat_metadata::{DatabaseUrl, Metadata, Source, SourceKind};
use serde_json::Value as Json;
use tokio::sync::RwLock;

const CLICKHOUSE_MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
const CLICKHOUSE_MAX_DATA_BYTES: usize = 64 * 1024 * 1024;

pub struct AppState {
    /// One (url, pool) per Postgres source name; the pool is recreated when
    /// the source's url changes (e.g. replace_metadata pointing 'default'
    /// at a per-test database).
    pub pools: RwLock<HashMap<String, (String, deadpool_postgres::Pool)>>,
    /// One db path/url per SQLite source name. SQLite uses no pool: the
    /// runtime opens a `rusqlite::Connection` per query inside
    /// `spawn_blocking` (see `execute_query_json`).
    pub sqlite_paths: RwLock<HashMap<String, String>>,
    /// One connection url per MySQL source name. Like SQLite, MySQL uses no
    /// pool: the runtime opens a `mysql::Conn` per query inside
    /// `spawn_blocking` (see `execute_query_json`). The url carries the
    /// database name, which is also the tracked schema.
    pub mysql_urls: RwLock<HashMap<String, String>>,
    pub engine: RwLock<Engine>,
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

/// Failure of a backend read in `execute_query_json`. The caller maps each
/// variant to the existing GraphQL error body so the Postgres path keeps
/// byte-for-byte identical error shaping (`Postgres` carries the real
/// `tokio_postgres::Error` for `db_error_json`).
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
    NoDefaultSource,
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
    NoDefaultSource,
    /// A row failed its insert/update permission check; the transaction was
    /// rolled back. `path` is the GraphQL error path for the body.
    CheckViolation {
        path: String,
    },
    /// A MySQL driver / SQL error (mapped to `data-exception`).
    Mysql(String),
    Other(String),
}

pub struct Engine {
    pub metadata: Metadata,
    /// Catalog snapshot per source name.
    pub catalogs: HashMap<String, Catalog>,
}

impl Engine {
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

fn resolve_source_url(source: &Source, default_url: &str) -> String {
    let Some(connection_info) = &source.configuration.connection_info else {
        return default_url.to_string();
    };
    match &connection_info.database_url {
        DatabaseUrl::Url(url) => url.clone(),
        DatabaseUrl::FromEnv { from_env } => {
            std::env::var(from_env).unwrap_or_else(|_| default_url.to_string())
        }
    }
}

impl AppState {
    async fn default_source_url(&self) -> Option<String> {
        let engine = self.engine.read().await;
        engine
            .metadata
            .sources
            .iter()
            .find(|source| source.name == "default")
            .or_else(|| engine.metadata.sources.first())
            .map(|source| resolve_source_url(source, &self.default_url))
    }

    pub async fn default_pool(&self) -> Option<deadpool_postgres::Pool> {
        let pools = self.pools.read().await;
        pools
            .get("default")
            .or_else(|| pools.values().next())
            .map(|(_, p)| p.clone())
    }

    /// The backend kind of the source the GraphQL schema is built against
    /// (the "default" source, or the first one) — mirrors
    /// `Engine::default_catalog`'s selection. Defaults to Postgres when no
    /// source is declared.
    pub async fn default_source_kind(&self) -> SourceKind {
        let engine = self.engine.read().await;
        engine
            .metadata
            .sources
            .iter()
            .find(|s| s.name == "default")
            .or_else(|| engine.metadata.sources.first())
            .map(|s| s.kind)
            .unwrap_or(SourceKind::Postgres)
    }

    /// Run a planned read operation against the default source's backend and
    /// return the assembled JSON `data` object. Dispatches on the source's
    /// backend kind so the Postgres path is byte-for-byte identical to the
    /// pre-multi-backend behavior (same SQL, same client call, same error
    /// shaping — the caller maps `QueryError` back to the existing bodies).
    pub async fn execute_query_json(&self, roots: &[RootField]) -> Result<Json, QueryError> {
        match self.default_source_kind().await {
            SourceKind::Postgres => {
                let sql = donat_sqlgen::operation_to_sql_opts(roots, self.stringify_numerics);
                let pool = self
                    .default_pool()
                    .await
                    .ok_or(QueryError::NoDefaultSource)?;
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
            SourceKind::Sqlite => {
                let sql =
                    donat_sqlgen::operation_to_sql_with(roots, AnyDialect::Sqlite(SqliteDialect));
                let path = {
                    let paths = self.sqlite_paths.read().await;
                    paths
                        .get("default")
                        .or_else(|| paths.values().next())
                        .cloned()
                        .ok_or(QueryError::NoDefaultSource)?
                };
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
            SourceKind::Mysql => {
                use mysql::prelude::Queryable;

                let sql =
                    donat_sqlgen::operation_to_sql_with(roots, AnyDialect::Mysql(MySqlDialect));
                let url = {
                    let urls = self.mysql_urls.read().await;
                    urls.get("default")
                        .or_else(|| urls.values().next())
                        .cloned()
                        .ok_or(QueryError::NoDefaultSource)?
                };
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
            SourceKind::Clickhouse => {
                let sql = donat_sqlgen::operation_to_sql_with(
                    roots,
                    AnyDialect::Clickhouse(ClickhouseDialect),
                );
                let url = self
                    .default_source_url()
                    .await
                    .ok_or(QueryError::NoDefaultSource)?;
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
    pub async fn execute_sqlite_mutations(
        &self,
        roots: &[donat_ir::MutationRoot],
    ) -> Result<Json, SqliteMutationError> {
        use donat_sqlgen::SqliteMutationPlan;

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

        let path = {
            let paths = self.sqlite_paths.read().await;
            paths
                .get("default")
                .or_else(|| paths.values().next())
                .cloned()
                .ok_or(SqliteMutationError::NoDefaultSource)?
        };

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

                // Assemble this root's response object from the plan's aliases,
                // mirroring the Postgres `Plan::Mutation` response shape.
                let mut obj = serde_json::Map::new();
                if let Some(ret_alias) = &plan.returning_alias {
                    obj.insert(ret_alias.clone(), Json::Array(returning));
                }
                if let Some(ar_alias) = &plan.affected_rows_alias {
                    obj.insert(ar_alias.clone(), Json::from(affected_rows));
                }
                if let Some((tn_alias, tn_value)) = &plan.typename {
                    obj.insert(tn_alias.clone(), Json::String(tn_value.clone()));
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
    pub async fn execute_mysql_mutations(
        &self,
        roots: &[donat_ir::MutationRoot],
    ) -> Result<Json, MysqlMutationError> {
        use donat_sqlgen::{MySqlMutationKind, MySqlMutationPlan};
        use mysql::prelude::Queryable;

        // Plan every root up front (alias + MySQL mutation plan), resolving each
        // table's primary key from the catalog (the IR mutation does not carry
        // it, but the insert companion SELECT needs it for last_insert_id()
        // recovery / the supplied-PK predicate). Preserve selection order.
        let planned: Vec<(String, MySqlMutationPlan)> = {
            let engine = self.engine.read().await;
            let catalog = engine.default_catalog();
            let pk_of = |t: &donat_ir::Table| -> Vec<String> {
                catalog
                    .tables
                    .get(&format!("{}.{}", t.schema, t.name))
                    .map(|info| info.primary_key.clone())
                    .unwrap_or_default()
            };
            roots
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
                .collect()
        };

        let url = {
            let urls = self.mysql_urls.read().await;
            urls.get("default")
                .or_else(|| urls.values().next())
                .cloned()
                .ok_or(MysqlMutationError::NoDefaultSource)?
        };

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

                // Assemble this root's response object from the plan's aliases,
                // mirroring the Postgres/SQLite `Plan::Mutation` response shape.
                let mut obj = serde_json::Map::new();
                if let Some(ret_alias) = &plan.returning_alias {
                    obj.insert(ret_alias.clone(), Json::Array(returning));
                }
                if let Some(ar_alias) = &plan.affected_rows_alias {
                    obj.insert(ar_alias.clone(), Json::from(affected_rows));
                }
                if let Some((tn_alias, tn_value)) = &plan.typename {
                    obj.insert(tn_alias.clone(), Json::String(tn_value.clone()));
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
        // Later same-named sources override earlier ones (the harness
        // appends a second 'default' pointing at a per-test database).
        let sources: Vec<(String, SourceKind, String)> = {
            let engine = self.engine.read().await;
            let mut resolved: Vec<(String, SourceKind, String)> = vec![];
            for s in &engine.metadata.sources {
                let url = resolve_source_url(s, &self.default_url);
                match resolved.iter_mut().find(|(n, _, _)| n == &s.name) {
                    Some(entry) => {
                        entry.1 = s.kind;
                        entry.2 = url;
                    }
                    None => resolved.push((s.name.clone(), s.kind, url)),
                }
            }
            resolved
        };

        let mut new_catalogs = HashMap::new();
        for (name, kind, url) in &sources {
            let catalog = match kind {
                SourceKind::Postgres => {
                    let existing = {
                        let pools = self.pools.read().await;
                        pools
                            .get(name)
                            .filter(|(u, _)| u == url)
                            .map(|(_, p)| p.clone())
                    };
                    let pool = match existing {
                        Some(pool) => pool,
                        None => {
                            let pool = make_pool(url)?;
                            self.pools
                                .write()
                                .await
                                .insert(name.clone(), (url.clone(), pool.clone()));
                            pool
                        }
                    };
                    let client = pool.get().await?;
                    ensure_check_violation_helper(&client).await?;
                    donat_catalog::introspect(&client).await?
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
                    self.sqlite_paths
                        .write()
                        .await
                        .insert(name.clone(), url.clone());
                    catalog
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
                    self.mysql_urls
                        .write()
                        .await
                        .insert(name.clone(), url.clone());
                    catalog
                }
                SourceKind::Clickhouse => {
                    let database = clickhouse_database(url)?;
                    let sql = "SELECT table, name, type, default_kind, is_in_primary_key \
                               FROM system.columns \
                               WHERE database = {database:String} \
                               ORDER BY table, position \
                               FORMAT JSONEachRow";
                    let text = clickhouse_post_with_database_param(&self.http, url, sql, &database)
                        .await
                        .map_err(anyhow::Error::msg)?;
                    donat_catalog::clickhouse_catalog_from_json_each_row(&text, &database)?
                }
            };
            new_catalogs.insert(name.clone(), catalog);
        }

        let mut engine = self.engine.write().await;
        for source in &mut engine.metadata.sources {
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
        engine.catalogs = new_catalogs;
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

async fn clickhouse_post_with_database_param(
    client: &reqwest::Client,
    url: &str,
    sql: &str,
    database: &str,
) -> Result<String, String> {
    let mut url = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("param_database", database);
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
        .append_pair("allow_experimental_json_type", "1");
    clickhouse_post(client, url.as_str(), sql, CLICKHOUSE_MAX_DATA_BYTES).await
}

#[cfg(test)]
mod clickhouse_transport_tests {
    use super::*;

    #[test]
    fn clickhouse_response_limit_rejects_the_chunk_that_crosses_it() {
        let mut body = Vec::new();
        append_clickhouse_chunk(&mut body, b"1234", 5).unwrap();
        let error = append_clickhouse_chunk(&mut body, b"56", 5).unwrap_err();
        assert_eq!(error, "ClickHouse response exceeds 5 bytes");
        assert_eq!(body, b"1234");
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
