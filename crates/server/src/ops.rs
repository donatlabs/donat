//! Runtime metadata operations + run_sql: the surface that tests-py setup
//! steps drive. Accepts both the legacy v1 names (`track_table`) and the
//! pg-prefixed v2 names (`pg_track_table`); `bulk` runs a list in order.
//!
//! In our model "everything builds from YAML", so these operations are the
//! protocol equivalent of editing the metadata directory and hot-reloading:
//! they mutate the in-memory [`Metadata`] and the engine re-plans against
//! the new state. There is no metadata database.

use dist_metadata::{
    ArrayRelationship, ObjectRelationship, PermissionEntry, QualifiedTable, TableEntry,
};
use dist_schema::{ADMIN_ROLE, PlanError, Planner, Session};
use serde_json::{Value as Json, json};

/// The admin superuser session (no role header on an admin-authenticated
/// request maps to the admin role, like Hasura).
fn admin_session() -> Session {
    Session {
        role: ADMIN_ROLE.to_string(),
        vars: std::collections::HashMap::new(),
        backend_request: false,
    }
}

use crate::state::{SharedState, ensure_default_source};

fn plan_err(e: PlanError) -> OpError {
    OpError {
        status: 400,
        body: json!({ "code": e.code, "error": e.message, "path": e.path }),
    }
}

#[derive(Debug)]
pub struct OpError {
    pub status: u16,
    pub body: Json,
}

impl OpError {
    fn bad_request(code: &str, error: impl Into<String>) -> Self {
        OpError {
            status: 400,
            body: json!({ "code": code, "error": error.into(), "path": "$" }),
        }
    }
}

/// Execute one metadata/query API operation (recursing into `bulk`).
pub async fn execute(
    state: &SharedState,
    op: &Json,
    session: Option<&Session>,
) -> Result<Json, OpError> {
    let op_type = op
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected an object with a 'type'"))?;
    let args = op.get("args").unwrap_or(&Json::Null);

    // v2/metadata names are the v1 names with a backend prefix.
    let normalized = op_type.strip_prefix("pg_").unwrap_or(op_type);
    tracing::debug!(op = %normalized, "metadata/query op");

    match normalized {
        "bulk" => {
            let items = args
                .as_array()
                .ok_or_else(|| OpError::bad_request("parse-failed", "bulk args must be a list"))?;
            let mut results = vec![];
            for item in items {
                results.push(Box::pin(execute(state, item, session)).await?);
            }
            Ok(Json::Array(results))
        }
        "run_sql" => run_sql(state, args).await,
        "insert" => match session {
            Some(session) => v1_insert(state, args, session).await,
            None => insert_rows(state, args).await,
        },
        // No session = admin-authenticated request with no explicit role
        // (the endpoint already passed check_admin_secret): run as admin.
        "select" => v1_select(state, args, session.unwrap_or(&admin_session())).await,
        "count" => v1_count(state, args, session.unwrap_or(&admin_session())).await,
        "update" => v1_update(state, args, session.unwrap_or(&admin_session())).await,
        "delete" => v1_delete(state, args, session.unwrap_or(&admin_session())).await,
        "track_table" => track_table(state, args).await,
        "untrack_table" => untrack_table(state, args).await,
        "create_object_relationship" => create_relationship(state, args, true).await,
        "create_array_relationship" => create_relationship(state, args, false).await,
        "drop_relationship" => drop_relationship(state, args).await,
        "create_select_permission" => create_permission(state, args, "select").await,
        "create_insert_permission" => create_permission(state, args, "insert").await,
        "create_update_permission" => create_permission(state, args, "update").await,
        "create_delete_permission" => create_permission(state, args, "delete").await,
        "drop_select_permission" => drop_permission(state, args, "select").await,
        "drop_insert_permission" => drop_permission(state, args, "insert").await,
        "drop_update_permission" => drop_permission(state, args, "update").await,
        "drop_delete_permission" => drop_permission(state, args, "delete").await,
        "track_function" => track_function(state, args).await,
        "create_function_permission" | "add_function_permission" => {
            function_permission(state, args, true).await
        }
        "drop_function_permission" => function_permission(state, args, false).await,
        "untrack_function" => untrack_function(state, args).await,
        "add_computed_field" => add_computed_field(state, args).await,
        "create_remote_relationship" | "update_remote_relationship" => {
            remote_relationship(state, args, true).await
        }
        "delete_remote_relationship" => remote_relationship(state, args, false).await,
        "drop_computed_field" => drop_computed_field(state, args).await,
        "add_inherited_role" => inherited_role(state, args, true).await,
        "drop_inherited_role" => inherited_role(state, args, false).await,
        "add_remote_schema" | "update_remote_schema" => remote_schema(state, args, true).await,
        "remove_remote_schema" => remote_schema(state, args, false).await,
        "add_remote_schema_permissions" => remote_schema_permission(state, args, true).await,
        "drop_remote_schema_permissions" => remote_schema_permission(state, args, false).await,
        "create_query_collection" => query_collection(state, args, true).await,
        "drop_query_collection" => query_collection(state, args, false).await,
        "add_query_to_collection" => collection_query(state, args, true).await,
        "drop_query_from_collection" => collection_query(state, args, false).await,
        "add_collection_to_allowlist" => allowlist(state, args, true).await,
        "drop_collection_from_allowlist" => allowlist(state, args, false).await,
        "clear_metadata" => clear_metadata(state).await,
        "export_metadata" => export_metadata(state).await,
        "get_inconsistent_metadata" => get_inconsistent_metadata(state).await,
        "replace_metadata" => replace_metadata(state, args).await,
        other => Err(OpError::bad_request(
            "not-supported",
            format!("operation '{other}' is not supported yet"),
        )),
    }
}

