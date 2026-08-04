//! Deploy-time reconciliation of immutable Process revisions.
//!
//! This module is intentionally unreachable from request handlers and serving
//! startup. It performs all writes in one source-local transaction after the
//! selected source has been migrated, introspected, and compiled.

use std::collections::BTreeSet;

use anyhow::{Context, bail};
use donat_metadata::ProcessLifecycle;

use super::{
    CompiledProcessDefinition, CompiledSourceProcessCatalog, PROCESS_RUNTIME_ABI_EPOCH,
    process_dependency_descriptors,
};

/// Reconcile one selected metadata source. No catalog or definition belonging
/// to another source is read as a substitute for `source_name`.
pub async fn reconcile(
    source_name: &str,
    database_url: &str,
    _source_catalog: &donat_catalog::Catalog,
    compiled_processes: &CompiledSourceProcessCatalog,
) -> anyhow::Result<()> {
    let (mut client, connection) = tokio_postgres::connect(database_url, crate::pgtls::connector())
        .await
        .context("connecting to selected source for Process reconciliation")?;
    let connection = tokio::spawn(connection);
    let transaction = client
        .transaction()
        .await
        .context("starting Process reconciliation transaction")?;

    // Multiple deployers targeting the same physical database serialize per
    // metadata source while different source names remain independent.
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&format!("donat.process.reconcile.v1:{source_name}")],
        )
        .await
        .context("locking selected Process source")?;

    let active_rows = transaction
        .query(
            "
            SELECT process_name, revision
            FROM donat.process_definition_versions
            WHERE source_name = $1 AND status = 'active'
            ORDER BY process_name, revision
            FOR UPDATE
            ",
            &[&source_name],
        )
        .await
        .map_err(migration_required)?;
    let desired_names = compiled_processes
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();

    // Omission is not a retirement declaration. It is accepted only when no
    // non-terminal work can still require the deployed revision.
    for row in active_rows {
        let process_name: String = row.get(0);
        let revision: String = row.get(1);
        if desired_names.contains(&process_name) {
            continue;
        }
        if has_live_process_work(&transaction, source_name, &process_name).await? {
            bail!(
                "cannot omit Process `{source_name}.{process_name}` revision `{revision}` while non-terminal work exists; declare lifecycle: retired"
            );
        }
        retire_revision(&transaction, source_name, &process_name, &revision).await?;
    }

    for (process_name, process) in compiled_processes.iter() {
        if process.source != source_name {
            bail!(
                "compiled Process `{}` belongs to source `{}`, not selected source `{source_name}`",
                process.name,
                process.source
            );
        }
        persist_or_verify_revision(&transaction, process).await?;

        // A process name has at most one active revision. Retiring the prior
        // row first avoids depending on constraint deferral for the partial
        // unique index.
        transaction
            .execute(
                "
                UPDATE donat.process_definition_versions
                SET status = 'retired',
                    retired_at = COALESCE(retired_at, statement_timestamp())
                WHERE source_name = $1
                  AND process_name = $2
                  AND status = 'active'
                  AND revision <> $3
                ",
                &[&source_name, process_name, &process.revision_fingerprint],
            )
            .await
            .context("retiring superseded Process revision")?;

        match process.definition.lifecycle {
            ProcessLifecycle::Active => {
                transaction
                    .execute(
                        "
                        UPDATE donat.process_definition_versions
                        SET status = 'active', retired_at = NULL
                        WHERE source_name = $1
                          AND process_name = $2
                          AND revision = $3
                        ",
                        &[&source_name, process_name, &process.revision_fingerprint],
                    )
                    .await
                    .context("activating compiled Process revision")?;
            }
            ProcessLifecycle::Retired => {
                retire_revision(
                    &transaction,
                    source_name,
                    process_name,
                    &process.revision_fingerprint,
                )
                .await?;
            }
        }
    }

    transaction
        .commit()
        .await
        .context("committing Process reconciliation")?;
    connection.abort();
    Ok(())
}

