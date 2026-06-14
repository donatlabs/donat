//! The PlanV1 contract — the versioned, serializable boundary between the
//! wasm core and the host. Additive evolution only within a major; the Go
//! mirror rejects an unknown major (gated by `core_abi_version`).

#![allow(dead_code)]

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
    /// v1: emitted empty until Task 2.7's follow-up wires event_triggers.
    pub hooks: Vec<Hook>,
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
