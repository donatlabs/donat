//! Deploy-time subcommands that are NOT part of the serving request path:
//!
//! - `migrate` — apply versioned `.sql` schema migrations (DDL) via refinery,
//!   tracked in a `refinery_schema_history` table. This is the only thing
//!   that mutates the database schema; the serving binary never runs DDL.
//! - `validate` — load the YAML metadata, introspect the (migrated) database,
//!   and report inconsistencies (tracked tables / relationship targets /
//!   computed-field functions missing from the DB, inherited-role permission
//!   conflicts). Non-zero exit on any inconsistency, so a deploy fails fast
//!   before the serving binary boots known-bad metadata.
//!
//! Together these replace the runtime `run_sql` / metadata-mutation API:
//! schema is migrated out-of-band, metadata is desired-state in YAML.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
struct SqlMigration {
    version: i64,
    name: String,
    path: PathBuf,
    sql: String,
}

const BUILTIN_CATALOG_MIGRATIONS: &[&str] = &[
    include_str!("../../../migrations/V20260613222215__donat_cron.sql"),
    include_str!("../../../migrations/V20260613222216__donat_event_log.sql"),
];

// Do not add new deploy-time catalogs here. This legacy bootstrap runs while
// a serving snapshot stages its existing cron/event support. Command journals,
// process journals, and their helpers must be applied through the explicit
// `donat migrate --migrations-dir ...` path only.

/// Apply all pending `.sql` migrations in `dir` to the database.
pub async fn run_migrate(database_url: &str, dir: &Path) -> Result<()> {
    let migrations = load_sql_migrations(dir)
        .with_context(|| format!("loading migrations from {}", dir.display()))?;
    if migrations.is_empty() {
        tracing::warn!(dir = %dir.display(), "no .sql migrations found");
        return Ok(());
    }
    let (mut client, conn) = tokio_postgres::connect(database_url, tokio_postgres::NoTls)
        .await
        .context("connecting to database for migrate")?;
    let conn = tokio::spawn(conn);

    ensure_engine_catalog(&client)
        .await
        .context("ensuring engine catalog")?;
    ensure_history_table(&client)
        .await
        .context("ensuring migration history table")?;
    let mut applied = applied_migrations(&client)
        .await
        .context("loading migration history")?;
    if applied.is_empty()
        && adopt_existing_schema_enabled()
        && existing_user_schema(&client).await?
    {
        adopt_existing_migrations(&mut client, &migrations)
            .await
            .context("adopting existing database schema")?;
        applied = applied_migrations(&client)
            .await
            .context("loading migration history after adoption")?;
    }

    if adopt_timestamp_versions(&mut client, &migrations, &applied).await? {
        applied = applied_migrations(&client)
            .await
            .context("loading migration history after renumbering")?;
    }

    let mut applied_count = 0usize;
    for migration in migrations {
        if let Some(applied_name) = applied.get(&migration.version) {
            if applied_name != &migration.name {
                bail!(
                    "migration version {} was already applied as {:?}, but filesystem has {:?}",
                    migration.version,
                    applied_name,
                    migration.name
                );
            }
            continue;
        }

        let tx = client
            .transaction()
            .await
            .with_context(|| format!("starting transaction for {}", migration.path.display()))?;
        tx.batch_execute(&migration.sql)
            .await
            .with_context(|| format!("applying migration {}", migration.path.display()))?;
        tx.execute(
            "INSERT INTO refinery_schema_history (version, name, applied_on) VALUES ($1, $2, now())",
            &[&migration.version, &migration.name],
        )
        .await
        .with_context(|| format!("recording migration {}", migration.path.display()))?;
        tx.commit()
            .await
            .with_context(|| format!("committing migration {}", migration.path.display()))?;

        tracing::info!(
            version = migration.version,
            name = %migration.name,
            "applied migration"
        );
        applied_count += 1;
    }

    if applied_count == 0 {
        tracing::info!("migrations already up to date");
    }
    conn.abort();
    Ok(())
}

fn load_sql_migrations(dir: &Path) -> Result<Vec<SqlMigration>> {
    let mut migrations = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading migrations directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
                bail!("migration path is not valid UTF-8: {}", path.display());
            };
            let up_sql = path.join("up.sql");
            if !up_sql.is_file() {
                continue;
            }
            let (version, name) = parse_migration_stem(dir_name)?;
            let sql = read_migration_sql(&up_sql)?;
            migrations.push(SqlMigration {
                version,
                name,
                path: up_sql,
                sql,
            });
            continue;
        }
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("sql") {
            continue;
        }
        let file_name = path.file_name().and_then(|s| s.to_str()).ok_or_else(|| {
            anyhow::anyhow!("migration path is not valid UTF-8: {}", path.display())
        })?;
        let stem = file_name
            .strip_suffix(".sql")
            .ok_or_else(|| anyhow::anyhow!("migration file must end with .sql: {file_name}"))?;
        let (version, name) = parse_migration_stem(stem)?;
        let sql = read_migration_sql(&path)?;
        migrations.push(SqlMigration {
            version,
            name,
            path,
            sql,
        });
    }
    migrations.sort_by(|a, b| a.version.cmp(&b.version).then_with(|| a.path.cmp(&b.path)));

    for pair in migrations.windows(2) {
        if pair[0].version == pair[1].version {
            bail!(
                "duplicate migration version {} in {} and {}",
                pair[0].version,
                pair[0].path.display(),
                pair[1].path.display()
            );
        }
    }

    Ok(migrations)
}

