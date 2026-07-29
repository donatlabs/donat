//! Boolean expression parsing: Donat bool_exp JSON -> IR predicate.
//!
//! Used for both the user's `where` argument and role row filters from
//! metadata. Session variables (string values starting with "x-donat-" or
//! "x-hasura-") are substituted only in permission filters; clients cannot
//! reference them in `where`.

use std::{borrow::Cow, collections::HashMap};

use donat_catalog::ColumnInfo;
use donat_ir::{BoolExp, CompareOp, Scalar, Table};
use serde_json::Value as Json;

use crate::plan::{PlanError, Planner, Session, TableCtx, is_session_var_name};

struct ComparisonField<'a> {
    column: &'a str,
    scalar_type: &'a str,
}

#[derive(Debug, Clone)]
pub(crate) struct PermissionSessionUse {
    pub(crate) name: String,
    pub(crate) operand: PermissionSessionOperand,
}

#[derive(Debug, Clone)]
pub(crate) enum PermissionSessionOperand {
    Scalar(ColumnInfo),
    List(ColumnInfo),
    Boolean,
    String,
    StringList,
    Decimal,
}

#[derive(Debug, Clone, Copy)]
enum PredicateOperator {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
    In,
    Nin,
    Like,
    Nlike,
    Ilike,
    Nilike,
    Similar,
    Nsimilar,
    Regex,
    Iregex,
    Nregex,
    Niregex,
    IsNull,
    Ceq,
    Cne,
    Cgt,
    Clt,
    Cgte,
    Clte,
    HasKey,
    HasKeysAny,
    HasKeysAll,
    Contains,
    ContainedIn,
    StContains,
    StCrosses,
    StEquals,
    StIntersects,
    StOverlaps,
    StTouches,
    StWithin,
    St3dIntersects,
    StDWithin { three_d: bool },
}

#[derive(Debug, Clone, Copy)]
enum PredicateSessionOperandKind {
    Scalar,
    List,
    Boolean,
    String,
    StringList,
    None,
    Distance,
}

impl PredicateOperator {
    fn session_operand_kind(self) -> PredicateSessionOperandKind {
        match self {
            Self::Eq
            | Self::Neq
            | Self::Gt
            | Self::Lt
            | Self::Gte
            | Self::Lte
            | Self::Like
            | Self::Nlike
            | Self::Ilike
            | Self::Nilike
            | Self::Similar
            | Self::Nsimilar
            | Self::Regex
            | Self::Iregex
            | Self::Nregex
            | Self::Niregex
            | Self::Contains
            | Self::ContainedIn
            | Self::StContains
            | Self::StCrosses
            | Self::StEquals
            | Self::StIntersects
            | Self::StOverlaps
            | Self::StTouches
            | Self::StWithin
            | Self::St3dIntersects => PredicateSessionOperandKind::Scalar,
            Self::In | Self::Nin => PredicateSessionOperandKind::List,
            Self::IsNull => PredicateSessionOperandKind::Boolean,
            Self::HasKey => PredicateSessionOperandKind::String,
            Self::HasKeysAny | Self::HasKeysAll => PredicateSessionOperandKind::StringList,
            Self::Ceq | Self::Cne | Self::Cgt | Self::Clt | Self::Cgte | Self::Clte => {
                PredicateSessionOperandKind::None
            }
            Self::StDWithin { .. } => PredicateSessionOperandKind::Distance,
        }
    }
}

fn normalize_logical_operator(name: &str) -> &str {
    match name {
        "$and" => "_and",
        "$or" => "_or",
        "$not" => "_not",
        other => other,
    }
}

fn normalize_predicate_operator(name: &str) -> Cow<'_, str> {
    name.strip_prefix('$').map_or_else(
        || Cow::Borrowed(name),
        |rest| Cow::Owned(format!("_{rest}")),
    )
}

