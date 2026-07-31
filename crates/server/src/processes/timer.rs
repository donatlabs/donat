//! Database-clock wake-up for durable Process timers.

use super::{ProcessRuntime, TransitionConsumption};

impl ProcessRuntime {
    /// Consume at most one due timer event.
    ///
    /// The timestamp and event live in the owning Postgres source. This method
    /// is only a filtered wake-up path through the same version-fenced
    /// transition transaction used by every other Process event.
    #[allow(dead_code)] // Public worker boundary is also exercised by integration tests.
    pub async fn consume_one_due_timer(&self) -> anyhow::Result<TransitionConsumption> {
        self.consume_one_transition_kind(Some("timer")).await
    }
}
