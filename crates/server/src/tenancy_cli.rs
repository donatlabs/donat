//! `donat tenancy plan` and `donat tenancy check`.
//!
//! The declaration is sixteen lines and the migration under it is hundreds,
//! all of it derivable. This derives it. The serving engine still runs no DDL:
//! `plan` writes a file for a person to read and commit, the way `codegen`
//! writes Go structs, and `check` writes nothing and exits non-zero when the
//! database is not what the declaration implies.

use anyhow::Context;
use donat_schema::tenancy_plan::{TenancyPlan, UniqueIndex, plan_tenancy, render_sql};

/// Every unique index and constraint in one schema, names and predicates
/// included.
///
/// The shared catalogue keeps only unconditional ones — the planner cannot use
/// a partial index as an `ON CONFLICT` target or a lookup — and it keeps column
/// sets without names. Both omissions matter here: the constraint that made
/// this module necessary was partial, and rescoping one needs its name.
const UNIQUE_INDEXES_SQL: &str = "\
SELECT n.nspname AS schema,
       t.relname AS table_name,
       i.relname AS index_name,
       array_agg(a.attname ORDER BY k.ord) AS columns,
       pg_get_expr(x.indpred, x.indrelid) AS predicate,
       (c.conname IS NOT NULL) AS is_constraint
  FROM pg_index x
  JOIN pg_class i ON i.oid = x.indexrelid
  JOIN pg_class t ON t.oid = x.indrelid
  JOIN pg_namespace n ON n.oid = t.relnamespace
  JOIN LATERAL unnest(x.indkey) WITH ORDINALITY AS k(attnum, ord) ON true
  LEFT JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
  LEFT JOIN pg_constraint c ON c.conindid = i.oid AND c.contype IN ('u', 'p')
 WHERE x.indisunique
   AND x.indisvalid
   AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'donat')
 GROUP BY n.nspname, t.relname, i.relname, x.indpred, x.indrelid, c.conname
 HAVING bool_and(a.attname IS NOT NULL)";

/// Which tracked tables already hold rows.
///
/// Cheap and approximate on purpose: `reltuples` is the planner's estimate, so
/// this asks the tables themselves. A tenanted deployment being created from a
/// single-tenant one is the case that needs it, and there the answer is "all of
/// them".
const POPULATED_SQL: &str = "\
SELECT n.nspname || '.' || c.relname AS name
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE c.relkind = 'r'
   AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'donat')
   AND (SELECT true FROM pg_catalog.pg_class WHERE false) IS NULL";

pub async fn populated_tables(
    client: &tokio_postgres::Client,
) -> Result<std::collections::BTreeSet<String>, tokio_postgres::Error> {
    let names: Vec<String> = client
        .query(POPULATED_SQL, &[])
        .await?
        .iter()
        .map(|row| row.get::<_, String>("name"))
        .collect();
    let mut populated = std::collections::BTreeSet::new();
    for name in names {
        let (schema, table) = name.split_once('.').expect("qualified");
        let sql = format!(
            "SELECT EXISTS (SELECT 1 FROM {}.{} LIMIT 1)",
            quote_ident(schema),
            quote_ident(table)
        );
        if client.query_one(&sql, &[]).await?.get::<_, bool>(0) {
            populated.insert(name);
        }
    }
    Ok(populated)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub async fn unique_indexes(
    client: &tokio_postgres::Client,
) -> Result<Vec<UniqueIndex>, tokio_postgres::Error> {
    let rows = client.query(UNIQUE_INDEXES_SQL, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| UniqueIndex {
            schema: row.get("schema"),
            table: row.get("table_name"),
            name: row.get("index_name"),
            columns: row.get("columns"),
            predicate: row.get("predicate"),
            constraint: row.get("is_constraint"),
        })
        .collect())
}

