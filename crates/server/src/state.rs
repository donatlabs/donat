//! Shared server state: per-source connection pools and the engine
//! snapshot (metadata + per-source catalogs) that metadata operations
//! mutate at runtime.

use std::collections::HashMap;
use std::sync::Arc;

use donat_backend::{AnyDialect, SqliteDialect};
use donat_catalog::Catalog;
use donat_ir::RootField;
use donat_metadata::{DatabaseUrl, Metadata, Source, SourceKind};
use serde_json::Value as Json;
use tokio::sync::RwLock;

pub struct AppState {
    /// One (url, pool) per Postgres source name; the pool is recreated when
    /// the source's url changes (e.g. replace_metadata pointing 'default'
    /// at a per-test database).
    pub pools: RwLock<HashMap<String, (String, deadpool_postgres::Pool)>>,
    /// One db path/url per SQLite source name. SQLite uses no pool: the
    /// runtime opens a `rusqlite::Connection` per query inside
    /// `spawn_blocking` (see `execute_query_json`).
    pub sqlite_paths: RwLock<HashMap<String, String>>,
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
    match &source.configuration.connection_info.database_url {
        DatabaseUrl::Url(url) => url.clone(),
        DatabaseUrl::FromEnv { from_env } => {
            std::env::var(from_env).unwrap_or_else(|_| default_url.to_string())
        }
    }
}

impl AppState {
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
        }
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
                    let catalog = tokio::task::spawn_blocking(
                        move || -> anyhow::Result<Catalog> {
                            let conn = rusqlite::Connection::open(&path)?;
                            Ok(donat_catalog::sqlite_introspect(&conn)?)
                        },
                    )
                    .await??;
                    self.sqlite_paths
                        .write()
                        .await
                        .insert(name.clone(), url.clone());
                    catalog
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
