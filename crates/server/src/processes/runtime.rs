//! Immutable, source-local Process runtime construction and worker lifecycle.
//!
//! A runtime is built only from one published [`crate::state::Engine`]
//! snapshot. It retains the exact deployed revisions and command/connector
//! catalogs that were validated together; workers never consult mutable
//! metadata or reconstruct a dependency while consuming journal rows.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use donat_schema::{CompiledCommandCatalog, FinalizedCommandCatalog};

use crate::connectors::ConnectorRegistry;
use crate::state::{SharedState, SourceRuntime};

use super::{DeployedSourceProcessCatalog, StartConsumption};

pub struct ProcessRuntime {
    pub source_name: String,
    pub pool: deadpool_postgres::Pool,
    pub deployed_catalog: Arc<DeployedSourceProcessCatalog>,
    // Task 10 consumes both catalogs from this same immutable snapshot.
    #[allow(dead_code)]
    pub command_catalog: Arc<CompiledCommandCatalog>,
    #[allow(dead_code)]
    pub finalized_command_catalog: Arc<FinalizedCommandCatalog>,
    #[allow(dead_code)]
    pub connector_registry: Arc<ConnectorRegistry>,
}

pub fn build_process_runtime(
    source_name: &str,
    source_runtime: &SourceRuntime,
    deployed_catalog: Arc<DeployedSourceProcessCatalog>,
    command_catalog: Arc<CompiledCommandCatalog>,
    finalized_command_catalog: Arc<FinalizedCommandCatalog>,
    connector_registry: Arc<ConnectorRegistry>,
) -> anyhow::Result<ProcessRuntime> {
    let SourceRuntime::Postgres { pool, .. } = source_runtime else {
        bail!("Process source `{source_name}` must use Postgres");
    };

    for definition in deployed_catalog
        .active
        .values()
        .chain(deployed_catalog.live_retired.values())
    {
        if definition.source != source_name {
            bail!(
                "deployed Process `{}.{}` cannot be installed in source runtime `{source_name}`",
                definition.source,
                definition.name
            );
        }
    }

    Ok(ProcessRuntime {
        source_name: source_name.to_owned(),
        pool: pool.clone(),
        deployed_catalog,
        command_catalog,
        finalized_command_catalog,
        connector_registry,
    })
}

/// Spawn one consumer loop per Postgres source that owns a deployed Process.
///
/// The immutable Engine snapshot is captured before any task is spawned.
/// Polling is only a wake-up mechanism; correctness lives entirely in the
/// source-local journal transaction.
pub async fn spawn(state: SharedState) -> anyhow::Result<()> {
    if process_workers_disabled() {
        tracing::info!("Process workers disabled by deployment configuration");
        return Ok(());
    }

    let engine = state.engine_snapshot().await;
    let mut runtimes = Vec::new();
    for (source_name, deployed_catalog) in &engine.deployed_process_catalog.sources {
        if deployed_catalog.active.is_empty() && deployed_catalog.live_retired.is_empty() {
            continue;
        }
        let source_runtime = engine.runtimes.get(source_name).with_context(|| {
            format!("runtime for deployed Process source `{source_name}` is missing")
        })?;
        runtimes.push(build_process_runtime(
            source_name,
            source_runtime,
            Arc::new(deployed_catalog.clone()),
            engine.command_catalog.clone(),
            engine.finalized_command_catalog.clone(),
            state.connectors.clone(),
        )?);
    }

    let poll_interval = process_poll_interval();
    for runtime in runtimes {
        tracing::info!(
            source = %runtime.source_name,
            poll_milliseconds = poll_interval.as_millis() as u64,
            "Process worker started"
        );
        tokio::spawn(run(runtime, poll_interval));
    }
    Ok(())
}

async fn run(runtime: ProcessRuntime, poll_interval: Duration) {
    loop {
        match runtime.consume_one_start().await {
            Ok(StartConsumption::NoWork) => {
                tokio::time::sleep(poll_interval).await;
            }
            Ok(StartConsumption::Started {
                request_id,
                instance_id,
            }) => {
                tracing::debug!(
                    source = %runtime.source_name,
                    %request_id,
                    %instance_id,
                    "Process start request consumed"
                );
            }
            Ok(StartConsumption::Duplicate {
                request_id,
                instance_id,
            }) => {
                tracing::debug!(
                    source = %runtime.source_name,
                    %request_id,
                    %instance_id,
                    "duplicate Process start request consumed"
                );
            }
            Err(error) => {
                tracing::error!(
                    source = %runtime.source_name,
                    error = %error,
                    "Process start consumer failed"
                );
                tokio::time::sleep(poll_interval).await;
            }
        }
    }
}

fn process_poll_interval() -> Duration {
    Duration::from_millis(
        std::env::var("DONAT_PROCESS_POLL_MILLISECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(250)
            .max(10),
    )
}

fn process_workers_disabled() -> bool {
    std::env::var("DONAT_PROCESS_WORKERS_DISABLED")
        .ok()
        .is_some_and(|value| {
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
}
