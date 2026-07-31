//! Immutable, source-local Process runtime construction and worker lifecycle.
//!
//! A runtime is built only from one published [`crate::state::Engine`]
//! snapshot. It retains the exact deployed revisions and command/connector
//! catalogs that were validated together; workers never consult mutable
//! metadata or reconstruct a dependency while consuming journal rows.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use donat_catalog::Catalog;
use donat_metadata::Metadata;
use donat_rules::RuleCatalog;
use donat_schema::{
    CompiledCommandCatalog, CompiledMultiSourceSchema, FinalizedCommandCatalog, Planner,
};

use crate::connectors::ConnectorRegistry;
use crate::state::{SharedState, SourceRuntime};

use super::{
    ActivityConsumption, DeployedSourceProcessCatalog, ProcessActivityExecutor, SignalConsumption,
    StartConsumption, TransitionConsumption,
};

/// Immutable planning inputs captured from the same published Engine
/// candidate as the deployed Process and finalized Command catalogs.
#[derive(Clone)]
pub struct ProcessPlanningSnapshot {
    metadata: Arc<Metadata>,
    catalogs: Arc<HashMap<String, Catalog>>,
    compiled: Arc<CompiledMultiSourceSchema>,
    rules: Arc<RuleCatalog>,
}

impl ProcessPlanningSnapshot {
    pub fn new(
        metadata: Arc<Metadata>,
        catalogs: Arc<HashMap<String, Catalog>>,
        compiled: Arc<CompiledMultiSourceSchema>,
        rules: Arc<RuleCatalog>,
    ) -> Self {
        Self {
            metadata,
            catalogs,
            compiled,
            rules,
        }
    }

    pub(crate) fn planner(
        &self,
        source_name: &str,
    ) -> Result<Planner<'_>, donat_schema::PlanError> {
        self.compiled
            .source_planner(&self.metadata, &self.catalogs, source_name)
    }

    pub(crate) fn rules(&self) -> &RuleCatalog {
        self.rules.as_ref()
    }
}

pub struct ProcessRuntime {
    pub source_name: String,
    pub pool: deadpool_postgres::Pool,
    pub deployed_catalog: Arc<DeployedSourceProcessCatalog>,
    pub planning_snapshot: Arc<ProcessPlanningSnapshot>,
    pub command_catalog: Arc<CompiledCommandCatalog>,
    pub finalized_command_catalog: Arc<FinalizedCommandCatalog>,
    pub connector_registry: Arc<ConnectorRegistry>,
    pub activity_executor: Arc<dyn ProcessActivityExecutor>,
}

pub fn build_process_runtime(
    source_name: &str,
    source_runtime: &SourceRuntime,
    deployed_catalog: Arc<DeployedSourceProcessCatalog>,
    planning_snapshot: Arc<ProcessPlanningSnapshot>,
    command_catalog: Arc<CompiledCommandCatalog>,
    finalized_command_catalog: Arc<FinalizedCommandCatalog>,
    connector_registry: Arc<ConnectorRegistry>,
) -> anyhow::Result<ProcessRuntime> {
    build_process_runtime_with_activity_executor(
        source_name,
        source_runtime,
        deployed_catalog,
        planning_snapshot,
        command_catalog,
        finalized_command_catalog,
        connector_registry.clone(),
        connector_registry,
    )
}