/// Hasura's run_sql: text-protocol execution, results as strings.
async fn run_sql(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    let sql = args
        .get("sql")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "run_sql needs args.sql"))?;
    let read_only = args
        .get("read_only")
        .and_then(Json::as_bool)
        .unwrap_or(false);

    let source = args
        .get("source")
        .and_then(Json::as_str)
        .unwrap_or("default");
    let pool = match state.pool(source).await {
        Some(pool) => pool,
        None => state.default_pool().await.ok_or_else(|| {
            OpError::bad_request("not-exists", format!("source \"{source}\" not found"))
        })?,
    };
    let client = pool.get().await.map_err(internal)?;
    let messages = client.simple_query(sql).await.map_err(|e| {
        let message = e
            .as_db_error()
            .map(|db| db.message().to_string())
            .unwrap_or_else(|| e.to_string());
        OpError {
            status: 400,
            body: json!({
                "code": "postgres-error",
                "error": "query execution failed",
                "path": "$",
                "internal": { "error": { "message": message } },
            }),
        }
    })?;

    let mut columns: Vec<String> = vec![];
    let mut rows: Vec<Vec<String>> = vec![];
    for message in &messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = message {
            if columns.is_empty() {
                columns = row.columns().iter().map(|c| c.name().to_string()).collect();
            }
            rows.push(
                (0..row.len())
                    .map(|i| row.get(i).unwrap_or("NULL").to_string())
                    .collect(),
            );
        }
    }

    if !read_only {
        state.reintrospect().await.map_err(internal)?;
    }

    if columns.is_empty() {
        Ok(json!({ "result_type": "CommandOk", "result": null }))
    } else {
        let mut result = vec![columns];
        result.append(&mut rows);
        Ok(json!({ "result_type": "TuplesOk", "result": result }))
    }
}

/// Run one generated SQL statement and decode the single json column.
async fn run_one(state: &SharedState, sql: &str) -> Result<Json, OpError> {
    let pool = state
        .default_pool()
        .await
        .ok_or_else(|| OpError::bad_request("not-exists", "no default source"))?;
    let client = pool.get().await.map_err(internal)?;
    tracing::debug!(%sql, "executing v1 data op");
    let row = client.query_one(sql, &[]).await.map_err(|e| {
        // dist_api.check_violation() raises 23514 with a JSON payload; the
        // v1 data API reports it as Hasura's permission-error shape.
        if let Some(db) = e.as_db_error()
            && db.code().code() == "23514"
        {
            let message = serde_json::from_str::<Json>(db.message())
                .ok()
                .and_then(|p| p.get("message").and_then(Json::as_str).map(str::to_string))
                .unwrap_or_else(|| db.message().to_string());
            return OpError {
                status: 400,
                body: json!({ "code": "permission-error", "error": message, "path": "$.args" }),
            };
        }
        let message = e
            .as_db_error()
            .map(|db| db.message().to_string())
            .unwrap_or_else(|| e.to_string());
        OpError {
            status: 400,
            body: json!({ "code": "postgres-error", "error": message, "path": "$" }),
        }
    })?;
    row.try_get::<_, Json>(0).map_err(internal)
}

/// v1 `select` with a role: rows array, permission-checked.
async fn v1_select(state: &SharedState, args: &Json, session: &Session) -> Result<Json, OpError> {
    let engine = state.engine.read().await;
    let catalog = engine.default_catalog();
    let planner = Planner::new(&engine.metadata, &catalog);
    let query = planner.plan_v1_select(args, session).map_err(plan_err)?;
    let sql = dist_sqlgen::operation_to_sql(&[dist_ir::RootField::Select {
        alias: "result".to_string(),
        query,
    }]);
    drop(engine);
    let mut root = run_one(state, &sql).await?;
    Ok(root
        .get_mut("result")
        .map(Json::take)
        .unwrap_or(Json::Array(vec![])))
}

/// v1 `count` with a role: `{ count: N }`.
async fn v1_count(state: &SharedState, args: &Json, session: &Session) -> Result<Json, OpError> {
    let engine = state.engine.read().await;
    let catalog = engine.default_catalog();
    let planner = Planner::new(&engine.metadata, &catalog);
    let query = planner.plan_v1_count(args, session).map_err(plan_err)?;
    let sql = dist_sqlgen::operation_to_sql(&[dist_ir::RootField::Select {
        alias: "result".to_string(),
        query,
    }]);
    drop(engine);
    let root = run_one(state, &sql).await?;
    let count = root
        .pointer("/result/count/count")
        .cloned()
        .unwrap_or(Json::Null);
    Ok(json!({ "count": count }))
}

