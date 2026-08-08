//! Stopping on purpose.
//!
//! A rolling deployment sends `SIGTERM` and then waits. Without an answer to
//! it the process dies where it stands: requests in flight become transport
//! errors their callers cannot distinguish from a network fault, and a durable
//! activity that had just taken a lease keeps it until the lease expires,
//! which is the one thing [[002-durable-process-operational-contracts]] asks a
//! deployment to avoid ("a rolling deployment must drain or fence
//! incompatible workers").
//!
//! Stopping happens in two phases, because a load balancer needs to be told
//! before it is true. On the signal the process reports itself **not ready**
//! and keeps serving; only after `DONAT_SHUTDOWN_READINESS_DELAY_SECONDS` does
//! it stop accepting. Without that gap the listener closes while the balancer
//! still believes in this replica, and every request it routes in the meantime
//! is refused at the socket — the connections in flight are drained politely
//! and the arriving ones are dropped on the floor, which is a strange way to
//! finish a deploy.
//!
//! After the gap the HTTP server stops accepting and finishes what it has; the
//! background workers finish the item they are on and decline to claim
//! another. Both are bounded — a drain that never ends is a deploy that never
//! finishes — after which the process exits regardless.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// How long the deployment gets to finish what it started.
///
/// Chosen to sit under a typical orchestrator's own grace period (Kubernetes
/// defaults to 30s) so the drain completes before `SIGKILL` arrives rather
/// than being cut off by it.
const DEFAULT_DRAIN_SECONDS: u64 = 25;

/// How long the process keeps serving after saying it is not ready.
///
/// Long enough for a balancer polling readiness on the usual few-second
/// interval to notice and take this replica out of rotation, short enough to
/// leave most of the grace period for the drain itself.
const DEFAULT_READINESS_DELAY_SECONDS: u64 = 5;

pub fn parse_readiness_delay(raw: Option<&str>) -> Duration {
    let seconds = raw
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_READINESS_DELAY_SECONDS);
    Duration::from_secs(seconds)
}

pub fn readiness_delay() -> Duration {
    parse_readiness_delay(
        std::env::var("DONAT_SHUTDOWN_READINESS_DELAY_SECONDS")
            .ok()
            .as_deref(),
    )
}

pub fn parse_drain_grace(raw: Option<&str>) -> Duration {
    let seconds = raw
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DRAIN_SECONDS);
    Duration::from_secs(seconds)
}

pub fn drain_grace() -> Duration {
    parse_drain_grace(
        std::env::var("DONAT_SHUTDOWN_GRACE_SECONDS")
            .ok()
            .as_deref(),
    )
}

/// The two phases of stopping.
///
/// `draining` is cancelled the moment the signal arrives, and is what the
/// readiness route reports. `stopping` follows after the readiness delay, and
/// is what actually closes the listener and stands the workers down.
#[derive(Clone)]
pub struct Shutdown {
    pub draining: CancellationToken,
    pub stopping: CancellationToken,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Shutdown {
    pub fn new() -> Self {
        Self {
            draining: CancellationToken::new(),
            stopping: CancellationToken::new(),
        }
    }

    /// Whether this process still wants traffic.
    pub fn is_ready(&self) -> bool {
        !self.draining.is_cancelled()
    }

    /// Begin the sequence without waiting for a signal. Used by tests, and by
    /// any caller that already knows the process is going away.
    pub fn begin(&self, readiness_delay: Duration) {
        let phases = self.clone();
        tokio::spawn(async move { phases.run(readiness_delay).await });
    }

    async fn run(self, readiness_delay: Duration) {
        self.draining.cancel();
        tracing::info!(
            target: "donat::shutdown",
            readiness_delay_seconds = readiness_delay.as_secs(),
            "reporting not ready; still serving so the balancer can take this replica out"
        );
        tokio::time::sleep(readiness_delay).await;
        tracing::info!(target: "donat::shutdown", "draining in-flight work");
        self.stopping.cancel();
    }
}

/// Start the two-phase stop when the deployment asks this process to stop.
///
/// `SIGTERM` is what an orchestrator sends; `SIGINT` is what a terminal sends.
/// Both mean the same thing here.
pub fn on_signal(shutdown: Shutdown, readiness_delay: Duration) {
    tokio::spawn(async move {
        let reason = wait_for_signal().await;
        tracing::info!(target: "donat::shutdown", reason, "stopping");
        shutdown.run(readiness_delay).await;
    });
}

#[cfg(unix)]
async fn wait_for_signal() -> &'static str {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(target: "donat::shutdown", %error, "cannot listen for SIGTERM");
            // Still honour Ctrl-C rather than leaving the process unstoppable.
            let _ = tokio::signal::ctrl_c().await;
            return "SIGINT";
        }
    };
    tokio::select! {
        _ = terminate.recv() => "SIGTERM",
        _ = tokio::signal::ctrl_c() => "SIGINT",
    }
}

