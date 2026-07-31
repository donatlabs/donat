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
    Applied { result: Json },
    Rejected { error: CommandBusinessRejection },
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
            let sqlstate = error
                .as_db_error()
                .map(|error| error.code().code().to_owned())
                .unwrap_or_else(|| "driver".to_owned());
            Err(anyhow!(
                "Process command database execution failed with SQLSTATE {sqlstate}"
            ))
        }
    }
}
