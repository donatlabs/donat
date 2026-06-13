//! Shared server state: per-source connection pools and the engine
//! snapshot (metadata + per-source catalogs) that metadata operations
//! mutate at runtime.

use std::collections::HashMap;
use std::sync::Arc;

use dist_catalog::Catalog;
use dist_metadata::{DatabaseUrl, Metadata, Source, SourceKind};
use tokio::sync::RwLock;

pub struct AppState {
    /// One (url, pool) per source name; the pool is recreated when the
    /// source's url changes (e.g. replace_metadata pointing 'default'
    /// at a per-test database).
    pub pools: RwLock<HashMap<String, (String, deadpool_postgres::Pool)>>,
    pub engine: RwLock<Engine>,
    /// The fallback/default database (also the metadata database in
    /// --hge-bin mode).
    pub default_url: String,
    pub admin_secret: Option<String>,
    /// HASURA_GRAPHQL_UNAUTHORIZED_ROLE: role for requests without one.
    pub unauthorized_role: Option<String>,
    /// --stringify-numeric-types
    pub stringify_numerics: bool,
    /// HASURA_GRAPHQL_INFER_FUNCTION_PERMISSIONS (default true).
    pub infer_function_permissions: bool,
    /// JWT authentication mode, when HASURA_GRAPHQL_JWT_SECRET is set.
    pub jwt: Option<crate::jwt::JwtConfig>,
    /// Webhook authentication mode: (url, "GET"|"POST").
    pub auth_hook: Option<(String, String)>,
    pub http: reqwest::Client,
    /// HASURA_GRAPHQL_ENABLE_ALLOWLIST: non-listed queries are rejected.
    pub allowlist_enabled: bool,
}

pub type SharedState = Arc<AppState>;

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

    /// Reconcile pools and catalogs with the current metadata sources,
    /// pruning metadata that refers to dropped objects (run_sql untracks
    /// dropped tables/functions, like Hasura).
    pub async fn sync_sources(&self) -> anyhow::Result<()> {
        // Later same-named sources override earlier ones (the harness
        // appends a second 'default' pointing at a per-test database).
        let sources: Vec<(String, String)> = {
            let engine = self.engine.read().await;
            let mut resolved: Vec<(String, String)> = vec![];
            for s in &engine.metadata.sources {
                let url = resolve_source_url(s, &self.default_url);
                match resolved.iter_mut().find(|(n, _)| n == &s.name) {
                    Some(entry) => entry.1 = url,
                    None => resolved.push((s.name.clone(), url)),
                }
            }
            resolved
        };

        let mut new_catalogs = HashMap::new();
        for (name, url) in &sources {
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
            let catalog = dist_catalog::introspect(&client).await?;
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
            CREATE SCHEMA IF NOT EXISTS dist_api;
            CREATE OR REPLACE FUNCTION dist_api.check_violation(msg text)
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
                "connection_info": { "database_url": { "from_env": "DIST_API_DATABASE_URL" } }
            }))
            .expect("static source configuration"),
            tables: vec![],
            functions: vec![],
        });
    }
}
