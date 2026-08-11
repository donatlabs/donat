//! The five bounds of spec 018 §4, and the failures they produce.
//!
//! A connector is bounded by its network: a response ceiling, a request
//! deadline, a rate limit. A local capability has none of those, so its bounds
//! are its own and all five are mandatory — [`LocalBounds::declare`] takes each
//! of them as a positional argument, which is why an operation cannot be
//! declared with four of them and a hope.
//!
//! The class of a refusal is decided here, once. A limit the input was already
//! over before any work started is `validation`: the same input will be over it
//! again, so a retry cannot help. The deadline is `timeout`, which a Process may
//! declare `retry_on`. Nothing partial is ever handed back either way.

use std::time::Duration;

use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};
use crate::sdk::operation::OperationError;

/// The longest one local execution may declare for itself.
///
/// A bound has to be bounded: an operation that may run for an hour on a
/// blocking thread is a worker a rolling deployment cannot drain inside its
/// grace period.
pub const MAX_CPU_DEADLINE: Duration = Duration::from_secs(120);

/// What one local operation may spend on one execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBounds {
    cpu_deadline: Duration,
    max_input_bytes: usize,
    max_output_bytes: usize,
    max_intermediate_bytes: usize,
    unit: &'static str,
    max_units: u64,
}

impl LocalBounds {
    /// The one constructor: every bound, or none of them.
    ///
    /// `unit` names what `max_units` counts — pages, rows, cells, events,
    /// pixels — because a count with no unit is a number an operator cannot
    /// act on.
    pub fn declare(
        cpu_deadline: Duration,
        max_input_bytes: usize,
        max_output_bytes: usize,
        max_intermediate_bytes: usize,
        unit: &'static str,
        max_units: u64,
    ) -> Result<Self, OperationError> {
        if cpu_deadline.is_zero() || cpu_deadline > MAX_CPU_DEADLINE {
            return Err(OperationError::new(
                "a local cpu deadline is positive and at most the shared local ceiling",
            ));
        }
        if max_input_bytes == 0 || max_output_bytes == 0 || max_intermediate_bytes == 0 {
            return Err(OperationError::new(
                "a local capability declares positive input, output, and working-memory ceilings",
            ));
        }
        if unit.trim().is_empty() || max_units == 0 {
            return Err(OperationError::new(
                "a local capability declares what it counts and how many of them it admits",
            ));
        }
        Ok(Self {
            cpu_deadline,
            max_input_bytes,
            max_output_bytes,
            max_intermediate_bytes,
            unit,
            max_units,
        })
    }

    pub const fn cpu_deadline(&self) -> Duration {
        self.cpu_deadline
    }

    pub const fn max_input_bytes(&self) -> usize {
        self.max_input_bytes
    }

    pub const fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    pub const fn max_intermediate_bytes(&self) -> usize {
        self.max_intermediate_bytes
    }

    /// What [`Self::max_units`] counts.
    pub const fn unit(&self) -> &'static str {
        self.unit
    }

    pub const fn max_units(&self) -> u64 {
        self.max_units
    }

    /// The canonical size of the input, before any work starts.
    pub fn admits_input_bytes(&self, bytes: usize) -> Result<(), ConnectorFailure> {
        if bytes > self.max_input_bytes {
            return Err(validation(
                "local_input_too_large",
                "local capability input exceeds the operation's declared input ceiling",
            ));
        }
        Ok(())
    }

    /// The capability-specific count its declaration names.
    pub fn admits_units(&self, units: u64) -> Result<(), ConnectorFailure> {
        if units > self.max_units {
            return Err(validation(
                "local_units_exceeded",
                "local capability input exceeds the operation's declared unit ceiling",
            ));
        }
        Ok(())
    }

    /// The size of the produced artifact, measured before it is handed back.
    pub fn admits_output_bytes(&self, bytes: usize) -> Result<(), ConnectorFailure> {
        if bytes > self.max_output_bytes {
            return Err(validation(
                "local_output_too_large",
                "local capability output exceeds the operation's declared output ceiling",
            ));
        }
        Ok(())
    }

    /// Peak working memory, charged by the implementation as it allocates.
    pub fn admits_intermediate_bytes(&self, bytes: usize) -> Result<(), ConnectorFailure> {
        if bytes > self.max_intermediate_bytes {
            return Err(validation(
                "local_intermediate_too_large",
                "local capability execution exceeds the operation's declared working-memory ceiling",
            ));
        }
        Ok(())
    }

    /// Wall-clock time spent on one execution.
    ///
    /// The boundary is inclusive: work that finished exactly at its deadline
    /// finished. Anything past it is a `timeout`, and its output is discarded
    /// rather than returned.
    pub fn admits_elapsed(&self, elapsed: Duration) -> Result<(), ConnectorFailure> {
        if elapsed > self.cpu_deadline {
            return Err(cpu_deadline_exceeded());
        }
        Ok(())
    }
}

/// The deadline failure, in one place: the declared bound and the caller's own
/// `start_to_close` ceiling both end an execution the same way.
pub(crate) fn cpu_deadline_exceeded() -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Timeout,
        "local_cpu_deadline_exceeded",
        "local capability execution reached its declared cpu deadline",
    )
}

/// The drain failure. `Timeout` rather than a class of its own, because a
/// Process routes on the eight closed classes and a drained execution is
/// exactly the case `retry_on: [timeout]` describes: nothing happened, and
/// running it again is safe.
pub(crate) fn drained() -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Timeout,
        "local_capability_drained",
        "local capability execution stopped because the deployment is draining",
    )
}

fn validation(code: &'static str, message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(ConnectorErrorClass::Validation, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ceiling that can be raised at will is not a ceiling; and every
    /// refusal names its own code, so an operator reading a journal can tell
    /// which limit was reached.
    #[test]
    fn each_bound_refuses_with_its_own_code() {
        let bounds = LocalBounds::declare(Duration::from_secs(1), 8, 8, 8, "rows", 2)
            .expect("a complete bound declaration is valid");
        assert_eq!(bounds.unit(), "rows");
        assert_eq!(
            bounds.admits_input_bytes(9).unwrap_err().code(),
            "local_input_too_large"
        );
        assert_eq!(
            bounds.admits_units(3).unwrap_err().code(),
            "local_units_exceeded"
        );
        assert_eq!(
            bounds.admits_output_bytes(9).unwrap_err().code(),
            "local_output_too_large"
        );
        assert_eq!(
            bounds.admits_intermediate_bytes(9).unwrap_err().code(),
            "local_intermediate_too_large"
        );
        assert_eq!(
            bounds
                .admits_elapsed(Duration::from_secs(2))
                .unwrap_err()
                .code(),
            "local_cpu_deadline_exceeded"
        );
        assert_eq!(bounds.admits_elapsed(Duration::from_secs(1)), Ok(()));
    }
}
