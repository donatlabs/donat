//! Read-only serving catalog for deployed Process revisions.
//!
//! Serving never reconciles or repairs this state. It verifies the
//! migration-owned helper and every active/live-retired revision against the
//! freshly compiled immutable candidate before publication.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::{Context, bail};
use donat_metadata::{Metadata, Process, ProcessLifecycle, SourceKind};
use donat_schema::{CompiledCommandCatalog, CompiledSourceCommandCatalog};

use crate::connectors::ConnectorRegistry;
use crate::state::SourceRuntime;

use super::{
    CompiledProcessCatalog, CompiledProcessDefinition, PROCESS_RUNTIME_ABI_EPOCH,
    compile_process_source_catalog, process_dependency_descriptors,
};

#[derive(Debug, Clone, Default)]
pub struct DeployedProcessCatalog {
    pub sources: BTreeMap<String, DeployedSourceProcessCatalog>,
}

impl DeployedProcessCatalog {
    pub fn source(&self, source_name: &str) -> Option<&DeployedSourceProcessCatalog> {
        self.sources.get(source_name)
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeployedSourceProcessCatalog {
    pub active: BTreeMap<String, Arc<CompiledProcessDefinition>>,
    pub live_retired: BTreeMap<(String, String), Arc<CompiledProcessDefinition>>,
}

impl DeployedSourceProcessCatalog {
    pub fn revision(
        &self,
        process_name: &str,
        revision: &str,
    ) -> Option<&Arc<CompiledProcessDefinition>> {
        self.active
            .get(process_name)
            .filter(|process| process.revision_fingerprint == revision)
            .or_else(|| {
                self.live_retired
                    .get(&(process_name.to_owned(), revision.to_owned()))
            })
    }
}

/// Verify the migration-owned permission helper without issuing DDL or DML.
pub async fn validate_check_violation_helper(
    client: &tokio_postgres::Client,
) -> anyhow::Result<()> {
    let compatible = client
        .query_opt(
            "
            SELECT
                pg_get_function_identity_arguments(procedure_.oid) = 'msg text',
                procedure_.prorettype = 'json'::regtype,
                language.lanname = 'plpgsql',
                has_function_privilege(current_user, procedure_.oid, 'EXECUTE')
            FROM pg_proc procedure_
            JOIN pg_namespace namespace
              ON namespace.oid = procedure_.pronamespace
            JOIN pg_language language
              ON language.oid = procedure_.prolang
            WHERE namespace.nspname = 'donat'
              AND procedure_.proname = 'check_violation'
              AND procedure_.prokind = 'f'
              AND procedure_.pronargs = 1
              AND procedure_.proargtypes[0] = 'text'::regtype
            ",
            &[],
        )
        .await
        .context("validating migration-owned donat.check_violation")?
        .is_some_and(|row| {
            row.get::<_, bool>(0)
                && row.get::<_, bool>(1)
                && row.get::<_, bool>(2)
                && row.get::<_, bool>(3)
        });
    if !compatible {
        bail!(
            "donat.check_violation(text) is missing, incompatible, or not executable; run `donat migrate` before serving"
        );
    }
    Ok(())
}

/// Validate every real Postgres source and build the exact immutable catalog
/// published to workers. No write query is issued by this function.
pub async fn validate_serving_catalogs(
    runtimes: &HashMap<String, SourceRuntime>,
    metadata: &Metadata,
    rules: &donat_rules::RuleCatalog,
    process_catalog: &CompiledProcessCatalog,
    command_catalog: &CompiledCommandCatalog,
    connectors: &ConnectorRegistry,
) -> anyhow::Result<DeployedProcessCatalog> {
    let mut sources = BTreeMap::new();
    for source in metadata
        .sources
        .iter()
        .filter(|source| source.kind == SourceKind::Postgres)
    {
        let runtime = runtimes.get(&source.name).ok_or_else(|| {
            anyhow::anyhow!(
                "Postgres runtime for Process source `{}` is missing",
                source.name
            )
        })?;
        let SourceRuntime::Postgres { pool, .. } = runtime else {
            bail!(
                "Process source `{}` is Postgres but its runtime is not",
                source.name
            );
        };
        let client = pool
            .get()
            .await
            .with_context(|| format!("checking Process source `{}`", source.name))?;
        validate_check_violation_helper(&client).await?;
        let commands = command_catalog.source(&source.name).ok_or_else(|| {
            anyhow::anyhow!(
                "compiled Command catalog for Process source `{}` is missing",
                source.name
            )
        })?;
        let compiled = process_catalog.source(&source.name);
        let deployed = load_source_catalog(
            &client,
            metadata,
            rules,
            &source.name,
            compiled,
            commands,
            connectors,
        )
        .await?;
        sources.insert(source.name.clone(), deployed);
    }
    Ok(DeployedProcessCatalog { sources })
}

async fn load_source_catalog(
    client: &tokio_postgres::Client,
    metadata: &Metadata,
    rules: &donat_rules::RuleCatalog,
    source_name: &str,
    current: Option<&super::CompiledSourceProcessCatalog>,
    commands: &CompiledSourceCommandCatalog,
    connectors: &ConnectorRegistry,
) -> anyhow::Result<DeployedSourceProcessCatalog> {
    let rows = client
        .query(
            "
            SELECT
                definition.process_name,
                definition.revision,
                definition.canonical_definition,
                definition.dependency_descriptors,
                definition.runtime_abi,
                definition.status,
                (
                    EXISTS (
                        SELECT 1
                        FROM donat.process_instances instance
                        WHERE instance.source_name = definition.source_name
                          AND instance.process_name = definition.process_name
                          AND instance.revision = definition.revision
                          AND instance.status = 'running'
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM donat.process_start_requests request
                        WHERE request.source_name = definition.source_name
                          AND request.process_name = definition.process_name
                          AND request.revision = definition.revision
                          AND request.status = 'pending'
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM donat.process_signal_requests request
                        WHERE request.source_name = definition.source_name
                          AND request.process_name = definition.process_name
                          AND request.process_revision = definition.revision
                          AND request.status = 'pending'
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM donat.process_events event
                        WHERE event.source_name = definition.source_name
                          AND event.process_name = definition.process_name
                          AND event.revision = definition.revision
                          AND event.status = 'pending'
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM donat.process_activity_jobs job
                        JOIN donat.process_instances instance
                          ON instance.source_name = job.source_name
                         AND instance.id = job.instance_id
                        WHERE job.source_name = definition.source_name
                          AND instance.process_name = definition.process_name
                          AND instance.revision = definition.revision
                          AND job.status IN ('scheduled', 'running')
                    )
                ) AS has_live_work
            FROM donat.process_definition_versions definition
            WHERE definition.source_name = $1
            ORDER BY definition.process_name, definition.revision
            ",
            &[&source_name],
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "deployed Process catalog for source `{source_name}` is unavailable; run `donat migrate`: {error}"
            )
        })?;

