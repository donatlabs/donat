//! Mutation planning (milestone M6): insert/update/delete root fields ->
//! IR, with the role's insert/update/delete permissions applied. As
//! everywhere, there is no admin bypass: the mutation root only exists for
//! a role that has the corresponding permission.

use donat_ir::*;
use donat_metadata::Columns;
use graphql_parser::query::{Field as GqlField, SelectionSet};
use serde_json::{Map as JsonMap, Value as Json};

use crate::plan::{
    Fragments, MutationKind, PlanError, Planner, Session, TableCtx, field_not_found, flatten,
    is_session_var_name, unexpected_arg, value_to_json,
};

impl<'a> Planner<'a> {
    /// Does the role have any mutation permission at all (respecting
    /// backend_only)? Donat reports "no mutations exist" when not.
    fn role_has_any_mutation(&self, session: &Session) -> bool {
        if !self.capabilities.mutations {
            return false;
        }
        // backend_only insert permissions don't exist for non-backend
        // requests: a role with only such permissions has an empty
        // mutation_root ("no mutations exist").
        let insert_usable = |list: &[donat_metadata::PermissionEntry<
            donat_metadata::InsertPermission,
        >]| {
            let usable = |p: &donat_metadata::PermissionEntry<donat_metadata::InsertPermission>| {
                !p.permission.backend_only || session.backend_request
            };
            list.iter().any(|p| p.role == session.role && usable(p))
                || self
                    .expand_role(&session.role)
                    .iter()
                    .any(|parent| list.iter().any(|p| &p.role == parent && usable(p)))
        };
        self.tables().iter().any(|t| {
            insert_usable(&t.insert_permissions)
                || self.any_role_perm(&t.update_permissions, &session.role)
                || self.any_role_perm(&t.delete_permissions, &session.role)
        }) || self.role_has_function_mutation(&session.role)
    }

