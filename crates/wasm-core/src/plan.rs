//! The PlanV1 contract — the versioned, serializable boundary between the
//! wasm core and the host. Additive evolution only within a major; the Go
//! mirror rejects an unknown major (gated by `core_abi_version`).

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PlanV1 {
    Query(PlanBody),
    Mutation(PlanBody),
    /// An operation whose top-level fields are actions: custom logic the
    /// engine does not resolve from SQL. The host calls each item and then
    /// asks the core to shape the results, so the binding and the shaping stay
    /// the engine's regardless of which host is serving.
    Action(ActionPlanBody),
    Error(PlanErrorBody),
}

#[derive(Debug, Serialize)]
pub struct ActionPlanBody {
    pub version: u32, // always crate::plan::PLAN_VERSION
    /// Query actions may run concurrently; mutation actions must not, because
    /// a client that ordered two writes in one operation gets that order.
    pub is_query: bool,
    pub items: Vec<donat_action::ActionItem>,
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
    /// The top-level response keys, in the order the client asked for them.
    /// The host assembles the response object from these rather than from the
    /// statement results alone, because a root `__typename` is answered by the
    /// planner and never reaches SQL.
    pub response: Vec<ResponseSlot>,
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
    /// How the host must read this statement's row.
    ///
    /// Almost every statement yields one JSON value in column 0. An idempotent
    /// command does not: its row carries the execution generation beside the
    /// result, because a replay has to be distinguishable from a first run.
    /// The host cannot infer that from the SQL, and a host that guessed would
    /// fail on the shape rather than on anything meaningful — so the plan says
    /// it.
    #[serde(default, skip_serializing_if = "ResultShape::is_default")]
    pub result: ResultShape,
}

/// The row shape a statement returns.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultShape {
    /// One JSON (or text) value in column 0.
    #[default]
    Value,
    /// Columns `root`, `invocation_id` and `replayed`: an idempotent command's
    /// durable execution generation alongside its result.
    CommandExecution,
}

impl ResultShape {
    fn is_default(&self) -> bool {
        matches!(self, ResultShape::Value)
    }
}

/// One top-level response key, in client order.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseSlot {
    /// Take this key from the statement results.
    SourceField { key: String },
    /// A root `__typename`: the planner already knows the answer.
    LocalTypename { key: String, value: String },
}

#[derive(Debug, Serialize)]
pub struct Hook {
    pub phase: String, // "post_commit"
    /// The response key of the statement whose result this hook carries.
    ///
    /// A mutation has one statement per root and a root may fire several
    /// triggers, so a trigger name does not identify a result. Without this the
    /// host has to guess, and guessing wrong hands a handler another root's
    /// payload.
    pub alias: String,
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
///
/// The map is built once via `LazyLock` and cloned on each call so callers
/// keep an owned `BTreeMap` (the serde contract for `PlanBody::error_map`
/// stays unchanged) without the cost of rebuilding on every compile.
use std::sync::LazyLock;

static ERROR_MAP: LazyLock<std::collections::BTreeMap<String, String>> = LazyLock::new(|| {
    use std::collections::BTreeMap;
    let mut m = BTreeMap::new();
    // 23514: check_violation — our donat.check_violation() stores a JSON
    // payload { "path": ..., "message": ... } in the PG error message.
    m.insert("23514".into(), "permission-error-from-payload".into());
    // 23505: unique_violation
    m.insert(
        "23505".into(),
        "constraint-violation:Uniqueness violation. ".into(),
    );
    // 23503: foreign_key_violation
    m.insert(
        "23503".into(),
        "constraint-violation:Foreign key violation. ".into(),
    );
    // 23502: not_null_violation
    m.insert(
        "23502".into(),
        "constraint-violation:Not-NULL violation. ".into(),
    );
    // All other SQLSTATE codes → data-exception (matches the `_` arm in gql.rs)
    m.insert("default".into(), "data-exception".into());
    m
});

pub fn default_error_map() -> std::collections::BTreeMap<String, String> {
    ERROR_MAP.clone()
}