#[cfg(not(unix))]
async fn wait_for_signal() -> &'static str {
    let _ = tokio::signal::ctrl_c().await;
    "SIGINT"
}

/// Wait out a polling interval, unless the deployment is stopping.
///
/// Returns `false` when the caller should return instead of asking for more
/// work. Background loops use this in place of a bare sleep so that stopping
/// does not have to wait out a full interval first.
pub async fn idle(interval: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(interval) => !shutdown.is_cancelled(),
        _ = shutdown.cancelled() => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is the whole point: report not ready first, keep serving, and
    /// only then close the listener. Reversed, the balancer routes into a
    /// socket that is already refusing.
    #[tokio::test]
    async fn readiness_falls_before_the_listener_closes() {
        let shutdown = Shutdown::new();
        assert!(shutdown.is_ready());

        shutdown.begin(Duration::from_millis(150));

        shutdown.draining.cancelled().await;
        assert!(
            !shutdown.is_ready(),
            "readiness must fall as soon as the signal arrives"
        );
        assert!(
            !shutdown.stopping.is_cancelled(),
            "the listener must stay open while the balancer reacts"
        );

        tokio::time::timeout(Duration::from_secs(5), shutdown.stopping.cancelled())
            .await
            .expect("the listener closes once the delay has passed");
    }

    /// A deployment that fronts the engine with something other than a polling
    /// balancer can ask for the old behaviour.
    #[tokio::test]
    async fn a_zero_delay_stops_immediately() {
        let shutdown = Shutdown::new();
        shutdown.begin(Duration::ZERO);
        tokio::time::timeout(Duration::from_secs(5), shutdown.stopping.cancelled())
            .await
            .expect("a zero delay does not hold the listener open");
    }

    #[test]
    fn readiness_delay_defaults_and_parses() {
        assert_eq!(parse_readiness_delay(None), Duration::from_secs(5));
        assert_eq!(parse_readiness_delay(Some(" 2 ")), Duration::from_secs(2));
        assert_eq!(parse_readiness_delay(Some("0")), Duration::ZERO);
        assert_eq!(parse_readiness_delay(Some("soon")), Duration::from_secs(5));
    }

    #[test]
    fn drain_grace_defaults_and_parses() {
        assert_eq!(parse_drain_grace(None), Duration::from_secs(25));
        assert_eq!(parse_drain_grace(Some(" 5 ")), Duration::from_secs(5));
        // "Do not wait" is a legitimate deployment choice.
        assert_eq!(parse_drain_grace(Some("0")), Duration::ZERO);
        // Anything unreadable keeps the default rather than becoming zero.
        assert_eq!(parse_drain_grace(Some("soon")), Duration::from_secs(25));
    }

    /// A stopping deployment must not wait out the interval it happened to be
    /// sleeping through — that is the difference between a drain and a stall.
    #[tokio::test]
    async fn idle_returns_immediately_once_cancelled() {
        let token = CancellationToken::new();
        assert!(idle(Duration::from_millis(1), &token).await);

        token.cancel();
        let started = std::time::Instant::now();
        assert!(!idle(Duration::from_secs(30), &token).await);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a cancelled idle must not wait out its interval"
        );
    }
}
