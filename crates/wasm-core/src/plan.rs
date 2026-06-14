//! The PlanV1 contract — the versioned, serializable boundary between the
//! wasm core and the host. Additive evolution only within a major; the Go
//! mirror rejects an unknown major (gated by `core_abi_version`).

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PlanV1 {
    Query(PlanBody),
    Mutation(PlanBody),
    Error(PlanErrorBody),
}

#[derive(Debug, Serialize)]
pub struct PlanBody {
    pub version: u32, // always crate::plan::PLAN_VERSION
    /// True for mutations (run statements in one transaction).
    pub transaction: bool,
    pub statements: Vec<Statement>,
    /// Post-commit hooks the executor must fire (Spec 003 Registry.Dispatch).
    /// v1: emitted empty until event_trigger wiring is added.
    pub hooks: Vec<Hook>,
    /// SQLSTATE -> Donat error directive; the host applies these to runtime
    /// pg errors (Spec 004). Static in v1 (matches gql.rs error table).
    pub error_map: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct Statement {
    /// Aliased response key for this root (the JSON object field name).
    pub alias: String,
    /// Fully-rendered SQL (literals inlined in v1 — see Spec 004 finding;
    /// `params` is reserved for the future $n refactor).
    pub sql: String,
    pub params: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct Hook {
    pub phase: String, // "post_commit"
    pub trigger: String,
    pub schema: String,
    pub table: String,
    pub op: String, // "INSERT" | "UPDATE" | "DELETE"
}

#[derive(Debug, Serialize)]
pub struct PlanErrorBody {
    pub version: u32, // always crate::plan::PLAN_VERSION
    /// The Donat error code (e.g. "validation-failed", "access-denied").
    pub code: String,
    pub path: String,
    pub message: String,
}

/// PlanV1 wire major. Bump with `ABI_VERSION` on a breaking change.
pub const PLAN_VERSION: u32 = 1;

/// Static SQLSTATE -> Donat error code/message mapping.
///
/// Values copied verbatim from `crates/server/src/gql.rs:884-917`
/// (`db_error_json` match arms) — these are the conformance contract strings.
///
/// Key `"23514"` (check_violation) sets `"permission-error-from-payload"` so
/// the host knows to parse the JSON payload for the nested path/message (the
/// engine encodes a JSON payload in the PG error message for 23514).
/// The `"default"` key covers all other SQLSTATE codes.
pub fn default_error_map() -> std::collections::BTreeMap<String, String> {
    use std::collections::BTreeMap;
    let mut m = BTreeMap::new();
    // 23514: check_violation — our donat.check_violation() stores a JSON
    // payload { "path": ..., "message": ... } in the PG error message.
    m.insert("23514".into(), "permission-error-from-payload".into());
    // 23505: unique_violation
    m.insert("23505".into(), "constraint-violation:Uniqueness violation. ".into());
    // 23503: foreign_key_violation
    m.insert("23503".into(), "constraint-violation:Foreign key violation. ".into());
    // 23502: not_null_violation
    m.insert("23502".into(), "constraint-violation:Not-NULL violation. ".into());
    // All other SQLSTATE codes → data-exception (matches the `_` arm in gql.rs)
    m.insert("default".into(), "data-exception".into());
    m
}
