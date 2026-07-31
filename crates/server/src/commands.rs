//! Typed internal result boundary for declarative Command execution.
//!
//! The GraphQL result remains column zero. Idempotent Command statements also
//! return the generation UUID elected inside that same SQL statement and
//! whether this execution replayed the canonical result. These fields are
//! consumed only by trusted runtime code and are never added to an API body.

use serde_json::Value as Json;
use tokio_postgres::Row;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandInvocationGeneration {
    pub invocation_id: Uuid,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandExecutionResult {
    pub invocation: CommandInvocationGeneration,
    pub result_json: Json,
}

pub fn decode_command_execution_result(
    row: &Row,
) -> Result<CommandExecutionResult, tokio_postgres::Error> {
    Ok(CommandExecutionResult {
        result_json: row.try_get("root")?,
        invocation: CommandInvocationGeneration {
            invocation_id: row.try_get("invocation_id")?,
            replayed: row.try_get("replayed")?,
        },
    })
}
