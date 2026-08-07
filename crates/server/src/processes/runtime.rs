//! Immutable, source-local Process runtime construction and worker lifecycle.
//!
//! A runtime is built only from one published [`crate::state::Engine`]
//! snapshot. It retains the exact deployed revisions and command/connector
//! catalogs that were validated together; workers never consult mutable
//! metadata or reconstruct a dependency while consuming journal rows.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use donat_catalog::Catalog;
use donat_metadata::Metadata;
use donat_rules::RuleCatalog;
use donat_schema::{
    CompiledCommandCatalog, CompiledMultiSourceSchema, FinalizedCommandCatalog, Planner,
};

use uuid::Uuid;

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
    /// Instances this process is already working on.
    ///
    /// `SKIP LOCKED` is what keeps two *deployments* off one instance; this is
    /// what keeps two workers of the same deployment from preparing the same
    /// transition and having one of them throw the work away.
    pub(crate) in_flight: Arc<Mutex<HashSet<Uuid>>>,
}

/// Releases the instance when the worker is done with it, however it ends.
pub(crate) struct InFlightGuard {
    in_flight: Arc<Mutex<HashSet<Uuid>>>,
    instance_id: Uuid,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Ok(mut in_flight) = self.in_flight.lock() {
            in_flight.remove(&self.instance_id);
        }
    }
}

impl ProcessRuntime {
    /// Claim an instance for this worker, or report that another worker of
    /// this deployment already has it.
    pub(crate) fn claim_in_flight(&self, instance_id: Uuid) -> Option<InFlightGuard> {
        let mut in_flight = self.in_flight.lock().ok()?;
        if !in_flight.insert(instance_id) {
            return None;
        }
        Some(InFlightGuard {
            in_flight: Arc::clone(&self.in_flight),
            instance_id,
        })
    }

    /// The instances other workers are holding right now.
    pub(crate) fn busy_instances(&self) -> Vec<Uuid> {
        self.in_flight
            .lock()
            .map(|in_flight| in_flight.iter().copied().collect())
            .unwrap_or_default()
    }
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
        in_flight: Arc::new(Mutex::new(HashSet::new())),
    })
}

/// Spawn one consumer loop per Postgres source that owns a deployed Process.
///
/// The immutable Engine snapshot is captured before any task is spawned.
/// Polling is only a wake-up mechanism; correctness lives entirely in the
/// source-local journal transaction.
pub async fn spawn(
    state: SharedState,
    shutdown: tokio_util::sync::CancellationToken,
    tasks: &tokio_util::task::TaskTracker,
) -> anyhow::Result<()> {
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
        let workers = transition_concurrency(&runtime.pool);
        tracing::info!(
            source = %runtime.source_name,
            poll_milliseconds = poll_interval.as_millis() as u64,
            transition_workers = workers,
            "Process worker started"
        );
        let runtime = Arc::new(runtime);
        for _ in 0..workers {
            let runtime = Arc::clone(&runtime);
            let worker_shutdown = shutdown.clone();
            tasks.spawn(supervise(
                runtime.source_name.clone(),
                "transition",
                shutdown.clone(),
                move || {
                    transition_worker(Arc::clone(&runtime), poll_interval, worker_shutdown.clone())
                },
            ));
        }
        let loops = Arc::clone(&runtime);
        let loop_shutdown = shutdown.clone();
        tasks.spawn(supervise(
            runtime.source_name.clone(),
            "source",
            shutdown.clone(),
            move || run(Arc::clone(&loops), poll_interval, loop_shutdown.clone()),
        ));
    }
    Ok(())
}

/// How long to wait before starting a worker that died back up. Long enough
/// that a worker panicking on every iteration cannot become a hot loop, short
/// enough that a deployment does not notice the gap.
const WORKER_RESTART_DELAY: Duration = Duration::from_millis(250);

