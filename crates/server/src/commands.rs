//! Typed internal result boundary for declarative Command execution.
//!
//! The GraphQL result remains column zero. Idempotent Command statements also
//! return the generation UUID elected inside that same SQL statement and
//! whether this execution replayed the canonical result. These fields are
//! consumed only by trusted runtime code and are never added to an API body.

use serde_json::Value as Json;
use tokio_postgres::Row;
use uuid::Uuid;

const COMMAND_GRAPHQL_ERROR_SQLSTATE: &str = "P0D01";
const COMMAND_GRAPHQL_ERROR_KIND: &str = "donat.graphql-error.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBusinessRejection {
    pub code: String,
    pub path: String,
    pub message: String,
}

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

/// Decode only the reserved, exact business-rejection envelope emitted by a
/// declarative Command. Permission checks, malformed reserved messages, and
/// every other database failure remain fatal to the owning transaction.
pub fn decode_command_business_rejection(
    error: &tokio_postgres::Error,
) -> Option<CommandBusinessRejection> {
    let database = error.as_db_error()?;
    if database.code().code() != COMMAND_GRAPHQL_ERROR_SQLSTATE {
        return None;
    }
    let Json::Object(mut payload) = serde_json::from_str::<Json>(database.message()).ok()? else {
        return None;
    };
    if payload.len() != 4
        || payload
            .remove("kind")
            .and_then(|value| value.as_str().map(str::to_owned))
            != Some(COMMAND_GRAPHQL_ERROR_KIND.to_owned())
    {
        return None;
    }
    let code = payload
        .remove("code")
        .and_then(|value| value.as_str().map(str::to_owned))?;
    let path = payload
        .remove("path")
        .and_then(|value| value.as_str().map(str::to_owned))?;
    let message = payload
        .remove("message")
        .and_then(|value| value.as_str().map(str::to_owned))?;
    if code.is_empty() || !path.starts_with('$') {
        return None;
    }
    Some(CommandBusinessRejection {
        code,
        path,
        message,
    })
}
