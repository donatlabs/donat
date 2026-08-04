//! Compile orchestration: parse → session → plan → PlanV1.

use std::collections::HashMap;

use serde::Deserialize;

use donat_schema::{
    CompiledMultiSourceSchema, MultiSourcePlan, MultiSourcePlanner, PlanError,
    ProcessEffectContractCatalog, QueryResponseSlot, Session, compile_command_catalog,
    compile_rule_catalog, finalize_command_effects,
};

use crate::plan::{PlanBody, PlanErrorBody, PlanV1, ResponseSlot, Statement, PLAN_VERSION};

/// Compiled engine state held per wasm instance.
///
/// The snapshot is compiled once, at `core_init`, exactly as `donat-server`
/// compiles its own: rules, then commands, then the serving schema. Doing it
/// per request would repay the cost on every call, and — more importantly —
/// would let a deployment serve traffic for a while before discovering that
/// its declarative metadata does not compile.
pub struct CoreState {
    pub metadata: donat_metadata::Metadata,
    pub catalogs: HashMap<String, donat_catalog_types::Catalog>,
    pub compiled: CompiledMultiSourceSchema,
}

impl CoreState {
    /// Compile a snapshot from a metadata + catalog config.
    ///
    /// Process effects are deliberately empty: a durable Process needs a
    /// journal and a transition queue, which live host-side in `donat-server`
    /// and have no counterpart here yet. `finalize_command_effects` therefore
    /// refuses a command that declares an effect against a Process this core
    /// cannot run, which is the honest outcome — the alternative is accepting
    /// the metadata and silently dropping the effect at runtime.
    pub fn compile_snapshot(
        metadata: donat_metadata::Metadata,
        catalogs: HashMap<String, donat_catalog_types::Catalog>,
    ) -> Result<Self, PlanError> {
        // Matches the server's default: a tracked function's permissions are
        // inferred from the underlying table unless a role entry says otherwise.
        const INFER_FUNCTION_PERMISSIONS: bool = true;

        let rules = compile_rule_catalog(&metadata)?;
        let commands =
            compile_command_catalog(&metadata, &catalogs, &rules, INFER_FUNCTION_PERMISSIONS)?;
        let process_effects = ProcessEffectContractCatalog::default();
        let finalized = finalize_command_effects(commands, &process_effects)?;
        let compiled = CompiledMultiSourceSchema::compile_with_command_catalog_and_process_effects(
            &metadata,
            &catalogs,
            &rules,
            &finalized,
            &process_effects,
            INFER_FUNCTION_PERMISSIONS,
        )?;
        Ok(Self {
            metadata,
            catalogs,
            compiled,
        })
    }
}

/// Translate the planner's response slots into the wire shape, so the host
/// can build the top-level object in the client's field order — including a
/// root `__typename`, which never reaches SQL.
fn response_slots(slots: &[QueryResponseSlot]) -> Vec<ResponseSlot> {
    slots
        .iter()
        .map(|slot| match slot {
            QueryResponseSlot::SourceField { key } => ResponseSlot::SourceField { key: key.clone() },
            QueryResponseSlot::LocalTypename { key, value } => ResponseSlot::LocalTypename {
                key: key.clone(),
                value: value.clone(),
            },
        })
        .collect()
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
    /// SQL dialect to target. Accepted values: `"postgres"` (default),
    /// `"sqlite"`, `"mysql"`. Unknown values fall back to Postgres.
    #[serde(default)]
    pub dialect: Option<String>,
}