/// Build the plan against one live database.
async fn derive(
    metadata_dir: &std::path::Path,
    database_url: &str,
    backfill: Option<&str>,
) -> anyhow::Result<TenancyPlan> {
    let metadata = donat_metadata::load_metadata_dir(metadata_dir)
        .with_context(|| format!("loading metadata from {}", metadata_dir.display()))?;
    let (client, connection) = tokio_postgres::connect(database_url, crate::pgtls::connector())
        .await
        .context("connecting to database")?;
    let connection = tokio::spawn(connection);
    let catalog = donat_catalog::introspect(&client)
        .await
        .context("introspecting database")?;
    let uniques = unique_indexes(&client)
        .await
        .context("reading unique indexes")?;
    let populated = populated_tables(&client)
        .await
        .context("reading which tables hold rows")?;
    connection.abort();
    Ok(plan_tenancy(
        &metadata, &catalog, &uniques, &populated, backfill,
    ))
}

fn report(plan: &TenancyPlan) {
    for item in &plan.unresolved {
        eprintln!("  {} {}", item.object, item.reason);
    }
}

/// `check`: say what differs, write nothing, exit non-zero if anything does.
pub async fn check(metadata_dir: &std::path::Path, database_url: &str) -> anyhow::Result<()> {
    let plan = derive(metadata_dir, database_url, None).await?;
    if plan.is_empty() {
        println!("the database is what the declaration implies");
        return Ok(());
    }
    if !plan.changes.is_empty() {
        println!(
            "{} statement(s) the declaration implies:\n",
            plan.changes.len()
        );
        println!("{}", render_sql(&plan));
    }
    if !plan.unresolved.is_empty() {
        eprintln!("{} left for a person:", plan.unresolved.len());
        report(&plan);
    }
    anyhow::bail!(
        "the database does not match the declaration; `donat tenancy plan` writes the migration"
    )
}

