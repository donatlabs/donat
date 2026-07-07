//! Boolean expression parsing: Donat bool_exp JSON -> IR predicate.
//!
//! Used for both the user's `where` argument and role row filters from
//! metadata. Session variables (string values starting with "x-donat-" or
//! "x-hasura-") are substituted only in permission filters; clients cannot
//! reference them in `where`.

use donat_ir::{BoolExp, CompareOp, Scalar, Table};
use serde_json::Value as Json;

use crate::plan::{PlanError, Planner, Session, TableCtx, is_session_var_name};

impl Planner<'_> {
    /// Parse a bool_exp against `ctx`'s table. `is_permission` enables
    /// session-variable substitution.
    pub(crate) fn parse_bool_exp(
        &self,
        value: &Json,
        ctx: &TableCtx<'_>,
        session: &Session,
        is_permission: bool,
        path: &str,
    ) -> Result<BoolExp, PlanError> {
        let Json::Object(map) = value else {
            return Err(PlanError::validation(
                path,
                "expected a bool expression object",
            ));
        };

        let mut conjuncts = vec![];
        for (key, sub) in map {
            // Donat accepts both the modern `_op` and the legacy `$op`
            // spellings for logical operators.
            let logical = match key.as_str() {
                "$and" => "_and",
                "$or" => "_or",
                "$not" => "_not",
                other => other,
            };
            match logical {
                "_and" => {
                    let items = as_array(sub, path)?;
                    let parsed: Result<Vec<_>, _> = items
                        .iter()
                        .map(|v| self.parse_bool_exp(v, ctx, session, is_permission, path))
                        .collect();
                    conjuncts.push(BoolExp::And(parsed?));
                }
                "_or" => {
                    let items = as_array(sub, path)?;
                    let parsed: Result<Vec<_>, _> = items
                        .iter()
                        .map(|v| self.parse_bool_exp(v, ctx, session, is_permission, path))
                        .collect();
                    conjuncts.push(BoolExp::Or(parsed?));
                }
                "_not" => {
                    conjuncts.push(BoolExp::Not(Box::new(self.parse_bool_exp(
                        sub,
                        ctx,
                        session,
                        is_permission,
                        path,
                    )?)));
                }
                "_exists" => {
                    let table_value = sub
                        .get("_table")
                        .ok_or_else(|| PlanError::validation(path, "_exists needs a _table"))?;
                    let table: donat_metadata::QualifiedTable =
                        serde_json::from_value(table_value.clone()).map_err(|e| {
                            PlanError::validation(path, format!("bad _exists table: {e}"))
                        })?;
                    let where_value = sub
                        .get("_where")
                        .ok_or_else(|| PlanError::validation(path, "_exists needs a _where"))?;
                    let Some(remote) = self.relationship_ctx(&table, session, is_permission) else {
                        return Err(PlanError::validation(
                            path,
                            format!("table \"{table}\" not found in _exists"),
                        ));
                    };
                    let inner =
                        self.parse_bool_exp(where_value, &remote, session, is_permission, path)?;
                    conjuncts.push(BoolExp::Exists {
                        table: Table {
                            schema: table.schema().to_string(),
                            name: table.name().to_string(),
                        },
                        predicate: Box::new(inner),
                    });
                }
                column if ctx.column_allowed_for_filter(column, is_permission) => {
                    let db_column = ctx
                        .column_db_name_for_filter(column, is_permission)
                        .expect("column existence checked by column_allowed_for_filter");
                    let info = ctx.column_info(&db_column).unwrap();
                    let ops = self.parse_ops(ctx, &db_column, sub, session, is_permission, path)?;
                    let mut parsed: Vec<BoolExp> = ops
                        .into_iter()
                        .map(|op| BoolExp::Compare {
                            column: db_column.clone(),
                            pg_type: info.pg_type.clone(),
                            op,
                        })
                        .collect();
                    conjuncts.push(match parsed.len() {
                        1 => parsed.pop().unwrap(),
                        _ => BoolExp::And(parsed),
                    });
                }
                rel_name => {
                    // Computed field in a filter?
                    if let Some(cf) = ctx
                        .entry
                        .computed_fields
                        .iter()
                        .find(|c| c.name == rel_name)
                    {
                        conjuncts.push(self.computed_field_predicate(
                            cf,
                            sub,
                            ctx,
                            session,
                            is_permission,
                            path,
                        )?);
                        continue;
                    }
                    // Relationship predicate?
                    let target = self.relationship_target(ctx, rel_name, path);
                    match target {
                        Some((remote_table, join)) => {
                            let Some(remote) =
                                self.relationship_ctx(&remote_table, session, is_permission)
                            else {
                                return Err(PlanError::validation(
                                    path,
                                    format!(
                                        "field '{rel_name}' not found in type: '{}_bool_exp'",
                                        ctx.type_name
                                    ),
                                ));
                            };
                            let mut inner =
                                self.parse_bool_exp(sub, &remote, session, is_permission, path)?;
                            // In user filters the remote table's own row
                            // filter applies too, so relationships can't
                            // leak invisible rows.
                            if !is_permission {
                                if let Some(perm) =
                                    self.permission_predicate(&remote, session, path)?
                                {
                                    inner = BoolExp::And(vec![inner, perm]);
                                }
                            }
                            conjuncts.push(BoolExp::Relationship {
                                table: Table {
                                    schema: remote_table.schema().to_string(),
                                    name: remote_table.name().to_string(),
                                },
                                join,
                                predicate: Box::new(inner),
                            });
                        }
                        None => {
                            return Err(PlanError::validation(
                                path,
                                format!(
                                    "field '{rel_name}' not found in type: '{}_bool_exp'",
                                    ctx.type_name
                                ),
                            ));
                        }
                    }
                }
            }
        }

        Ok(match conjuncts.len() {
            1 => conjuncts.pop().unwrap(),
            _ => BoolExp::And(conjuncts),
        })
    }

    /// A computed field referenced inside a bool_exp: a scalar function
    /// comparison, or EXISTS over a table-valued function's rows.
    fn computed_field_predicate(
        &self,
        cf: &donat_metadata::ComputedField,
        value: &Json,
        ctx: &TableCtx<'_>,
        session: &Session,
        is_permission: bool,
        path: &str,
    ) -> Result<BoolExp, PlanError> {
        let def = &cf.definition;
        let finfo = self
            .catalog_function(def.function.schema(), def.function.name())
            .ok_or_else(|| {
                PlanError::validation(
                    path,
                    format!("function for computed field '{}' not found", cf.name),
                )
            })?;
        let args: Vec<donat_ir::RowFunctionArg> = finfo
            .args
            .iter()
            .map(|a| {
                let is_session = a
                    .name
                    .as_deref()
                    .zip(def.session_argument.as_deref())
                    .is_some_and(|(n, s)| n == s);
                if is_session {
                    donat_ir::RowFunctionArg::SessionJson(crate::plan::session_json(session))
                } else {
                    donat_ir::RowFunctionArg::Row
                }
            })
            .collect();

        if let Some((rschema, rname)) = &finfo.returns_table {
            let remote_table = donat_metadata::QualifiedTable::Qualified {
                schema: rschema.clone(),
                name: rname.clone(),
            };
            let Some(remote) = self.relationship_ctx(&remote_table, session, is_permission) else {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "field '{}' not found in type: '{}_bool_exp'",
                        cf.name, ctx.type_name
                    ),
                ));
            };
            let mut inner = self.parse_bool_exp(value, &remote, session, is_permission, path)?;
            if !is_permission {
                if let Some(perm) = self.permission_predicate(&remote, session, path)? {
                    inner = BoolExp::And(vec![inner, perm]);
                }
            }
            Ok(BoolExp::RowFunctionExists {
                schema: finfo.schema.clone(),
                name: finfo.name.clone(),
                args,
                predicate: Box::new(inner),
            })
        } else {
            let pg_type = finfo
                .returns_scalar
                .clone()
                .unwrap_or_else(|| "text".into());
            let ops = self.parse_ops(ctx, &cf.name, value, session, is_permission, path)?;
            let mut out: Vec<BoolExp> = ops
                .into_iter()
                .map(|op| BoolExp::ComputedCompare {
                    schema: finfo.schema.clone(),
                    name: finfo.name.clone(),
                    args: args.clone(),
                    pg_type: pg_type.clone(),
                    op,
                })
                .collect();
            Ok(match out.len() {
                1 => out.pop().unwrap(),
                _ => BoolExp::And(out),
            })
        }
    }

    /// Parse the operator object for one field. A non-object value is the
    /// legacy implicit `_eq`: `{ column: value }`.
    fn parse_ops(
        &self,
        ctx: &TableCtx<'_>,
        column: &str,
        value: &Json,
        session: &Session,
        is_permission: bool,
        path: &str,
    ) -> Result<Vec<CompareOp>, PlanError> {
        let Json::Object(ops) = value else {
            let resolved = resolve_session(value, session, is_permission, path)?;
            return Ok(vec![CompareOp::Eq(Scalar::Json(resolved))]);
        };

        let mut out = vec![];
        for (raw_op_name, op_value) in ops {
            // Legacy `$op` spelling -> `_op`.
            let normalized;
            let op_name: &str = if let Some(rest) = raw_op_name.strip_prefix('$') {
                normalized = format!("_{rest}");
                &normalized
            } else {
                raw_op_name
            };
            let scalar = |v: &Json| -> Result<Scalar, PlanError> {
                Ok(Scalar::Json(resolve_session(
                    v,
                    session,
                    is_permission,
                    path,
                )?))
            };
            let list = |v: &Json| -> Result<Vec<Scalar>, PlanError> {
                // A session variable may itself hold an array, as JSON
                // ("[1,2]") or as a Postgres array literal ("{a,b}").
                let resolved = resolve_session(v, session, is_permission, path)?;
                match resolved {
                    Json::Array(items) => items.into_iter().map(|i| Ok(Scalar::Json(i))).collect(),
                    Json::String(s) => parse_array_literal(&s)
                        .ok_or_else(|| PlanError::validation(path, "expected an array of values"))
                        .map(|items| items.into_iter().map(Scalar::Json).collect()),
                    _ => Err(PlanError::validation(path, "expected an array of values")),
                }
            };
            let string_list = |v: &Json| -> Result<Vec<String>, PlanError> {
                list(v).map(|items| {
                    items
                        .into_iter()
                        .map(|s| match s.as_json() {
                            Json::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect()
                })
            };

            let op = match op_name {
                "_eq" => CompareOp::Eq(scalar(op_value)?),
                "_neq" | "_ne" => CompareOp::Neq(scalar(op_value)?),
                "_gt" => CompareOp::Gt(scalar(op_value)?),
                "_lt" => CompareOp::Lt(scalar(op_value)?),
                "_gte" => CompareOp::Gte(scalar(op_value)?),
                "_lte" => CompareOp::Lte(scalar(op_value)?),
                "_in" => CompareOp::In(list(op_value)?),
                "_nin" => CompareOp::Nin(list(op_value)?),
                "_like" => CompareOp::Like(scalar(op_value)?),
                "_nlike" => CompareOp::Nlike(scalar(op_value)?),
                "_ilike" => CompareOp::Ilike(scalar(op_value)?),
                "_nilike" => CompareOp::Nilike(scalar(op_value)?),
                "_similar" => CompareOp::Similar(scalar(op_value)?),
                "_nsimilar" => CompareOp::Nsimilar(scalar(op_value)?),
                "_regex" => CompareOp::Regex(scalar(op_value)?),
                "_iregex" => CompareOp::Iregex(scalar(op_value)?),
                "_nregex" => CompareOp::Nregex(scalar(op_value)?),
                "_niregex" => CompareOp::Niregex(scalar(op_value)?),
                "_is_null" => {
                    let v = resolve_session(op_value, session, is_permission, path)?;
                    CompareOp::IsNull(v.as_bool().unwrap_or(false))
                }
                // Column-to-column comparisons.
                "_ceq" => self.column_compare("=", op_value, ctx, path)?,
                "_cne" | "_cneq" => self.column_compare("<>", op_value, ctx, path)?,
                "_cgt" => self.column_compare(">", op_value, ctx, path)?,
                "_clt" => self.column_compare("<", op_value, ctx, path)?,
                "_cgte" => self.column_compare(">=", op_value, ctx, path)?,
                "_clte" => self.column_compare("<=", op_value, ctx, path)?,
                // jsonb operators.
                "_has_key" => CompareOp::HasKey(scalar(op_value)?),
                "_has_keys_any" => CompareOp::HasKeysAny(string_list(op_value)?),
                "_has_keys_all" => CompareOp::HasKeysAll(string_list(op_value)?),
                "_contains" => CompareOp::Contains(scalar(op_value)?),
                "_contained_in" => CompareOp::ContainedIn(scalar(op_value)?),
                // PostGIS operators.
                "_st_contains" => st_op("ST_Contains", op_value, &scalar)?,
                "_st_crosses" => st_op("ST_Crosses", op_value, &scalar)?,
                "_st_equals" => st_op("ST_Equals", op_value, &scalar)?,
                "_st_intersects" => st_op("ST_Intersects", op_value, &scalar)?,
                "_st_overlaps" => st_op("ST_Overlaps", op_value, &scalar)?,
                "_st_touches" => st_op("ST_Touches", op_value, &scalar)?,
                "_st_within" => st_op("ST_Within", op_value, &scalar)?,
                "_st_3d_intersects" => st_op("ST_3DIntersects", op_value, &scalar)?,
                "_st_d_within" | "_st_3d_d_within" => {
                    let obj = op_value.as_object().ok_or_else(|| {
                        PlanError::validation(path, "expected { distance, from } for _st_d_within")
                    })?;
                    let distance = obj.get("distance").ok_or_else(|| {
                        PlanError::validation(path, "missing 'distance' in _st_d_within")
                    })?;
                    let from = obj.get("from").ok_or_else(|| {
                        PlanError::validation(path, "missing 'from' in _st_d_within")
                    })?;
                    CompareOp::StDWithin {
                        distance: scalar(distance)?,
                        from: scalar(from)?,
                        three_d: op_name == "_st_3d_d_within",
                    }
                }
                other => {
                    return Err(PlanError::validation(
                        path,
                        format!("unexpected operator \"{other}\" for column '{column}'"),
                    ));
                }
            };
            out.push(op);
        }

        Ok(out)
    }

    /// `_ceq` and friends: the operand is a column name, a `["$", col]`
    /// root path, or a `[relationship, col]` path.
    fn column_compare(
        &self,
        sql_op: &str,
        value: &Json,
        ctx: &TableCtx<'_>,
        path: &str,
    ) -> Result<CompareOp, PlanError> {
        let local = |column: &str| CompareOp::CompareColumn {
            sql_op: sql_op.to_string(),
            column: ctx
                .column_db_name(column)
                .unwrap_or_else(|| column.to_string()),
            root: false,
        };
        match value {
            Json::String(column) => Ok(local(column)),
            Json::Array(items) => {
                let parts: Vec<&str> = items.iter().filter_map(Json::as_str).collect();
                if parts.len() != items.len() {
                    return Err(PlanError::validation(path, "expected a column name"));
                }
                match parts.as_slice() {
                    [column] => Ok(local(column)),
                    ["$", column] => Ok(CompareOp::CompareColumn {
                        sql_op: sql_op.to_string(),
                        column: ctx
                            .column_db_name(column)
                            .unwrap_or_else(|| column.to_string()),
                        root: true,
                    }),
                    [rel, column] => {
                        let Some((remote_table, join)) = self.relationship_target(ctx, rel, path)
                        else {
                            return Err(PlanError::validation(
                                path,
                                format!("relationship '{rel}' not found"),
                            ));
                        };
                        Ok(CompareOp::CompareColumnRel {
                            sql_op: sql_op.to_string(),
                            table: Table {
                                schema: remote_table.schema().to_string(),
                                name: remote_table.name().to_string(),
                            },
                            join,
                            column: column.to_string(),
                        })
                    }
                    _ => Err(PlanError::validation(path, "expected a column name")),
                }
            }
            _ => Err(PlanError::validation(path, "expected a column name")),
        }
    }
}

fn as_array<'v>(value: &'v Json, path: &str) -> Result<&'v Vec<Json>, PlanError> {
    value
        .as_array()
        .ok_or_else(|| PlanError::validation(path, "expected an array of bool expressions"))
}