async fn persist_or_verify_revision(
    transaction: &tokio_postgres::Transaction<'_>,
    process: &CompiledProcessDefinition,
) -> anyhow::Result<()> {
    let canonical_definition = serde_json::to_value(&process.definition)
        .context("serializing compiled Process definition")?;
    let dependencies =
        process_dependency_descriptors(&process.definition_fingerprint, &process.dependencies);
    let inserted = transaction
        .execute(
            "
            INSERT INTO donat.process_definition_versions (
                source_name,
                process_name,
                revision,
                canonical_definition,
                dependency_descriptors,
                runtime_abi,
                status
            )
            VALUES ($1, $2, $3, $4, $5, $6, 'retired')
            ON CONFLICT (source_name, process_name, revision) DO NOTHING
            ",
            &[
                &process.source,
                &process.name,
                &process.revision_fingerprint,
                &canonical_definition,
                &dependencies,
                &(PROCESS_RUNTIME_ABI_EPOCH as i32),
            ],
        )
        .await
        .map_err(migration_required)?;

    if inserted == 1 {
        return Ok(());
    }

    let row = transaction
        .query_one(
            "
            SELECT canonical_definition, dependency_descriptors, runtime_abi
            FROM donat.process_definition_versions
            WHERE source_name = $1
              AND process_name = $2
              AND revision = $3
            FOR UPDATE
            ",
            &[
                &process.source,
                &process.name,
                &process.revision_fingerprint,
            ],
        )
        .await
        .context("reading existing Process revision")?;
    let stored_definition: serde_json::Value = row.get(0);
    let stored_dependencies: serde_json::Value = row.get(1);
    let stored_runtime_abi: i32 = row.get(2);
    if stored_definition != canonical_definition
        || stored_dependencies != dependencies
        || stored_runtime_abi != PROCESS_RUNTIME_ABI_EPOCH as i32
    {
        bail!(
            "persisted Process revision collision for `{}.{}` revision `{}`",
            process.source,
            process.name,
            process.revision_fingerprint
        );
    }
    Ok(())
}

async fn retire_revision(
    transaction: &tokio_postgres::Transaction<'_>,
    source_name: &str,
    process_name: &str,
    revision: &str,
) -> anyhow::Result<()> {
    transaction
        .execute(
            "
            UPDATE donat.process_definition_versions
            SET status = 'retired',
                retired_at = COALESCE(retired_at, statement_timestamp())
            WHERE source_name = $1
              AND process_name = $2
              AND revision = $3
            ",
            &[&source_name, &process_name, &revision],
        )
        .await
        .context("retiring Process revision")?;
    Ok(())
}

async fn has_live_process_work(
    transaction: &tokio_postgres::Transaction<'_>,
    source_name: &str,
    process_name: &str,
) -> anyhow::Result<bool> {
    Ok(transaction
        .query_one(
            "
            SELECT
                EXISTS (
                    SELECT 1
                    FROM donat.process_instances instance
                    WHERE instance.source_name = $1
                      AND instance.process_name = $2
                      AND instance.status = 'running'
                )
                OR EXISTS (
                    SELECT 1
                    FROM donat.process_start_requests request
                    WHERE request.source_name = $1
                      AND request.process_name = $2
                      AND request.status = 'pending'
                )
                OR EXISTS (
                    SELECT 1
                    FROM donat.process_signal_requests request
                    WHERE request.source_name = $1
                      AND request.process_name = $2
                      AND request.status = 'pending'
                )
                OR EXISTS (
                    SELECT 1
                    FROM donat.process_events event
                    WHERE event.source_name = $1
                      AND event.process_name = $2
                      AND event.status = 'pending'
                )
                OR EXISTS (
                    SELECT 1
                    FROM donat.process_activity_jobs job
                    JOIN donat.process_instances instance
                      ON instance.source_name = job.source_name
                     AND instance.id = job.instance_id
                    WHERE job.source_name = $1
                      AND instance.process_name = $2
                      AND job.status IN ('scheduled', 'running')
                )
            ",
            &[&source_name, &process_name],
        )
        .await
        .context("checking live Process work across revisions")?
        .get(0))
}

fn migration_required(error: tokio_postgres::Error) -> anyhow::Error {
    anyhow::anyhow!("Process catalog is unavailable or incompatible; run `donat migrate`: {error}")
}