/// Keep one worker running for the life of the process.
///
/// Workers are spawned tasks, and a spawned task that panics is simply gone.
/// Several of them lose themselves one at a time, with nothing to show for it
/// but a deployment that gets slower and eventually stops — the failure this
/// whole queue exists to prevent, arriving by another route. So a worker that
/// dies is started again, and the log says which one and why.
async fn supervise<F, Fut>(
    source: String,
    worker: &'static str,
    shutdown: tokio_util::sync::CancellationToken,
    make: F,
) where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    loop {
        match tokio::spawn(make()).await {
            // A worker that returned because the deployment is stopping has
            // finished its item and declined the next one. Restarting it here
            // would undo exactly the drain the signal asked for.
            Ok(()) if shutdown.is_cancelled() => {
                tracing::info!(%source, worker, "Process worker drained");
                return;
            }
            Ok(()) => {
                tracing::warn!(%source, worker, "Process worker returned; starting it again");
            }
            Err(joined) if joined.is_panic() => {
                tracing::error!(
                    %source,
                    worker,
                    "Process worker panicked; starting it again"
                );
            }
            // Cancelled: the runtime is going away, and so is this.
            Err(_) => return,
        }
        if !crate::shutdown::idle(WORKER_RESTART_DELAY, &shutdown).await {
            return;
        }
    }
}

/// One transition worker: take the next due transition, apply it, repeat.
///
/// Several of these run per source. The queue itself is the hand-off — each
/// worker claims a different instance with `SKIP LOCKED` — so a transition that
/// takes a second is that instance's second and nobody else's. A worker that
/// finds nothing waits out one poll interval before asking again.
async fn transition_worker(
    runtime: Arc<ProcessRuntime>,
    poll_interval: Duration,
    shutdown: tokio_util::sync::CancellationToken,
) {
    loop {
        // Claiming another instance now would take a lease this process is
        // about to stop renewing. The one already in hand has been applied.
        if shutdown.is_cancelled() {
            return;
        }
        let mut progressed = false;
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
            Ok(TransitionConsumption::Deferred {
                instance_id,
                event_id,
                attempts,
                delay_milliseconds,
            }) => {
                // Progress: the queue moved on. `defer_transition` has already
                // logged the cause (ADR 031) — this is the loop's own record
                // that it did not stall.
                progressed = true;
                tracing::debug!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    attempts,
                    delay_milliseconds,
                    "Process transition deferred; the queue continues"
                );
            }
            Ok(TransitionConsumption::CommandFailed {
                instance_id,
                event_id,
                code,
            }) => {
                progressed = true;
                tracing::warn!(
                    source = %runtime.source_name,
                    %instance_id,
                    %event_id,
                    code,
                    "Process command state failed unrecoverably"
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
            }
        }
        if !progressed && !crate::shutdown::idle(poll_interval, &shutdown).await {
            return;
        }
    }
}

