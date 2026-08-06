//! Compile orchestration: parse → session → plan → PlanV1.

use std::collections::HashMap;

use serde::Deserialize;

use donat_schema::{
    CompiledMultiSourceSchema, MultiSourcePlan, MultiSourcePlanner, PlanError, QueryResponseSlot,
    Session, compile_command_catalog, compile_rule_catalog, finalize_command_effects,
};

use crate::plan::{PLAN_VERSION, PlanBody, PlanErrorBody, PlanV1, ResponseSlot, Statement};

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
    /// Resolved once at init, because it is the same for every request and
    /// building it per request would re-derive the signing chain each time.
    /// Empty when the deployment declares no attachment.
    pub storage: donat_storage::StorageRegistry,
}

impl CoreState {
    /// Compile a snapshot from a metadata + catalog config.
    pub fn compile_snapshot(
        metadata: donat_metadata::Metadata,
        catalogs: HashMap<String, donat_catalog_types::Catalog>,
    ) -> Result<Self, PlanError> {
        Self::compile_snapshot_with_secrets(metadata, catalogs, HashMap::new())
    }

    /// The same, with the deployment secrets the storage registry needs.
    pub fn compile_snapshot_with_secrets(
        metadata: donat_metadata::Metadata,
        catalogs: HashMap<String, donat_catalog_types::Catalog>,
        secrets: HashMap<String, String>,
    ) -> Result<Self, PlanError> {
        // Matches the server's default: a tracked function's permissions are
        // inferred from the underlying table unless a role entry says otherwise.
        const INFER_FUNCTION_PERMISSIONS: bool = true;

        let rules = compile_rule_catalog(&metadata)?;
        let commands =
            compile_command_catalog(&metadata, &catalogs, &rules, INFER_FUNCTION_PERMISSIONS)?;

        // Process definitions are compiled here for one reason: a command that
        // starts or signals a Process needs that Process's effect contract in
        // order to compile at all. Deriving the contract differently in wasm
        // than in the engine would let the two disagree about what a command
        // may start, which is the divergence this whole core exists to avoid,
        // so the same compiler runs on both.
        //
        // Connectors resolve to nothing. This host cannot execute a connector
        // activity, so a Process that declares one fails to compile with the
        // ordinary unknown-operation error rather than being accepted and
        // never run.
        let processes = {
            let dependencies = donat_processes::ServerProcessDependencies::new(
                &metadata,
                &commands,
                &rules,
                &donat_processes::NoConnectors,
            );
            donat_processes::compile_process_catalog(&metadata, &dependencies)?
        };
        let process_effects = donat_processes::build_process_effect_contract_catalog(&processes)?;
        let finalized = finalize_command_effects(commands, &process_effects)?;
        let compiled = CompiledMultiSourceSchema::compile_with_command_catalog_and_process_effects(
            &metadata,
            &catalogs,
            &rules,
            &finalized,
            &process_effects,
            INFER_FUNCTION_PERMISSIONS,
        )?;
        // A deployment that declares no attachment resolves to an empty
        // registry and never notices this. One that does declare an attachment
        // but whose secret the host did not supply must fail here rather than
        // at the first request for a URL it cannot sign.
        let storage = donat_storage::StorageRegistry::build_with(&metadata, &|name| {
            secrets.get(name).cloned()
        })
        .map_err(|e| {
            PlanError::validation(
                "storage",
                format!("the storage configuration did not resolve: {e:?}"),
            )
        })?;

        Ok(Self {
            metadata,
            catalogs,
            compiled,
            storage,
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
            QueryResponseSlot::SourceField { key } => {
                ResponseSlot::SourceField { key: key.clone() }
            }
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
    /// The instant this request is signed as of, as RFC 3339.
    ///
    /// wasm has no clock, and file URLs are signed with a day-scoped key and
    /// carry an expiry, so the time has to arrive with the request. Absent is
    /// only valid for a deployment with no attachments; one that has them and
    /// omits the clock is refused rather than served URLs dated from the epoch.
    #[serde(default)]
    pub now: Option<String>,
    /// Absolute prefix for engine-served URLs. Empty means same-origin.
    #[serde(default)]
    pub external_base_url: Option<String>,
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
    //    Actions come first: they are custom GraphQL fields the engine never
    //    resolves from SQL, so an action operation must not reach the planner
    //    at all — there it would fail as an unknown root field.
    if let Some(ctx) =
        donat_action::match_action(&state.metadata, &doc, input.operation_name.as_deref())
    {
        return action_plan(&ctx, &session, &doc, input);
    }

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
    // Attach the request's storage material, so a file column can be selected
    // and an upload URL minted. A deployment that declares no attachment
    // resolves to an empty registry and skips this entirely, so the planner
    // behaves exactly as it did before.
    let storage_ctx = if state.storage.is_empty() {
        None
    } else {
        match request_storage(state, input) {
            Ok(ctx) => Some(ctx),
            Err(e) => return error_plan(&e),
        }
    };
    let mut planner = planner;
    if let Some(ctx) = &storage_ctx {
        planner.set_storage(ctx);
    }

    let plan = match planner.plan(
        &doc,
        input.operation_name.as_deref(),
        &input.variables,
        &session,
    ) {
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
                    vec![Statement {
                        alias: "data".into(),
                        sql,
                        params: vec![],
                    }]
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
        MultiSourcePlan::Mutation {
            roots, response, ..
        } => {
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
                hooks.extend(hooks_for_root(root, &alias, &state.metadata));
                statements.push(Statement {
                    alias,
                    sql,
                    params: vec![],
                });
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
    alias: &str,
    metadata: &donat_metadata::Metadata,
) -> Vec<crate::plan::Hook> {
    let mut out = Vec::new();
    for (schema, table, op) in written_tables(root) {
        collect_hooks(metadata, alias, schema, table, op, &mut out);
    }
    out
}

fn collect_hooks(
    metadata: &donat_metadata::Metadata,
    alias: &str,
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
                        alias: alias.to_string(),
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

/// Build the plan for an operation whose top-level fields are actions.
///
/// Every item is resolved to either a literal `__typename` or one call the host
/// must make. Nothing here knows how the call travels: a handler-less action is
/// one the host resolves in its own process, and one with a handler is a
/// webhook, which is the host's decision to make and the core's to describe.
fn action_plan(
    ctx: &donat_action::ActionContext,
    session: &Session,
    doc: &graphql_parser::query::Document<'static, String>,
    input: &CompileInput,
) -> PlanV1 {
    let items = match donat_action::selection_items(doc, input.operation_name.as_deref()) {
        Ok(items) => items,
        Err(e) => return action_error_plan(&e),
    };
    let mut planned = Vec::with_capacity(items.len());
    for item in items {
        match donat_action::plan_item(ctx, &session.role, &session.vars, item, &input.variables) {
            Ok(planned_item) => planned.push(planned_item),
            Err(e) => return action_error_plan(&e),
        }
    }
    PlanV1::Action(crate::plan::ActionPlanBody {
        version: PLAN_VERSION,
        is_query: ctx.is_query(),
        items: planned,
    })
}

fn action_error_plan(e: &donat_action::ActionError) -> PlanV1 {
    PlanV1::Error(PlanErrorBody {
        version: PLAN_VERSION,
        code: e.code.clone(),
        path: e.path.clone(),
        message: e.message.clone(),
    })
}

/// What the host sends back once it has called every action in the plan.
///
/// The core is stateless between calls, so the request is repeated rather than
/// remembered: shaping needs the selection set, and holding it across an
/// arbitrary host call would make one instance unusable for anything else while
/// a handler was in flight.
#[derive(Deserialize)]
pub struct ShapeInput {
    pub query: String,
    #[serde(default)]
    pub operation_name: Option<String>,
    #[serde(default)]
    pub variables: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub session_vars: HashMap<String, String>,
    /// What each action returned, keyed by the plan's response alias.
    #[serde(default)]
    pub results: serde_json::Map<String, serde_json::Value>,
}

/// The shaped response, or the error that shaping produced.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ShapeResult {
    Data {
        data: serde_json::Map<String, serde_json::Value>,
    },
    Error(PlanErrorBody),
}

/// Shape the results of an action operation against its declared output types
/// and the caller's selection set.
///
/// This is the same `validate` the standalone server applies to a webhook
/// response. An embedded host that skipped it would return whatever a Go
/// function happened to produce, so a field declared `String!` could answer
/// `null` on one host and error on the other.
pub fn shape(state: &CoreState, input: &ShapeInput) -> ShapeResult {
    let session = match session_from(&input.session_vars) {
        Ok(s) => s,
        Err(e) => return shape_error(&e.path, &e.code, &e.message),
    };
    let doc = match graphql_parser::parse_query::<String>(&input.query) {
        Ok(d) => d.into_static(),
        Err(e) => return shape_error("$", "validation-failed", &e.to_string()),
    };
    let Some(ctx) =
        donat_action::match_action(&state.metadata, &doc, input.operation_name.as_deref())
    else {
        return shape_error("$", "unexpected", "this operation does not target actions");
    };
    let items = match donat_action::selection_items(&doc, input.operation_name.as_deref()) {
        Ok(items) => items,
        Err(e) => return shape_error(&e.path, &e.code, &e.message),
    };

    let mut data = serde_json::Map::new();
    for item in items {
        let planned = match donat_action::plan_item(
            &ctx,
            &session.role,
            &session.vars,
            item,
            &input.variables,
        ) {
            Ok(planned) => planned,
            Err(e) => return shape_error(&e.path, &e.code, &e.message),
        };
        match planned {
            donat_action::ActionItem::Typename { alias, value } => {
                data.insert(alias, serde_json::Value::String(value));
            }
            donat_action::ActionItem::Call(call) => {
                // The host is expected to answer every call the plan named; a
                // missing one is a host bug, and treating it as `null` would
                // hide it behind a nullability error somewhere else.
                let Some(raw) = input.results.get(&call.alias) else {
                    return shape_error(
                        "$",
                        "unexpected",
                        &format!("the host returned no result for action '{}'", call.name),
                    );
                };
                let Some(action) = ctx.find(&call.name) else {
                    return shape_error(
                        "$",
                        "unexpected",
                        &format!(
                            "action '{}' vanished between planning and shaping",
                            call.name
                        ),
                    );
                };
                let Some(field) = action_field(item) else {
                    return shape_error("$", "unexpected", "an action item lost its field");
                };
                let ty = donat_action::parse_type(&action.definition.output_type);
                match donat_action::validate(
                    ctx.custom_types(),
                    &ty,
                    raw,
                    &field.selection_set.items,
                ) {
                    Ok(shaped) => {
                        data.insert(call.alias, shaped);
                    }
                    // Output-shape errors are reported at the top level, as the
                    // standalone server reports them.
                    Err(message) => return shape_error("$", "unexpected", &message),
                }
            }
        }
    }
    ShapeResult::Data { data }
}

fn action_field<'a>(
    item: &'a graphql_parser::query::Selection<'static, String>,
) -> Option<&'a graphql_parser::query::Field<'static, String>> {
    match item {
        graphql_parser::query::Selection::Field(f) => Some(f),
        _ => None,
    }
}

fn shape_error(path: &str, code: &str, message: &str) -> ShapeResult {
    ShapeResult::Error(PlanErrorBody {
        version: PLAN_VERSION,
        code: code.to_string(),
        path: path.to_string(),
        message: message.to_string(),
    })
}

/// Build the per-request storage material from the clock the host supplied.
fn request_storage<'a>(
    state: &'a CoreState,
    input: &CompileInput,
) -> Result<donat_storage::RequestContext<'a>, PlanError> {
    let Some(raw) = input.now.as_deref() else {
        return Err(PlanError::new(
            "$",
            "unexpected",
            "this deployment declares file attachments, so the request must carry \
             `now`: URLs are signed with a day-scoped key and carry an expiry, \
             and wasm has no clock to read",
        ));
    };
    let now = chrono::DateTime::parse_from_rfc3339(raw)
        .map_err(|e| {
            PlanError::new(
                "$",
                "unexpected",
                format!("`now` is not an RFC 3339 instant: {e}"),
            )
        })?
        .with_timezone(&chrono::Utc);

    Ok(donat_storage::RequestContext {
        registry: &state.storage,
        now,
        // Production mints a fresh id per request; the entropy comes from the
        // host through `donat_host.random_bytes`.
        fixed_upload_id: None,
        external_base_url: input.external_base_url.clone().unwrap_or_default(),
    })
}

/// What the host must know to finish an upload it has already stored.
///
/// The engine never touches the bytes: a presigned `PUT` writes them straight
/// to the store. Finishing means asking the store what it actually received,
/// moving the object out from under the URL that wrote it, and recording the
/// verified size — four operations, every one of them signed.
///
/// The signing lives here rather than in the host. `donat-storage` already
/// owns it and has a twin in SQL that the conformance suite pins against a
/// real MinIO; a third implementation in Go would be a third thing to keep
/// exactly right, and ADR 033 already names the two as the price of the
/// design. The host does dumb HTTP with URLs it is handed.
#[derive(Deserialize)]
pub struct CompletionInput {
    pub upload_id: String,
    pub attachment: String,
    pub backend: String,
    /// Where the presigned PUT wrote the bytes.
    pub staging_key: String,
    /// RFC 3339. wasm has no clock, and a signature carries an expiry.
    pub now: String,
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CompletionResult {
    Urls(CompletionUrls),
    Error(PlanErrorBody),
}

#[derive(serde::Serialize)]
pub struct CompletionUrls {
    /// Ask the store what it holds, so a claim cannot succeed for bytes
    /// nobody uploaded.
    pub head_url: String,
    /// Copy to the address the row will name. The presigned PUT stays valid
    /// for its whole lifetime and cannot be revoked, so bytes left where it
    /// wrote them could be replaced after a claim certified them.
    pub copy_url: String,
    pub copy_headers: Vec<(String, String)>,
    /// Drop the staging object, after the row points at the final key.
    pub delete_url: String,
    pub final_key: String,
    /// The declaration's ceiling, so the host can refuse an object that grew
    /// between the request and the upload.
    pub max_bytes: u64,
}

/// Sign the operations that finish one upload.
pub fn file_completion(state: &CoreState, input: &CompletionInput) -> CompletionResult {
    let Ok(upload_id) = uuid::Uuid::parse_str(&input.upload_id) else {
        return completion_error("upload_id is not a uuid");
    };
    let Ok(now) = chrono::DateTime::parse_from_rfc3339(&input.now) else {
        return completion_error("`now` is not an RFC 3339 instant");
    };
    let now = now.with_timezone(&chrono::Utc);

    let Some(spec) = state.storage.attachment(&input.attachment) else {
        return completion_error(&format!("unknown attachment '{}'", input.attachment));
    };
    let Some(donat_storage::Backend::S3(s3)) = state.storage.backend(&input.backend) else {
        return completion_error(&format!("unknown storage backend '{}'", input.backend));
    };

    let final_key = spec.object_key(upload_id);
    let (copy_url, copy_headers) = s3.presign_copy(&input.staging_key, &final_key, now, 60);
    CompletionResult::Urls(CompletionUrls {
        head_url: s3.presign("HEAD", &input.staging_key, now, 60),
        copy_url,
        copy_headers,
        delete_url: s3.presign("DELETE", &input.staging_key, now, 60),
        final_key,
        max_bytes: spec.max_bytes,
    })
}

fn completion_error(message: &str) -> CompletionResult {
    CompletionResult::Error(PlanErrorBody {
        version: PLAN_VERSION,
        code: "unexpected".to_string(),
        path: "$".to_string(),
        message: message.to_string(),
    })
}