async fn v1_insert(state: &SharedState, args: &Json, session: &Session) -> Result<Json, OpError> {
    let engine = state.engine.read().await;
    let catalog = engine.default_catalog();
    let planner = Planner::new(&engine.metadata, &catalog);
    let insert = planner.plan_v1_insert(args, session).map_err(plan_err)?;
    let sql = dist_sqlgen::mutation_to_sql(&dist_ir::MutationRoot::Insert {
        alias: "result".to_string(),
        insert,
    });
    drop(engine);
    run_one(state, &sql).await
}

async fn v1_update(state: &SharedState, args: &Json, session: &Session) -> Result<Json, OpError> {
    let engine = state.engine.read().await;
    let catalog = engine.default_catalog();
    let planner = Planner::new(&engine.metadata, &catalog);
    let update = planner.plan_v1_update(args, session).map_err(plan_err)?;
    let sql = dist_sqlgen::mutation_to_sql(&dist_ir::MutationRoot::Update {
        alias: "result".to_string(),
        update,
    });
    drop(engine);
    run_one(state, &sql).await
}

async fn v1_delete(state: &SharedState, args: &Json, session: &Session) -> Result<Json, OpError> {
    let engine = state.engine.read().await;
    let catalog = engine.default_catalog();
    let planner = Planner::new(&engine.metadata, &catalog);
    let delete = planner.plan_v1_delete(args, session).map_err(plan_err)?;
    let sql = dist_sqlgen::mutation_to_sql(&dist_ir::MutationRoot::Delete {
        alias: "result".to_string(),
        delete,
    });
    drop(engine);
    run_one(state, &sql).await
}

/// Legacy v1 data API `insert`: seeds rows in setup fixtures.
async fn insert_rows(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    let objects = args
        .get("objects")
        .and_then(Json::as_array)
        .ok_or_else(|| OpError::bad_request("parse-failed", "insert needs args.objects"))?;
    if objects.is_empty() {
        return Ok(json!({ "affected_rows": 0 }));
    }

    let engine = state.engine.read().await;
    let catalog = engine.default_catalog();
    let info = catalog
        .table(table.schema(), table.name())
        .ok_or_else(|| {
            OpError::bad_request("not-exists", format!("table \"{table}\" does not exist"))
        })?;

    // Union of columns across all objects, in catalog order.
    let mut columns: Vec<&str> = vec![];
    for object in objects {
        let Some(map) = object.as_object() else {
            return Err(OpError::bad_request("parse-failed", "objects must be objects"));
        };
        for key in map.keys() {
            if !columns.contains(&key.as_str()) {
                columns.push(key);
            }
        }
    }

    let mut rows = vec![];
    for object in objects {
        let map = object.as_object().unwrap();
        let mut values = vec![];
        for col in &columns {
            let pg_type = info
                .column(col)
                .map(|c| c.pg_type.as_str())
                .unwrap_or("text");
            let value = match map.get(*col) {
                None => "DEFAULT".to_string(),
                Some(Json::Null) => "NULL".to_string(),
                Some(Json::Bool(b)) => if *b { "TRUE" } else { "FALSE" }.to_string(),
                Some(Json::Number(n)) => format!("({n})::{}", quote_ident_sql(pg_type)),
                Some(Json::String(s)) => {
                    format!("({})::{}", quote_lit_sql(s), quote_ident_sql(pg_type))
                }
                Some(other) => {
                    format!("({})::{}", quote_lit_sql(&other.to_string()), quote_ident_sql(pg_type))
                }
            };
            values.push(value);
        }
        rows.push(format!("({})", values.join(", ")));
    }
    let column_list: Vec<String> = columns.iter().map(|c| quote_ident_sql(c)).collect();
    let sql = format!(
        "INSERT INTO {}.{} ({}) VALUES {}",
        quote_ident_sql(table.schema()),
        quote_ident_sql(table.name()),
        column_list.join(", "),
        rows.join(", ")
    );
    drop(engine);

    let pool = state
        .default_pool()
        .await
        .ok_or_else(|| OpError::bad_request("not-exists", "no default source"))?;
    let client = pool.get().await.map_err(internal)?;
    client.simple_query(&sql).await.map_err(|e| {
        let message = e
            .as_db_error()
            .map(|db| db.message().to_string())
            .unwrap_or_else(|| e.to_string());
        OpError {
            status: 400,
            body: json!({
                "code": "postgres-error",
                "error": "insert failed",
                "path": "$",
                "internal": { "error": { "message": message } },
            }),
        }
    })?;
    Ok(json!({ "affected_rows": objects.len() }))
}