fn classify_predicate_operator(
    capabilities: &donat_backend::Capabilities,
    name: &str,
) -> Option<PredicateOperator> {
    Some(match name {
        "_eq" => PredicateOperator::Eq,
        "_neq" | "_ne" => PredicateOperator::Neq,
        "_gt" => PredicateOperator::Gt,
        "_lt" => PredicateOperator::Lt,
        "_gte" => PredicateOperator::Gte,
        "_lte" => PredicateOperator::Lte,
        "_in" => PredicateOperator::In,
        "_nin" => PredicateOperator::Nin,
        "_like" => PredicateOperator::Like,
        "_nlike" => PredicateOperator::Nlike,
        "_ilike" => PredicateOperator::Ilike,
        "_nilike" => PredicateOperator::Nilike,
        "_similar" if capabilities.regex_ops => PredicateOperator::Similar,
        "_nsimilar" if capabilities.regex_ops => PredicateOperator::Nsimilar,
        "_regex" if capabilities.regex_ops => PredicateOperator::Regex,
        "_iregex" if capabilities.regex_ops => PredicateOperator::Iregex,
        "_nregex" if capabilities.regex_ops => PredicateOperator::Nregex,
        "_niregex" if capabilities.regex_ops => PredicateOperator::Niregex,
        "_is_null" => PredicateOperator::IsNull,
        "_ceq" => PredicateOperator::Ceq,
        "_cne" | "_cneq" => PredicateOperator::Cne,
        "_cgt" => PredicateOperator::Cgt,
        "_clt" => PredicateOperator::Clt,
        "_cgte" => PredicateOperator::Cgte,
        "_clte" => PredicateOperator::Clte,
        "_has_key"
            if matches!(
                capabilities.json_ops,
                donat_backend::capabilities::JsonOps::Jsonb
            ) =>
        {
            PredicateOperator::HasKey
        }
        "_has_keys_any"
            if matches!(
                capabilities.json_ops,
                donat_backend::capabilities::JsonOps::Jsonb
            ) =>
        {
            PredicateOperator::HasKeysAny
        }
        "_has_keys_all"
            if matches!(
                capabilities.json_ops,
                donat_backend::capabilities::JsonOps::Jsonb
            ) =>
        {
            PredicateOperator::HasKeysAll
        }
        "_contains"
            if matches!(
                capabilities.json_ops,
                donat_backend::capabilities::JsonOps::Jsonb
            ) =>
        {
            PredicateOperator::Contains
        }
        "_contained_in"
            if matches!(
                capabilities.json_ops,
                donat_backend::capabilities::JsonOps::Jsonb
            ) =>
        {
            PredicateOperator::ContainedIn
        }
        "_st_contains" if capabilities.geo => PredicateOperator::StContains,
        "_st_crosses" if capabilities.geo => PredicateOperator::StCrosses,
        "_st_equals" if capabilities.geo => PredicateOperator::StEquals,
        "_st_intersects" if capabilities.geo => PredicateOperator::StIntersects,
        "_st_overlaps" if capabilities.geo => PredicateOperator::StOverlaps,
        "_st_touches" if capabilities.geo => PredicateOperator::StTouches,
        "_st_within" if capabilities.geo => PredicateOperator::StWithin,
        "_st_3d_intersects" if capabilities.geo => PredicateOperator::St3dIntersects,
        "_st_d_within" if capabilities.geo => PredicateOperator::StDWithin { three_d: false },
        "_st_3d_d_within" if capabilities.geo => PredicateOperator::StDWithin { three_d: true },
        _ => return None,
    })
}

