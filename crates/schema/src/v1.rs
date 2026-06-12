//! Legacy v1 data API planning (`/v1/query` select/count/insert/update/
//! delete with a role). Reuses the same IR and permission machinery as
//! GraphQL; only the argument syntax differs. As everywhere: no admin
//! role — these planners always run as the request's explicit role.

use dist_ir::*;
use dist_metadata::Columns;
use serde_json::Value as Json;

use crate::plan::{PlanError, Planner, Session, TableCtx};

fn parse_table(args: &Json, path: &str) -> Result<dist_metadata::QualifiedTable, PlanError> {
    let table = args
        .get("table")
        .ok_or_else(|| PlanError::validation(path, "expected a table"))?;
    serde_json::from_value(table.clone())
        .map_err(|e| PlanError::validation(path, format!("cannot parse table: {e}")))
}

impl<'a> Planner<'a> {
    fn v1_table_ctx(
        &self,
        args: &Json,
        session: &Session,
        path: &str,
    ) -> Result<(dist_metadata::QualifiedTable, TableCtx<'a>), PlanError> {
        let table = parse_table(args, path)?;
        let ctx = self.table_ctx_by_name(&table, &session.role).ok_or_else(|| {
            PlanError::new(
                path,
                "permission-denied",
                format!(
                    "role \"{}\" does not have permission to select from table \"{table}\"",
                    session.role
                ),
            )
        })?;
        Ok((table, ctx))
    }

    /// v1 `select`: returns the rows array directly.
    pub fn plan_v1_select(
        &self,
        args: &Json,
        session: &Session,
    ) -> Result<SelectQuery, PlanError> {
        let path = "$";
        let (_, ctx) = self.v1_table_ctx(args, session, path)?;
        self.v1_select_on(&ctx, args, session, path)
    }

    fn v1_select_on(
        &self,
        ctx: &TableCtx<'a>,
        args: &Json,
        session: &Session,
        path: &str,
    ) -> Result<SelectQuery, PlanError> {
        let columns = args
            .get("columns")
            .and_then(Json::as_array)
            .ok_or_else(|| PlanError::validation(path, "expected 'columns'"))?;
        let fields = self.v1_output_fields(ctx, columns, session, path)?;

        let mut predicates = vec![];
        if let Some(w) = args.get("where").filter(|w| !w.is_null()) {
            predicates.push(self.parse_bool_exp(w, ctx, session, false, path)?);
        }
        if let Some(p) = self.permission_predicate(ctx, session, path)? {
            predicates.push(p);
        }
        let predicate = match predicates.len() {
            0 => None,
            1 => predicates.pop(),
            _ => Some(BoolExp::And(predicates)),
        };

        let mut limit = args.get("limit").and_then(Json::as_u64);
        let offset = args.get("offset").and_then(Json::as_u64);
        if let Some(perm_limit) = ctx.select_perm_limit() {
            limit = Some(limit.map_or(perm_limit, |l| l.min(perm_limit)));
        }

        let order_by = match args.get("order_by") {
            None | Some(Json::Null) => vec![],
            Some(value) => v1_order_by(ctx, value, path)?,
        };

        Ok(SelectQuery {
            from: FromSource::Table(Table {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            }),
            fields,
            predicate,
            order_by,
            limit,
            nodes_limit: None,
            offset,
            distinct_on: vec![],
            single: false,
        })
    }