async fn run(
    runtime: Arc<ProcessRuntime>,
    poll_interval: Duration,
    shutdown: tokio_util::sync::CancellationToken,
) {
    loop {
        if shutdown.is_cancelled() {
            return;
        }
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
                if !crate::shutdown::idle(poll_interval, &shutdown).await {
                    return;
                }
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
                if !crate::shutdown::idle(poll_interval, &shutdown).await {
                    return;
                }
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
                if !crate::shutdown::idle(poll_interval, &shutdown).await {
                    return;
                }
                continue;
            }
        }
        if !progressed && !crate::shutdown::idle(poll_interval, &shutdown).await {
            return;
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

/// How many transitions one deployment applies at a time.
///
/// The queue is a work queue, not a line: a transition that takes a second is
/// that instance's second, not everybody's. The bound is what keeps a burst of
/// due timers from opening a connection per instance — the same reason an HTTP
/// server has a worker pool rather than a thread per request.
fn transition_concurrency(pool: &deadpool_postgres::Pool) -> usize {
    // A transition worker spends its life waiting on the database, so what
    // bounds it is connections, not cores — the same thing that bounds an HTTP
    // server talking to one database. Scaling up therefore means giving the
    // source more connections (`max_connections` on the source), and the
    // worker count follows.
    //
    // Half the pool, because the pool is shared: API requests, the start,
    // signal, activity and timer consumers all draw from it. Workers that took
    // every connection would starve the surface they exist to serve, and a
    // worker that cannot get a connection would spend its attempt waiting for
    // one.
    transition_worker_count(
        pool.status().max_size,
        std::env::var("DONAT_PROCESS_TRANSITION_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0),
    )
}

/// Half the pool by default, and never the whole of it.
fn transition_worker_count(pool_size: usize, configured: Option<usize>) -> usize {
    let ceiling = pool_size.saturating_sub(1).max(1);
    configured.unwrap_or((pool_size / 2).max(1)).min(ceiling)
}

/// Apply up to `concurrency` transitions at once, and report what each worker
/// did. Workers claim different instances — one instance's transitions stay
/// serialized, everything else overlaps.
pub async fn consume_transitions_concurrently(
    runtime: &Arc<ProcessRuntime>,
    concurrency: usize,
) -> Vec<anyhow::Result<TransitionConsumption>> {
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..concurrency.max(1) {
        let runtime = Arc::clone(runtime);
        workers.spawn(async move { runtime.consume_one_transition().await });
    }
    let mut outcomes = Vec::new();
    while let Some(joined) = workers.join_next().await {
        match joined {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => outcomes.push(Err(anyhow::anyhow!(
                "Process transition worker panicked: {error}"
            ))),
        }
    }
    outcomes
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

#[cfg(test)]
mod worker_pool_tests {
    use super::{supervise, transition_worker_count};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Transition workers wait on the database, so the pool is what bounds
    /// them — the same bound an HTTP server hits against one database. Scaling
    /// up is a matter of giving the source more connections.
    #[test]
    fn workers_follow_the_pool_and_never_take_all_of_it() {
        assert_eq!(transition_worker_count(16, None), 8);
        assert_eq!(transition_worker_count(64, None), 32);
        assert_eq!(transition_worker_count(200, None), 100);
        // The pool is shared with the API and the other consumers: a worker
        // per connection would starve the surface the workers exist to serve.
        assert_eq!(transition_worker_count(16, Some(1_000)), 15);
        assert_eq!(transition_worker_count(4, Some(4)), 3);
        // A pool too small to share still runs one worker rather than none.
        assert_eq!(transition_worker_count(1, None), 1);
        assert_eq!(transition_worker_count(2, None), 1);
        // And an operator may ask for fewer.
        assert_eq!(transition_worker_count(32, Some(2)), 2);
    }

    /// A worker that panics is started again rather than quietly lost.
    ///
    /// Without this, N spawned workers disappear one panic at a time and the
    /// only symptom is a deployment that gets slower — the failure the queue
    /// exists to prevent, arriving by another route.
    #[tokio::test]
    async fn a_worker_that_panics_is_started_again() {
        let starts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&starts);
        let supervisor = tokio::spawn(supervise(
            "test".to_owned(),
            "unit",
            tokio_util::sync::CancellationToken::new(),
            move || {
                let counted = Arc::clone(&counted);
                async move {
                    // Panics the first two times it is started, then stays up.
                    if counted.fetch_add(1, Ordering::SeqCst) < 2 {
                        panic!("a worker fell over");
                    }
                    std::future::pending::<()>().await;
                }
            },
        ));

        // Two restarts, each after the supervisor's delay, plus slack.
        for _ in 0..20 {
            tokio::time::sleep(super::WORKER_RESTART_DELAY).await;
            if starts.load(Ordering::SeqCst) >= 3 {
                break;
            }
        }
        supervisor.abort();

        assert!(
            starts.load(Ordering::SeqCst) >= 3,
            "a panicking worker was not restarted: {} start(s)",
            starts.load(Ordering::SeqCst)
        );
    }

    /// A worker that returned because the deployment is stopping must stay
    /// stopped. The supervisor's whole job is to start workers again, so it is
    /// also the one thing that can undo a drain.
    #[tokio::test]
    async fn a_drained_worker_is_not_started_again() {
        let starts = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&starts);
        let shutdown = tokio_util::sync::CancellationToken::new();
        shutdown.cancel();

        let supervisor = tokio::spawn(supervise("test".to_owned(), "unit", shutdown, move || {
            let counted = Arc::clone(&counted);
            // Returns immediately, the way a worker does once it sees that
            // the deployment is stopping.
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        }));

        tokio::time::timeout(std::time::Duration::from_secs(5), supervisor)
            .await
            .expect("the supervisor returns instead of restarting forever")
            .expect("the supervisor did not panic");
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "a drained worker must be started exactly once"
        );
    }
}
