//! Mutation planning (milestone M6): insert/update/delete root fields ->
//! IR, with the role's insert/update/delete permissions applied. As
//! everywhere, there is no admin bypass: the mutation root only exists for
//! a role that has the corresponding permission.

use std::collections::BTreeMap;

use donat_backend::capabilities::{JsonOps, UpsertKind};
use donat_ir::*;
use donat_metadata::{
    Columns, Command, CommandEffect, CommandIdempotencyKey, CommandIdempotencyScope, CommandStep,
    CommandStepOperation, CommandValue,
};
use donat_rules::{RuleType, SqlBinding, SqlBindings, SqlExpression, lower_postgres_expression};
use graphql_parser::query::{Field as GqlField, SelectionSet};
use serde_json::{Map as JsonMap, Value as Json};

use crate::commands::{CompiledCommand, command_retention_seconds};
use crate::plan::{
    Fragments, MutationKind, PlanError, Planner, Session, TableCtx, field_not_found, flatten,
    is_session_var_name, unexpected_arg, value_to_json,
};

struct ResolvedCommandStep {
    cte: String,
    columns: BTreeMap<String, CommandColumn>,
    returning: Vec<CommandColumn>,
    many: bool,
}

struct CommandItemContext {
    fields: BTreeMap<String, CommandColumn>,
}

