//! Process-owned execution of one finalized declarative Command.
//!
//! Planning happens before the journal transaction opens. This module owns
//! only the PostgreSQL savepoint boundary and typed result/rejection decode.

use anyhow::{Context, anyhow};
use donat_ir::{CommandMutation, MutationRoot};
use serde_json::Value as Json;
use tokio_postgres::Transaction;

use crate::commands::{
    CommandBusinessRejection, decode_command_business_rejection, decode_command_execution_result,
};

#[derive(Debug, Clone, PartialEq)]
pub enum ProcessCommandOutcome {
    Applied {
        result: Json,
    },
    Rejected {
        error: CommandBusinessRejection,
    },
    /// The database refused the write in a way that retrying cannot change —
    /// a constraint violation, a missing object, a denied privilege. Retrying
    /// it can only produce the same refusal, and doing so forever holds the
    /// head of the shared transition queue against every other instance.
    Unrecoverable {
        code: &'static str,
    },
}

pub async fn execute_process_command_in_savepoint(
    transaction: &Transaction<'_>,
    command: &CommandMutation,
    stringify_numerics: bool,
) -> anyhow::Result<ProcessCommandOutcome> {
    transaction
        .batch_execute("SAVEPOINT donat_process_command")
        .await
        .context("creating Process command savepoint")?;

    let root = MutationRoot::Command {
        alias: "process_command".to_owned(),
        command: command.clone(),
    };
    let sql = donat_sqlgen::mutation_to_sql_opts(&root, stringify_numerics);
    match transaction.query_one(&sql, &[]).await {
        Ok(row) => {
            let result = if command.idempotency.is_some() {
                decode_command_execution_result(&row)
                    .map(|decoded| decoded.result_json)
                    .map_err(|_| anyhow!("cannot decode Process command execution result"))?
            } else {
                row.try_get::<_, Json>("root")
                    .map_err(|_| anyhow!("cannot decode Process command result"))?
            };
            transaction
                .batch_execute("RELEASE SAVEPOINT donat_process_command")
                .await
                .context("releasing applied Process command savepoint")?;
            Ok(ProcessCommandOutcome::Applied { result })
        }
        Err(error) => {
            if let Some(error) = decode_command_business_rejection(&error) {
                transaction
                    .batch_execute(
                        "ROLLBACK TO SAVEPOINT donat_process_command; \
                         RELEASE SAVEPOINT donat_process_command",
                    )
                    .await
                    .context("rolling back rejected Process command savepoint")?;
                return Ok(ProcessCommandOutcome::Rejected { error });
            }
            // The log is the only account of why a command did not land, so it
            // names the schema object that refused it: the SQLSTATE alone
            // cannot distinguish one check constraint from another in a command
            // that writes several tables. The durable journal keeps only the
            // safe code — a constraint name is an operator concern.
            let (sqlstate, relation) = match error.as_db_error() {
                Some(error) => (
                    error.code().code().to_owned(),
                    format!(
                        " on {}.{}",
                        error.table().unwrap_or("?"),
                        error.constraint().unwrap_or("?")
                    ),
                ),
                None => ("driver".to_owned(), String::new()),
            };
            if let Some(code) = unrecoverable_command_failure(&sqlstate) {
                tracing::error!(
                    sqlstate = %sqlstate,
                    detail = %relation.trim_start(),
                    "Process command failed unrecoverably; failing the instance"
                );
                transaction
                    .batch_execute(
                        "ROLLBACK TO SAVEPOINT donat_process_command; \
                         RELEASE SAVEPOINT donat_process_command",
                    )
                    .await
                    .context("rolling back unrecoverable Process command savepoint")?;
                return Ok(ProcessCommandOutcome::Unrecoverable { code });
            }
            // The cause travels with it. A lock timeout, a deadlock or a
            // dropped connection is retryable, and the only thing that can
            // tell the consumer so is the database error itself — an
            // `anyhow!` string would arrive as an unclassifiable failure and
            // end the instance for a condition that clears on its own.
            Err(anyhow::Error::new(error).context(format!(
                "Process command database execution failed with SQLSTATE {sqlstate}{relation}"
            )))
        }
    }
}

/// Whether a SQLSTATE means "this will refuse again, however often you ask".
///
/// Integrity violations and access-rule violations are decisions about the
/// state or the schema, not weather: a unique index that refused a row refuses
/// the same row a thousand retries later. Everything else — serialization
/// failures, deadlocks, a connection that dropped — stays retryable, which is
/// what a durable runtime is for.
fn unrecoverable_command_failure(sqlstate: &str) -> Option<&'static str> {
    match sqlstate.get(..2) {
        // 23xxx: integrity constraint violation.
        Some("23") => Some("command_constraint_violation"),
        // 42xxx: syntax error or access rule violation.
        Some("42") => Some("command_not_permitted"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::unrecoverable_command_failure;

    #[test]
    fn a_refusal_about_state_or_schema_is_not_worth_retrying() {
        // Unique violation, foreign key violation, check violation.
        assert_eq!(
            unrecoverable_command_failure("23505"),
            Some("command_constraint_violation")
        );
        assert_eq!(
            unrecoverable_command_failure("23503"),
            Some("command_constraint_violation")
        );
        assert_eq!(
            unrecoverable_command_failure("23514"),
            Some("command_constraint_violation")
        );
        // Undefined column, insufficient privilege.
        assert_eq!(
            unrecoverable_command_failure("42703"),
            Some("command_not_permitted")
        );
        assert_eq!(
            unrecoverable_command_failure("42501"),
            Some("command_not_permitted")
        );
    }

    #[test]
    fn a_refusal_the_next_attempt_may_survive_stays_retryable() {
        // Serialization failure and deadlock are exactly what a durable retry
        // exists for; a dropped connection likewise.
        assert_eq!(unrecoverable_command_failure("40001"), None);
        assert_eq!(unrecoverable_command_failure("40P01"), None);
        assert_eq!(unrecoverable_command_failure("08006"), None);
        assert_eq!(unrecoverable_command_failure("driver"), None);
        assert_eq!(unrecoverable_command_failure(""), None);
    }
}