fn quote_ident_sql(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

fn quote_lit_sql(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn parse_table(args: &Json) -> Result<QualifiedTable, OpError> {
    let table = args.get("table").unwrap_or(args);
    if let Some(name) = table.as_str() {
        return Ok(QualifiedTable::Name(name.to_string()));
    }
    if let Some(obj) = table.as_object() {
        let name = obj.get("name").and_then(Json::as_str);
        // v1 track_table style: { schema, name } directly in args
        let name = name.or_else(|| args.get("name").and_then(Json::as_str));
        if let Some(name) = name {
            let schema = obj
                .get("schema")
                .and_then(Json::as_str)
                .or_else(|| args.get("schema").and_then(Json::as_str))
                .unwrap_or("public");
            return Ok(QualifiedTable::Qualified {
                schema: schema.to_string(),
                name: name.to_string(),
            });
        }
    }
    Err(OpError::bad_request("parse-failed", "cannot parse table"))
}

async fn track_table(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    {
        let engine = state.engine.read().await;
        if engine
            .default_catalog()
            .table(table.schema(), table.name())
            .is_none()
        {
            return Err(OpError::bad_request(
                "not-exists",
                format!("no such table/view exists in source: \"{table}\""),
            ));
        }
    }
    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let tables = &mut engine.metadata.sources[0].tables;
    if tables.iter().any(|t| {
        t.table.schema() == table.schema() && t.table.name() == table.name()
    }) {
        return Err(OpError {
            status: 400,
            body: json!({
                "code": "already-tracked",
                "error": format!("view/table already tracked: \"{table}\""),
                "path": "$",
            }),
        });
    }
    let configuration = args
        .get("configuration")
        .cloned()
        .filter(|v| !v.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
    tables.push(TableEntry {
        table,
        configuration,
        is_enum: false,
        object_relationships: vec![],
        array_relationships: vec![],
        computed_fields: vec![],
        remote_relationships: vec![],
        insert_permissions: vec![],
        select_permissions: vec![],
        update_permissions: vec![],
        delete_permissions: vec![],
    });
    Ok(json!({ "message": "success" }))
}

async fn untrack_table(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let tables = &mut engine.metadata.sources[0].tables;
    let before = tables.len();
    tables.retain(|t| {
        !(t.table.schema() == table.schema() && t.table.name() == table.name())
    });
    if tables.len() == before {
        return Err(OpError::bad_request(
            "already-untracked",
            format!("view/table already untracked: \"{table}\""),
        ));
    }
    Ok(json!({ "message": "success" }))
}

/// Find the tracked entry for `args.table`, or fail.
fn entry_mut<'a>(
    tables: &'a mut Vec<TableEntry>,
    table: &QualifiedTable,
) -> Result<&'a mut TableEntry, OpError> {
    tables
        .iter_mut()
        .find(|t| t.table.schema() == table.schema() && t.table.name() == table.name())
        .ok_or_else(|| {
            OpError::bad_request("not-exists", format!("table \"{table}\" is not tracked"))
        })
}

async fn create_relationship(
    state: &SharedState,
    args: &Json,
    object: bool,
) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    let name = args
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "relationship needs a name"))?
        .to_string();
    let using = args
        .get("using")
        .cloned()
        .ok_or_else(|| OpError::bad_request("parse-failed", "relationship needs 'using'"))?;

    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let entry = entry_mut(&mut engine.metadata.sources[0].tables, &table)?;
    if object {
        let using: dist_metadata::ObjRelUsing = serde_json::from_value(using)
            .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
        entry.object_relationships.push(ObjectRelationship {
            name,
            using,
            comment: None,
        });
    } else {
        let using: dist_metadata::ArrRelUsing = serde_json::from_value(using)
            .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
        entry.array_relationships.push(ArrayRelationship {
            name,
            using,
            comment: None,
        });
    }
    Ok(json!({ "message": "success" }))
}

async fn drop_relationship(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    let name = args
        .get("relationship")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "drop_relationship needs a name"))?;
    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let entry = entry_mut(&mut engine.metadata.sources[0].tables, &table)?;
    entry.object_relationships.retain(|r| r.name != name);
    entry.array_relationships.retain(|r| r.name != name);
    Ok(json!({ "message": "success" }))
}

async fn create_permission(
    state: &SharedState,
    args: &Json,
    kind: &str,
) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    let role = args
        .get("role")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "permission needs a role"))?
        .to_string();
    let permission = args
        .get("permission")
        .cloned()
        .ok_or_else(|| OpError::bad_request("parse-failed", "missing 'permission'"))?;

    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let entry = entry_mut(&mut engine.metadata.sources[0].tables, &table)?;

    fn push<T: serde::de::DeserializeOwned>(
        list: &mut Vec<PermissionEntry<T>>,
        role: String,
        permission: Json,
    ) -> Result<(), OpError> {
        if list.iter().any(|p| p.role == role) {
            return Err(OpError::bad_request(
                "already-exists",
                format!("permission already defined for role \"{role}\""),
            ));
        }
        let permission: T = serde_json::from_value(permission)
            .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
        list.push(PermissionEntry {
            role,
            permission,
            comment: None,
        });
        Ok(())
    }

    match kind {
        "select" => push(&mut entry.select_permissions, role, permission)?,
        "insert" => push(&mut entry.insert_permissions, role, permission)?,
        "update" => push(&mut entry.update_permissions, role, permission)?,
        "delete" => push(&mut entry.delete_permissions, role, permission)?,
        _ => unreachable!(),
    }
    Ok(json!({ "message": "success" }))
}