/// `plan`: write the migration, and name what it will not decide.
pub async fn plan(
    metadata_dir: &std::path::Path,
    database_url: &str,
    out: &std::path::Path,
    stamp: &str,
    backfill: Option<&str>,
) -> anyhow::Result<()> {
    let derived = derive(metadata_dir, database_url, backfill).await?;
    if !derived.unresolved.is_empty() {
        eprintln!(
            "{} object(s) this will not decide — settle them and re-run:",
            derived.unresolved.len()
        );
        report(&derived);
        eprintln!();
    }
    if derived.changes.is_empty() {
        println!("nothing to write: the database already carries what the declaration implies");
        return Ok(());
    }

    let path = out.join(format!("V{stamp}__tenancy.sql"));
    let body = format!(
        "-- Derived by `donat tenancy plan` from tenancy.yaml and the database.\n\
         -- Read it before committing: it is a migration like any other.\n\n{}",
        render_sql(&derived)
    );
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

// --------------------------------------------------------------- offboarding

use donat_schema::tenancy_offboard::{Offboarding, Reach, plan_offboarding};

async fn offboarding(
    metadata_dir: &std::path::Path,
    database_url: &str,
) -> anyhow::Result<(Offboarding, tokio_postgres::Client)> {
    let metadata = donat_metadata::load_metadata_dir(metadata_dir)
        .with_context(|| format!("loading metadata from {}", metadata_dir.display()))?;
    let (client, connection) = tokio_postgres::connect(database_url, crate::pgtls::connector())
        .await
        .context("connecting to database")?;
    tokio::spawn(connection);
    let catalog = donat_catalog::introspect(&client)
        .await
        .context("introspecting database")?;
    let plan = plan_offboarding(&metadata, &catalog);
    if !plan.refusals.is_empty() {
        for refusal in &plan.refusals {
            eprintln!("  {} {}", refusal.object, refusal.reason);
        }
        anyhow::bail!("the walk cannot be ordered; nothing was read or removed");
    }
    Ok((plan, client))
}

/// `SELECT`/`DELETE` over one table, however the tenant is reached on it.
fn predicate(reach: &Reach, table: &str, tenant: &str) -> String {
    match reach {
        Reach::Key(column) => format!("{} = {}", quote_ident(column), quote_literal(tenant)),
        // The declaration says these are reached through a relationship whose
        // remote carries the key; the join is that relationship spelled out.
        Reach::Via { remote, .. } => format!(
            "EXISTS (SELECT 1 FROM {remote} AS r WHERE r.tenant_id = {} \
             AND r.id::text = {table}.id::text)",
            quote_literal(tenant)
        ),
    }
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Read one tenant's rows out, in the reverse of the removal order — parents
/// before children, which is how a person reads them.
pub async fn export(
    metadata_dir: &std::path::Path,
    database_url: &str,
    tenant: &str,
    out: &std::path::Path,
) -> anyhow::Result<()> {
    let (plan, client) = offboarding(metadata_dir, database_url).await?;
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;

    let mut total = 0usize;
    for step in plan.steps.iter().rev() {
        let sql = format!(
            "SELECT coalesce(json_agg(t), '[]'::json) FROM {} AS t WHERE {}",
            step.table,
            predicate(&step.reach, "t", tenant)
        );
        let rows: serde_json::Value = client
            .query_one(&sql, &[])
            .await
            .with_context(|| format!("reading {}", step.table))?
            .get(0);
        let count = rows.as_array().map(Vec::len).unwrap_or_default();
        total += count;
        if count == 0 {
            continue;
        }
        let path = out.join(format!("{}.json", step.table));
        std::fs::write(&path, serde_json::to_vec_pretty(&rows)?)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    println!("wrote {total} row(s) for `{tenant}` to {}", out.display());
    println!(
        "not included, and no tool here can include them: rows in backups, and anything a \
         connector sent to an upstream."
    );
    Ok(())
}

/// Take one tenant away, children first.
pub async fn erase(
    metadata_dir: &std::path::Path,
    database_url: &str,
    tenant: &str,
    confirm: &str,
) -> anyhow::Result<()> {
    if confirm != tenant {
        anyhow::bail!(
            "`--confirm` has to repeat the tenant id. A flag that is merely present is a flag \
             that gets pasted."
        );
    }
    let metadata = donat_metadata::load_metadata_dir(metadata_dir)
        .with_context(|| format!("loading metadata from {}", metadata_dir.display()))?;
    let tenancy = metadata
        .tenancy
        .as_ref()
        .context("this deployment declares no tenancy, so it has no tenant to remove")?;

    let (plan, mut client) = offboarding(metadata_dir, database_url).await?;

    // Two deliberate acts with a gap between them, and the gap is where
    // somebody notices.
    let serving: Vec<String> = tenancy
        .registry
        .status
        .serving
        .iter()
        .map(|value| quote_literal(value))
        .collect();
    let still_serving: bool = client
        .query_one(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM {} WHERE {} = {} AND {} IN ({}))",
                tenancy.registry.table,
                quote_ident(&tenancy.registry.key),
                quote_literal(tenant),
                quote_ident(&tenancy.registry.status.column),
                serving.join(", ")
            ),
            &[],
        )
        .await
        .context("reading the registry")?
        .get(0);
    if still_serving {
        anyhow::bail!(
            "`{tenant}` is still being served. Stop serving it in the registry first, so removal \
             is two deliberate acts rather than one."
        );
    }

    let transaction = client
        .build_transaction()
        .start()
        .await
        .context("beginning")?;
    let mut total = 0u64;
    for step in &plan.steps {
        let sql = format!(
            "DELETE FROM {} AS t WHERE {}",
            step.table,
            predicate(&step.reach, "t", tenant)
        );
        let removed = transaction
            .execute(&sql, &[])
            .await
            .with_context(|| format!("removing from {}", step.table))?;
        if removed > 0 {
            println!("  {:<40} {removed}", step.table);
        }
        total += removed;
    }
    transaction.commit().await.context("committing")?;
    println!("removed {total} row(s) for `{tenant}`");
    println!(
        "still there, and no tool here can reach them: rows in backups, and anything a connector \
         sent to an upstream."
    );
    Ok(())
}