/// Map the caller-supplied dialect name to a concrete [`donat_backend::AnyDialect`].
/// Returns `AnyDialect::Postgres(PostgresDialect)` for `None`, `"postgres"`, or
/// any unrecognised string — preserving the default Postgres output byte-for-byte.
fn dialect_of(name: Option<&str>) -> donat_backend::AnyDialect {
    match name {
        Some("sqlite") => donat_backend::AnyDialect::Sqlite(donat_backend::SqliteDialect),
        Some("mysql") => donat_backend::AnyDialect::Mysql(donat_backend::MySqlDialect),
        // None, "postgres", or anything unknown defaults to Postgres.
        _ => donat_backend::AnyDialect::Postgres(donat_backend::PostgresDialect),
    }
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
    let backend_request = match lower.get("x-donat-use-backend-only-permissions") {
        None => false,
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" | "y" => true,
            "false" | "f" | "no" | "n" => false,
            _ => {
                return Err(PlanError::new(
                    "$",
                    "bad-request",
                    "x-donat-use-backend-only-permissions:  Not a valid boolean text. True values are [\"true\",\"t\",\"yes\",\"y\"] and  False values are [\"false\",\"f\",\"no\",\"n\"]. All values are case insensitive",
                ));
            }
        },
    };
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

    // 3. Plan (permissions woven in, session vars substituted). The
    //    multi-source planner is what knows about declarative commands: the
    //    single-source `Planner` constructor hardcodes `commands: None`, so a
    //    command root would not exist in the schema it compiles.
    let planner = match MultiSourcePlanner::from_compiled(
        &state.metadata,
        &state.catalogs,
        &state.compiled,
    ) {
        Ok(p) => p,
        Err(e) => return error_plan(&e),
    };
    let plan =
        match planner.plan(&doc, input.operation_name.as_deref(), &input.variables, &session) {
            Ok(p) => p,
            Err(e) => return error_plan(&e),
        };

    // 3b. Resolve the SQL dialect for this request.
    let dialect = dialect_of(input.dialect.as_deref());

    match plan {
        // 4a. Query: one combined statement aliased "data".
        MultiSourcePlan::Query { sources, response } => {
            // A cross-source query needs the host to run one statement per
            // source and merge the results, which this core does not describe
            // yet. Refusing is better than emitting one source's SQL and
            // quietly dropping the rest of the response.
            if sources.len() > 1 {
                return error_plan(&PlanError::new(
                    "$",
                    "not-supported",
                    "the embedded core plans one source per operation; \
                     this query spans several",
                ));
            }
            // An operation that selects only a root `__typename` reaches no
            // source at all, and then there is no SQL to run: the host builds
            // the whole response from the slots below.
            let statements = match sources.first() {
                None => vec![],
                Some(plan) => {
                    // For Postgres (the default), use operation_to_sql_opts so that
                    // stringify_numerics is honoured and the output is byte-identical
                    // to the previous behaviour. For other dialects, operation_to_sql_with
                    // is used (stringify_numerics is always false for non-Postgres dialects
                    // — the dialect API does not expose it).
                    let sql = match dialect {
                        donat_backend::AnyDialect::Postgres(_) => {
                            donat_sqlgen::operation_to_sql_opts(
                                &plan.roots,
                                input.stringify_numerics,
                            )
                        }
                        _ => donat_sqlgen::operation_to_sql_with(&plan.roots, dialect),
                    };
                    vec![Statement { alias: "data".into(), sql, params: vec![] }]
                }
            };
            PlanV1::Query(PlanBody {
                version: PLAN_VERSION,
                transaction: false,
                statements,
                hooks: vec![],
                response: response_slots(&response),
                error_map: crate::plan::default_error_map(),
            })
        }

        // 4b. Mutation: one statement per root, wrapped in a transaction.
        MultiSourcePlan::Mutation { roots, response, .. } => {
            let mut statements = Vec::new();
            let mut hooks = Vec::new();
            for root in &roots {
                let alias = match root {
                    donat_ir::MutationRoot::FunctionCall { alias, .. }
                    | donat_ir::MutationRoot::Insert { alias, .. }
                    | donat_ir::MutationRoot::Update { alias, .. }
                    | donat_ir::MutationRoot::Delete { alias, .. }
                    | donat_ir::MutationRoot::Command { alias, .. }
                    | donat_ir::MutationRoot::RequestFileUpload { alias, .. }
                    | donat_ir::MutationRoot::Typename { alias, .. } => alias.clone(),
                };
                // Same dialect/stringify_numerics split as the query path above.
                let sql = match dialect {
                    donat_backend::AnyDialect::Postgres(_) => {
                        donat_sqlgen::mutation_to_sql_opts(root, input.stringify_numerics)
                    }
                    _ => donat_sqlgen::mutation_to_sql_with(root, dialect),
                };
                statements.push(Statement { alias, sql, params: vec![] });
                hooks.extend(hooks_for_root(root, &state.metadata));
            }
            PlanV1::Mutation(PlanBody {
                version: PLAN_VERSION,
                transaction: true,
                statements,
                hooks,
                response: response_slots(&response),
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

/// The tables a mutation root writes, and how.
///
/// A CRUD root writes exactly one table. A declarative command writes as many
/// as its steps do, which is why the pair is a list: a command is the only
/// place where one root field commits to several tables at once, and a host
/// that only watched the root would never learn about any of them.
fn written_tables(root: &donat_ir::MutationRoot) -> Vec<(&str, &str, &'static str)> {
    use donat_ir::CommandExecutionStep as Step;

    match root {
        donat_ir::MutationRoot::Insert { insert, .. } => {
            vec![(&insert.table.schema, &insert.table.name, "INSERT")]
        }
        donat_ir::MutationRoot::Update { update, .. } => {
            vec![(&update.table.schema, &update.table.name, "UPDATE")]
        }
        donat_ir::MutationRoot::Delete { delete, .. } => {
            vec![(&delete.table.schema, &delete.table.name, "DELETE")]
        }
        // A command's writes are its steps. Reading them from the resolved IR
        // rather than from the metadata declaration means a step the planner
        // dropped cannot leave a hook behind that fires for a write that did
        // not happen.
        donat_ir::MutationRoot::Command { command, .. } => command
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Insert { table, .. }
                | Step::InsertMany { table, .. }
                | Step::InsertRows { table, .. } => {
                    Some((table.schema.as_str(), table.name.as_str(), "INSERT"))
                }
                Step::Update { table, .. }
                | Step::UpdateWhen { table, .. }
                | Step::UpdateMany { table, .. } => {
                    Some((table.schema.as_str(), table.name.as_str(), "UPDATE"))
                }
                Step::Delete { table, .. } => {
                    Some((table.schema.as_str(), table.name.as_str(), "DELETE"))
                }
                // Reads, assertions and projections commit nothing.
                _ => None,
            })
            .collect(),
        // These roots write no tracked table row: a function call is opaque to
        // trigger metadata, minting an upload URL touches only the engine's own
        // catalog, and a typename never reaches the database.
        donat_ir::MutationRoot::FunctionCall { .. }
        | donat_ir::MutationRoot::RequestFileUpload { .. }
        | donat_ir::MutationRoot::Typename { .. } => vec![],
    }
}

/// Derive the post-commit hooks a single mutation root should fire.
///
/// For each table the root writes, scan all sources in metadata for a matching
/// `TableEntry` and collect any `EventTrigger`s whose definition covers the
/// operation.
fn hooks_for_root(
    root: &donat_ir::MutationRoot,
    metadata: &donat_metadata::Metadata,
) -> Vec<crate::plan::Hook> {
    let mut out = Vec::new();
    for (schema, table, op) in written_tables(root) {
        collect_hooks(metadata, schema, table, op, &mut out);
    }
    out
}

fn collect_hooks(
    metadata: &donat_metadata::Metadata,
    schema: &str,
    table: &str,
    op: &str,
    out: &mut Vec<crate::plan::Hook>,
) {
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
}