    let current_by_key = current
        .into_iter()
        .flat_map(|catalog| catalog.iter())
        .map(|(name, process)| {
            (
                (name.clone(), process.revision_fingerprint.clone()),
                process,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut seen_current = BTreeSet::new();
    let mut active = BTreeMap::new();
    let mut live_retired = BTreeMap::new();

    for row in rows {
        let process_name: String = row.get(0);
        let revision: String = row.get(1);
        let canonical_definition: serde_json::Value = row.get(2);
        let dependency_descriptors: serde_json::Value = row.get(3);
        let runtime_abi: i32 = row.get(4);
        let status: String = row.get(5);
        let has_live_work: bool = row.get(6);
        let key = (process_name.clone(), revision.clone());
        let relevant = status == "active" || has_live_work;

        if !relevant {
            continue;
        }
        if runtime_abi != PROCESS_RUNTIME_ABI_EPOCH as i32 {
            bail!(
                "deployed Process `{source_name}.{process_name}` revision `{revision}` uses runtime ABI {runtime_abi}, binary requires {}",
                PROCESS_RUNTIME_ABI_EPOCH
            );
        }

        let process = match current_by_key.get(&key) {
            Some(process) => {
                verify_persisted_definition(
                    source_name,
                    process,
                    &canonical_definition,
                    &dependency_descriptors,
                )?;
                seen_current.insert(key.clone());
                Arc::new((*process).clone())
            }
            None if status == "active" => {
                bail!(
                    "database has active Process `{source_name}.{process_name}` revision `{revision}` that is not active in metadata; run `donat migrate --metadata-dir ... --source {source_name}`"
                );
            }
            None => Arc::new(recompile_retired_definition(
                metadata,
                rules,
                source_name,
                commands,
                connectors,
                &process_name,
                &revision,
                canonical_definition,
                &dependency_descriptors,
            )?),
        };

        if status == "active" {
            if process.definition.lifecycle != ProcessLifecycle::Active {
                bail!(
                    "Process `{source_name}.{process_name}` is active in the database but retired in metadata"
                );
            }
            if active.insert(process_name.clone(), process).is_some() {
                bail!(
                    "source `{source_name}` has more than one active revision for Process `{process_name}`"
                );
            }
        } else if has_live_work {
            live_retired.insert(key, process);
        }
    }

    if let Some(current) = current {
        for (process_name, process) in current.iter() {
            let key = (process_name.clone(), process.revision_fingerprint.clone());
            let expected_status = match process.definition.lifecycle {
                ProcessLifecycle::Active => "active",
                ProcessLifecycle::Retired => "retired",
            };
            let present = match expected_status {
                "active" => active
                    .get(process_name)
                    .is_some_and(|deployed| deployed.revision_fingerprint == key.1),
                _ => {
                    seen_current.contains(&key)
                        || live_retired.contains_key(&key)
                        || persisted_revision_exists(client, source_name, &key.0, &key.1, "retired")
                            .await?
                }
            };
            if !present {
                bail!(
                    "Process `{source_name}.{process_name}` revision `{}` is not deployed as {expected_status}; run `donat migrate --metadata-dir ... --source {source_name}`",
                    process.revision_fingerprint
                );
            }
        }
    }

    Ok(DeployedSourceProcessCatalog {
        active,
        live_retired,
    })
}

fn verify_persisted_definition(
    source_name: &str,
    process: &CompiledProcessDefinition,
    canonical_definition: &serde_json::Value,
    dependency_descriptors: &serde_json::Value,
) -> anyhow::Result<()> {
    let expected_definition = serde_json::to_value(&process.definition)
        .context("serializing current Process definition")?;
    let expected_dependencies =
        process_dependency_descriptors(&process.definition_fingerprint, &process.dependencies);
    if &expected_definition != canonical_definition
        || &expected_dependencies != dependency_descriptors
    {
        bail!(
            "deployed Process `{source_name}.{}` revision `{}` failed canonical definition/dependency verification",
            process.name,
            process.revision_fingerprint
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn recompile_retired_definition(
    metadata: &Metadata,
    rules: &donat_rules::RuleCatalog,
    source_name: &str,
    commands: &CompiledSourceCommandCatalog,
    connectors: &ConnectorRegistry,
    process_name: &str,
    revision: &str,
    canonical_definition: serde_json::Value,
    dependency_descriptors: &serde_json::Value,
) -> anyhow::Result<CompiledProcessDefinition> {
    let definition: Process =
        serde_json::from_value(canonical_definition.clone()).with_context(|| {
            format!(
                "decoding live-retired Process `{source_name}.{process_name}` revision `{revision}`"
            )
        })?;
    if definition.source != source_name || definition.name != process_name {
        bail!(
            "persisted Process identity does not match `{source_name}.{process_name}` revision `{revision}`"
        );
    }

    let mut retained_metadata = metadata.clone();
    retained_metadata
        .processes
        .retain(|process| process.source != source_name || process.name != process_name);
    retained_metadata.processes.push(definition);
    let compiled = compile_process_source_catalog(
        &retained_metadata,
        source_name,
        commands,
        rules,
        connectors,
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "live-retired Process `{source_name}.{process_name}` cannot be recompiled at {}: {}",
            error.path,
            error.message
        )
    })?;
    let process = compiled.process(process_name).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "live-retired Process `{source_name}.{process_name}` disappeared during recompilation"
        )
    })?;
    if process.revision_fingerprint != revision {
        bail!(
            "live-retired Process `{source_name}.{process_name}` recompiled to revision `{}`, expected `{revision}`",
            process.revision_fingerprint
        );
    }
    verify_persisted_definition(
        source_name,
        &process,
        &canonical_definition,
        dependency_descriptors,
    )?;
    Ok(process)
}

async fn persisted_revision_exists(
    client: &tokio_postgres::Client,
    source_name: &str,
    process_name: &str,
    revision: &str,
    status: &str,
) -> anyhow::Result<bool> {
    Ok(client
        .query_one(
            "
            SELECT EXISTS (
                SELECT 1
                FROM donat.process_definition_versions
                WHERE source_name = $1
                  AND process_name = $2
                  AND revision = $3
                  AND status = $4
            )
            ",
            &[&source_name, &process_name, &revision, &status],
        )
        .await
        .context("checking deployed Process revision status")?
        .get(0))
}