    pub(crate) fn plan_mutation(
        &self,
        selection_set: &SelectionSet<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
    ) -> Result<Vec<MutationRoot>, PlanError> {
        if !self.capabilities.mutations {
            return Err(PlanError::validation("$", "no mutations exist"));
        }
        let mut out = vec![];
        for field in flatten(selection_set, fragments, vars, None)? {
            let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
            if field.name == "__typename" {
                out.push(MutationRoot::Typename {
                    alias,
                    value: "mutation_root".to_string(),
                });
                continue;
            }
            let path = format!("$.selectionSet.{}", field.name);
            let not_found = || {
                // Donat reports an empty mutation_root differently.
                if !self.role_has_any_mutation(session) {
                    PlanError::validation("$", "no mutations exist")
                } else {
                    PlanError::validation(
                        &path,
                        format!("field '{}' not found in type: 'mutation_root'", field.name),
                    )
                }
            };
            // Tracked function exposed as a mutation?
            if let Some(result) =
                self.try_plan_function_mutation(field, fragments, vars, session, &path)
            {
                let query = result?;
                out.push(MutationRoot::FunctionCall { alias, query });
                continue;
            }
            let Some(&(kind, idx)) = self.mutation_roots.get(&field.name) else {
                return Err(not_found());
            };
            // Selection context (select permission) — needed for returning.
            // The mutation permission itself is checked per kind below.
            let Some(ctx) = self.mutation_table_ctx(idx) else {
                return Err(not_found());
            };

            match kind {
                MutationKind::Insert | MutationKind::InsertOne => {
                    let insert = self.plan_insert(
                        &ctx, kind, field, fragments, vars, session, &path, not_found,
                    )?;
                    out.push(MutationRoot::Insert { alias, insert });
                }
                MutationKind::Update | MutationKind::UpdateByPk => {
                    let update = self.plan_update(
                        &ctx, kind, field, fragments, vars, session, &path, not_found,
                    )?;
                    out.push(MutationRoot::Update { alias, update });
                }
                MutationKind::Delete | MutationKind::DeleteByPk => {
                    let delete = self.plan_delete(
                        &ctx, kind, field, fragments, vars, session, &path, not_found,
                    )?;
                    out.push(MutationRoot::Delete { alias, delete });
                }
            }
        }
        if out.is_empty() {
            return Err(PlanError::validation("$", "selection set cannot be empty"));
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_insert(
        &self,
        ctx: &TableCtx<'a>,
        kind: MutationKind,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
        not_found: impl Fn() -> PlanError,
    ) -> Result<InsertMutation, PlanError> {
        let perm = self
            .resolve_role_perm(&ctx.entry.insert_permissions, &session.role, |p| {
                !p.backend_only || session.backend_request
            })
            .ok_or_else(&not_found)?;

        let mut objects: Vec<Json> = vec![];
        let mut on_conflict = None;
        for (arg, value) in &field.arguments {
            let value = value_to_json(value, vars, path)?;
            match (kind, arg.as_str()) {
                (MutationKind::Insert, "objects") => {
                    // GraphQL list coercion: a single object is [object].
                    objects = match value {
                        Json::Array(items) => items,
                        other @ Json::Object(_) => vec![other],
                        _ => {
                            return Err(PlanError::validation(path, "objects must be a list"));
                        }
                    };
                }
                (MutationKind::InsertOne, "object") => objects = vec![value],
                (_, "on_conflict") => {
                    if !value.is_null() {
                        on_conflict = Some(self.parse_on_conflict(&value, ctx, session, path)?);
                    }
                }
                (_, other) => return Err(unexpected_arg(path, other)),
            }
        }
        if objects.is_empty() {
            return Err(PlanError::validation(
                path,
                "expecting a non-empty list of objects",
            ));
        }

        let mut nested_object_inserts = vec![];

        // Column union across objects, validated against the insert mask.
        let mut columns: Vec<String> = vec![];
        for object in &objects {
            let Some(map) = object.as_object() else {
                return Err(PlanError::validation(path, "objects must be objects"));
            };
            for key in map.keys() {
                let Some(db_key) = ctx.column_db_name(key) else {
                    let value = map.get(key).expect("key came from map");
                    if let Some(nested) =
                        self.parse_nested_object_insert(ctx, key, value, session, path)?
                    {
                        if objects.len() != 1 {
                            return Err(PlanError::validation(
                                path,
                                "nested object inserts support a single object",
                            ));
                        }
                        nested_object_inserts.push(nested);
                        continue;
                    }
                    return Err(field_not_found(
                        path,
                        key,
                        &format!("{}_insert_input", ctx.type_name),
                    ));
                };
                let allowed = match &perm.columns {
                    Columns::Star => ctx.info.column(&db_key).is_some(),
                    Columns::List(cols) => {
                        cols.iter().any(|c| c == &db_key) && ctx.info.column(&db_key).is_some()
                    }
                };
                if !allowed {
                    return Err(field_not_found(
                        path,
                        key,
                        &format!("{}_insert_input", ctx.type_name),
                    ));
                }
                if !columns.contains(&db_key) {
                    columns.push(db_key);
                }
            }
        }

        // Permission presets (`set`) override user values.
        let mut preset_values: Vec<(String, Scalar)> = vec![];
        for (col, value) in &perm.set {
            if ctx.info.column(col).is_none() {
                continue;
            }
            let resolved = match value {
                Json::String(s) if is_session_var_name(s) => {
                    let v = session.var(s).ok_or_else(|| {
                        PlanError::new(
                            "$",
                            "not-found",
                            format!("missing session variable: \"{}\"", s.to_ascii_lowercase()),
                        )
                    })?;
                    Json::String(v.to_string())
                }
                other => other.clone(),
            };
            if !columns.contains(col) {
                columns.push(col.clone());
            }
            preset_values.push((col.clone(), Scalar::Json(resolved)));
        }

        let typed_columns: Vec<(String, String)> = columns
            .iter()
            .map(|c| {
                let pg_type = ctx
                    .info
                    .column(c)
                    .map(|i| i.sql_type().to_string())
                    .unwrap();
                (c.clone(), pg_type)
            })
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
                        let gql_col = ctx.column_graphql_name(col);
                        map.get(&gql_col).map(|v| Scalar::Json(v.clone()))
                    })
                    .collect()
            })
            .collect();

        let check = self.parse_check_exp(&perm.check, ctx, session, path)?;
        let output =
            self.parse_mutation_output(ctx, kind, field, fragments, vars, session, path)?;

        Ok(InsertMutation {
            table: Table {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            },
            columns: typed_columns,
            rows,
            nested_object_inserts,
            on_conflict,
            check,
            check_path: format!("{path}.args.objects"),
            output,
        })
    }

    fn parse_nested_object_insert(
        &self,
        ctx: &TableCtx<'a>,
        key: &str,
        value: &Json,
        session: &Session,
        path: &str,
    ) -> Result<Option<NestedObjectInsert>, PlanError> {
        let Some(rel) = ctx
            .entry
            .object_relationships
            .iter()
            .find(|r| r.name == key)
        else {
            return Ok(None);
        };
        let Some(manual) = &rel.using.manual_configuration else {
            return Ok(None);
        };
        if manual.insertion_order.as_deref() != Some("after_parent") {
            return Ok(None);
        }

        let Some(remote_ctx) = self.table_ctx_by_name(&manual.remote_table, &session.role) else {
            return Ok(None);
        };
        let remote_perm = self
            .resolve_role_perm(&remote_ctx.entry.insert_permissions, &session.role, |p| {
                !p.backend_only || session.backend_request
            })
            .ok_or_else(|| {
                field_not_found(path, key, &format!("{}_insert_input", ctx.type_name))
            })?;

        let obj = value.as_object().ok_or_else(|| {
            PlanError::validation(
                path,
                format!(
                    "field '{key}' must be an object in type: '{}_insert_input'",
                    ctx.type_name
                ),
            )
        })?;
        let data = obj.get("data").ok_or_else(|| {
            PlanError::validation(path, "expecting a value for the argument \"data\"")
        })?;
        for arg in obj.keys() {
            if arg != "data" {
                return Err(field_not_found(
                    path,
                    arg,
                    &format!("{}_obj_rel_insert_input", remote_ctx.type_name),
                ));
            }
        }
        let data_obj = data.as_object().ok_or_else(|| {
            PlanError::validation(
                path,
                format!(
                    "field 'data' must be an object in type: '{}_obj_rel_insert_input'",
                    remote_ctx.type_name
                ),
            )
        })?;

        let mapped_child_cols = manual
            .column_mapping
            .values()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut columns = vec![];
        let mut row = vec![];
        for (child_key, child_value) in data_obj {
            let Some(db_key) = remote_ctx.column_db_name(child_key) else {
                return Err(field_not_found(
                    path,
                    child_key,
                    &format!("{}_insert_input", remote_ctx.type_name),
                ));
            };
            if mapped_child_cols.contains(&db_key) {
                return Err(field_not_found(
                    path,
                    child_key,
                    &format!("{}_insert_input", remote_ctx.type_name),
                ));
            }
            let allowed = match &remote_perm.columns {
                Columns::Star => remote_ctx.info.column(&db_key).is_some(),
                Columns::List(cols) => {
                    cols.iter().any(|c| c == &db_key) && remote_ctx.info.column(&db_key).is_some()
                }
            };
            if !allowed {
                return Err(field_not_found(
                    path,
                    child_key,
                    &format!("{}_insert_input", remote_ctx.type_name),
                ));
            }
            let pg_type = remote_ctx
                .info
                .column(&db_key)
                .map(|i| i.pg_type.clone())
                .unwrap();
            columns.push((db_key, pg_type));
            row.push(Some(Scalar::Json(child_value.clone())));
        }
        for (col, value) in &remote_perm.set {
            if mapped_child_cols.contains(col) || remote_ctx.info.column(col).is_none() {
                continue;
            }
            let resolved = match value {
                Json::String(s) if is_session_var_name(s) => {
                    let v = session.var(s).ok_or_else(|| {
                        PlanError::new(
                            "$",
                            "not-found",
                            format!("missing session variable: \"{}\"", s.to_ascii_lowercase()),
                        )
                    })?;
                    Json::String(v.to_string())
                }
                other => other.clone(),
            };
            if !columns.iter().any(|(existing, _)| existing == col) {
                let pg_type = remote_ctx
                    .info
                    .column(col)
                    .map(|i| i.pg_type.clone())
                    .unwrap();
                columns.push((col.clone(), pg_type));
                row.push(Some(Scalar::Json(resolved)));
            }
        }

        Ok(Some(NestedObjectInsert {
            relationship_name: key.to_string(),
            table: Table {
                schema: remote_ctx.info.schema.clone(),
                name: remote_ctx.info.name.clone(),
            },
            column_mapping: manual
                .column_mapping
                .iter()
                .map(|(parent, child)| (parent.clone(), child.clone()))
                .collect(),
            columns,
            row,
            check: self.parse_check_exp(&remote_perm.check, &remote_ctx, session, path)?,
            check_path: format!("{path}.args.object.{key}.data"),
        }))
    }

    fn parse_on_conflict(
        &self,
        value: &Json,
        ctx: &TableCtx<'a>,
        session: &Session,
        path: &str,
    ) -> Result<OnConflict, PlanError> {
        let obj = value
            .as_object()
            .ok_or_else(|| PlanError::validation(path, "on_conflict must be an object"))?;
        let constraint = obj
            .get("constraint")
            .and_then(Json::as_str)
            .ok_or_else(|| PlanError::validation(path, "on_conflict needs a constraint"))?
            .to_string();
        let update_columns: Vec<String> = obj
            .get("update_columns")
            .and_then(Json::as_array)
            .map(|cols| {
                cols.iter()
                    .map(|c| {
                        let Some(name) = c.as_str() else {
                            return Err(PlanError::validation(
                                &format!("{path}.args.on_conflict"),
                                "erroneous column name",
                            ));
                        };
                        ctx.column_db_name(name).ok_or_else(|| {
                            PlanError::validation(
                                &format!("{path}.args.on_conflict"),
                                "erroneous column name",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        for col in &update_columns {
            if ctx.info.column(col).is_none() {
                return Err(PlanError::validation(
                    &format!("{path}.args.on_conflict"),
                    "erroneous column name",
                ));
            }
        }
        let mut predicate = match obj.get("where") {
            Some(Json::Null) | None => None,
            Some(w) => Some(self.parse_bool_exp(w, ctx, session, false, path)?),
        };

        // DO UPDATE acts as an update: the role's update-permission filter
        // restricts which existing rows may be updated, and its presets
        // are applied.
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
                    let filter_ctx = self.filter_ctx_of(ctx);
                    let filter =
                        self.parse_bool_exp(&update_perm.filter, &filter_ctx, session, true, path)?;
                    predicate = Some(match predicate.take() {
                        Some(p) => BoolExp::And(vec![p, filter]),
                        None => filter,
                    });
                }
                for (col, value) in &update_perm.set {
                    let Some(info) = ctx.info.column(col) else {
                        continue;
                    };
                    let resolved = match value {
                        Json::String(s) if is_session_var_name(s) => {
                            let v = session.var(s).ok_or_else(|| {
                                PlanError::new(
                                    "$",
                                    "not-found",
                                    format!(
                                        "missing session variable: \"{}\"",
                                        s.to_ascii_lowercase()
                                    ),
                                )
                            })?;
                            Json::String(v.to_string())
                        }
                        other => other.clone(),
                    };
                    set_ops.push(SetOp::Set {
                        column: col.clone(),
                        pg_type: info.sql_type().to_string(),
                        value: Scalar::Json(resolved),
                    });
                }
            }
        }

        Ok(OnConflict {
            constraint,
            update_columns,
            predicate,
            set_ops,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_update(
        &self,
        ctx: &TableCtx<'a>,
        kind: MutationKind,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
        not_found: impl Fn() -> PlanError,
    ) -> Result<UpdateMutation, PlanError> {
        // Admin: full update access (all columns, no row filter, no check).
        let perm = self
            .resolve_role_perm(&ctx.entry.update_permissions, &session.role, |_| true)
            .ok_or_else(&not_found)?;

        let allowed = |col: &str| -> bool {
            let Some(db_col) = ctx.column_db_name(col) else {
                return false;
            };
            ctx.info.column(&db_col).is_some()
                && match &perm.columns {
                    Columns::Star => true,
                    Columns::List(cols) => cols.iter().any(|c| c == &db_col),
                }
        };

        let mut sets: Vec<SetOp> = vec![];
        let mut user_where = None;
        let mut pk_predicate: Vec<BoolExp> = vec![];
        let mut saw_where = false;

        for (arg, value) in &field.arguments {
            let value = value_to_json(value, vars, path)?;
            match (kind, arg.as_str()) {
                (_, "_set") => {
                    let map = value
                        .as_object()
                        .ok_or_else(|| PlanError::validation(path, "_set must be an object"))?;
                    for (col, v) in map {
                        if !allowed(col) {
                            return Err(field_not_found(
                                path,
                                col,
                                &format!("{}_set_input", ctx.type_name),
                            ));
                        }
                        let db_col = ctx.column_db_name(col).unwrap();
                        sets.push(SetOp::Set {
                            column: db_col.clone(),
                            pg_type: ctx.info.column(&db_col).unwrap().sql_type().to_string(),
                            value: Scalar::Json(v.clone()),
                        });
                    }
                }
                (_, "_inc") => {
                    let map = value
                        .as_object()
                        .ok_or_else(|| PlanError::validation(path, "_inc must be an object"))?;
                    for (col, v) in map {
                        if !allowed(col) {
                            return Err(field_not_found(
                                path,
                                col,
                                &format!("{}_inc_input", ctx.type_name),
                            ));
                        }
                        let db_col = ctx.column_db_name(col).unwrap();
                        sets.push(SetOp::Inc {
                            column: db_col.clone(),
                            pg_type: ctx.info.column(&db_col).unwrap().sql_type().to_string(),
                            value: Scalar::Json(v.clone()),
                        });
                    }
                }
                (_, "_append") => {
                    let map = value
                        .as_object()
                        .ok_or_else(|| PlanError::validation(path, "_append must be an object"))?;
                    for (col, v) in map {
                        if !allowed(col) {
                            return Err(field_not_found(
                                path,
                                col,
                                &format!("{}_append_input", ctx.type_name),
                            ));
                        }
                        let db_col = ctx.column_db_name(col).unwrap();
                        let info = ctx.info.column(&db_col).unwrap();
                        if info.pg_type != "jsonb" {
                            return Err(field_not_found(
                                path,
                                col,
                                &format!("{}_append_input", ctx.type_name),
                            ));
                        }
                        sets.push(SetOp::JsonbAppend {
                            column: db_col.clone(),
                            value: Scalar::Json(v.clone()),
                        });
                    }
                }
                (MutationKind::Update, "where") => {
                    saw_where = true;
                    user_where = Some(self.parse_bool_exp(&value, ctx, session, false, path)?);
                }
                (MutationKind::UpdateByPk, "pk_columns") => {
                    let map = value.as_object().ok_or_else(|| {
                        PlanError::validation(path, "pk_columns must be an object")
                    })?;
                    for (col, v) in map {
                        let Some(db_col) = ctx.column_db_name(col) else {
                            return Err(field_not_found(path, col, &ctx.type_name));
                        };
                        let Some(info) = ctx.info.column(&db_col) else {
                            return Err(field_not_found(path, col, &ctx.type_name));
                        };
                        pk_predicate.push(BoolExp::Compare {
                            column: db_col,
                            pg_type: info.sql_type().to_string(),
                            op: CompareOp::Eq(Scalar::Json(v.clone())),
                        });
                    }
                }
                (_, other) => return Err(unexpected_arg(path, other)),
            }
        }

        if kind == MutationKind::Update && !saw_where {
            return Err(PlanError::validation(
                path,
                "expecting a value for the argument \"where\"",
            ));
        }
        if kind == MutationKind::UpdateByPk && pk_predicate.is_empty() {
            return Err(PlanError::validation(
                path,
                "expecting a value for the argument \"pk_columns\"",
            ));
        }

        // Permission presets.
        for (col, value) in &perm.set {
            if ctx.info.column(col).is_none() {
                continue;
            }
            let resolved = match value {
                Json::String(s) if is_session_var_name(s) => {
                    let v = session.var(s).ok_or_else(|| {
                        PlanError::new(
                            "$",
                            "not-found",
                            format!("missing session variable: \"{}\"", s.to_ascii_lowercase()),
                        )
                    })?;
                    Json::String(v.to_string())
                }
                other => other.clone(),
            };
            sets.push(SetOp::Set {
                column: col.clone(),
                pg_type: ctx.info.column(col).unwrap().sql_type().to_string(),
                value: Scalar::Json(resolved),
            });
        }

        if sets.is_empty() {
            return Err(PlanError::validation(
                path,
                "at least any one of _set, _inc, _append is expected",
            ));
        }

        // Predicate: pk/user where AND the role's update filter.
        let mut predicates = pk_predicate;
        if let Some(w) = user_where {
            predicates.push(w);
        }
        if !perm.filter.is_null() && !perm.filter.as_object().is_some_and(|o| o.is_empty()) {
            let filter_ctx = self.filter_ctx_of(ctx);
            predicates.push(self.parse_bool_exp(&perm.filter, &filter_ctx, session, true, path)?);
        }
        let predicate = match predicates.len() {
            0 => None,
            1 => predicates.pop(),
            _ => Some(BoolExp::And(predicates)),
        };

        let check = match &perm.check {
            Some(check) => self.parse_check_exp(check, ctx, session, path)?,
            None => None,
        };
        let output =
            self.parse_mutation_output(ctx, kind, field, fragments, vars, session, path)?;

        Ok(UpdateMutation {
            table: Table {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            },
            sets,
            predicate,
            check,
            check_path: "$".to_string(),
            output,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_delete(
        &self,
        ctx: &TableCtx<'a>,
        kind: MutationKind,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
        not_found: impl Fn() -> PlanError,
    ) -> Result<DeleteMutation, PlanError> {
        let perm = self
            .resolve_role_perm(&ctx.entry.delete_permissions, &session.role, |_| true)
            .ok_or_else(&not_found)?;

        let mut user_where = None;
        let mut pk_predicate: Vec<BoolExp> = vec![];
        let mut saw_where = false;
        for (arg, value) in &field.arguments {
            let value = value_to_json(value, vars, path)?;
            match (kind, arg.as_str()) {
                (MutationKind::Delete, "where") => {
                    saw_where = true;
                    user_where = Some(self.parse_bool_exp(&value, ctx, session, false, path)?);
                }
                (MutationKind::DeleteByPk, col) => {
                    let Some(db_col) = ctx.column_db_name(col) else {
                        return Err(unexpected_arg(path, col));
                    };
                    let Some(info) = ctx.info.column(&db_col) else {
                        return Err(unexpected_arg(path, col));
                    };
                    if !ctx.info.primary_key.iter().any(|c| c == &db_col) {
                        return Err(unexpected_arg(path, col));
                    }
                    pk_predicate.push(BoolExp::Compare {
                        column: db_col,
                        pg_type: info.sql_type().to_string(),
                        op: CompareOp::Eq(Scalar::Json(value)),
                    });
                }
                (_, other) => return Err(unexpected_arg(path, other)),
            }
        }
        if kind == MutationKind::Delete && !saw_where {
            return Err(PlanError::validation(
                path,
                "expecting a value for the argument \"where\"",
            ));
        }

        let mut predicates = pk_predicate;
        if let Some(w) = user_where {
            predicates.push(w);
        }
        if !perm.filter.is_null() && !perm.filter.as_object().is_some_and(|o| o.is_empty()) {
            let filter_ctx = self.filter_ctx_of(ctx);
            predicates.push(self.parse_bool_exp(&perm.filter, &filter_ctx, session, true, path)?);
        }
        let predicate = match predicates.len() {
            0 => None,
            1 => predicates.pop(),
            _ => Some(BoolExp::And(predicates)),
        };

        let output =
            self.parse_mutation_output(ctx, kind, field, fragments, vars, session, path)?;

        Ok(DeleteMutation {
            table: Table {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            },
            predicate,
            output,
        })
    }

    /// Parse an insert/update `check` expression (None when empty).
    fn parse_check_exp(
        &self,
        check: &Json,
        ctx: &TableCtx<'a>,
        session: &Session,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        if check.is_null() || check.as_object().is_some_and(|o| o.is_empty()) {
            return Ok(None);
        }
        let filter_ctx = self.filter_ctx_of(ctx);
        Ok(Some(self.parse_bool_exp(
            check,
            &filter_ctx,
            session,
            true,
            path,
        )?))
    }

    /// The mutation's selection set: `{ affected_rows, returning }` or the
    /// row itself for `_one`/`_by_pk` roots.
    #[allow(clippy::too_many_arguments)]
    fn parse_mutation_output(
        &self,
        ctx: &TableCtx<'a>,
        kind: MutationKind,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Result<MutationOutput, PlanError> {
        let single = matches!(
            kind,
            MutationKind::InsertOne | MutationKind::UpdateByPk | MutationKind::DeleteByPk
        );

        // Returning rows requires the role to have a select permission.
        let select_ctx = self.relationship_ctx(
            &donat_metadata::QualifiedTable::Qualified {
                schema: ctx.info.schema.clone(),
                name: ctx.info.name.clone(),
            },
            session,
            false,
        );

        if single {
            let Some(select_ctx) = select_ctx else {
                return Err(PlanError::validation(
                    path,
                    format!("field '{}' not found in type: 'mutation_root'", field.name),
                ));
            };
            let fields = self.walk_table_selection(
                &select_ctx,
                &field.selection_set,
                fragments,
                vars,
                session,
                path,
            )?;
            return Ok(MutationOutput::SingleRow(fields));
        }

        let response_type = format!("{}_mutation_response", ctx.type_name);
        let mut out = vec![];
        for sub in flatten(&field.selection_set, fragments, vars, Some(&response_type))? {
            let alias = sub.alias.clone().unwrap_or_else(|| sub.name.clone());
            let fpath = format!("{path}.selectionSet.{}", sub.name);
            match sub.name.as_str() {
                "__typename" => out.push(MutationResponseField::Typename {
                    alias,
                    value: response_type.clone(),
                }),
                "affected_rows" => out.push(MutationResponseField::AffectedRows { alias }),
                "returning" => {
                    let Some(select_ctx) = select_ctx.as_ref() else {
                        return Err(field_not_found(&fpath, "returning", &response_type));
                    };
                    let fields = self.walk_table_selection(
                        select_ctx,
                        &sub.selection_set,
                        fragments,
                        vars,
                        session,
                        &fpath,
                    )?;
                    out.push(MutationResponseField::Returning { alias, fields });
                }
                other => return Err(field_not_found(&fpath, other, &response_type)),
            }
        }
        Ok(MutationOutput::Response(out))
    }
}