impl Planner<'_> {
    /// Collect session-variable operands from one metadata permission filter
    /// using the same closed bool-exp/operator grammar as runtime planning.
    pub(crate) fn collect_permission_session_uses(
        &self,
        value: &Json,
        ctx: &TableCtx<'_>,
        path: &str,
    ) -> Result<Vec<PermissionSessionUse>, PlanError> {
        let permission_probe = Session {
            role: String::new(),
            vars: HashMap::new(),
            backend_request: false,
        };
        let mut uses = Vec::new();
        self.collect_bool_exp_session_uses(value, ctx, &permission_probe, path, &mut uses)?;
        Ok(uses)
    }

    fn collect_bool_exp_session_uses(
        &self,
        value: &Json,
        ctx: &TableCtx<'_>,
        permission_probe: &Session,
        path: &str,
        uses: &mut Vec<PermissionSessionUse>,
    ) -> Result<(), PlanError> {
        let Json::Object(map) = value else {
            return Err(PlanError::validation(
                path,
                "expected a bool expression object",
            ));
        };

        for (key, sub) in map {
            match normalize_logical_operator(key) {
                "_and" | "_or" => {
                    for item in as_array(sub, path)? {
                        self.collect_bool_exp_session_uses(
                            item,
                            ctx,
                            permission_probe,
                            path,
                            uses,
                        )?;
                    }
                }
                "_not" => {
                    self.collect_bool_exp_session_uses(sub, ctx, permission_probe, path, uses)?
                }
                "_exists" => {
                    let table_value = sub
                        .get("_table")
                        .ok_or_else(|| PlanError::validation(path, "_exists needs a _table"))?;
                    let table: donat_metadata::QualifiedTable =
                        serde_json::from_value(table_value.clone()).map_err(|error| {
                            PlanError::validation(path, format!("bad _exists table: {error}"))
                        })?;
                    let where_value = sub
                        .get("_where")
                        .ok_or_else(|| PlanError::validation(path, "_exists needs a _where"))?;
                    let Some(remote) = self.relationship_ctx(&table, permission_probe, true) else {
                        return Err(PlanError::validation(
                            path,
                            format!("table \"{table}\" not found in _exists"),
                        ));
                    };
                    self.collect_bool_exp_session_uses(
                        where_value,
                        &remote,
                        permission_probe,
                        path,
                        uses,
                    )?;
                }
                column if ctx.column_allowed_for_filter(column, true) => {
                    let db_column = ctx
                        .column_db_name_for_filter(column, true)
                        .expect("permission-filter column existence was checked");
                    let info = ctx
                        .column_info(&db_column)
                        .expect("permission-filter column has catalog info");
                    let column_path = format!("{path}.{column}");
                    self.collect_operator_session_uses(info, sub, &column_path, uses)?;
                }
                field_name => {
                    if let Some(computed) = ctx
                        .entry
                        .computed_fields
                        .iter()
                        .find(|computed| computed.name == field_name)
                    {
                        let definition = &computed.definition;
                        if definition.session_argument.is_some() {
                            return Err(PlanError::validation(
                                path,
                                format!(
                                    "computed field '{}' uses session_argument and cannot publish a closed session-variable contract",
                                    computed.name
                                ),
                            ));
                        }
                        let function = self
                            .catalog_function(
                                definition.function.schema(),
                                definition.function.name(),
                            )
                            .ok_or_else(|| {
                                PlanError::validation(
                                    path,
                                    format!(
                                        "function for computed field '{}' not found",
                                        computed.name
                                    ),
                                )
                            })?;
                        if let Some((schema, name)) = &function.returns_table {
                            let remote_table = donat_metadata::QualifiedTable::Qualified {
                                schema: schema.clone(),
                                name: name.clone(),
                            };
                            let Some(remote) =
                                self.relationship_ctx(&remote_table, permission_probe, true)
                            else {
                                return Err(PlanError::validation(
                                    path,
                                    format!(
                                        "field '{}' not found in type: '{}_bool_exp'",
                                        computed.name, ctx.type_name
                                    ),
                                ));
                            };
                            self.collect_bool_exp_session_uses(
                                sub,
                                &remote,
                                permission_probe,
                                path,
                                uses,
                            )?;
                        } else {
                            let info = ColumnInfo {
                                name: computed.name.clone(),
                                pg_type: function
                                    .returns_scalar
                                    .clone()
                                    .unwrap_or_else(|| "text".to_owned()),
                                pg_typmod: -1,
                                native_type: None,
                                nullable: false,
                                has_default: false,
                            };
                            let field_path = format!("{path}.{}", computed.name);
                            self.collect_operator_session_uses(&info, sub, &field_path, uses)?;
                        }
                        continue;
                    }

                    let Some((remote_table, _)) = self.relationship_target(ctx, field_name, path)
                    else {
                        return Err(PlanError::validation(
                            path,
                            format!(
                                "field '{field_name}' not found in type: '{}_bool_exp'",
                                ctx.type_name
                            ),
                        ));
                    };
                    let Some(remote) = self.relationship_ctx(&remote_table, permission_probe, true)
                    else {
                        return Err(PlanError::validation(
                            path,
                            format!(
                                "field '{field_name}' not found in type: '{}_bool_exp'",
                                ctx.type_name
                            ),
                        ));
                    };
                    self.collect_bool_exp_session_uses(sub, &remote, permission_probe, path, uses)?;
                }
            }
        }
        Ok(())
    }

    fn collect_operator_session_uses(
        &self,
        column: &ColumnInfo,
        value: &Json,
        path: &str,
        uses: &mut Vec<PermissionSessionUse>,
    ) -> Result<(), PlanError> {
        let Json::Object(operators) = value else {
            collect_permission_session_operand(
                value,
                PermissionSessionOperand::Scalar(column.clone()),
                uses,
            );
            return Ok(());
        };

        for (raw_name, operand) in operators {
            let name = normalize_predicate_operator(raw_name);
            let Some(operator) = classify_predicate_operator(&self.capabilities, &name) else {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "unexpected operator \"{name}\" for column '{}'",
                        column.name
                    ),
                ));
            };
            match operator.session_operand_kind() {
                PredicateSessionOperandKind::Scalar => collect_permission_session_operand(
                    operand,
                    PermissionSessionOperand::Scalar(column.clone()),
                    uses,
                ),
                PredicateSessionOperandKind::List => collect_permission_session_operand(
                    operand,
                    PermissionSessionOperand::List(column.clone()),
                    uses,
                ),
                PredicateSessionOperandKind::Boolean => collect_permission_session_operand(
                    operand,
                    PermissionSessionOperand::Boolean,
                    uses,
                ),
                PredicateSessionOperandKind::String => collect_permission_session_operand(
                    operand,
                    PermissionSessionOperand::String,
                    uses,
                ),
                PredicateSessionOperandKind::StringList => collect_permission_session_operand(
                    operand,
                    PermissionSessionOperand::StringList,
                    uses,
                ),
                PredicateSessionOperandKind::None => {}
                PredicateSessionOperandKind::Distance => {
                    let object = operand.as_object().ok_or_else(|| {
                        PlanError::validation(path, "expected { distance, from } for _st_d_within")
                    })?;
                    let distance = object.get("distance").ok_or_else(|| {
                        PlanError::validation(path, "missing 'distance' in _st_d_within")
                    })?;
                    let from = object.get("from").ok_or_else(|| {
                        PlanError::validation(path, "missing 'from' in _st_d_within")
                    })?;
                    collect_permission_session_operand(
                        distance,
                        PermissionSessionOperand::Decimal,
                        uses,
                    );
                    collect_permission_session_operand(
                        from,
                        PermissionSessionOperand::Scalar(column.clone()),
                        uses,
                    );
                }
            }
        }
        Ok(())
    }

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
            let logical = normalize_logical_operator(key);
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
                    let column_path = format!("{path}.{column}");
                    let ops = self.parse_ops(
                        ctx,
                        ComparisonField {
                            column: &db_column,
                            scalar_type: &info.pg_type,
                        },
                        sub,
                        session,
                        is_permission,
                        &column_path,
                    )?;
                    let mut parsed: Vec<BoolExp> = ops
                        .into_iter()
                        .map(|op| BoolExp::Compare {
                            column: db_column.clone(),
                            pg_type: info.sql_type().to_string(),
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
                            if !is_permission
                                && let Some(perm) =
                                    self.permission_predicate(&remote, session, path)?
                            {
                                inner = BoolExp::And(vec![inner, perm]);
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
            if !is_permission
                && let Some(perm) = self.permission_predicate(&remote, session, path)?
            {
                inner = BoolExp::And(vec![inner, perm]);
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
            let field_path = format!("{path}.{}", cf.name);
            let ops = self.parse_ops(
                ctx,
                ComparisonField {
                    column: &cf.name,
                    scalar_type: &pg_type,
                },
                value,
                session,
                is_permission,
                &field_path,
            )?;
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
        field: ComparisonField<'_>,
        value: &Json,
        session: &Session,
        is_permission: bool,
        path: &str,
    ) -> Result<Vec<CompareOp>, PlanError> {
        let ComparisonField {
            column,
            scalar_type,
        } = field;
        let Json::Object(ops) = value else {
            let resolved = resolve_session(value, session, is_permission, path)?;
            return Ok(vec![CompareOp::Eq(Scalar::Json(resolved))]);
        };

        let mut out = vec![];
        for (raw_op_name, op_value) in ops {
            // Legacy `$op` spelling -> `_op`.
            let op_name = normalize_predicate_operator(raw_op_name);
            let op_path = format!("{path}.{op_name}");
            let Some(operator) = classify_predicate_operator(&self.capabilities, &op_name) else {
                return Err(PlanError::validation(
                    path,
                    format!("unexpected operator \"{op_name}\" for column '{column}'"),
                ));
            };
            let scalar = |v: &Json| -> Result<Scalar, PlanError> {
                let resolved = resolve_session(v, session, is_permission, &op_path)?;
                validate_comparison_scalar(scalar_type, resolved, &op_path)
            };
            let list = |v: &Json| -> Result<Vec<Scalar>, PlanError> {
                // A session variable may itself hold an array, as JSON
                // ("[1,2]") or as a Postgres array literal ("{a,b}").
                let resolved = resolve_session(v, session, is_permission, &op_path)?;
                match resolved {
                    Json::Array(items) => items
                        .into_iter()
                        .map(|item| validate_comparison_scalar(scalar_type, item, &op_path))
                        .collect(),
                    Json::String(s) => parse_array_literal(&s)
                        .ok_or_else(|| {
                            PlanError::validation(&op_path, "expected an array of values")
                        })?
                        .into_iter()
                        .map(|item| validate_comparison_scalar(scalar_type, item, &op_path))
                        .collect(),
                    _ => Err(PlanError::validation(
                        &op_path,
                        "expected an array of values",
                    )),
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

            let op = match operator {
                PredicateOperator::Eq => CompareOp::Eq(scalar(op_value)?),
                PredicateOperator::Neq => CompareOp::Neq(scalar(op_value)?),
                PredicateOperator::Gt => CompareOp::Gt(scalar(op_value)?),
                PredicateOperator::Lt => CompareOp::Lt(scalar(op_value)?),
                PredicateOperator::Gte => CompareOp::Gte(scalar(op_value)?),
                PredicateOperator::Lte => CompareOp::Lte(scalar(op_value)?),
                PredicateOperator::In => CompareOp::In(list(op_value)?),
                PredicateOperator::Nin => CompareOp::Nin(list(op_value)?),
                PredicateOperator::Like => CompareOp::Like(scalar(op_value)?),
                PredicateOperator::Nlike => CompareOp::Nlike(scalar(op_value)?),
                PredicateOperator::Ilike => CompareOp::Ilike(scalar(op_value)?),
                PredicateOperator::Nilike => CompareOp::Nilike(scalar(op_value)?),
                PredicateOperator::Similar => CompareOp::Similar(scalar(op_value)?),
                PredicateOperator::Nsimilar => CompareOp::Nsimilar(scalar(op_value)?),
                PredicateOperator::Regex => CompareOp::Regex(scalar(op_value)?),
                PredicateOperator::Iregex => CompareOp::Iregex(scalar(op_value)?),
                PredicateOperator::Nregex => CompareOp::Nregex(scalar(op_value)?),
                PredicateOperator::Niregex => CompareOp::Niregex(scalar(op_value)?),
                PredicateOperator::IsNull => {
                    let v = resolve_session(op_value, session, is_permission, &op_path)?;
                    CompareOp::IsNull(parse_is_null_operand(
                        op_value,
                        &v,
                        is_permission,
                        &op_path,
                    )?)
                }
                // Column-to-column comparisons.
                PredicateOperator::Ceq => self.column_compare("=", op_value, ctx, path)?,
                PredicateOperator::Cne => self.column_compare("<>", op_value, ctx, path)?,
                PredicateOperator::Cgt => self.column_compare(">", op_value, ctx, path)?,
                PredicateOperator::Clt => self.column_compare("<", op_value, ctx, path)?,
                PredicateOperator::Cgte => self.column_compare(">=", op_value, ctx, path)?,
                PredicateOperator::Clte => self.column_compare("<=", op_value, ctx, path)?,
                // jsonb operators.
                PredicateOperator::HasKey => CompareOp::HasKey(scalar(op_value)?),
                PredicateOperator::HasKeysAny => CompareOp::HasKeysAny(string_list(op_value)?),
                PredicateOperator::HasKeysAll => CompareOp::HasKeysAll(string_list(op_value)?),
                PredicateOperator::Contains => CompareOp::Contains(scalar(op_value)?),
                PredicateOperator::ContainedIn => CompareOp::ContainedIn(scalar(op_value)?),
                // PostGIS operators.
                PredicateOperator::StContains => st_op("ST_Contains", op_value, &scalar)?,
                PredicateOperator::StCrosses => st_op("ST_Crosses", op_value, &scalar)?,
                PredicateOperator::StEquals => st_op("ST_Equals", op_value, &scalar)?,
                PredicateOperator::StIntersects => st_op("ST_Intersects", op_value, &scalar)?,
                PredicateOperator::StOverlaps => st_op("ST_Overlaps", op_value, &scalar)?,
                PredicateOperator::StTouches => st_op("ST_Touches", op_value, &scalar)?,
                PredicateOperator::StWithin => st_op("ST_Within", op_value, &scalar)?,
                PredicateOperator::St3dIntersects => st_op("ST_3DIntersects", op_value, &scalar)?,
                PredicateOperator::StDWithin { three_d } => {
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
                        three_d,
                    }
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

fn validate_comparison_scalar(
    scalar_type: &str,
    value: Json,
    path: &str,
) -> Result<Scalar, PlanError> {
    let valid_date = match &value {
        Json::Null => true,
        Json::String(text) => chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok(),
        _ => false,
    };
    if scalar_type == "date" && !valid_date {
        return Err(PlanError::validation(
            path,
            format!("expected a date, but found {value}"),
        ));
    }
    Ok(Scalar::Json(value))
}

fn collect_permission_session_operand(
    value: &Json,
    operand: PermissionSessionOperand,
    uses: &mut Vec<PermissionSessionUse>,
) {
    if let Json::String(name) = value
        && is_session_var_name(name)
    {
        uses.push(PermissionSessionUse {
            name: name.clone(),
            operand,
        });
    }
}

fn parse_is_null_operand(
    original: &Json,
    resolved: &Json,
    is_permission: bool,
    path: &str,
) -> Result<bool, PlanError> {
    if let Some(value) = resolved.as_bool() {
        return Ok(value);
    }
    if is_permission
        && matches!(original, Json::String(name) if is_session_var_name(name))
        && let Json::String(value) = resolved
    {
        if value.eq_ignore_ascii_case("true") {
            return Ok(true);
        }
        if value.eq_ignore_ascii_case("false") {
            return Ok(false);
        }
        return Err(PlanError::validation(path, "expected a boolean"));
    }
    // Preserve the established literal/user-filter behavior. Only a resolved
    // permission session variable receives the stricter text decoding above.
    Ok(false)
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