async fn drop_permission(state: &SharedState, args: &Json, kind: &str) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    let role = args
        .get("role")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "permission needs a role"))?;
    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let entry = entry_mut(&mut engine.metadata.sources[0].tables, &table)?;
    match kind {
        "select" => entry.select_permissions.retain(|p| p.role != role),
        "insert" => entry.insert_permissions.retain(|p| p.role != role),
        "update" => entry.update_permissions.retain(|p| p.role != role),
        "delete" => entry.delete_permissions.retain(|p| p.role != role),
        _ => unreachable!(),
    }
    Ok(json!({ "message": "success" }))
}

/// `track_function` v1: args is {schema, name}; v2: {function, configuration}.
async fn track_function(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    let function = if args.get("function").is_some() {
        let f = args.get("function").unwrap();
        parse_table(&json!({ "table": f }))?
    } else {
        parse_table(args)?
    };
    {
        let engine = state.engine.read().await;
        if engine
            .default_catalog()
            .function(function.schema(), function.name())
            .is_none()
        {
            return Err(OpError::bad_request(
                "not-exists",
                format!("no such function exists: \"{function}\""),
            ));
        }
    }
    let configuration = args
        .get("configuration")
        .cloned()
        .filter(|v| !v.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let functions = &mut engine.metadata.sources[0].functions;
    if functions.iter().any(|f| {
        f.function.schema() == function.schema() && f.function.name() == function.name()
    }) {
        return Err(OpError::bad_request(
            "already-tracked",
            format!("function already tracked: \"{function}\""),
        ));
    }
    functions.push(dist_metadata::FunctionEntry {
        function,
        configuration,
        permissions: vec![],
    });
    Ok(json!({ "message": "success" }))
}

async fn function_permission(
    state: &SharedState,
    args: &Json,
    add: bool,
) -> Result<Json, OpError> {
    let function = if args.get("function").is_some() {
        let f = args.get("function").unwrap();
        parse_table(&json!({ "table": f }))?
    } else {
        parse_table(args)?
    };
    let role = args
        .get("role")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected a role"))?
        .to_string();
    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let entry = engine.metadata.sources[0]
        .functions
        .iter_mut()
        .find(|f| {
            f.function.schema() == function.schema() && f.function.name() == function.name()
        })
        .ok_or_else(|| {
            OpError::bad_request("not-exists", format!("function \"{function}\" not tracked"))
        })?;
    if add {
        if !entry.permissions.iter().any(|p| p.role == role) {
            entry
                .permissions
                .push(dist_metadata::FunctionPermission { role });
        }
    } else {
        entry.permissions.retain(|p| p.role != role);
    }
    Ok(json!({ "message": "success" }))
}

async fn untrack_function(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    let function = if args.get("function").is_some() {
        let f = args.get("function").unwrap();
        parse_table(&json!({ "table": f }))?
    } else {
        parse_table(args)?
    };
    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    engine.metadata.sources[0].functions.retain(|f| {
        !(f.function.schema() == function.schema() && f.function.name() == function.name())
    });
    Ok(json!({ "message": "success" }))
}

async fn remote_relationship(state: &SharedState, args: &Json, add: bool) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    let name = args
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected a name"))?
        .to_string();
    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let entry = entry_mut(&mut engine.metadata.sources[0].tables, &table)?;
    entry.remote_relationships.retain(|r| r.name != name);
    if add {
        let rel: dist_metadata::RemoteRelationship = serde_json::from_value(args.clone())
            .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
        entry.remote_relationships.push(rel);
    }
    Ok(json!({ "message": "success" }))
}

async fn add_computed_field(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    let name = args
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "computed field needs a name"))?
        .to_string();
    let definition: dist_metadata::ComputedFieldDefinition = args
        .get("definition")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?
        .ok_or_else(|| OpError::bad_request("parse-failed", "missing 'definition'"))?;

    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let entry = entry_mut(&mut engine.metadata.sources[0].tables, &table)?;
    entry.computed_fields.push(dist_metadata::ComputedField {
        name,
        definition,
        comment: None,
    });
    Ok(json!({ "message": "success" }))
}

async fn drop_computed_field(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    let table = parse_table(args)?;
    let name = args
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "computed field needs a name"))?;
    let mut engine = state.engine.write().await;
    ensure_default_source(&mut engine.metadata);
    let entry = entry_mut(&mut engine.metadata.sources[0].tables, &table)?;
    entry.computed_fields.retain(|c| c.name != name);
    Ok(json!({ "message": "success" }))
}

