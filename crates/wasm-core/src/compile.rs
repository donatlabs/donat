//! Compile orchestration (filled in Tasks 2.4–2.7).

use std::collections::HashMap;

use serde::Deserialize;

use donat_schema::{PlanError, Session};

/// Deserialized engine state held per wasm instance.
pub struct CoreState {
    pub metadata: donat_metadata::Metadata,
    pub catalog: donat_catalog_types::Catalog,
}

/// The JSON payload that `core_compile` receives from the host.
#[derive(Deserialize)]
pub struct CompileInput {
    pub query: String,
    #[serde(default)]
    pub operation_name: Option<String>,
    #[serde(default)]
    pub variables: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub session_vars: HashMap<String, String>,
    #[serde(default)]
    pub stringify_numerics: bool,
}

/// Build a Session from the session-vars map, applying the no-admin rule:
/// a request with no x-donat-role is denied exactly as the engine denies it.
///
/// The denial code and message are copied verbatim from
/// `crates/server/src/gql.rs` `session_from_headers` (trusted branch, no
/// role found): code `"access-denied"`, message
/// `"x-donat-role header is required (this engine has no admin role)"`.
pub fn session_from(vars: &HashMap<String, String>) -> Result<Session, PlanError> {
    // Lowercase keys to match Session::var lookups.
    let lower: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    let role = match lower.get("x-donat-role") {
        Some(r) if !r.is_empty() => r.clone(),
        _ => {
            return Err(PlanError::new(
                "$",
                "access-denied",
                "x-donat-role header is required (this engine has no admin role)",
            ));
        }
    };
    let backend_request = lower
        .get("x-donat-use-backend-only-permissions")
        .map(|v| v == "true")
        .unwrap_or(false);
    Ok(Session {
        role,
        vars: lower,
        backend_request,
    })
}