fn read_migration_sql(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading migration {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_migration_stem(stem: &str) -> Result<(i64, String)> {
    let stem = stem
        .strip_prefix('V')
        .or_else(|| stem.strip_prefix('v'))
        .unwrap_or(stem);
    let (version, name) = stem
        .split_once("__")
        .or_else(|| stem.split_once('_'))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "migration must use V<version>__<name>.sql or <version>_<name>/up.sql format: {stem}"
            )
        })?;
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit()) {
        bail!("migration version must be a valid integer: {stem}");
    }
    if name.trim().is_empty() {
        bail!("migration name must not be empty: {stem}");
    }
    let version = version
        .parse::<i64>()
        .with_context(|| format!("migration version must fit in BIGINT: {stem}"))?;
    Ok((version, name.to_string()))
}

async fn ensure_engine_catalog(client: &tokio_postgres::Client) -> Result<()> {
    client
        .batch_execute("CREATE EXTENSION IF NOT EXISTS pgcrypto SCHEMA public;")
        .await?;
    for sql in BUILTIN_CATALOG_MIGRATIONS {
        client.batch_execute(sql).await?;
    }
    Ok(())
}

async fn ensure_history_table(client: &tokio_postgres::Client) -> Result<()> {
    client
        .batch_execute(
            "
            CREATE TABLE IF NOT EXISTS refinery_schema_history (
                version BIGINT PRIMARY KEY,
                name TEXT NOT NULL,
                applied_on TIMESTAMPTZ NOT NULL DEFAULT now()
            );
            ALTER TABLE refinery_schema_history
                ALTER COLUMN version TYPE BIGINT;
            ALTER TABLE refinery_schema_history
                ALTER COLUMN name TYPE TEXT;
            ",
        )
        .await?;
    Ok(())
}

async fn applied_migrations(client: &tokio_postgres::Client) -> Result<HashMap<i64, String>> {
    let rows = client
        .query("SELECT version, name FROM refinery_schema_history", &[])
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get::<_, i64>(0), row.get::<_, String>(1)))
        .collect())
}

/// Below this a version is a legacy hand-numbered sequence rather than a
/// timestamp. The smallest timestamp form in use — epoch milliseconds — is
/// already thirteen digits, so nothing that means "a moment in time" lands
/// anywhere near a counter.
const LEGACY_SEQUENTIAL_VERSION_CEILING: i64 = 1_000_000;

/// Carry a history written under sequential versions onto the timestamp
/// versions the same migrations now carry.
///
/// A migration's identity is the change it made, not the number someone typed
/// in front of it. Renumbering the files would otherwise make every already
/// applied migration look new, and the next deploy would try to create tables
/// that exist. The join is the migration name, which is what survives the
/// rename.
///
/// This only ever moves a row from a legacy counter onto a timestamp, and only
/// when exactly one migration on each side carries that name; anything
/// ambiguous is left alone for the version check below to report.
async fn adopt_timestamp_versions(
    client: &mut tokio_postgres::Client,
    migrations: &[SqlMigration],
    applied: &HashMap<i64, String>,
) -> Result<bool> {
    let mut moves = Vec::new();
    for migration in migrations {
        if migration.version < LEGACY_SEQUENTIAL_VERSION_CEILING
            || applied.contains_key(&migration.version)
        {
            continue;
        }
        if migrations
            .iter()
            .filter(|other| other.name == migration.name)
            .count()
            != 1
        {
            continue;
        }
        let mut legacy = applied
            .iter()
            .filter(|(version, name)| {
                **version < LEGACY_SEQUENTIAL_VERSION_CEILING && *name == &migration.name
            })
            .map(|(version, _)| *version);
        let (Some(from), None) = (legacy.next(), legacy.next()) else {
            continue;
        };
        moves.push((from, migration.version, migration.name.clone()));
    }
    if moves.is_empty() {
        return Ok(false);
    }

    let tx = client.transaction().await?;
    for (from, to, name) in &moves {
        tx.execute(
            "UPDATE refinery_schema_history SET version = $1 WHERE version = $2",
            &[to, from],
        )
        .await
        .with_context(|| format!("renumbering applied migration {name}"))?;
    }
    tx.commit().await?;
    tracing::warn!(
        renumbered = moves.len(),
        "carried applied migrations from sequential versions onto their timestamp versions"
    );
    Ok(true)
}

fn adopt_existing_schema_enabled() -> bool {
    std::env::var("DONAT_ADOPT_EXISTING_SCHEMA")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

async fn existing_user_schema(client: &tokio_postgres::Client) -> Result<bool> {
    let count: i64 = client
        .query_one(
            "
            SELECT count(*)
            FROM pg_class c
            JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f')
              AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'donat')
              AND NOT (n.nspname = 'public' AND c.relname = 'refinery_schema_history')
            ",
            &[],
        )
        .await?
        .get(0);
    Ok(count > 0)
}

async fn adopt_existing_migrations(
    client: &mut tokio_postgres::Client,
    migrations: &[SqlMigration],
) -> Result<()> {
    let tx = client.transaction().await?;
    for migration in migrations {
        tx.execute(
            "INSERT INTO refinery_schema_history (version, name, applied_on) VALUES ($1, $2, now()) ON CONFLICT (version) DO NOTHING",
            &[&migration.version, &migration.name],
        )
        .await
        .with_context(|| format!("recording adopted migration {}", migration.path.display()))?;
    }
    tx.commit().await?;
    tracing::warn!(
        count = migrations.len(),
        "adopted existing database schema into migration history"
    );
    Ok(())
}

/// Load metadata + introspect the database and report inconsistencies.
/// Returns the list of human-readable inconsistencies (empty = consistent).
pub async fn check_consistency(database_url: &str, metadata_dir: &Path) -> Result<Vec<String>> {
    crate::validate::check_consistency(database_url, metadata_dir).await
}