    fn v1_output_fields(
        &self,
        ctx: &TableCtx<'a>,
        columns: &[Json],
        session: &Session,
        path: &str,
    ) -> Result<Vec<OutputField>, PlanError> {
        let mut out = vec![];
        for (idx, item) in columns.iter().enumerate() {
            match item {
                Json::String(name) if name == "*" => {
                    for col in &ctx.info.columns {
                        if ctx.column_allowed(&col.name) {
                            out.push(OutputField {
                                alias: col.name.clone(),
                                value: FieldValue::Column {
                                    column: col.name.clone(),
                                    pg_type: col.pg_type.clone(),
                                },
                            });
                        }
                    }
                }
                Json::String(name) => {
                    if !ctx.column_allowed(name) {
                        // Hasura distinguishes hidden vs unknown columns.
                        if ctx.info.column(name).is_some() {
                            return Err(PlanError::new(
                                &format!("{path}.args.columns[{idx}]"),
                                "permission-denied",
                                format!(
                                    "role \"{}\" does not have permission to select column \"{name}\"",
                                    session.role
                                ),
                            ));
                        }
                        return Err(PlanError::validation(
                            path,
                            format!("column \"{name}\" not found"),
                        ));
                    }
                    let info = ctx.info.column(name).unwrap();
                    out.push(OutputField {
                        alias: name.clone(),
                        value: FieldValue::Column {
                            column: info.name.clone(),
                            pg_type: info.pg_type.clone(),
                        },
                    });
                }
                Json::Object(obj) => {
                    let name = obj
                        .get("name")
                        .and_then(Json::as_str)
                        .ok_or_else(|| {
                            PlanError::validation(path, "relationship column needs a name")
                        })?;
                    let Some((remote_table, join)) =
                        self.relationship_target(ctx, name, path)
                    else {
                        return Err(PlanError::validation(
                            path,
                            format!("relationship \"{name}\" not found"),
                        ));
                    };
                    let Some(remote) =
                        self.relationship_ctx(&remote_table, session, false)
                    else {
                        return Err(PlanError::new(
                            path,
                            "permission-denied",
                            format!(
                                "role \"{}\" does not have permission to select from table \"{remote_table}\"",
                                session.role
                            ),
                        ));
                    };
                    let args_json = Json::Object(obj.clone());
                    let query = self.v1_select_on(&remote, &args_json, session, path)?;
                    let is_object = ctx
                        .entry
                        .object_relationships
                        .iter()
                        .any(|r| r.name == name);
                    if is_object {
                        out.push(OutputField {
                            alias: name.to_string(),
                            value: FieldValue::Object {
                                query: SelectQuery {
                                    single: true,
                                    ..query
                                },
                                join,
                            },
                        });
                    } else {
                        out.push(OutputField {
                            alias: name.to_string(),
                            value: FieldValue::Array {
                                query,
                                join,
                                aggregate: false,
                            },
                        });
                    }
                }
                other => {
                    return Err(PlanError::validation(
                        path,
                        format!("unexpected column spec: {other}"),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// v1 `count`: `{ count: N }`.
    pub fn plan_v1_count(
        &self,
        args: &Json,
        session: &Session,
    ) -> Result<SelectQuery, PlanError> {
        let path = "$";
        let table = parse_table(args, path)?;
        // Hasura's count op reports the permission failure with its own shape.
        let ctx = self.table_ctx_by_name(&table, &session.role).ok_or_else(|| {
            PlanError::new(
                "$.args",
                "permission-denied",
                format!(
                    "select on \"{}\" for role \"{}\" is not allowed. ; \"count\" is only allowed if the role has \"select\" permissions on the table",
                    table.name(),
                    session.role
                ),
            )
        })?;

        let mut predicates = vec![];
        if let Some(w) = args.get("where").filter(|w| !w.is_null()) {
            predicates.push(self.parse_bool_exp(w, ctx_ref(&ctx), session, false, path)?);
        }
        if let Some(p) = self.permission_predicate(&ctx, session, path)? {
            predicates.push(p);
        }
        let predicate = match predicates.len() {
            0 => None,
            1 => predicates.pop(),
            _ => Some(BoolExp::And(predicates)),
        };

        let distinct = args
            .get("distinct")
            .and_then(Json::as_array)
            .map(|cols| {
                cols.iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(SelectQuery {
            from: FromSource::Table(Table {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            }),
            fields: vec![OutputField {
                alias: "count".to_string(),
                value: FieldValue::Aggregate {
                    fields: vec![AggregateField {
                        alias: "count".to_string(),
                        op: AggregateOp::Count {
                            distinct: !distinct.is_empty(),
                            columns: distinct,
                        },
                    }],
                },
            }],
            predicate,
            order_by: vec![],
            limit: None,
            nodes_limit: None,
            offset: None,
            distinct_on: vec![],
            single: false,
        })
    }

    /// v1 `update`: `{ affected_rows: N [, returning: [...] ] }`.
    pub fn plan_v1_update(
        &self,
        args: &Json,
        session: &Session,
    ) -> Result<UpdateMutation, PlanError> {
        let path = "$";
        let table = parse_table(args, path)?;
        let ctx = self
            .relationship_ctx(&table, session, true)
            .ok_or_else(|| PlanError::validation(path, format!("table \"{table}\" not tracked")))?;
        let perm = ctx
            .entry
            .update_permissions
            .iter()
            .find(|p| p.role == session.role)
            .map(|p| &p.permission)
            .ok_or_else(|| {
                PlanError::new(
                    path,
                    "permission-denied",
                    format!(
                        "role \"{}\" does not have permission to update table \"{table}\"",
                        session.role
                    ),
                )
            })?;

        let allowed = |col: &str| -> bool {
            ctx.info.column(col).is_some()
                && match &perm.columns {
                    Columns::Star => true,
                    Columns::List(cols) => cols.iter().any(|c| c == col),
                }
        };

        let mut sets = vec![];
        for (key, kind) in [("$set", "set"), ("$inc", "inc"), ("$mul", "mul")] {
            if let Some(Json::Object(map)) = args.get(key) {
                for (col, v) in map {
                    if perm.set.contains_key(col) {
                        return Err(PlanError::new(
                            &format!("{path}.args[\"$set\"]"),
                            "not-supported",
                            format!(
                                "column \"{col}\" is not updatable for role \"{}\"; its value is predefined in permission",
                                session.role
                            ),
                        ));
                    }
                    if !allowed(col) {
                        return Err(PlanError::new(
                            &format!("{path}.args[\"{key}\"]"),
                            "permission-denied",
                            format!("role \"{}\" does not have permission to update column \"{col}\"", session.role),
                        ));
                    }
                    let pg_type = ctx.info.column(col).unwrap().pg_type.clone();
                    let value = Scalar::Json(v.clone());
                    sets.push(match kind {
                        "inc" => SetOp::Inc {
                            column: col.clone(),
                            pg_type,
                            value,
                        },
                        _ => SetOp::Set {
                            column: col.clone(),
                            pg_type,
                            value,
                        },
                    });
                }
            }
        }
        // Update-permission presets are applied on every v1 update.
        for (col, value) in &perm.set {
            let Some(info) = ctx.info.column(col) else { continue };
            sets.push(SetOp::Set {
                column: col.clone(),
                pg_type: info.pg_type.clone(),
                value: Scalar::Json(resolve_preset(value, session)?),
            });
        }

        if sets.is_empty() {
            return Err(PlanError::validation(path, "expected '$set'"));
        }

        let mut predicates = vec![];
        if let Some(w) = args.get("where").filter(|w| !w.is_null()) {
            predicates.push(self.parse_bool_exp(w, &ctx, session, false, path)?);
        }
        if !perm.filter.is_null() && !perm.filter.as_object().is_some_and(|o| o.is_empty()) {
            predicates.push(self.parse_bool_exp(&perm.filter, &ctx, session, true, path)?);
        }
        let predicate = match predicates.len() {
            0 => None,
            1 => predicates.pop(),
            _ => Some(BoolExp::And(predicates)),
        };

        let output = self.v1_mutation_output(&ctx, args, session, path)?;

        Ok(UpdateMutation {
            table: Table {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            },
            sets,
            predicate,
            check: None,
            check_path: "$".to_string(),
            output,
        })
    }

    /// v1 `delete`.
    pub fn plan_v1_delete(
        &self,
        args: &Json,
        session: &Session,
    ) -> Result<DeleteMutation, PlanError> {
        let path = "$";
        let table = parse_table(args, path)?;
        let ctx = self
            .relationship_ctx(&table, session, true)
            .ok_or_else(|| PlanError::validation(path, format!("table \"{table}\" not tracked")))?;
        let perm = ctx
            .entry
            .delete_permissions
            .iter()
            .find(|p| p.role == session.role)
            .map(|p| &p.permission)
            .ok_or_else(|| {
                PlanError::new(
                    path,
                    "permission-denied",
                    format!(
                        "role \"{}\" does not have permission to delete from table \"{table}\"",
                        session.role
                    ),
                )
            })?;

        let mut predicates = vec![];
        if let Some(w) = args.get("where").filter(|w| !w.is_null()) {
            predicates.push(self.parse_bool_exp(w, &ctx, session, false, path)?);
        }
        if !perm.filter.is_null() && !perm.filter.as_object().is_some_and(|o| o.is_empty()) {
            predicates.push(self.parse_bool_exp(&perm.filter, &ctx, session, true, path)?);
        }
        let predicate = match predicates.len() {
            0 => None,
            1 => predicates.pop(),
            _ => Some(BoolExp::And(predicates)),
        };

        let output = self.v1_mutation_output(&ctx, args, session, path)?;

        Ok(DeleteMutation {
            table: Table {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            },
            predicate,
            output,
        })
    }

    /// v1 `insert` with a role: permission-checked InsertMutation.
    pub fn plan_v1_insert(
        &self,
        args: &Json,
        session: &Session,
    ) -> Result<InsertMutation, PlanError> {
        let path = "$";
        let table = parse_table(args, path)?;
        let ctx = self
            .relationship_ctx(&table, session, true)
            .ok_or_else(|| PlanError::validation(path, format!("table \"{table}\" not tracked")))?;
        let perm = ctx
            .entry
            .insert_permissions
            .iter()
            .find(|p| {
                p.role == session.role
                    && (!p.permission.backend_only || session.backend_request)
            })
            .map(|p| &p.permission)
            .ok_or_else(|| {
                PlanError::new(
                    "$.args",
                    "permission-denied",
                    // Hasura's exact v1 shape, trailing space included.
                    format!(
                        "insert on \"{}\" for role \"{}\" is not allowed. ",
                        table.name(),
                        session.role
                    ),
                )
            })?;

        let objects = args
            .get("objects")
            .and_then(Json::as_array)
            .ok_or_else(|| PlanError::validation(path, "expected 'objects'"))?;
        if objects.is_empty() {
            return Err(PlanError::validation(path, "objects must be non-empty"));
        }

        let mut columns: Vec<String> = vec![];
        for object in objects {
            let Some(map) = object.as_object() else {
                return Err(PlanError::validation(path, "objects must be objects"));
            };
            for key in map.keys() {
                let allowed = match &perm.columns {
                    Columns::Star => ctx.info.column(key).is_some(),
                    Columns::List(cols) => {
                        cols.iter().any(|c| c == key) && ctx.info.column(key).is_some()
                    }
                };
                if !allowed {
                    return Err(PlanError::new(
                        path,
                        "permission-denied",
                        format!("role \"{}\" does not have permission to insert column \"{key}\"", session.role),
                    ));
                }
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        }

        let mut preset_values: Vec<(String, Scalar)> = vec![];
        for (col, value) in &perm.set {
            if ctx.info.column(col).is_none() {
                continue;
            }
            let resolved = resolve_preset(value, session)?;
            if !columns.contains(col) {
                columns.push(col.clone());
            }
            preset_values.push((col.clone(), Scalar::Json(resolved)));
        }

        let typed_columns: Vec<(String, String)> = columns
            .iter()
            .map(|c| (c.clone(), ctx.info.column(c).unwrap().pg_type.clone()))
            .collect();
        let rows: Vec<Vec<Option<Scalar>>> = objects
            .iter()
            .map(|object| {
                let map = object.as_object().unwrap();
                typed_columns
                    .iter()
                    .map(|(col, _)| {
                        if let Some((_, preset)) = preset_values.iter().find(|(c, _)| c == col) {
                            return Some(preset.clone());
                        }
                        map.get(col).map(|v| Scalar::Json(v.clone()))
                    })
                    .collect()
            })
            .collect();

        let check = if perm.check.is_null()
            || perm.check.as_object().is_some_and(|o| o.is_empty())
        {
            None
        } else {
            Some(self.parse_bool_exp(&perm.check, &ctx, session, true, path)?)
        };

        // v1 on_conflict: { constraint | constraint_on, action: update|ignore }.
        let on_conflict = match args.get("on_conflict") {
            None | Some(Json::Null) => None,
            Some(oc) => {
                let constraint = oc
                    .get("constraint")
                    .and_then(Json::as_str)
                    .ok_or_else(|| {
                        PlanError::validation(path, "on_conflict needs a constraint")
                    })?
                    .to_string();
                let action = oc.get("action").and_then(Json::as_str).unwrap_or("update");
                let update_columns = if action == "ignore" {
                    vec![]
                } else {
                    // `update` re-applies every inserted column...
                    columns.clone()
                };
                // ...restricted by the role's update permission, whose
                // filter also gates which existing rows may change.
                let mut predicate = None;
                let mut set_ops = vec![];
                if !update_columns.is_empty() {
                    if let Some(update_perm) = ctx
                        .entry
                        .update_permissions
                        .iter()
                        .find(|p| p.role == session.role)
                        .map(|p| &p.permission)
                    {
                        if !update_perm.filter.is_null()
                            && !update_perm.filter.as_object().is_some_and(|o| o.is_empty())
                        {
                            predicate = Some(self.parse_bool_exp(
                                &update_perm.filter,
                                &ctx,
                                session,
                                true,
                                path,
                            )?);
                        }
                        for (col, value) in &update_perm.set {
                            let Some(info) = ctx.info.column(col) else { continue };
                            set_ops.push(SetOp::Set {
                                column: col.clone(),
                                pg_type: info.pg_type.clone(),
                                value: Scalar::Json(resolve_preset(value, session)?),
                            });
                        }
                    }
                }
                Some(OnConflict {
                    constraint,
                    update_columns,
                    predicate,
                    set_ops,
                })
            }
        };

        let output = self.v1_mutation_output(&ctx, args, session, path)?;

        Ok(InsertMutation {
            table: Table {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            },
            columns: typed_columns,
            rows,
            on_conflict,
            check,
            check_path: "$".to_string(),
            output,
        })
    }

    /// `returning` columns of a v1 mutation, if requested.
    fn v1_mutation_output(
        &self,
        ctx: &TableCtx<'a>,
        args: &Json,
        session: &Session,
        path: &str,
    ) -> Result<MutationOutput, PlanError> {
        let mut fields = vec![MutationResponseField::AffectedRows {
            alias: "affected_rows".to_string(),
        }];
        if let Some(cols) = args.get("returning").and_then(Json::as_array) {
            // Returning requires select permission.
            let table = dist_metadata::QualifiedTable::Qualified {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            };
            let Some(select_ctx) = self.relationship_ctx(&table, session, false) else {
                return Err(PlanError::new(
                    path,
                    "permission-denied",
                    format!(
                        "role \"{}\" does not have permission to select from table \"{table}\"",
                        session.role
                    ),
                ));
            };
            let specs = self.v1_output_fields(&select_ctx, cols, session, path)?;
            fields.push(MutationResponseField::Returning {
                alias: "returning".to_string(),
                fields: specs,
            });
        }
        Ok(MutationOutput::Response(fields))
    }
}

fn ctx_ref<'b, 'a>(ctx: &'b TableCtx<'a>) -> &'b TableCtx<'a> {
    ctx
}

fn resolve_preset(value: &Json, session: &Session) -> Result<Json, PlanError> {
    match value {
        Json::String(s) if s.len() >= 8 && s[..8].eq_ignore_ascii_case("x-hasura") => {
            let v = session.var(s).ok_or_else(|| {
                PlanError::new(
                    "$",
                    "not-found",
                    format!("missing session variable: \"{}\"", s.to_ascii_lowercase()),
                )
            })?;
            Ok(Json::String(v.to_string()))
        }
        other => Ok(other.clone()),
    }
}

/// v1 order_by: `["+col", "-col"]` or `[{column, type, nulls}]` or a single
/// such value.
fn v1_order_by(ctx: &TableCtx, value: &Json, path: &str) -> Result<Vec<OrderBy>, PlanError> {
    let items: Vec<&Json> = match value {
        Json::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut out = vec![];
    for item in items {
        let (column, descending, nulls_first) = match item {
            Json::String(s) => {
                let (desc, name) = match s.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, s.strip_prefix('+').unwrap_or(s)),
                };
                (name.to_string(), desc, None)
            }
            Json::Object(obj) => {
                let column = obj
                    .get("column")
                    .and_then(Json::as_str)
                    .ok_or_else(|| PlanError::validation(path, "order_by needs a column"))?
                    .to_string();
                let descending = obj.get("type").and_then(Json::as_str) == Some("desc");
                let nulls_first = obj
                    .get("nulls")
                    .and_then(Json::as_str)
                    .map(|n| n == "first");
                (column, descending, nulls_first)
            }
            _ => return Err(PlanError::validation(path, "bad order_by")),
        };
        if !ctx.column_allowed(&column) {
            return Err(PlanError::validation(
                path,
                format!("column \"{column}\" not found"),
            ));
        }
        let direction = if descending {
            OrderDirection::Desc
        } else {
            OrderDirection::Asc
        };
        let nulls = match (nulls_first, descending) {
            (Some(true), _) => NullsOrder::First,
            (Some(false), _) => NullsOrder::Last,
            (None, true) => NullsOrder::First,
            (None, false) => NullsOrder::Last,
        };
        out.push(OrderBy {
            target: OrderByTarget::Column(column),
            direction,
            nulls,
        });
    }
    Ok(out)
}