/// Assemble a runtime with an explicit connector-execution boundary.
///
/// Serving uses the immutable [`ConnectorRegistry`] for both catalog and
/// execution. The separate argument keeps activity orchestration testable
/// without weakening the production registry or admitting raw HTTP.
#[allow(clippy::too_many_arguments)]
pub fn build_process_runtime_with_activity_executor(
    source_name: &str,
    source_runtime: &SourceRuntime,
    deployed_catalog: Arc<DeployedSourceProcessCatalog>,
    planning_snapshot: Arc<ProcessPlanningSnapshot>,
    command_catalog: Arc<CompiledCommandCatalog>,
    finalized_command_catalog: Arc<FinalizedCommandCatalog>,
    connector_registry: Arc<ConnectorRegistry>,
    activity_executor: Arc<dyn ProcessActivityExecutor>,
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
        planning_snapshot,
        command_catalog,
        finalized_command_catalog,
        connector_registry,
        activity_executor,
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
    let compiled = engine
        .compiled
        .clone()
        .context("Process workers require a compiled serving schema snapshot")?;
    let planning_snapshot = Arc::new(ProcessPlanningSnapshot::new(
        Arc::new(engine.metadata.clone()),
        Arc::new(engine.catalogs.clone()),
        compiled,
        engine.rule_catalog.clone(),
    ));
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
            planning_snapshot.clone(),
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
        let mut progressed = false;
        match runtime.consume_one_start().await {
            Ok(StartConsumption::NoWork) => {}
            Ok(StartConsumption::Started {
                request_id,
                instance_id,
            }) => {
                progressed = true;
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
                progressed = true;
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
                    error = format_args!("{error:#}"),
                    "Process start consumer failed"
                );
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        }
        match consume_one_signal(&runtime).await {
            Ok(signal_progressed) => progressed |= signal_progressed,
            Err(error) => {
                tracing::error!(
                    source = %runtime.source_name,
                    error = format_args!("{error:#}"),
                    "Process signal consumer failed"
                );
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        }
        match runtime.consume_one_transition().await {
            Ok(TransitionConsumption::NoWork) => {}
            Ok(TransitionConsumption::Advanced {
                instance_id,
                event_id,
                from_state,
                to_state,
            }) => {
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    from_state,
                    to_state,
                    "Process state advanced"
                );
            }
            Ok(TransitionConsumption::Completed {
                instance_id,
                event_id,
                state,
            }) => {
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    state,
                    "Process instance completed"
                );
            }
            Ok(TransitionConsumption::Failed {
                instance_id,
                event_id,
                state,
                code,
            }) => {
                progressed = true;
                tracing::warn!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    state,
                    code,
                    "Process instance failed explicitly"
                );
            }
            Ok(TransitionConsumption::CommandRejected {
                instance_id,
                event_id,
                error,
            }) => {
                progressed = true;
                tracing::warn!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    code = %error.code,
                    path = %error.path,
                    "Process command state rejected"
                );
            }
            Ok(TransitionConsumption::ActivityScheduled {
                instance_id,
                event_id,
                activity_job_id,
                state,
            }) => {
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    %activity_job_id,
                    state,
                    "Process activity scheduled"
                );
            }
            Ok(TransitionConsumption::FanOutExpanded {
                instance_id,
                event_id,
                state,
                item_count,
                scheduled_count,
            }) => {
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    state,
                    item_count,
                    scheduled_count,
                    "Process bounded fan-out expanded"
                );
            }
            Ok(TransitionConsumption::FanOutItemCompleted {
                instance_id,
                event_id,
                state,
                ordinal,
            }) => {
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    state,
                    ordinal,
                    "Process bounded fan-out item completed"
                );
            }
            Ok(TransitionConsumption::WaitEntered {
                instance_id,
                event_id,
                timer_event_id,
                state,
            }) => {
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    %timer_event_id,
                    state,
                    "Process wait became receptive"
                );
            }
            Err(error) => {
                tracing::error!(
                    source = %runtime.source_name,
                    error = format_args!("{error:#}"),
                    "Process transition consumer failed"
                );
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        }
        match runtime.consume_one_activity().await {
            Ok(ActivityConsumption::NoWork) => {}
            Ok(ActivityConsumption::CapacityDeferred { activity_job_id }) => {
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %activity_job_id,
                    "Process activity deferred by durable capacity"
                );
            }
            Ok(ActivityConsumption::ScheduleToStartTimedOut {
                instance_id,
                activity_job_id,
            }) => {
                progressed = true;
                tracing::warn!(
                    source = %runtime.source_name,
                    %instance_id,
                    %activity_job_id,
                    "Process activity schedule-to-start expired"
                );
            }
            Ok(ActivityConsumption::Succeeded {
                instance_id,
                activity_job_id,
                attempt,
                lease_generation,
            }) => {
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %instance_id,
                    %activity_job_id,
                    attempt,
                    lease_generation,
                    "Process activity completed"
                );
            }
            Ok(ActivityConsumption::RetryScheduled {
                instance_id,
                activity_job_id,
                failed_attempt,
                next_attempt,
            }) => {
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %instance_id,
                    %activity_job_id,
                    failed_attempt,
                    next_attempt,
                    "Process activity retry scheduled"
                );
            }
            Ok(ActivityConsumption::RetryExhausted {
                instance_id,
                activity_job_id,
                attempt,
                lease_generation,
                last_class,
            }) => {
                progressed = true;
                tracing::warn!(
                    source = %runtime.source_name,
                    %instance_id,
                    %activity_job_id,
                    attempt,
                    lease_generation,
                    ?last_class,
                    "Process activity retry budget exhausted"
                );
            }
            Ok(ActivityConsumption::Failed {
                instance_id,
                activity_job_id,
                attempt,
                lease_generation,
                class,
            }) => {
                progressed = true;
                tracing::warn!(
                    source = %runtime.source_name,
                    %instance_id,
                    %activity_job_id,
                    attempt,
                    lease_generation,
                    ?class,
                    "Process activity failed"
                );
            }
            Ok(ActivityConsumption::StaleCompletion {
                instance_id,
                activity_job_id,
                attempt,
                lease_generation,
            }) => {
                progressed = true;
                tracing::warn!(
                    source = %runtime.source_name,
                    %instance_id,
                    %activity_job_id,
                    attempt,
                    lease_generation,
                    "stale Process activity completion ignored"
                );
            }
            Err(error) => {
                tracing::error!(
                    source = %runtime.source_name,
                    error = format_args!("{error:#}"),
                    "Process activity consumer failed"
                );
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        }
        if !progressed {
            tokio::time::sleep(poll_interval).await;
        }
    }
}

async fn consume_one_signal(runtime: &ProcessRuntime) -> anyhow::Result<bool> {
    Ok(match runtime.consume_one_signal().await? {
        SignalConsumption::NoWork => false,
        SignalConsumption::Accepted {
            request_id,
            instance_id,
            event_id,
        } => {
            tracing::debug!(
                source = %runtime.source_name,
                %request_id,
                %instance_id,
                %event_id,
                "Process signal accepted"
            );
            true
        }
        SignalConsumption::Duplicate { request_id } => {
            tracing::debug!(
                source = %runtime.source_name,
                %request_id,
                "duplicate Process signal audited"
            );
            true
        }
        SignalConsumption::Unmatched { request_id } => {
            tracing::warn!(
                source = %runtime.source_name,
                %request_id,
                "unmatched Process signal audited"
            );
            true
        }
        SignalConsumption::Ambiguous { request_id } => {
            tracing::warn!(
                source = %runtime.source_name,
                %request_id,
                "ambiguous Process signal audited"
            );
            true
        }
        SignalConsumption::GuardFalse { request_id } => {
            tracing::warn!(
                source = %runtime.source_name,
                %request_id,
                "guard-false Process signal audited"
            );
            true
        }
        SignalConsumption::UnexpectedState { request_id } => {
            tracing::warn!(
                source = %runtime.source_name,
                %request_id,
                "unexpected-state Process signal audited"
            );
            true
        }
    })
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
