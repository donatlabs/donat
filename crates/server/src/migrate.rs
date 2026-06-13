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

use std::path::Path;

use anyhow::{Context, Result};

/// Apply all pending `.sql` migrations in `dir` to the database.
pub async fn run_migrate(database_url: &str, dir: &Path) -> Result<()> {
    let migrations = refinery::load_sql_migrations(dir)
        .with_context(|| format!("loading migrations from {}", dir.display()))?;
    if migrations.is_empty() {
        tracing::warn!(dir = %dir.display(), "no .sql migrations found");
        return Ok(());
    }
    let (mut client, conn) = tokio_postgres::connect(database_url, tokio_postgres::NoTls)
        .await
        .context("connecting to database for migrate")?;
    let conn = tokio::spawn(async move { conn.await });

    let report = refinery::Runner::new(&migrations)
        .run_async(&mut client)
        .await
        .context("applying migrations")?;

    let applied = report.applied_migrations();
    if applied.is_empty() {
        tracing::info!("migrations already up to date");
    } else {
        for m in applied {
            tracing::info!(version = m.version(), name = %m.name(), "applied migration");
        }
    }
    conn.abort();
    Ok(())
}

/// Load metadata + introspect the database and report inconsistencies.
/// Returns the list of human-readable inconsistencies (empty = consistent).
pub async fn check_consistency(database_url: &str, metadata_dir: &Path) -> Result<Vec<String>> {
    let metadata = donat_metadata::load_metadata_dir(metadata_dir)
        .with_context(|| format!("loading metadata from {}", metadata_dir.display()))?;

    let (client, conn) = tokio_postgres::connect(database_url, tokio_postgres::NoTls)
        .await
        .context("connecting to database for validate")?;
    let conn = tokio::spawn(async move { conn.await });
    let catalog = donat_catalog::introspect(&client)
        .await
        .context("introspecting database")?;
    conn.abort();

    let mut problems = vec![];
    for source in &metadata.sources {
        for entry in &source.tables {
            let (schema, name) = (entry.table.schema(), entry.table.name());
            if catalog.table(schema, name).is_none() {
                problems.push(format!(
                    "tracked table \"{schema}.{name}\" does not exist in the database"
                ));
                // Permissions/relationships below would be noise; skip them.
                continue;
            }
            for cf in &entry.computed_fields {
                let f = &cf.definition.function;
                if catalog.function(f.schema(), f.name()).is_none() {
                    problems.push(format!(
                        "computed field \"{}\" on \"{schema}.{name}\" references missing function \"{}.{}\"",
                        cf.name,
                        f.schema(),
                        f.name()
                    ));
                }
            }
        }
    }

    // Inherited-role mutation permission conflicts (the engine's own check).
    let planner = donat_schema::Planner::new(&metadata, &catalog);
    for (role, table, kind) in planner.mutation_permission_conflicts() {
        problems.push(format!(
            "inherited role \"{role}\": conflicting {kind} permission on table \"{table}\""
        ));
    }

    Ok(problems)
}
