//! Compile orchestration: parse → session → plan → PlanV1.

use std::collections::HashMap;

use serde::Deserialize;

use donat_schema::{PlanError, Planner, Session};

use crate::plan::{PlanBody, PlanErrorBody, PlanV1, Statement, PLAN_VERSION};

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

/// Compile a GraphQL request against the loaded engine state, producing a
/// versioned PlanV1 ready for serialisation to the host.
///
/// Query path: one combined SQL statement keyed `"data"`, `transaction:false`.
/// Mutation path: one statement per root, run in a single transaction.
/// All error cases (bad role, parse error, planner error) return `PlanV1::Error`.
pub fn compile(state: &CoreState, input: &CompileInput) -> PlanV1 {
    // 1. Resolve the session (no-admin rule enforced here).
    let session = match session_from(&input.session_vars) {
        Ok(s) => s,
        Err(e) => return error_plan(&e),
    };

    // 2. Parse the GraphQL document.
    let doc = match graphql_parser::parse_query::<String>(&input.query) {
        Ok(d) => d.into_static(),
        Err(e) => {
            return PlanV1::Error(PlanErrorBody {
                version: PLAN_VERSION,
                code: "validation-failed".into(),
                path: "$".into(),
                message: e.to_string(),
            });
        }
    };

    // 3. Plan (permissions woven in, session vars substituted).
    let planner = Planner::new(&state.metadata, &state.catalog);
    let plan =
        match planner.plan(&doc, input.operation_name.as_deref(), &input.variables, &session) {
            Ok(p) => p,
            Err(e) => return error_plan(&e),
        };

    match plan {
        // 4a. Query: one combined statement aliased "data".
        donat_schema::Plan::Query(roots) => {
            let sql =
                donat_sqlgen::operation_to_sql_opts(&roots, input.stringify_numerics);
            PlanV1::Query(PlanBody {
                version: PLAN_VERSION,
                transaction: false,
                statements: vec![Statement { alias: "data".into(), sql, params: vec![] }],
                hooks: vec![],
                error_map: crate::plan::default_error_map(),
            })
        }

        // 4b. Mutation: one statement per root, wrapped in a transaction.
        donat_schema::Plan::Mutation(roots) => {
            let mut statements = Vec::new();
            let mut hooks = Vec::new();
            for root in &roots {
                let alias = match root {
                    donat_ir::MutationRoot::FunctionCall { alias, .. }
                    | donat_ir::MutationRoot::Insert { alias, .. }
                    | donat_ir::MutationRoot::Update { alias, .. }
                    | donat_ir::MutationRoot::Delete { alias, .. }
                    | donat_ir::MutationRoot::Typename { alias, .. } => alias.clone(),
                };
                statements.push(Statement {
                    alias,
                    sql: donat_sqlgen::mutation_to_sql_opts(root, input.stringify_numerics),
                    params: vec![],
                });
                hooks.extend(hooks_for_root(root, &state.metadata));
            }
            PlanV1::Mutation(PlanBody {
                version: PLAN_VERSION,
                transaction: true,
                statements,
                hooks,
                error_map: crate::plan::default_error_map(),
            })
        }
    }
}

/// Convert a planner error into a `PlanV1::Error` body.
fn error_plan(e: &PlanError) -> PlanV1 {
    PlanV1::Error(PlanErrorBody {
        version: PLAN_VERSION,
        code: e.code.to_string(),
        path: e.path.clone(),
        message: e.message.clone(),
    })
}

/// Derive the post-commit hooks a single mutation root should fire.
///
/// For each mutation root that targets a table, scan all sources in metadata
/// for a matching `TableEntry` and collect any `EventTrigger`s whose
/// definition covers the operation.  `FunctionCall` and `Typename` roots
/// produce no hooks.
fn hooks_for_root(
    root: &donat_ir::MutationRoot,
    metadata: &donat_metadata::Metadata,
) -> Vec<crate::plan::Hook> {
    // Resolve (schema, table, op_string) for this root.
    let (schema, table, op): (&str, &str, &str) = match root {
        donat_ir::MutationRoot::Insert { insert, .. } => {
            (&insert.table.schema, &insert.table.name, "INSERT")
        }
        donat_ir::MutationRoot::Update { update, .. } => {
            (&update.table.schema, &update.table.name, "UPDATE")
        }
        donat_ir::MutationRoot::Delete { delete, .. } => {
            (&delete.table.schema, &delete.table.name, "DELETE")
        }
        // FunctionCall and Typename carry no table reference → no hooks.
        donat_ir::MutationRoot::FunctionCall { .. }
        | donat_ir::MutationRoot::Typename { .. } => return vec![],
    };

    let mut out = Vec::new();
    for source in &metadata.sources {
        for entry in &source.tables {
            if entry.table.schema() != schema || entry.table.name() != table {
                continue;
            }
            for et in &entry.event_triggers {
                let covers = match op {
                    "INSERT" => et.definition.insert.is_some(),
                    "UPDATE" => et.definition.update.is_some(),
                    "DELETE" => et.definition.delete.is_some(),
                    _ => false,
                };
                if covers {
                    out.push(crate::plan::Hook {
                        phase: "post_commit".into(),
                        trigger: et.name.clone(),
                        schema: schema.to_string(),
                        table: table.to_string(),
                        op: op.to_string(),
                    });
                }
            }
        }
    }
    out
}