fn st_op(
    function: &str,
    value: &Json,
    scalar: &dyn Fn(&Json) -> Result<Scalar, PlanError>,
) -> Result<CompareOp, PlanError> {
    Ok(CompareOp::StOp {
        function: function.to_string(),
        value: scalar(value)?,
    })
}

/// Parse "[1,2]" (JSON) or "{a,b}" (Postgres array literal) into values.
fn parse_array_literal(s: &str) -> Option<Vec<Json>> {
    if let Ok(Json::Array(items)) = serde_json::from_str::<Json>(s) {
        return Some(items);
    }
    let inner = s.trim().strip_prefix('{')?.strip_suffix('}')?;
    if inner.trim().is_empty() {
        return Some(vec![]);
    }
    Some(
        inner
            .split(',')
            .map(|part| {
                let trimmed = part.trim().trim_matches('"');
                Json::String(trimmed.to_string())
            })
            .collect(),
    )
}

/// In permission filters, string values starting with "x-donat-" or
/// "x-hasura-" (case-insensitive) refer to session variables.
fn resolve_session(
    value: &Json,
    session: &Session,
    is_permission: bool,
    path: &str,
) -> Result<Json, PlanError> {
    if !is_permission {
        return Ok(value.clone());
    }
    match value {
        Json::String(s) if is_session_var_name(s) => {
            let _ = path;
            match session.var(s) {
                Some(v) => Ok(Json::String(v.to_string())),
                // Donat reports this with path "$" regardless of depth.
                None => Err(PlanError::new(
                    "$",
                    "not-found",
                    format!("missing session variable: \"{}\"", s.to_ascii_lowercase()),
                )),
            }
        }
        other => Ok(other.clone()),
    }
}
