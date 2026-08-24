//! Versioned SQL migrations applied to a test database.

use std::path::Path;

use anyhow::{Context, Result, anyhow};

/// Apply a checked-in directory of versioned SQL migrations to a suite
/// database before its first engine request. Migration paths are selected by
/// the test harness, never from an HTTP request.
pub fn apply_sql_migration_dir(database_url: &str, dir: &Path) -> Result<()> {
    let mut migrations = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading migration directory {}", dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("reading migration entry in {}", dir.display()))?;
        let path = entry.path();
        if !entry
            .file_type()
            .with_context(|| format!("reading migration file type {}", path.display()))?
            .is_file()
        {
            return Err(anyhow!(
                "migration entry {} is not a regular file",
                path.display()
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("migration file name {} is not UTF-8", path.display()))?;
        let (version, description) = name
            .strip_prefix('V')
            .and_then(|name| name.split_once("__"))
            .and_then(|(version, description)| {
                description.strip_suffix(".sql").map(|d| (version, d))
            })
            .ok_or_else(|| anyhow!("invalid migration file name {name}"))?;
        if version.is_empty()
            || version.starts_with('0')
            || !version.bytes().all(|byte| byte.is_ascii_digit())
            || description.is_empty()
            || !description
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(anyhow!("invalid migration file name {name}"));
        }
        let version = version
            .parse::<u64>()
            .with_context(|| format!("invalid migration version in {name}"))?;
        migrations.push((version, path));
    }

    migrations.sort_by_key(|(version, _)| *version);
    for pair in migrations.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(anyhow!("duplicate version {}", pair[0].0));
        }
    }

    let mut client = postgres::Client::connect(database_url, postgres::NoTls)
        .with_context(|| format!("connecting to migration database {database_url}"))?;
    for (_, path) in migrations {
        let sql = std::fs::read_to_string(&path)
            .with_context(|| format!("reading migration {}", path.display()))?;
        client
            .batch_execute(&sql)
            .with_context(|| format!("applying migration {}", path.display()))?;
    }
    Ok(())
}