impl<'a> Planner<'a> {
    /// Does the role have any mutation permission at all (respecting
    /// backend_only)? Donat reports "no mutations exist" when not.
    pub(crate) fn role_has_any_mutation(&self, session: &Session) -> bool {
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
            || self
                .command_definitions()
                .any(|command| self.command_is_visible(command, session))
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
            if let Some(result) = self.try_plan_command(field, fragments, vars, session, &path) {
                out.push(MutationRoot::Command {
                    alias,
                    command: result?,
                });
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

    pub(crate) fn command_is_visible(&self, command: &CompiledCommand, session: &Session) -> bool {
        if self.expose_all_commands {
            return true;
        }
        self.command_is_permitted(command.definition(), session)
            && self
                .validate_command_runtime_permissions(command.definition(), session, "$")
                .is_ok()
    }

    fn try_plan_command(
        &self,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Option<Result<CommandMutation, PlanError>> {
        let command = self.command_named(&field.name)?;
        if !self.command_is_permitted(command.definition(), session) {
            return None;
        }
        Some((|| {
            self.validate_command_runtime_permissions(command.definition(), session, path)?;
            let arguments =
                self.parse_command_arguments(command.definition(), field, vars, path)?;
            let selection =
                self.plan_command_selection(command.definition(), field, fragments, vars, path)?;
            self.resolve_command_execution(command, arguments, selection, session, path)
        })())
    }

    fn resolve_command_execution(
        &self,
        command: &CompiledCommand,
        arguments: BTreeMap<String, Scalar>,
        selection: Vec<CommandResultSelection>,
        session: &Session,
        path: &str,
    ) -> Result<CommandMutation, PlanError> {
        let definition = command.definition();
        let mut resolved_steps = BTreeMap::new();
        let mut steps = Vec::with_capacity(definition.steps.len());
        for (index, step) in definition.steps.iter().enumerate() {
            let cte = format!("_cmd_step_{index}");
            let step_path = format!("{path}.steps[{index}]");
            let (resolved, output) = self.resolve_command_step(
                command,
                step,
                cte,
                &arguments,
                &resolved_steps,
                session,
                &step_path,
                path,
            )?;
            steps.push(resolved);
            resolved_steps.insert(step.name.clone(), output);
        }

        let guards = definition
            .guards
            .iter()
            .enumerate()
            .map(|(index, guard)| {
                self.resolve_command_rule(
                    command,
                    &guard.rule,
                    &guard.bindings,
                    &arguments,
                    &resolved_steps,
                    None,
                    &format!("{path}.guards[{index}]"),
                    path.to_owned(),
                    guard
                        .message
                        .clone()
                        .unwrap_or_else(|| "command guard rejected".to_owned()),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        let result = definition
            .result
            .fields
            .iter()
            .map(|field| {
                Ok(CommandResultField {
                    name: field.name.clone(),
                    value: self.resolve_command_result_value(
                        &field.value,
                        &resolved_steps,
                        path,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, PlanError>>()?;

        let idempotency =
            self.resolve_command_idempotency(definition, &arguments, session, path)?;
        let effects = definition
            .effects
            .iter()
            .map(|effect| match effect {
                CommandEffect::StartProcess { .. } => CommandEffectKind::StartProcess,
                CommandEffect::SignalProcess { .. } => CommandEffectKind::SignalProcess,
            })
            .collect();

        Ok(CommandMutation {
            name: definition.name.clone(),
            steps,
            guards,
            result,
            idempotency,
            effects,
            selection,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_command_step(
        &self,
        command: &CompiledCommand,
        step: &CommandStep,
        cte: String,
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        session: &Session,
        path: &str,
        error_path: &str,
    ) -> Result<(CommandExecutionStep, ResolvedCommandStep), PlanError> {
        let output = |cte: String, columns: &[CommandColumn], many| ResolvedCommandStep {
            cte,
            columns: columns
                .iter()
                .cloned()
                .map(|column| (column.name.clone(), column))
                .collect(),
            returning: columns.to_vec(),
            many,
        };
        match &step.operation {
            CommandStepOperation::Assert { assert } => {
                let rule = self.resolve_command_rule(
                    command,
                    &assert.rule,
                    &assert.bindings,
                    arguments,
                    previous_steps,
                    None,
                    path,
                    error_path.to_owned(),
                    assert
                        .message
                        .clone()
                        .unwrap_or_else(|| "command assertion rejected".to_owned()),
                )?;
                Ok((
                    CommandExecutionStep::Assert {
                        name: step.name.clone(),
                        rule,
                    },
                    ResolvedCommandStep {
                        cte,
                        columns: BTreeMap::new(),
                        returning: Vec::new(),
                        many: false,
                    },
                ))
            }
            CommandStepOperation::SelectOne { select_one } => {
                let context = self.command_table_context(&select_one.table, session, path)?;
                let returning =
                    self.command_columns(&select_one.table, &select_one.returning, path)?;
                let by = self.resolve_command_assignments(
                    command,
                    &select_one.by,
                    &select_one.table,
                    arguments,
                    previous_steps,
                    None,
                    path,
                )?;
                let filter = self.permission_predicate(&context, session, path)?;
                let resolved = CommandExecutionStep::SelectOne {
                    name: step.name.clone(),
                    cte: cte.clone(),
                    table: Table {
                        schema: context.info.schema.clone(),
                        name: context.info.name.clone(),
                    },
                    by,
                    returning: returning.clone(),
                    require_found: select_one.require_found,
                    filter,
                    error_path: error_path.to_owned(),
                };
                Ok((resolved, output(cte, &returning, false)))
            }
            CommandStepOperation::Insert { insert } => {
                let context = self.command_table_context(&insert.table, session, path)?;
                let permission = self
                    .resolve_role_perm(
                        &context.entry.insert_permissions,
                        &session.role,
                        |permission| !permission.backend_only || session.backend_request,
                    )
                    .ok_or_else(|| {
                        PlanError::validation(path, "command insert permission is missing")
                    })?;
                let mut object = self.resolve_command_assignments(
                    command,
                    &insert.object,
                    &insert.table,
                    arguments,
                    previous_steps,
                    None,
                    path,
                )?;
                self.apply_command_presets(
                    &mut object,
                    &permission.set,
                    &insert.table,
                    session,
                    path,
                )?;
                let returning = self.command_columns(&insert.table, &insert.returning, path)?;
                let check = self.parse_check_exp(&permission.check, &context, session, path)?;
                let resolved = CommandExecutionStep::Insert {
                    name: step.name.clone(),
                    cte: cte.clone(),
                    table: Table {
                        schema: context.info.schema.clone(),
                        name: context.info.name.clone(),
                    },
                    object,
                    returning: returning.clone(),
                    check,
                    error_path: error_path.to_owned(),
                };
                Ok((resolved, output(cte, &returning, false)))
            }
            CommandStepOperation::InsertMany { insert_many } => {
                let context = self.command_table_context(&insert_many.table, session, path)?;
                let permission = self
                    .resolve_role_perm(
                        &context.entry.insert_permissions,
                        &session.role,
                        |permission| !permission.backend_only || session.backend_request,
                    )
                    .ok_or_else(|| {
                        PlanError::validation(path, "command insert permission is missing")
                    })?;
                let CommandValue::Argument { arg } = &insert_many.for_each else {
                    return Err(PlanError::validation(
                        path,
                        "insert_many for_each must resolve a declared argument before SQLgen",
                    ));
                };
                let items = command_argument(arguments, arg, path)?;
                let item = CommandItemContext {
                    fields: self.command_item_fields(
                        command,
                        &insert_many.object,
                        &insert_many.table,
                        path,
                    )?,
                };
                let mut object = self.resolve_command_assignments(
                    command,
                    &insert_many.object,
                    &insert_many.table,
                    arguments,
                    previous_steps,
                    Some(&item),
                    path,
                )?;
                self.apply_command_presets(
                    &mut object,
                    &permission.set,
                    &insert_many.table,
                    session,
                    path,
                )?;
                let returning =
                    self.command_columns(&insert_many.table, &insert_many.returning, path)?;
                let check = self.parse_check_exp(&permission.check, &context, session, path)?;
                let resolved = CommandExecutionStep::InsertMany {
                    name: step.name.clone(),
                    cte: cte.clone(),
                    table: Table {
                        schema: context.info.schema.clone(),
                        name: context.info.name.clone(),
                    },
                    items,
                    item_fields: item.fields.into_values().collect(),
                    object,
                    returning: returning.clone(),
                    allow_empty: insert_many.allow_empty,
                    check,
                    error_path: error_path.to_owned(),
                };
                Ok((resolved, output(cte, &returning, true)))
            }
            CommandStepOperation::Update { update } => {
                let context = self.command_table_context(&update.table, session, path)?;
                let permission = self
                    .resolve_role_perm(&context.entry.update_permissions, &session.role, |_| true)
                    .ok_or_else(|| {
                        PlanError::validation(path, "command update permission is missing")
                    })?;
                let predicate = self.resolve_command_assignments(
                    command,
                    &update.predicate,
                    &update.table,
                    arguments,
                    previous_steps,
                    None,
                    path,
                )?;
                let mut set = self.resolve_command_assignments(
                    command,
                    &update.set,
                    &update.table,
                    arguments,
                    previous_steps,
                    None,
                    path,
                )?;
                self.apply_command_presets(
                    &mut set,
                    &permission.set,
                    &update.table,
                    session,
                    path,
                )?;
                let returning = self.command_columns(&update.table, &update.returning, path)?;
                let filter =
                    self.command_permission_filter(&permission.filter, &context, session, path)?;
                let check = match &permission.check {
                    Some(check) => self.parse_check_exp(check, &context, session, path)?,
                    None => None,
                };
                let resolved = CommandExecutionStep::Update {
                    name: step.name.clone(),
                    cte: cte.clone(),
                    table: Table {
                        schema: context.info.schema.clone(),
                        name: context.info.name.clone(),
                    },
                    predicate,
                    set,
                    returning: returning.clone(),
                    require_affected: update.require_affected,
                    filter,
                    check,
                    error_path: error_path.to_owned(),
                };
                Ok((resolved, output(cte, &returning, false)))
            }
            CommandStepOperation::Delete { delete } => {
                let context = self.command_table_context(&delete.table, session, path)?;
                let permission = self
                    .resolve_role_perm(&context.entry.delete_permissions, &session.role, |_| true)
                    .ok_or_else(|| {
                        PlanError::validation(path, "command delete permission is missing")
                    })?;
                let predicate = self.resolve_command_assignments(
                    command,
                    &delete.predicate,
                    &delete.table,
                    arguments,
                    previous_steps,
                    None,
                    path,
                )?;
                let returning = self.command_columns(&delete.table, &delete.returning, path)?;
                let filter =
                    self.command_permission_filter(&permission.filter, &context, session, path)?;
                let resolved = CommandExecutionStep::Delete {
                    name: step.name.clone(),
                    cte: cte.clone(),
                    table: Table {
                        schema: context.info.schema.clone(),
                        name: context.info.name.clone(),
                    },
                    predicate,
                    returning: returning.clone(),
                    require_affected: delete.require_affected,
                    filter,
                    error_path: error_path.to_owned(),
                };
                Ok((resolved, output(cte, &returning, false)))
            }
        }
    }

    fn command_table_context(
        &self,
        table: &donat_metadata::QualifiedTable,
        session: &Session,
        path: &str,
    ) -> Result<TableCtx<'a>, PlanError> {
        self.table_ctx_by_name(table, &session.role).ok_or_else(|| {
            PlanError::validation(
                path,
                format!("role '{}' lacks select permission", session.role),
            )
        })
    }

    fn command_columns(
        &self,
        table: &donat_metadata::QualifiedTable,
        names: &[String],
        path: &str,
    ) -> Result<Vec<CommandColumn>, PlanError> {
        let info = self.catalog_table(table).ok_or_else(|| {
            PlanError::validation(path, "command table is absent from the immutable catalog")
        })?;
        names
            .iter()
            .map(|name| {
                info.column(name).map(command_column).ok_or_else(|| {
                    PlanError::validation(path, format!("unknown command column '{name}'"))
                })
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_command_assignments(
        &self,
        command: &CompiledCommand,
        assignments: &BTreeMap<String, CommandValue>,
        table: &donat_metadata::QualifiedTable,
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        item: Option<&CommandItemContext>,
        path: &str,
    ) -> Result<Vec<CommandAssignment>, PlanError> {
        let info = self.catalog_table(table).ok_or_else(|| {
            PlanError::validation(path, "command table is absent from the immutable catalog")
        })?;
        assignments
            .iter()
            .map(|(name, value)| {
                let column = info.column(name).map(command_column).ok_or_else(|| {
                    PlanError::validation(path, format!("unknown command column '{name}'"))
                })?;
                let value = self.resolve_command_value(
                    command,
                    value,
                    &column,
                    arguments,
                    previous_steps,
                    item,
                    path,
                )?;
                Ok(CommandAssignment { column, value })
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_command_value(
        &self,
        command: &CompiledCommand,
        value: &CommandValue,
        target: &CommandColumn,
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        item: Option<&CommandItemContext>,
        path: &str,
    ) -> Result<CommandExecutionValue, PlanError> {
        match value {
            CommandValue::Argument { arg } => Ok(CommandExecutionValue::Scalar {
                value: command_argument(arguments, arg, path)?,
                pg_type: target.pg_type.clone(),
            }),
            CommandValue::Literal { literal } => Ok(CommandExecutionValue::Scalar {
                value: Scalar::Json(literal.clone()),
                pg_type: target.pg_type.clone(),
            }),
            CommandValue::Step {
                step,
                column: Some(column),
            } => {
                let step = previous_steps.get(step).ok_or_else(|| {
                    PlanError::validation(path, "command step value was not resolved before use")
                })?;
                if step.many {
                    return Err(PlanError::validation(
                        path,
                        "a multi-row command step cannot lower to one scalar value",
                    ));
                }
                let column = step.columns.get(column).cloned().ok_or_else(|| {
                    PlanError::validation(path, "command step column was not resolved before use")
                })?;
                Ok(CommandExecutionValue::StepColumn {
                    cte: step.cte.clone(),
                    column,
                })
            }
            CommandValue::Item { item: field } => {
                let item = item.ok_or_else(|| {
                    PlanError::validation(path, "insert_many item value was not resolved")
                })?;
                let source = item.fields.get(field).ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!("unknown resolved insert_many item field '{field}'"),
                    )
                })?;
                Ok(CommandExecutionValue::Item {
                    field: field.clone(),
                    pg_type: source.pg_type.clone(),
                })
            }
            CommandValue::Rule { rule, bindings } => {
                let expression = self.lower_command_rule_expression(
                    command,
                    rule,
                    bindings,
                    arguments,
                    previous_steps,
                    item,
                    path,
                )?;
                Ok(CommandExecutionValue::Rule {
                    sql: expression.into_sql(),
                    pg_type: target.pg_type.clone(),
                })
            }
            CommandValue::Step { column: None, .. } | CommandValue::SessionVariable { .. } => Err(
                PlanError::validation(path, "command value cannot lower to a scalar target"),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_command_rule(
        &self,
        command: &CompiledCommand,
        name: &str,
        bindings: &BTreeMap<String, CommandValue>,
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        item: Option<&CommandItemContext>,
        diagnostic_path: &str,
        error_path: String,
        message: String,
    ) -> Result<CommandRule, PlanError> {
        let expression = self.lower_command_rule_expression(
            command,
            name,
            bindings,
            arguments,
            previous_steps,
            item,
            diagnostic_path,
        )?;
        Ok(CommandRule {
            sql: expression.into_sql(),
            pg_type: "bool".to_owned(),
            error_path,
            message,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_command_rule_expression(
        &self,
        command: &CompiledCommand,
        name: &str,
        bindings: &BTreeMap<String, CommandValue>,
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        item: Option<&CommandItemContext>,
        path: &str,
    ) -> Result<SqlExpression, PlanError> {
        let rule = command.rules().rule(name).ok_or_else(|| {
            PlanError::validation(path, format!("unknown compiled rule '{name}'"))
        })?;
        let mut sql_bindings = Vec::with_capacity(rule.bindings.len());
        for (binding_name, expected_type) in &rule.bindings {
            let value = bindings.get(binding_name).ok_or_else(|| {
                PlanError::validation(path, format!("compiled rule '{name}' is missing a binding"))
            })?;
            sql_bindings.push((
                binding_name.clone(),
                self.resolve_command_rule_binding(
                    command,
                    value,
                    expected_type,
                    arguments,
                    previous_steps,
                    item,
                    path,
                )?,
            ));
        }
        lower_postgres_expression(rule, &SqlBindings::new(sql_bindings)).map_err(|error| {
            PlanError::validation(
                path,
                format!("cannot lower compiled rule '{name}': {error}"),
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_command_rule_binding(
        &self,
        command: &CompiledCommand,
        value: &CommandValue,
        expected_type: &donat_rules::RuleType,
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        item: Option<&CommandItemContext>,
        path: &str,
    ) -> Result<SqlBinding, PlanError> {
        match value {
            CommandValue::Argument { arg } => Ok(SqlBinding::literal(
                command_argument(arguments, arg, path)?.as_json().clone(),
            )),
            CommandValue::Literal { literal } => Ok(SqlBinding::literal(literal.clone())),
            CommandValue::Step {
                step,
                column: Some(column),
            } => {
                let step = previous_steps.get(step).ok_or_else(|| {
                    PlanError::validation(path, "command rule references an unresolved step")
                })?;
                if step.many {
                    return Err(PlanError::validation(
                        path,
                        "a compiled rule cannot bind a multi-row step column as a scalar",
                    ));
                }
                let column = step.columns.get(column).ok_or_else(|| {
                    PlanError::validation(path, "command rule references an unresolved step column")
                })?;
                Ok(SqlBinding::expression(SqlExpression::scalar_subquery(
                    &step.cte,
                    &column.name,
                    expected_type.clone(),
                )))
            }
            CommandValue::Item { item: field } => {
                let item = item.ok_or_else(|| {
                    PlanError::validation(path, "command rule item binding escaped insert_many")
                })?;
                if !item.fields.contains_key(field) {
                    return Err(PlanError::validation(
                        path,
                        "command rule references an unresolved insert_many item field",
                    ));
                }
                Ok(SqlBinding::expression(SqlExpression::column(
                    "_cmd_item",
                    field,
                    expected_type.clone(),
                )))
            }
            CommandValue::Rule { rule, bindings } => {
                Ok(SqlBinding::expression(self.lower_command_rule_expression(
                    command,
                    rule,
                    bindings,
                    arguments,
                    previous_steps,
                    item,
                    path,
                )?))
            }
            CommandValue::Step { column: None, .. } => Err(PlanError::validation(
                path,
                "compiled rule row bindings require a dedicated typed row expression",
            )),
            CommandValue::SessionVariable { .. } => Err(PlanError::validation(
                path,
                "session variables are not legal compiled Rule bindings",
            )),
        }
    }

    fn resolve_command_result_value(
        &self,
        value: &CommandValue,
        steps: &BTreeMap<String, ResolvedCommandStep>,
        path: &str,
    ) -> Result<CommandResultValue, PlanError> {
        match value {
            CommandValue::Step { step, column: None } => {
                let step = steps.get(step).ok_or_else(|| {
                    PlanError::validation(path, "command result references an unresolved step")
                })?;
                Ok(CommandResultValue::StepRow {
                    cte: step.cte.clone(),
                    many: step.many,
                    columns: step.returning.clone(),
                })
            }
            CommandValue::Step {
                step,
                column: Some(column),
            } => {
                let step = steps.get(step).ok_or_else(|| {
                    PlanError::validation(path, "command result references an unresolved step")
                })?;
                let column = step.columns.get(column).cloned().ok_or_else(|| {
                    PlanError::validation(
                        path,
                        "command result references an unresolved step column",
                    )
                })?;
                Ok(CommandResultValue::StepColumn {
                    cte: step.cte.clone(),
                    column,
                })
            }
            CommandValue::Literal { literal } => Ok(CommandResultValue::Scalar {
                value: Scalar::Json(literal.clone()),
                pg_type: command_result_literal_type(literal).to_owned(),
            }),
            _ => Err(PlanError::validation(
                path,
                "command result did not lower to a declared result producer",
            )),
        }
    }

    fn resolve_command_idempotency(
        &self,
        command: &Command,
        arguments: &BTreeMap<String, Scalar>,
        session: &Session,
        path: &str,
    ) -> Result<Option<CommandIdempotency>, PlanError> {
        let Some(idempotency) = &command.idempotency else {
            return Ok(None);
        };
        let CommandIdempotencyKey::Argument { argument } = &idempotency.key;
        let key = command_argument(arguments, argument, path)?;
        let scope = idempotency
            .scope
            .iter()
            .map(|part| match part {
                CommandIdempotencyScope::Argument { argument } => {
                    command_argument(arguments, argument, path)
                }
                CommandIdempotencyScope::SessionVariable { session_variable } => {
                    let value = session.var(session_variable).ok_or_else(|| {
                        PlanError::new(
                            path,
                            "not-found",
                            format!(
                                "missing session variable: \"{}\"",
                                session_variable.to_ascii_lowercase()
                            ),
                        )
                    })?;
                    Ok(Scalar::Json(Json::String(value.to_owned())))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let input = Scalar::Json(Json::Object(
            arguments
                .iter()
                .map(|(name, value)| (name.clone(), value.as_json().clone()))
                .collect(),
        ));
        Ok(Some(CommandIdempotency {
            key,
            scope,
            input,
            retention_seconds: idempotency
                .retention
                .as_deref()
                .map(command_retention_seconds)
                .transpose()
                .map_err(|message| PlanError::validation(path, message))?,
            error_path: path.to_owned(),
        }))
    }

    fn command_permission_filter(
        &self,
        filter: &Json,
        context: &TableCtx<'a>,
        session: &Session,
        path: &str,
    ) -> Result<Option<BoolExp>, PlanError> {
        if filter.is_null() || filter.as_object().is_some_and(|object| object.is_empty()) {
            return Ok(None);
        }
        let filter_context = self.filter_ctx_of(context);
        self.parse_bool_exp(filter, &filter_context, session, true, path)
            .map(Some)
    }

    fn apply_command_presets(
        &self,
        assignments: &mut Vec<CommandAssignment>,
        presets: &BTreeMap<String, Json>,
        table: &donat_metadata::QualifiedTable,
        session: &Session,
        path: &str,
    ) -> Result<(), PlanError> {
        let info = self.catalog_table(table).ok_or_else(|| {
            PlanError::validation(path, "command table is absent from the immutable catalog")
        })?;
        for (name, value) in presets {
            let column = info.column(name).map(command_column).ok_or_else(|| {
                PlanError::validation(path, "permission preset names an unknown column")
            })?;
            let value = match value {
                Json::String(value) if is_session_var_name(value) => {
                    let value = session.var(value).ok_or_else(|| {
                        PlanError::new(
                            path,
                            "not-found",
                            "missing session variable required by a command permission preset",
                        )
                    })?;
                    Json::String(value.to_owned())
                }
                value => value.clone(),
            };
            let assignment = CommandAssignment {
                value: CommandExecutionValue::Scalar {
                    value: Scalar::Json(value),
                    pg_type: column.pg_type.clone(),
                },
                column,
            };
            if let Some(existing) = assignments
                .iter_mut()
                .find(|existing| existing.column.name == assignment.column.name)
            {
                *existing = assignment;
            } else {
                assignments.push(assignment);
            }
        }
        Ok(())
    }

    fn command_item_fields(
        &self,
        command: &CompiledCommand,
        object: &BTreeMap<String, CommandValue>,
        table: &donat_metadata::QualifiedTable,
        path: &str,
    ) -> Result<BTreeMap<String, CommandColumn>, PlanError> {
        let info = self.catalog_table(table).ok_or_else(|| {
            PlanError::validation(path, "command table is absent from the immutable catalog")
        })?;
        let mut fields = BTreeMap::new();
        // Direct item values establish the concrete catalog type when a field
        // is shared with a Rule binding. Rules may then safely cast that typed
        // derived-row column to their already validated profile type.
        for (target, value) in object {
            let Some(item) = direct_command_item(value) else {
                continue;
            };
            let column = info
                .column(target)
                .map(command_column)
                .ok_or_else(|| PlanError::validation(path, "insert_many item target is unknown"))?;
            match fields.insert(item.to_owned(), column.clone()) {
                Some(existing) if existing.pg_type != column.pg_type => {
                    return Err(PlanError::validation(
                        path,
                        "one insert_many item field cannot target incompatible PostgreSQL types",
                    ));
                }
                _ => {}
            }
        }
        for value in object.values() {
            self.collect_command_rule_item_fields(command, value, &mut fields, path)?;
        }
        Ok(fields)
    }

    fn collect_command_rule_item_fields(
        &self,
        command: &CompiledCommand,
        value: &CommandValue,
        fields: &mut BTreeMap<String, CommandColumn>,
        path: &str,
    ) -> Result<(), PlanError> {
        let CommandValue::Rule { rule, bindings } = value else {
            return Ok(());
        };
        let compiled = command.rules().rule(rule).ok_or_else(|| {
            PlanError::validation(path, format!("unknown compiled rule '{rule}'"))
        })?;
        for (binding, expected_type) in &compiled.bindings {
            let value = bindings.get(binding).ok_or_else(|| {
                PlanError::validation(path, format!("compiled rule '{rule}' is missing a binding"))
            })?;
            self.collect_command_rule_item_binding(command, value, expected_type, fields, path)?;
        }
        Ok(())
    }

    fn collect_command_rule_item_binding(
        &self,
        command: &CompiledCommand,
        value: &CommandValue,
        expected_type: &RuleType,
        fields: &mut BTreeMap<String, CommandColumn>,
        path: &str,
    ) -> Result<(), PlanError> {
        match value {
            CommandValue::Item { item } => {
                let inferred = CommandColumn {
                    name: item.clone(),
                    pg_type: command_rule_item_pg_type(expected_type).to_owned(),
                    nullable: command_rule_item_nullable(expected_type),
                };
                match fields.get(item) {
                    // A direct item assignment uses the concrete target-column
                    // type. The Rules lowerer explicitly casts the trusted
                    // alias reference, so retaining that narrower type is
                    // sound for an assignable Rule binding.
                    Some(existing) if existing.pg_type != inferred.pg_type => {}
                    Some(_) => {}
                    None => {
                        fields.insert(item.clone(), inferred);
                    }
                }
                Ok(())
            }
            CommandValue::Rule { .. } => {
                self.collect_command_rule_item_fields(command, value, fields, path)
            }
            _ => Ok(()),
        }
    }

    fn command_is_permitted(&self, command: &Command, session: &Session) -> bool {
        command
            .permissions
            .iter()
            .any(|permission| permission.role == session.role)
    }

    fn validate_command_runtime_permissions(
        &self,
        command: &Command,
        session: &Session,
        path: &str,
    ) -> Result<(), PlanError> {
        for (index, step) in command.steps.iter().enumerate() {
            let step_path = format!("{path}.steps[{index}]");
            let Some(table) = command_step_table(step) else {
                continue;
            };
            let entry = self.entry_for(table).ok_or_else(|| {
                PlanError::validation(
                    &step_path,
                    format!(
                        "command target '{}.{}' is not tracked",
                        table.schema(),
                        table.name()
                    ),
                )
            })?;
            let info = self.catalog_table(table).ok_or_else(|| {
                PlanError::validation(
                    &step_path,
                    format!(
                        "command target '{}.{}' does not exist in the catalog",
                        table.schema(),
                        table.name()
                    ),
                )
            })?;
            let select = self
                .table_ctx_by_name(table, &session.role)
                .ok_or_else(|| {
                    PlanError::validation(
                        &step_path,
                        format!(
                            "role '{}' lacks select permission on table '{}.{}'",
                            session.role, info.schema, info.name
                        ),
                    )
                })?;
            let require_select = |columns: Vec<&String>| -> Result<(), PlanError> {
                for column in columns {
                    if !select.column_allowed(column) {
                        return Err(PlanError::validation(
                            &step_path,
                            format!(
                                "role '{}' lacks select permission for column '{}' on table '{}.{}'",
                                session.role, column, info.schema, info.name
                            ),
                        ));
                    }
                }
                Ok(())
            };
            match &step.operation {
                CommandStepOperation::SelectOne { select_one } => {
                    require_select(
                        select_one
                            .by
                            .keys()
                            .chain(select_one.returning.iter())
                            .collect(),
                    )?;
                }
                CommandStepOperation::Insert { insert } => {
                    let permission = self
                        .resolve_role_perm(&entry.insert_permissions, &session.role, |permission| {
                            !permission.backend_only || session.backend_request
                        })
                        .ok_or_else(|| {
                            PlanError::validation(
                                &step_path,
                                format!(
                                    "role '{}' lacks insert permission on table '{}.{}'",
                                    session.role, info.schema, info.name
                                ),
                            )
                        })?;
                    require_command_columns(
                        &permission.columns,
                        insert.object.keys(),
                        "insert",
                        &session.role,
                        info,
                        &step_path,
                    )?;
                    require_select(insert.returning.iter().collect())?;
                }
                CommandStepOperation::InsertMany { insert_many } => {
                    let permission = self
                        .resolve_role_perm(&entry.insert_permissions, &session.role, |permission| {
                            !permission.backend_only || session.backend_request
                        })
                        .ok_or_else(|| {
                            PlanError::validation(
                                &step_path,
                                format!(
                                    "role '{}' lacks insert permission on table '{}.{}'",
                                    session.role, info.schema, info.name
                                ),
                            )
                        })?;
                    require_command_columns(
                        &permission.columns,
                        insert_many.object.keys(),
                        "insert",
                        &session.role,
                        info,
                        &step_path,
                    )?;
                    require_select(insert_many.returning.iter().collect())?;
                }
                CommandStepOperation::Update { update } => {
                    let permission = self
                        .resolve_role_perm(&entry.update_permissions, &session.role, |_| true)
                        .ok_or_else(|| {
                            PlanError::validation(
                                &step_path,
                                format!(
                                    "role '{}' lacks update permission on table '{}.{}'",
                                    session.role, info.schema, info.name
                                ),
                            )
                        })?;
                    require_command_columns(
                        &permission.columns,
                        update.set.keys(),
                        "update",
                        &session.role,
                        info,
                        &step_path,
                    )?;
                    require_select(
                        update
                            .predicate
                            .keys()
                            .chain(update.returning.iter())
                            .collect(),
                    )?;
                }
                CommandStepOperation::Delete { delete } => {
                    self.resolve_role_perm(&entry.delete_permissions, &session.role, |_| true)
                        .ok_or_else(|| {
                            PlanError::validation(
                                &step_path,
                                format!(
                                    "role '{}' lacks delete permission on table '{}.{}'",
                                    session.role, info.schema, info.name
                                ),
                            )
                        })?;
                    require_select(
                        delete
                            .predicate
                            .keys()
                            .chain(delete.returning.iter())
                            .collect(),
                    )?;
                }
                CommandStepOperation::Assert { .. } => unreachable!("asserts have no table"),
            }
        }
        Ok(())
    }

    fn parse_command_arguments(
        &self,
        command: &Command,
        field: &GqlField<'static, String>,
        vars: &JsonMap<String, Json>,
        path: &str,
    ) -> Result<BTreeMap<String, Scalar>, PlanError> {
        let mut arguments = BTreeMap::new();
        for (name, value) in &field.arguments {
            let definition = command
                .arguments
                .iter()
                .find(|argument| argument.name == *name)
                .ok_or_else(|| unexpected_arg(path, name))?;
            let value = value_to_json(value, vars, path)?;
            validate_command_argument_value(
                self.metadata(),
                &definition.type_,
                &value,
                &format!("{path}.args.{name}"),
            )?;
            arguments.insert(name.clone(), Scalar::Json(value));
        }
        for definition in &command.arguments {
            if definition.type_.ends_with('!') && !arguments.contains_key(&definition.name) {
                return Err(PlanError::validation(
                    path,
                    format!("missing required field argument: \"{}\"", definition.name),
                ));
            }
        }
        Ok(arguments)
    }

    fn plan_command_selection(
        &self,
        command: &Command,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        path: &str,
    ) -> Result<Vec<CommandResultSelection>, PlanError> {
        let result_type = format!("{}Result", command_pascal_case(&command.name));
        let fields = flatten(&field.selection_set, fragments, vars, Some(&result_type))?;
        if fields.is_empty() {
            return Err(PlanError::validation(
                path,
                format!("missing selection set for type '{result_type}'"),
            ));
        }
        fields
            .iter()
            .map(|selected| {
                let alias = selected
                    .alias
                    .clone()
                    .unwrap_or_else(|| selected.name.clone());
                if selected.name == "__typename" {
                    return Ok(CommandResultSelection::Typename {
                        alias,
                        value: result_type.clone(),
                    });
                }
                let result = command
                    .result
                    .get(&selected.name)
                    .ok_or_else(|| field_not_found(path, &selected.name, &result_type))?;
                match result {
                    CommandValue::Step { step, column: None } => {
                        let step = command
                            .steps
                            .iter()
                            .find(|candidate| candidate.name == *step)
                            .expect("the static compiler retains result steps");
                        let selections = self.plan_command_row_selection(
                            command, step, selected, fragments, vars, path,
                        )?;
                        if command_step_is_many(step) {
                            Ok(CommandResultSelection::List {
                                alias,
                                field: selected.name.clone(),
                                selections,
                            })
                        } else {
                            Ok(CommandResultSelection::Object {
                                alias,
                                field: selected.name.clone(),
                                selections,
                            })
                        }
                    }
                    CommandValue::Step {
                        column: Some(..), ..
                    }
                    | CommandValue::Literal { .. } => {
                        if !selected.selection_set.items.is_empty() {
                            return Err(PlanError::validation(
                                path,
                                format!("field '{}' must not have a selection set", selected.name),
                            ));
                        }
                        Ok(CommandResultSelection::Scalar {
                            alias,
                            field: selected.name.clone(),
                        })
                    }
                    _ => unreachable!("the static command compiler limits result values"),
                }
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_command_row_selection(
        &self,
        command: &Command,
        step: &CommandStep,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        path: &str,
    ) -> Result<Vec<CommandResultSelection>, PlanError> {
        let row_type = format!(
            "{}{}Row",
            command_pascal_case(&command.name),
            command_pascal_case(&step.name)
        );
        let fields = flatten(&field.selection_set, fragments, vars, Some(&row_type))?;
        if fields.is_empty() {
            return Err(PlanError::validation(
                path,
                format!("missing selection set for type '{row_type}'"),
            ));
        }
        fields
            .iter()
            .map(|selected| {
                let alias = selected
                    .alias
                    .clone()
                    .unwrap_or_else(|| selected.name.clone());
                if selected.name == "__typename" {
                    return Ok(CommandResultSelection::Typename {
                        alias,
                        value: row_type.clone(),
                    });
                }
                if !command_step_returning(step)
                    .iter()
                    .any(|column| column == &selected.name)
                {
                    return Err(field_not_found(path, &selected.name, &row_type));
                }
                if !selected.selection_set.items.is_empty() {
                    return Err(PlanError::validation(
                        path,
                        format!("field '{}' must not have a selection set", selected.name),
                    ));
                }
                Ok(CommandResultSelection::Scalar {
                    alias,
                    field: selected.name.clone(),
                })
            })
            .collect()
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
                (_, "on_conflict") if self.capabilities.upsert != UpsertKind::None => {
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
                    if self.capabilities.nested_inserts
                        && let Some(nested) =
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
        if !update_columns.is_empty()
            && let Some(update_perm) = ctx
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
                                format!("missing session variable: \"{}\"", s.to_ascii_lowercase()),
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
                (_, "_append") if self.capabilities.json_ops == JsonOps::Jsonb => {
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

fn command_column(column: &donat_catalog::ColumnInfo) -> CommandColumn {
    CommandColumn {
        name: column.name.clone(),
        pg_type: column.sql_type().to_owned(),
        nullable: column.nullable,
    }
}

fn command_argument(
    arguments: &BTreeMap<String, Scalar>,
    name: &str,
    path: &str,
) -> Result<Scalar, PlanError> {
    arguments.get(name).cloned().ok_or_else(|| {
        PlanError::validation(
            path,
            format!("command argument '{name}' was not resolved before SQL lowering"),
        )
    })
}

fn command_result_literal_type(value: &Json) -> &'static str {
    match value {
        Json::Bool(_) => "bool",
        Json::Number(number) if number.is_i64() || number.is_u64() => "int4",
        Json::Number(_) => "float8",
        Json::String(_) => "text",
        Json::Null | Json::Array(_) | Json::Object(_) => "jsonb",
    }
}

fn direct_command_item(value: &CommandValue) -> Option<&str> {
    match value {
        CommandValue::Item { item } => Some(item),
        _ => None,
    }
}

fn command_rule_item_pg_type(type_: &RuleType) -> &'static str {
    match type_ {
        RuleType::Bool => "boolean",
        RuleType::String | RuleType::Enum { .. } => "text",
        RuleType::Int | RuleType::Decimal => "numeric",
        RuleType::Uuid => "uuid",
        RuleType::Date => "date",
        RuleType::Timestamp => "timestamptz",
        RuleType::List(_) | RuleType::Object { .. } => "jsonb",
        RuleType::Nullable(inner) => command_rule_item_pg_type(inner),
    }
}

fn command_rule_item_nullable(type_: &RuleType) -> bool {
    matches!(type_, RuleType::Nullable(_))
}

fn command_step_table(step: &CommandStep) -> Option<&donat_metadata::QualifiedTable> {
    match &step.operation {
        CommandStepOperation::SelectOne { select_one } => Some(&select_one.table),
        CommandStepOperation::Insert { insert } => Some(&insert.table),
        CommandStepOperation::InsertMany { insert_many } => Some(&insert_many.table),
        CommandStepOperation::Update { update } => Some(&update.table),
        CommandStepOperation::Delete { delete } => Some(&delete.table),
        CommandStepOperation::Assert { .. } => None,
    }
}

fn command_step_returning(step: &CommandStep) -> &[String] {
    match &step.operation {
        CommandStepOperation::SelectOne { select_one } => &select_one.returning,
        CommandStepOperation::Insert { insert } => &insert.returning,
        CommandStepOperation::InsertMany { insert_many } => &insert_many.returning,
        CommandStepOperation::Update { update } => &update.returning,
        CommandStepOperation::Delete { delete } => &delete.returning,
        CommandStepOperation::Assert { .. } => &[],
    }
}

fn command_step_is_many(step: &CommandStep) -> bool {
    matches!(step.operation, CommandStepOperation::InsertMany { .. })
}

fn command_pascal_case(name: &str) -> String {
    let mut output = String::new();
    let mut capitalize = true;
    for character in name.chars() {
        if character == '_' || character == '-' {
            capitalize = true;
        } else if capitalize {
            output.extend(character.to_uppercase());
            capitalize = false;
        } else {
            output.push(character);
        }
    }
    output
}

fn require_command_columns<'a>(
    allowed: &Columns,
    columns: impl IntoIterator<Item = &'a String>,
    operation: &str,
    role: &str,
    table: &donat_catalog::TableInfo,
    path: &str,
) -> Result<(), PlanError> {
    for column in columns {
        let permitted = match allowed {
            Columns::Star => true,
            Columns::List(allowed) => allowed.iter().any(|allowed| allowed == column),
        };
        if !permitted {
            return Err(PlanError::validation(
                path,
                format!(
                    "role '{role}' lacks {operation} permission for column '{column}' on table '{}.{}'",
                    table.schema, table.name
                ),
            ));
        }
    }
    Ok(())
}

fn validate_command_argument_value(
    metadata: &donat_metadata::Metadata,
    type_: &str,
    value: &Json,
    path: &str,
) -> Result<(), PlanError> {
    let (type_, nullable) = match type_.strip_suffix('!') {
        Some(inner) => (inner, false),
        None => (type_, true),
    };
    if value.is_null() {
        return if nullable {
            Ok(())
        } else {
            Err(PlanError::validation(
                path,
                "null is not allowed for a non-null argument",
            ))
        };
    }
    if let Some(inner) = type_
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
    {
        return match value {
            Json::Array(items) => items.iter().enumerate().try_for_each(|(index, item)| {
                validate_command_argument_value(metadata, inner, item, &format!("{path}[{index}]"))
            }),
            item => validate_command_argument_value(metadata, inner, item, path),
        };
    }
    let builtin_valid = match type_ {
        "Boolean" | "bool" => Some(value.is_boolean()),
        "String" | "string" | "ID" | "uuid" | "date" | "timestamp" | "timestamptz" => {
            Some(value.is_string())
        }
        "Int" | "int" => Some(
            value
                .as_i64()
                .is_some_and(|number| (i32::MIN as i64..=i32::MAX as i64).contains(&number))
                || value
                    .as_u64()
                    .is_some_and(|number| number <= i32::MAX as u64),
        ),
        "Float" | "float" | "decimal" => Some(value.is_number()),
        "json" | "jsonb" => Some(true),
        _ => None,
    };
    if let Some(valid) = builtin_valid {
        return if valid {
            Ok(())
        } else {
            Err(PlanError::validation(
                path,
                format!("argument does not match declared type '{type_}'"),
            ))
        };
    }
    if let Some(input) = metadata
        .custom_types
        .input_objects
        .iter()
        .find(|input| input.name == type_)
    {
        let object = value.as_object().ok_or_else(|| {
            PlanError::validation(path, format!("argument must be input object '{type_}'"))
        })?;
        for field in object.keys() {
            if !input
                .fields
                .iter()
                .any(|candidate| candidate.name == *field)
            {
                return Err(PlanError::validation(
                    path,
                    format!("field '{field}' is not declared by input object '{type_}'"),
                ));
            }
        }
        for field in &input.fields {
            match object.get(&field.name) {
                Some(value) => validate_command_argument_value(
                    metadata,
                    &field.type_,
                    value,
                    &format!("{path}.{}", field.name),
                )?,
                None if field.type_.ends_with('!') => {
                    return Err(PlanError::validation(
                        path,
                        format!("missing required input field: \"{}\"", field.name),
                    ));
                }
                None => {}
            }
        }
        return Ok(());
    }
    if let Some(enum_) = metadata
        .custom_types
        .enums
        .iter()
        .find(|enum_| enum_.name == type_)
    {
        let value = value.as_str().ok_or_else(|| {
            PlanError::validation(path, format!("argument must be enum '{type_}'"))
        })?;
        return if enum_
            .values
            .iter()
            .any(|candidate| candidate.value == value)
        {
            Ok(())
        } else {
            Err(PlanError::validation(
                path,
                format!("'{value}' is not a value of enum '{type_}'"),
            ))
        };
    }
    if metadata
        .custom_types
        .scalars
        .iter()
        .any(|scalar| scalar.name == type_)
    {
        return if value.is_array() || value.is_object() {
            Err(PlanError::validation(
                path,
                format!("argument must be a scalar '{type_}'"),
            ))
        } else {
            Ok(())
        };
    }
    Err(PlanError::validation(
        path,
        format!("unknown command argument type '{type_}'"),
    ))
}