async fn inherited_role(state: &SharedState, args: &Json, add: bool) -> Result<Json, OpError> {
    let role_name = args
        .get("role_name")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected role_name"))?
        .to_string();
    let mut engine = state.engine.write().await;
    if add {
        let role_set: Vec<String> = args
            .get("role_set")
            .and_then(Json::as_array)
            .map(|roles| {
                roles
                    .iter()
                    .filter_map(|r| r.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if role_set.is_empty() {
            return Err(OpError::bad_request("parse-failed", "expected a non-empty role_set"));
        }
        // Reject cycles: walking the parents must never reach role_name.
        let mut stack = role_set.clone();
        let mut seen = std::collections::HashSet::new();
        while let Some(current) = stack.pop() {
            if current == role_name {
                return Err(OpError::bad_request(
                    "cyclic-role-hierarchy",
                    format!("found cycle(s) in roles: {role_name}"),
                ));
            }
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(inherited) = engine
                .metadata
                .inherited_roles
                .iter()
                .find(|r| r.role_name == current)
            {
                stack.extend(inherited.role_set.iter().cloned());
            }
        }
        engine
            .metadata
            .inherited_roles
            .retain(|r| r.role_name != role_name);
        engine
            .metadata
            .inherited_roles
            .push(dist_metadata::InheritedRole {
                role_name,
                role_set,
            });
    } else {
        engine
            .metadata
            .inherited_roles
            .retain(|r| r.role_name != role_name);
    }
    Ok(json!({ "message": "success" }))
}

async fn remote_schema(state: &SharedState, args: &Json, add: bool) -> Result<Json, OpError> {
    let name = args
        .get("name")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected a name"))?
        .to_string();
    let mut engine = state.engine.write().await;
    // update_remote_schema keeps the existing permissions; the previous
    // entry is restored if the update fails validation.
    let previous = engine
        .metadata
        .remote_schemas
        .iter()
        .find(|r| r.name == name)
        .cloned();
    let kept_permissions = previous
        .as_ref()
        .map(|r| r.permissions.clone())
        .unwrap_or_default();
    engine.metadata.remote_schemas.retain(|r| r.name != name);
    if add {
        let mut schema: dist_metadata::RemoteSchema = serde_json::from_value(args.clone())
            .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
        if schema.permissions.is_empty() {
            schema.permissions = kept_permissions;
        }
        // Introspect upstream so permission SDLs can be validated.
        let raw_url = schema
            .definition
            .url
            .clone()
            .or_else(|| {
                schema
                    .definition
                    .url_from_env
                    .as_ref()
                    .and_then(|v| std::env::var(v).ok())
            })
            .unwrap_or_default();
        let url = crate::remote::resolve_url_template(&raw_url);
        engine.metadata.remote_schemas.push(schema);
        drop(engine);
        const INTROSPECTION: &str = "query { __schema { types { kind name fields(includeDeprecated: true) { name args { name defaultValue type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } enumValues(includeDeprecated: true) { name } inputFields { name type { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } possibleTypes { name } interfaces { name } } } }";
        if let Ok(resp) = state
            .http
            .post(&url)
            .json(&json!({ "query": INTROSPECTION }))
            .send()
            .await
        {
            if let Ok(body) = resp.json::<Json>().await {
                let upstream = crate::remote_validate::parse_upstream(&body);
                // update_remote_schema re-validates the permissions it keeps
                // against the new upstream; a mismatch fails the update.
                let engine = state.engine.read().await;
                if let Some(schema) = engine.metadata.remote_schemas.iter().find(|r| r.name == name)
                {
                    for perm in &schema.permissions {
                        if let Err(report) =
                            crate::remote_validate::validate(&perm.definition.schema, &upstream)
                        {
                            drop(engine);
                            let mut eng = state.engine.write().await;
                            eng.metadata.remote_schemas.retain(|r| r.name != name);
                            if let Some(prev) = previous {
                                eng.metadata.remote_schemas.push(prev);
                            }
                            return Err(OpError {
                                status: 400,
                                body: json!({
                                    "code": "remote-schema-error",
                                    "error": report,
                                    "path": "$.args",
                                }),
                            });
                        }
                    }
                }
                state
                    .remote_upstreams
                    .write()
                    .await
                    .insert(name.clone(), upstream);
            }
        }
        return Ok(json!({ "message": "success" }));
    }
    Ok(json!({ "message": "success" }))
}

async fn remote_schema_permission(
    state: &SharedState,
    args: &Json,
    add: bool,
) -> Result<Json, OpError> {
    let name = args
        .get("remote_schema")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected remote_schema"))?;
    let role = args
        .get("role")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected role"))?
        .to_string();
    let mut engine = state.engine.write().await;
    let entry = engine
        .metadata
        .remote_schemas
        .iter_mut()
        .find(|r| r.name == name)
        .ok_or_else(|| {
            OpError::bad_request(
                "not-exists",
                format!("remote schema \"{name}\" does not exist"),
            )
        })?;
    entry.permissions.retain(|p| p.role != role);
    if add {
        let definition: dist_metadata::RemoteSchemaPermissionDefinition =
            serde_json::from_value(args.get("definition").cloned().unwrap_or(Json::Null))
                .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
        // The schema document must parse AND match the upstream schema.
        graphql_parser::parse_schema::<String>(&definition.schema).map_err(|e| {
            OpError::bad_request("parse-failed", format!("invalid schema document: {e}"))
        })?;
        let upstreams = state.remote_upstreams.read().await;
        if let Some(upstream) = upstreams.get(name) {
            if let Err(report) = crate::remote_validate::validate(&definition.schema, upstream)
            {
                return Err(OpError {
                    status: 400,
                    body: json!({
                        "code": "validation-failed",
                        "error": report,
                        "path": "$.args",
                    }),
                });
            }
        }
        drop(upstreams);
        entry
            .permissions
            .push(dist_metadata::RemoteSchemaPermission { role, definition });
    }
    Ok(json!({ "message": "success" }))
}

async fn query_collection(state: &SharedState, args: &Json, add: bool) -> Result<Json, OpError> {
    // create uses {name}, drop uses {collection}.
    let name = args
        .get("name")
        .or_else(|| args.get("collection"))
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected a name"))?
        .to_string();
    let mut engine = state.engine.write().await;
    if add {
        let collection: dist_metadata::QueryCollection =
            serde_json::from_value(args.clone())
                .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
        engine
            .metadata
            .query_collections
            .retain(|c| c.name != name);
        engine.metadata.query_collections.push(collection);
    } else {
        engine
            .metadata
            .query_collections
            .retain(|c| c.name != name);
        let cascade = args.get("cascade").and_then(Json::as_bool).unwrap_or(false);
        if cascade {
            engine.metadata.allowlist.retain(|a| a.collection != name);
        }
    }
    Ok(json!({ "message": "success" }))
}

async fn collection_query(state: &SharedState, args: &Json, add: bool) -> Result<Json, OpError> {
    let collection = args
        .get("collection_name")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected collection_name"))?;
    let query_name = args
        .get("query_name")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected query_name"))?
        .to_string();
    let mut engine = state.engine.write().await;
    let entry = engine
        .metadata
        .query_collections
        .iter_mut()
        .find(|c| c.name == collection)
        .ok_or_else(|| {
            OpError::bad_request(
                "not-exists",
                format!("query collection with name \"{collection}\" does not exist"),
            )
        })?;
    if add {
        let query = args
            .get("query")
            .and_then(Json::as_str)
            .ok_or_else(|| OpError::bad_request("parse-failed", "expected query"))?
            .to_string();
        if entry.definition.queries.iter().any(|q| q.name == query_name) {
            // Hasura scopes metadata-op argument errors to $.args.
            return Err(OpError {
                status: 400,
                body: json!({
                    "code": "already-exists",
                    "error": format!("query with name \"{query_name}\" already exists in collection \"{collection}\""),
                    "path": "$.args",
                }),
            });
        }
        entry.definition.queries.push(dist_metadata::CollectionQuery {
            name: query_name,
            query,
        });
    } else {
        entry.definition.queries.retain(|q| q.name != query_name);
    }
    Ok(json!({ "message": "success" }))
}

async fn allowlist(state: &SharedState, args: &Json, add: bool) -> Result<Json, OpError> {
    let collection = args
        .get("collection")
        .and_then(Json::as_str)
        .ok_or_else(|| OpError::bad_request("parse-failed", "expected collection"))?
        .to_string();
    let mut engine = state.engine.write().await;
    if add {
        if !engine
            .metadata
            .query_collections
            .iter()
            .any(|c| c.name == collection)
        {
            return Err(OpError::bad_request(
                "not-exists",
                format!("query collection with name \"{collection}\" does not exist"),
            ));
        }
        if !engine
            .metadata
            .allowlist
            .iter()
            .any(|a| a.collection == collection)
        {
            engine
                .metadata
                .allowlist
                .push(dist_metadata::AllowlistEntry { collection });
        }
    } else {
        engine
            .metadata
            .allowlist
            .retain(|a| a.collection != collection);
    }
    Ok(json!({ "message": "success" }))
}

async fn clear_metadata(state: &SharedState) -> Result<Json, OpError> {
    let mut engine = state.engine.write().await;
    for source in &mut engine.metadata.sources {
        source.tables.clear();
        source.functions.clear();
    }
    engine.metadata.inherited_roles.clear();
    engine.metadata.query_collections.clear();
    engine.metadata.allowlist.clear();
    engine.metadata.remote_schemas.clear();
    Ok(json!({ "message": "success" }))
}

async fn export_metadata(state: &SharedState) -> Result<Json, OpError> {
    let engine = state.engine.read().await;
    serde_json::to_value(&engine.metadata).map_err(internal)
}

/// Inherited-role permission conflicts, in Hasura's inconsistency shape.
async fn get_inconsistent_metadata(state: &SharedState) -> Result<Json, OpError> {
    let engine = state.engine.read().await;
    let catalog = engine.default_catalog();
    let planner = Planner::new(&engine.metadata, &catalog);
    let objects: Vec<Json> = planner
        .mutation_permission_conflicts()
        .into_iter()
        .map(|(role, table, kind)| {
            json!({
                "reason": format!(
                    "Could not inherit permission for the role '{role}' for the entity: '{kind} permission, table: {table}, source: 'default''"
                ),
                "name": role,
                "type": "inherited role permission inconsistency",
                "entity": {
                    "permission_type": kind,
                    "source": "default",
                    "table": table,
                },
            })
        })
        .collect();
    Ok(json!({
        "is_consistent": objects.is_empty(),
        "inconsistent_objects": objects,
    }))
}

async fn replace_metadata(state: &SharedState, args: &Json) -> Result<Json, OpError> {
    // Accept both {metadata: {...}} and the metadata object directly.
    let metadata_value = args.get("metadata").unwrap_or(args).clone();
    let mut metadata: dist_metadata::Metadata = serde_json::from_value(metadata_value)
        .map_err(|e| OpError::bad_request("parse-failed", e.to_string()))?;
    if let Some(cycle) = find_role_cycle(&metadata.inherited_roles) {
        return Err(OpError::bad_request(
            "invalid-configuration",
            format!(
                "found cycle(s) in roles: {}",
                serde_json::to_string(&cycle).unwrap_or_default()
            ),
        ));
    }
    ensure_default_source(&mut metadata);
    state.engine.write().await.metadata = metadata;
    state.sync_sources().await.map_err(internal)?;
    Ok(json!({ "message": "success" }))
}

/// First cycle in the inherited-role graph, as the visited path ending at
/// the repeated role. Roles and their parents are walked in sorted order
/// to match Hasura's reported path.
fn find_role_cycle(roles: &[dist_metadata::InheritedRole]) -> Option<Vec<String>> {
    use std::collections::HashSet;
    let by_name: std::collections::BTreeMap<&str, &dist_metadata::InheritedRole> =
        roles.iter().map(|r| (r.role_name.as_str(), r)).collect();

    fn dfs<'a>(
        name: &'a str,
        by_name: &std::collections::BTreeMap<&'a str, &'a dist_metadata::InheritedRole>,
        path: &mut Vec<&'a str>,
        done: &mut HashSet<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(pos) = path.iter().position(|p| *p == name) {
            let mut cycle: Vec<String> = path[pos..].iter().map(|s| s.to_string()).collect();
            cycle.push(name.to_string());
            return Some(cycle);
        }
        if done.contains(name) {
            return None;
        }
        let Some(role) = by_name.get(name) else {
            return None;
        };
        path.push(name);
        let mut parents: Vec<&str> = role.role_set.iter().map(String::as_str).collect();
        parents.sort();
        for parent in parents {
            if let Some(cycle) = dfs(parent, by_name, path, done) {
                return Some(cycle);
            }
        }
        path.pop();
        done.insert(name);
        None
    }

    let mut done = HashSet::new();
    for name in by_name.keys().copied().collect::<Vec<_>>() {
        let mut path = vec![];
        if let Some(cycle) = dfs(name, &by_name, &mut path, &mut done) {
            return Some(cycle);
        }
    }
    None
}

fn internal(e: impl std::fmt::Display) -> OpError {
    OpError {
        status: 500,
        body: json!({ "code": "unexpected", "error": e.to_string(), "path": "$" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(name: &str, set: &[&str]) -> dist_metadata::InheritedRole {
        dist_metadata::InheritedRole {
            role_name: name.to_string(),
            role_set: set.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn op_error_bad_request_shape() {
        let e = OpError::bad_request("parse-failed", "boom");
        assert_eq!(e.status, 400);
        assert_eq!(e.body, json!({ "code": "parse-failed", "error": "boom", "path": "$" }));
    }

    #[test]
    fn plan_error_maps_to_hasura_body() {
        let e = plan_err(PlanError::new("$.args", "validation-failed", "nope"));
        assert_eq!(e.status, 400);
        assert_eq!(
            e.body,
            json!({ "code": "validation-failed", "error": "nope", "path": "$.args" })
        );
    }

    #[test]
    fn internal_error_is_500_unexpected() {
        let e = internal("kaput");
        assert_eq!(e.status, 500);
        assert_eq!(e.body, json!({ "code": "unexpected", "error": "kaput", "path": "$" }));
    }

    #[test]
    fn parse_table_accepts_all_spellings() {
        let t = parse_table(&json!({ "table": "author" })).unwrap();
        assert_eq!((t.schema(), t.name()), ("public", "author"));
        let t = parse_table(&json!({ "table": { "schema": "s", "name": "t" } })).unwrap();
        assert_eq!((t.schema(), t.name()), ("s", "t"));
        // v1 track_table style: schema/name directly in args.
        let t = parse_table(&json!({ "name": "t2", "schema": "s2" })).unwrap();
        assert_eq!((t.schema(), t.name()), ("s2", "t2"));
        // Qualified form defaults the schema to public.
        let t = parse_table(&json!({ "table": { "name": "t3" } })).unwrap();
        assert_eq!((t.schema(), t.name()), ("public", "t3"));
    }

    #[test]
    fn parse_table_rejects_garbage() {
        let e = parse_table(&json!(42)).unwrap_err();
        assert_eq!(e.status, 400);
        assert_eq!(e.body["code"], "parse-failed");
    }

    #[test]
    fn sql_quoting_escapes() {
        assert_eq!(quote_ident_sql("plain"), "\"plain\"");
        assert_eq!(quote_ident_sql("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_lit_sql("O'Hara"), "'O''Hara'");
    }

    #[test]
    fn role_cycle_detected_with_path() {
        let roles = vec![role("a", &["b"]), role("b", &["c"]), role("c", &["a"])];
        assert_eq!(
            find_role_cycle(&roles),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string(), "a".to_string()])
        );
        // Self-reference is the shortest cycle.
        assert_eq!(
            find_role_cycle(&[role("x", &["x"])]),
            Some(vec!["x".to_string(), "x".to_string()])
        );
    }

    #[test]
    fn role_cycle_absent_for_dag() {
        // Diamond: shared parents are fine, only true cycles count.
        let roles = vec![role("a", &["b", "c"]), role("b", &["c"])];
        assert_eq!(find_role_cycle(&roles), None);
    }
}
