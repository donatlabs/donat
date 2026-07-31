//! Mutation planning (milestone M6): insert/update/delete root fields ->
//! IR, with the role's insert/update/delete permissions applied. As
//! everywhere, there is no admin bypass: the mutation root only exists for
//! a role that has the corresponding permission.

use std::collections::BTreeMap;

use donat_backend::capabilities::{JsonOps, UpsertKind};
use donat_ir::*;
use donat_metadata::{
    Columns, Command, CommandAggregate, CommandCondition as MetadataCommandCondition,
    CommandIdempotencyKey, CommandIdempotencyScope, CommandIdempotencyScopeSpec,
    CommandResultValue as MetadataCommandResultValue, CommandStep, CommandStepOperation,
    CommandValue,
};
use donat_rules::{
    PostgresDecisionHitPolicy, RuleType, SqlBinding, SqlBindings, SqlExpression,
    lower_postgres_decision, lower_postgres_expression,
};
use graphql_parser::query::{Field as GqlField, SelectionSet};
use serde_json::{Map as JsonMap, Value as Json};

use crate::commands::{CompiledCommand, command_retention_seconds};
use crate::plan::{
    Fragments, MutationKind, PlanError, Planner, Session, TableCtx, field_not_found, flatten,
    is_session_var_name, unexpected_arg, value_to_json,
};
use crate::process_effects::{FinalizedCommandEffect, FinalizedCompiledCommand};

struct ResolvedCommandStep {
    cte: String,
    columns: BTreeMap<String, CommandColumn>,
    returning: Vec<CommandColumn>,
    field_rows: BTreeMap<String, Vec<CommandColumn>>,
    many: bool,
    guaranteed_non_empty: bool,
    kind: ResolvedCommandStepKind,
}

struct CommandItemContext {
    fields: BTreeMap<String, CommandColumn>,
    alias: &'static str,
}

struct CommandCurrentContext {
    fields: BTreeMap<String, CommandColumn>,
    alias: &'static str,
}

#[derive(Clone)]
struct ResolvedCommandRowSet {
    cte: String,
    columns: BTreeMap<String, CommandColumn>,
    guaranteed_non_empty: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResolvedCommandStepKind {
    Scalar,
    SelectMany,
    Aggregate,
    UpdateMany,
    ProjectMany,
    FixedRows,
    DecisionMany,
    Allocation,
}

impl ResolvedCommandStepKind {
    fn is_row_set(self) -> bool {
        matches!(
            self,
            Self::SelectMany
                | Self::UpdateMany
                | Self::ProjectMany
                | Self::FixedRows
                | Self::DecisionMany
        )
    }
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
            let finalized_effects = match self.finalized_command_named(&field.name) {
                Some(finalized) => finalized.effects.as_slice(),
                None if command.definition().effects.is_empty() => &[],
                None => {
                    return Err(PlanError::new(
                        path,
                        "unexpected",
                        "command Process effects are absent from the immutable serving snapshot",
                    ));
                }
            };
            let arguments = self.parse_command_arguments(command, field, vars, path)?;
            let selection =
                self.plan_command_selection(command.definition(), field, fragments, vars, path)?;
            self.resolve_command_execution(
                command,
                finalized_effects,
                arguments,
                selection,
                session,
                path,
            )
        })())
    }

    /// Plan one Process-owned Command from an already published finalized
    /// snapshot. This is deliberately not a GraphQL adapter: arguments and
    /// the classic role/session are supplied explicitly, the complete closed
    /// result contract is selected, and no ambient request field can enter.
    pub fn plan_process_command(
        &self,
        expected: &FinalizedCompiledCommand,
        arguments: BTreeMap<String, Json>,
        session: &Session,
        path: &str,
    ) -> Result<CommandMutation, PlanError> {
        let name = expected.command.definition().name.as_str();
        let published = self.finalized_command_named(name).ok_or_else(|| {
            PlanError::new(
                path,
                "unexpected",
                format!("finalized command `{name}` is absent from the serving snapshot"),
            )
        })?;
        if published.command.descriptor().definition_fingerprint
            != expected.command.descriptor().definition_fingerprint
            || published.command.descriptor().source != expected.command.descriptor().source
            || !same_finalized_effect_identities(&published.effects, &expected.effects)
        {
            return Err(PlanError::new(
                path,
                "unexpected",
                format!("finalized command `{name}` does not match the serving snapshot"),
            ));
        }

        let command = &published.command;
        if !self.command_is_permitted(command.definition(), session) {
            return Err(PlanError::validation(
                path,
                format!(
                    "command `{name}` is not executable as role `{}`",
                    session.role
                ),
            ));
        }
        self.validate_command_runtime_permissions(command.definition(), session, path)?;
        validate_closed_process_session(command, session, path)?;

        let mut supplied = arguments;
        let mut coerced = BTreeMap::new();
        for argument in &command.definition().arguments {
            let value = supplied.remove(&argument.name).unwrap_or(Json::Null);
            if value.is_null() && argument.type_.ends_with('!') {
                return Err(PlanError::validation(
                    path,
                    format!("missing required field argument: \"{}\"", argument.name),
                ));
            }
            let value = coerce_command_argument_value(
                self.metadata(),
                command.rules(),
                &argument.type_,
                &value,
                &format!("{path}.arguments.{}", argument.name),
            )?;
            coerced.insert(argument.name.clone(), Scalar::Json(value));
        }
        if let Some(extra) = supplied.keys().next() {
            return Err(PlanError::validation(
                &format!("{path}.arguments.{extra}"),
                format!("unexpected command argument `{extra}`"),
            ));
        }

        let selection = complete_process_command_selection(command.descriptor(), path)?;
        self.resolve_command_execution(
            command,
            &published.effects,
            coerced,
            selection,
            session,
            path,
        )
    }

    fn resolve_command_execution(
        &self,
        command: &CompiledCommand,
        finalized_effects: &[FinalizedCommandEffect],
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
            let argument_rows = self
                .resolve_command_argument_rows(command, step, &cte, &arguments, &step_path, path)?;
            if let Some((input, _)) = &argument_rows {
                steps.push(input.clone());
            }
            let (resolved, output) = self.resolve_command_step(
                command,
                step,
                cte,
                &arguments,
                &resolved_steps,
                argument_rows.as_ref().map(|(_, rows)| rows),
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
                        command,
                        &field.name,
                        &field.value,
                        &arguments,
                        &resolved_steps,
                        path,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, PlanError>>()?;

        let idempotency =
            self.resolve_command_idempotency(command, &arguments, &resolved_steps, session, path)?;
        let effects = self.resolve_finalized_command_effects(
            command,
            finalized_effects,
            &arguments,
            &resolved_steps,
            session,
            path,
        )?;

        Ok(CommandMutation {
            identity: CommandIdentity {
                source: command.source().to_owned(),
                name: definition.name.clone(),
                role: session.role.clone(),
            },
            name: definition.name.clone(),
            steps,
            guards,
            result,
            idempotency,
            effects,
            selection,
        })
    }

    fn resolve_finalized_command_effects(
        &self,
        command: &CompiledCommand,
        effects: &[FinalizedCommandEffect],
        arguments: &BTreeMap<String, Scalar>,
        resolved_steps: &BTreeMap<String, ResolvedCommandStep>,
        session: &Session,
        path: &str,
    ) -> Result<Vec<ResolvedCommandEffect>, PlanError> {
        if effects.len() != command.definition().effects.len() {
            return Err(PlanError::new(
                path,
                "unexpected",
                "finalized command effect count changed after serving snapshot publication",
            ));
        }
        effects
            .iter()
            .enumerate()
            .map(|(index, effect)| {
                let effect_path = format!("{path}.effects[{index}]");
                match effect {
                    FinalizedCommandEffect::Start(effect) => {
                        self.validate_resolved_effect_identity(
                            command,
                            &effect.source,
                            effect.effect_position,
                            index,
                            &effect_path,
                        )?;
                        let caller_role = effect
                            .caller_session_variables
                            .contains_key(&session.role)
                            .then(|| session.role.clone());
                        let caller_session_variables = effect
                            .caller_session_variables
                            .get(&session.role)
                            .into_iter()
                            .flatten()
                            .map(|name| {
                                let value = session.var(name).ok_or_else(|| {
                                    PlanError::new(
                                        &effect_path,
                                        "not-found",
                                        format!(
                                            "missing session variable: \"{}\"",
                                            name.to_ascii_lowercase()
                                        ),
                                    )
                                })?;
                                Ok((
                                    name.clone(),
                                    CommandExecutionValue::Scalar {
                                        value: Scalar::Json(Json::String(value.to_owned())),
                                        pg_type: "text".to_owned(),
                                    },
                                ))
                            })
                            .collect::<Result<BTreeMap<_, _>, PlanError>>()?;
                        Ok(ResolvedCommandEffect::StartProcess(
                            ResolvedStartProcessEffect {
                                source: effect.source.clone(),
                                process_name: effect.process_name.clone(),
                                process_revision: effect.process_revision.clone(),
                                start_policy: effect.start_policy,
                                input: self.resolve_effect_values(
                                    command,
                                    &effect.input,
                                    arguments,
                                    resolved_steps,
                                    session,
                                    &format!("{effect_path}.input"),
                                )?,
                                semantic_idempotency_key: resolve_effect_idempotency_key(
                                    command,
                                    &effect.semantic_idempotency_key,
                                    arguments,
                                    &effect_path,
                                )?,
                                caller_role,
                                caller_session_variables,
                                command_invocation_id: CommandInvocationIdSource::CurrentExecution,
                                effect_position: effect.effect_position,
                            },
                        ))
                    }
                    FinalizedCommandEffect::Signal(effect) => {
                        self.validate_resolved_effect_identity(
                            command,
                            &effect.source,
                            effect.effect_position,
                            index,
                            &effect_path,
                        )?;
                        Ok(ResolvedCommandEffect::SignalProcess(
                            ResolvedSignalProcessEffect {
                                source: effect.source.clone(),
                                process_name: effect.process_name.clone(),
                                process_revision: effect.process_revision.clone(),
                                signal_name: effect.signal_name.clone(),
                                correlation: self.resolve_effect_values(
                                    command,
                                    &effect.correlation,
                                    arguments,
                                    resolved_steps,
                                    session,
                                    &format!("{effect_path}.correlate"),
                                )?,
                                payload: self.resolve_effect_values(
                                    command,
                                    &effect.payload,
                                    arguments,
                                    resolved_steps,
                                    session,
                                    &format!("{effect_path}.payload"),
                                )?,
                                semantic_idempotency_key: resolve_effect_idempotency_key(
                                    command,
                                    &effect.semantic_idempotency_key,
                                    arguments,
                                    &effect_path,
                                )?,
                                command_invocation_id: CommandInvocationIdSource::CurrentExecution,
                                effect_position: effect.effect_position,
                            },
                        ))
                    }
                }
            })
            .collect()
    }

    fn resolve_effect_values(
        &self,
        command: &CompiledCommand,
        values: &BTreeMap<String, CommandValue>,
        arguments: &BTreeMap<String, Scalar>,
        resolved_steps: &BTreeMap<String, ResolvedCommandStep>,
        session: &Session,
        path: &str,
    ) -> Result<BTreeMap<String, CommandExecutionValue>, PlanError> {
        values
            .iter()
            .map(|(name, value)| {
                let value_path = format!("{path}.{name}");
                let target = resolved_value_column(
                    command,
                    name,
                    value,
                    resolved_steps,
                    None,
                    None,
                    &value_path,
                )?;
                let resolved = self.resolve_command_value(
                    command,
                    value,
                    &target,
                    arguments,
                    resolved_steps,
                    None,
                    None,
                    Some(session),
                    &value_path,
                )?;
                Ok((name.clone(), resolved))
            })
            .collect()
    }

    fn validate_resolved_effect_identity(
        &self,
        command: &CompiledCommand,
        source: &str,
        effect_position: u32,
        expected_position: usize,
        path: &str,
    ) -> Result<(), PlanError> {
        if source != command.source() {
            return Err(PlanError::new(
                path,
                "unexpected",
                "finalized command effect crossed its source boundary",
            ));
        }
        if usize::try_from(effect_position).ok() != Some(expected_position) {
            return Err(PlanError::new(
                path,
                "unexpected",
                "finalized command effect position changed after compilation",
            ));
        }
        Ok(())
    }

    fn resolve_command_argument_rows(
        &self,
        command: &CompiledCommand,
        step: &CommandStep,
        cte: &str,
        arguments: &BTreeMap<String, Scalar>,
        path: &str,
        error_path: &str,
    ) -> Result<Option<(CommandExecutionStep, ResolvedCommandRowSet)>, PlanError> {
        let (source, minimum_items, maximum_items) = match &step.operation {
            CommandStepOperation::Aggregate { aggregate } => (
                &aggregate.from,
                aggregate.minimum_items.unwrap_or(0),
                aggregate.maximum_items,
            ),
            CommandStepOperation::UpdateMany { update_many } => (
                &update_many.for_each,
                update_many.minimum_items.unwrap_or(0),
                update_many.maximum_items,
            ),
            _ => return Ok(None),
        };
        let CommandValue::Argument { arg } = source else {
            return Ok(None);
        };
        let maximum_items = maximum_items.ok_or_else(|| {
            PlanError::validation(
                path,
                "command argument row-set bound escaped deployment validation",
            )
        })?;
        let items = command_argument(arguments, arg, path)?;
        let rows = items.as_json().as_array().ok_or_else(|| {
            PlanError::validation(path, "command argument row-set must be a list")
        })?;
        if rows.len() > maximum_items as usize {
            return Err(PlanError::validation(
                path,
                format!("argument row-set exceeds maximum_items {maximum_items}"),
            ));
        }
        if rows.len() < minimum_items as usize {
            return Err(PlanError::validation(
                path,
                format!("argument row-set requires minimum_items {minimum_items}"),
            ));
        }
        if rows.iter().any(|row| !row.is_object()) {
            return Err(PlanError::validation(
                path,
                "command argument row-set items must be objects",
            ));
        }
        let columns = command_argument_row_columns(command, arg, path)?;
        let input_cte = format!("{cte}_input");
        let resolved = ResolvedCommandRowSet {
            cte: input_cte.clone(),
            columns: columns
                .iter()
                .cloned()
                .map(|column| (column.name.clone(), column))
                .collect(),
            guaranteed_non_empty: minimum_items > 0,
        };
        Ok(Some((
            CommandExecutionStep::ArgumentRows {
                name: format!("{}_input", step.name),
                cte: input_cte,
                items,
                columns,
                minimum_items,
                maximum_items,
                error_path: error_path.to_owned(),
            },
            resolved,
        )))
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_command_step(
        &self,
        command: &CompiledCommand,
        step: &CommandStep,
        cte: String,
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        argument_rows: Option<&ResolvedCommandRowSet>,
        session: &Session,
        path: &str,
        error_path: &str,
    ) -> Result<(CommandExecutionStep, ResolvedCommandStep), PlanError> {
        let output = |cte: String, columns: &[CommandColumn], many, kind| ResolvedCommandStep {
            cte,
            columns: columns
                .iter()
                .cloned()
                .map(|column| (column.name.clone(), column))
                .collect(),
            returning: columns.to_vec(),
            field_rows: BTreeMap::new(),
            many,
            guaranteed_non_empty: false,
            kind,
        };
        let guaranteed_output =
            |cte: String, columns: &[CommandColumn], many, kind, guaranteed_non_empty| {
                let mut output = output(cte, columns, many, kind);
                output.guaranteed_non_empty = guaranteed_non_empty;
                output
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
                        field_rows: BTreeMap::new(),
                        many: false,
                        guaranteed_non_empty: false,
                        kind: ResolvedCommandStepKind::Scalar,
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
                    None,
                    session,
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
                Ok((
                    resolved,
                    output(cte, &returning, false, ResolvedCommandStepKind::Scalar),
                ))
            }
            CommandStepOperation::SelectMany { select_many } => {
                let context = self.command_table_context(&select_many.table, session, path)?;
                let returning =
                    self.command_columns(&select_many.table, &select_many.returning, path)?;
                let equality = self.resolve_command_assignments(
                    command,
                    &select_many.by,
                    &select_many.table,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    session,
                    path,
                )?;
                let order_by =
                    self.command_columns(&select_many.table, &select_many.order_by, path)?;
                let filter = self.permission_predicate(&context, session, path)?;
                let resolved = CommandExecutionStep::SelectMany {
                    name: step.name.clone(),
                    cte: cte.clone(),
                    table: Table {
                        schema: context.info.schema.clone(),
                        name: context.info.name.clone(),
                    },
                    equality,
                    order_by,
                    returning: returning.clone(),
                    require_non_empty: select_many.require_non_empty,
                    filter,
                    error_path: error_path.to_owned(),
                };
                Ok((
                    resolved,
                    guaranteed_output(
                        cte,
                        &returning,
                        true,
                        ResolvedCommandStepKind::SelectMany,
                        select_many.require_non_empty,
                    ),
                ))
            }
            CommandStepOperation::Aggregate { aggregate } => {
                let input = self.resolve_any_command_row_set(
                    &aggregate.from,
                    previous_steps,
                    argument_rows,
                    path,
                )?;
                let values = aggregate
                    .values
                    .iter()
                    .map(|(name, aggregate)| {
                        resolve_command_aggregate(name, aggregate, &input, path)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let returning = values
                    .iter()
                    .map(|aggregate| aggregate.output().clone())
                    .collect::<Vec<_>>();
                let resolved = CommandExecutionStep::Aggregate {
                    name: step.name.clone(),
                    cte: cte.clone(),
                    input_cte: input.cte.clone(),
                    values,
                    error_path: error_path.to_owned(),
                };
                Ok((
                    resolved,
                    guaranteed_output(
                        cte,
                        &returning,
                        false,
                        ResolvedCommandStepKind::Aggregate,
                        true,
                    ),
                ))
            }
            CommandStepOperation::Project { project } => {
                let values = self.resolve_command_named_values(
                    command,
                    &project.values,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    path,
                )?;
                let returning = values
                    .iter()
                    .map(|value| value.column.clone())
                    .collect::<Vec<_>>();
                Ok((
                    CommandExecutionStep::Project {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        values,
                        error_path: error_path.to_owned(),
                    },
                    output(cte, &returning, false, ResolvedCommandStepKind::Scalar),
                ))
            }
            CommandStepOperation::ProjectMany { project_many } => {
                let input = self.resolve_any_command_row_set(
                    &project_many.from,
                    previous_steps,
                    None,
                    path,
                )?;
                let current = CommandCurrentContext {
                    fields: input.columns.clone(),
                    alias: "_cmd_input",
                };
                let values = self.resolve_command_named_values(
                    command,
                    &project_many.values,
                    arguments,
                    previous_steps,
                    None,
                    Some(&current),
                    path,
                )?;
                let returning = values
                    .iter()
                    .map(|value| value.column.clone())
                    .collect::<Vec<_>>();
                Ok((
                    CommandExecutionStep::ProjectMany {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        input_cte: input.cte.clone(),
                        maximum_rows: project_many.maximum_rows,
                        values,
                        error_path: error_path.to_owned(),
                    },
                    guaranteed_output(
                        cte,
                        &returning,
                        true,
                        ResolvedCommandStepKind::ProjectMany,
                        input.guaranteed_non_empty,
                    ),
                ))
            }
            CommandStepOperation::FixedRows { fixed_rows } => {
                let first = fixed_rows
                    .rows
                    .first()
                    .expect("the static compiler rejects empty fixed_rows");
                let first_values = self.resolve_command_named_values(
                    command,
                    first,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    path,
                )?;
                let columns = first_values
                    .iter()
                    .map(|value| value.column.clone())
                    .collect::<Vec<_>>();
                let mut rows = vec![first_values.into_iter().map(|value| value.value).collect()];
                for row in fixed_rows.rows.iter().skip(1) {
                    rows.push(self.resolve_fixed_row(
                        command,
                        row,
                        &columns,
                        arguments,
                        previous_steps,
                        path,
                    )?);
                }
                Ok((
                    CommandExecutionStep::FixedRows {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        maximum_rows: fixed_rows.maximum_rows,
                        columns: columns.clone(),
                        rows,
                        error_path: error_path.to_owned(),
                    },
                    guaranteed_output(
                        cte,
                        &columns,
                        true,
                        ResolvedCommandStepKind::FixedRows,
                        true,
                    ),
                ))
            }
            CommandStepOperation::Decision { decision } => {
                let (resolved_decision, input, returning) = self.resolve_command_decision(
                    command,
                    &decision.decision_table,
                    &decision.input,
                    &decision.returning,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    path,
                )?;
                Ok((
                    CommandExecutionStep::Decision {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        decision: resolved_decision,
                        input,
                        returning: returning.clone(),
                        error_path: error_path.to_owned(),
                    },
                    output(cte, &returning, false, ResolvedCommandStepKind::Scalar),
                ))
            }
            CommandStepOperation::DecisionMany { decision_many } => {
                let row_set = self.resolve_any_command_row_set(
                    &decision_many.from,
                    previous_steps,
                    None,
                    path,
                )?;
                let current = CommandCurrentContext {
                    fields: row_set.columns.clone(),
                    alias: "_cmd_input",
                };
                let (resolved_decision, input, returning) = self.resolve_command_decision(
                    command,
                    &decision_many.decision_table,
                    &decision_many.input,
                    &decision_many.returning,
                    arguments,
                    previous_steps,
                    None,
                    Some(&current),
                    path,
                )?;
                let order_by = decision_many
                    .order_by
                    .iter()
                    .map(|name| {
                        returning
                            .iter()
                            .find(|column| column.name == *name)
                            .cloned()
                            .ok_or_else(|| {
                                PlanError::validation(
                                    path,
                                    "decision_many order field was not resolved",
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((
                    CommandExecutionStep::DecisionMany {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        input_cte: row_set.cte.clone(),
                        decision: resolved_decision,
                        input,
                        returning: returning.clone(),
                        order_by,
                        error_path: error_path.to_owned(),
                    },
                    guaranteed_output(
                        cte,
                        &returning,
                        true,
                        ResolvedCommandStepKind::DecisionMany,
                        row_set.guaranteed_non_empty,
                    ),
                ))
            }
            CommandStepOperation::AssertWhen { assert_when } => {
                let condition =
                    resolve_command_condition(&assert_when.when, arguments, command, path)?;
                let rule = self.resolve_command_rule(
                    command,
                    &assert_when.rule,
                    &assert_when.bindings,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    path,
                    error_path.to_owned(),
                    assert_when
                        .message
                        .clone()
                        .unwrap_or_else(|| "conditional command assertion rejected".to_owned()),
                )?;
                Ok((
                    CommandExecutionStep::AssertWhen {
                        name: step.name.clone(),
                        condition,
                        rule,
                    },
                    ResolvedCommandStep {
                        cte,
                        columns: BTreeMap::new(),
                        returning: Vec::new(),
                        field_rows: BTreeMap::new(),
                        many: false,
                        guaranteed_non_empty: false,
                        kind: ResolvedCommandStepKind::Scalar,
                    },
                ))
            }
            CommandStepOperation::Insert { insert } => {
                let context = self.command_table_context(&insert.table, session, path)?;
                let permission = self
                    .resolve_command_role_perm(
                        &context.entry.command_insert_permissions,
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
                    None,
                    session,
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
                Ok((
                    resolved,
                    output(cte, &returning, false, ResolvedCommandStepKind::Scalar),
                ))
            }
            CommandStepOperation::InsertWhen { insert_when } => {
                let condition =
                    resolve_command_condition(&insert_when.when, arguments, command, path)?;
                let context = self.command_table_context(&insert_when.table, session, path)?;
                let permission = self
                    .resolve_command_role_perm(
                        &context.entry.command_insert_permissions,
                        &context.entry.insert_permissions,
                        &session.role,
                        |permission| !permission.backend_only || session.backend_request,
                    )
                    .ok_or_else(|| {
                        PlanError::validation(path, "command insert permission is missing")
                    })?;
                let mut object = self.resolve_command_assignments(
                    command,
                    &insert_when.object,
                    &insert_when.table,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    session,
                    path,
                )?;
                self.apply_command_presets(
                    &mut object,
                    &permission.set,
                    &insert_when.table,
                    session,
                    path,
                )?;
                let returning =
                    self.command_columns(&insert_when.table, &insert_when.returning, path)?;
                let check = self.parse_check_exp(&permission.check, &context, session, path)?;
                Ok((
                    CommandExecutionStep::InsertWhen {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        condition,
                        table: Table {
                            schema: context.info.schema.clone(),
                            name: context.info.name.clone(),
                        },
                        object,
                        returning: returning.clone(),
                        check,
                        error_path: error_path.to_owned(),
                    },
                    output(cte, &returning, false, ResolvedCommandStepKind::Scalar),
                ))
            }
            CommandStepOperation::InsertMany { insert_many } => {
                let context = self.command_table_context(&insert_many.table, session, path)?;
                let permission = self
                    .resolve_command_role_perm(
                        &context.entry.command_insert_permissions,
                        &context.entry.insert_permissions,
                        &session.role,
                        |permission| !permission.backend_only || session.backend_request,
                    )
                    .ok_or_else(|| {
                        PlanError::validation(path, "command insert permission is missing")
                    })?;
                let (items, input_rows, item_fields) = match &insert_many.for_each {
                    CommandValue::Argument { arg } => (
                        Some(command_argument(arguments, arg, path)?),
                        None,
                        self.command_item_fields(
                            command,
                            &insert_many.object,
                            &insert_many.table,
                            path,
                        )?,
                    ),
                    CommandValue::Step { where_nonzero, .. } => {
                        let input = self.resolve_any_command_row_set(
                            &insert_many.for_each,
                            previous_steps,
                            None,
                            path,
                        )?;
                        (
                            None,
                            Some((input.cte, where_nonzero.clone())),
                            input.columns,
                        )
                    }
                    _ => {
                        return Err(PlanError::validation(
                            path,
                            "insert_many for_each did not resolve to a bounded row set",
                        ));
                    }
                };
                let item = CommandItemContext {
                    fields: item_fields,
                    alias: "_cmd_item",
                };
                let mut object = self.resolve_command_assignments(
                    command,
                    &insert_many.object,
                    &insert_many.table,
                    arguments,
                    previous_steps,
                    Some(&item),
                    None,
                    session,
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
                let table = Table {
                    schema: context.info.schema.clone(),
                    name: context.info.name.clone(),
                };
                let item_fields = item.fields.into_values().collect();
                let resolved = match (items, input_rows) {
                    (Some(items), None) => CommandExecutionStep::InsertMany {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        table,
                        items,
                        item_fields,
                        object,
                        returning: returning.clone(),
                        allow_empty: insert_many.allow_empty,
                        check,
                        error_path: error_path.to_owned(),
                    },
                    (None, Some((input_cte, where_nonzero))) => CommandExecutionStep::InsertRows {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        table,
                        input_cte,
                        where_nonzero,
                        item_fields,
                        object,
                        returning: returning.clone(),
                        allow_empty: insert_many.allow_empty,
                        check,
                        error_path: error_path.to_owned(),
                    },
                    _ => unreachable!("insert_many source resolution is closed"),
                };
                Ok((
                    resolved,
                    output(cte, &returning, true, ResolvedCommandStepKind::Scalar),
                ))
            }
            CommandStepOperation::Update { update } => {
                let context = self.command_table_context(&update.table, session, path)?;
                let permission = self
                    .resolve_command_role_perm(
                        &context.entry.command_update_permissions,
                        &context.entry.update_permissions,
                        &session.role,
                        |_| true,
                    )
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
                    None,
                    session,
                    path,
                )?;
                let mut set = self.resolve_command_assignments(
                    command,
                    &update.set,
                    &update.table,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    session,
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
                Ok((
                    resolved,
                    output(cte, &returning, false, ResolvedCommandStepKind::Scalar),
                ))
            }
            CommandStepOperation::UpdateWhen { update_when } => {
                let condition =
                    resolve_command_condition(&update_when.when, arguments, command, path)?;
                let context = self.command_table_context(&update_when.table, session, path)?;
                let permission = self
                    .resolve_command_role_perm(
                        &context.entry.command_update_permissions,
                        &context.entry.update_permissions,
                        &session.role,
                        |_| true,
                    )
                    .ok_or_else(|| {
                        PlanError::validation(path, "command update permission is missing")
                    })?;
                let predicate = self.resolve_command_assignments(
                    command,
                    &update_when.predicate,
                    &update_when.table,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    session,
                    path,
                )?;
                let mut set = self.resolve_command_assignments(
                    command,
                    &update_when.set,
                    &update_when.table,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    session,
                    path,
                )?;
                self.apply_command_presets(
                    &mut set,
                    &permission.set,
                    &update_when.table,
                    session,
                    path,
                )?;
                let returning =
                    self.command_columns(&update_when.table, &update_when.returning, path)?;
                let filter =
                    self.command_permission_filter(&permission.filter, &context, session, path)?;
                let check = match &permission.check {
                    Some(check) => self.parse_check_exp(check, &context, session, path)?,
                    None => None,
                };
                Ok((
                    CommandExecutionStep::UpdateWhen {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        condition,
                        table: Table {
                            schema: context.info.schema.clone(),
                            name: context.info.name.clone(),
                        },
                        predicate,
                        set,
                        returning: returning.clone(),
                        require_affected: update_when.require_affected,
                        filter,
                        check,
                        error_path: error_path.to_owned(),
                    },
                    output(cte, &returning, false, ResolvedCommandStepKind::Scalar),
                ))
            }
            CommandStepOperation::UpdateMany { update_many } => {
                let input = self.resolve_any_command_row_set(
                    &update_many.for_each,
                    previous_steps,
                    argument_rows,
                    path,
                )?;
                let context = self.command_table_context(&update_many.table, session, path)?;
                let permission = self
                    .resolve_command_role_perm(
                        &context.entry.command_update_permissions,
                        &context.entry.update_permissions,
                        &session.role,
                        |_| true,
                    )
                    .ok_or_else(|| {
                        PlanError::validation(path, "command update permission is missing")
                    })?;
                let item = CommandItemContext {
                    fields: input.columns.clone(),
                    alias: "_cmd_input",
                };
                let current = CommandCurrentContext {
                    fields: context
                        .info
                        .columns
                        .iter()
                        .map(|column| (column.name.clone(), command_column(column)))
                        .collect(),
                    alias: "_cmd_target",
                };
                let key_bindings = update_many
                    .by
                    .iter()
                    .filter(|(name, _)| context.info.primary_key.contains(name))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<BTreeMap<_, _>>();
                let guard_bindings = update_many
                    .by
                    .iter()
                    .filter(|(name, _)| !context.info.primary_key.contains(name))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<BTreeMap<_, _>>();
                let primary_key = self.resolve_command_assignments(
                    command,
                    &key_bindings,
                    &update_many.table,
                    arguments,
                    previous_steps,
                    Some(&item),
                    Some(&current),
                    session,
                    path,
                )?;
                let guards = self.resolve_command_assignments(
                    command,
                    &guard_bindings,
                    &update_many.table,
                    arguments,
                    previous_steps,
                    Some(&item),
                    Some(&current),
                    session,
                    path,
                )?;
                let mut assignments = self.resolve_command_assignments(
                    command,
                    &update_many.set,
                    &update_many.table,
                    arguments,
                    previous_steps,
                    Some(&item),
                    Some(&current),
                    session,
                    path,
                )?;
                self.apply_command_presets(
                    &mut assignments,
                    &permission.set,
                    &update_many.table,
                    session,
                    path,
                )?;
                let check = update_many
                    .check
                    .as_ref()
                    .map(|check| {
                        self.resolve_command_rule(
                            command,
                            &check.rule,
                            &check.bindings,
                            arguments,
                            previous_steps,
                            Some(&item),
                            Some(&current),
                            path,
                            error_path.to_owned(),
                            "command update_many check rejected".to_owned(),
                        )
                    })
                    .transpose()?;
                let returning =
                    self.command_columns(&update_many.table, &update_many.returning, path)?;
                let filter =
                    self.command_permission_filter(&permission.filter, &context, session, path)?;
                let permission_check = match &permission.check {
                    Some(check) => self.parse_check_exp(check, &context, session, path)?,
                    None => None,
                };
                let resolved = CommandExecutionStep::UpdateMany {
                    name: step.name.clone(),
                    cte: cte.clone(),
                    table: Table {
                        schema: context.info.schema.clone(),
                        name: context.info.name.clone(),
                    },
                    input_cte: input.cte.clone(),
                    primary_key,
                    guards,
                    assignments,
                    check,
                    returning: returning.clone(),
                    require_each: update_many.require_each,
                    filter,
                    permission_check,
                    error_path: error_path.to_owned(),
                };
                Ok((
                    resolved,
                    output(cte, &returning, true, ResolvedCommandStepKind::UpdateMany),
                ))
            }
            CommandStepOperation::Delete { delete } => {
                let context = self.command_table_context(&delete.table, session, path)?;
                let permission = self
                    .resolve_command_role_perm(
                        &context.entry.command_delete_permissions,
                        &context.entry.delete_permissions,
                        &session.role,
                        |_| true,
                    )
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
                    None,
                    session,
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
                Ok((
                    resolved,
                    output(cte, &returning, false, ResolvedCommandStepKind::Scalar),
                ))
            }
            CommandStepOperation::AllocateMany { allocate_many } => {
                let input = self.resolve_any_command_row_set(
                    &allocate_many.from,
                    previous_steps,
                    None,
                    path,
                )?;
                let required = |name: &str| {
                    input.columns.get(name).cloned().ok_or_else(|| {
                        PlanError::validation(
                            path,
                            format!("allocate_many input is missing required field '{name}'"),
                        )
                    })
                };
                let requested = required(&allocate_many.exact_quantity_columns.requested)?;
                let available = required(&allocate_many.exact_quantity_columns.available)?;
                let allocated = CommandColumn {
                    name: allocate_many.exact_quantity_columns.allocated.clone(),
                    pg_type: requested.pg_type.clone(),
                    logical_type: requested.pg_type.clone(),
                    nullable: false,
                };
                let backordered = CommandColumn {
                    name: allocate_many.exact_quantity_columns.backordered.clone(),
                    pg_type: requested.pg_type.clone(),
                    logical_type: requested.pg_type.clone(),
                    nullable: false,
                };
                let group_key = allocate_many
                    .group_key
                    .iter()
                    .map(|name| required(name))
                    .collect::<Result<Vec<_>, _>>()?;
                required("order_line_id")?;
                let request_column = resolved_value_column(
                    command,
                    "_cmd_request_id",
                    &allocate_many.request_id,
                    previous_steps,
                    None,
                    None,
                    path,
                )?;
                let request_id = self.resolve_command_value(
                    command,
                    &allocate_many.request_id,
                    &request_column,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    None,
                    path,
                )?;
                let output_column = |name: &str| -> Result<CommandColumn, PlanError> {
                    match name {
                        "allocation_id" => Ok(CommandColumn {
                            name: name.to_owned(),
                            pg_type: "uuid".to_owned(),
                            logical_type: "uuid".to_owned(),
                            nullable: false,
                        }),
                        "first_line_sequence" => {
                            let mut column = required("line_sequence")?;
                            column.name = name.to_owned();
                            Ok(column)
                        }
                        "items" => Ok(CommandColumn {
                            name: name.to_owned(),
                            pg_type: "jsonb".to_owned(),
                            logical_type: "jsonb".to_owned(),
                            nullable: false,
                        }),
                        name if name == allocated.name => Ok(allocated.clone()),
                        name if name == backordered.name => Ok(backordered.clone()),
                        name => required(name),
                    }
                };
                let groups = allocate_many
                    .returning
                    .groups
                    .iter()
                    .map(|name| output_column(name))
                    .collect::<Result<Vec<_>, _>>()?;
                let lines = allocate_many
                    .returning
                    .lines
                    .iter()
                    .map(|name| output_column(name))
                    .collect::<Result<Vec<_>, _>>()?;
                let backorders = allocate_many
                    .returning
                    .backorders
                    .iter()
                    .map(|name| output_column(name))
                    .collect::<Result<Vec<_>, _>>()?;
                let order_columns = |names: &[String], columns: &[CommandColumn]| {
                    names
                        .iter()
                        .map(|name| {
                            columns
                                .iter()
                                .find(|column| column.name == *name)
                                .cloned()
                                .ok_or_else(|| {
                                    PlanError::validation(
                                        path,
                                        format!(
                                            "allocate_many order field '{name}' is not returned"
                                        ),
                                    )
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()
                };
                let group_order_by = order_columns(&allocate_many.group_order_by, &groups)?;
                let line_order_by = order_columns(&allocate_many.line_order_by, &lines)?;
                let mut field_rows = BTreeMap::new();
                field_rows.insert("groups".to_owned(), groups.clone());
                field_rows.insert("lines".to_owned(), lines.clone());
                field_rows.insert("backorders".to_owned(), backorders.clone());
                Ok((
                    CommandExecutionStep::AllocateMany {
                        name: step.name.clone(),
                        cte: cte.clone(),
                        input_cte: input.cte,
                        request_id,
                        group_key,
                        requested,
                        available,
                        allocated,
                        backordered,
                        groups,
                        lines,
                        backorders,
                        group_order_by,
                        line_order_by,
                        maximum_rows: 256,
                        error_path: error_path.to_owned(),
                    },
                    ResolvedCommandStep {
                        cte,
                        columns: BTreeMap::new(),
                        returning: Vec::new(),
                        field_rows,
                        many: false,
                        guaranteed_non_empty: false,
                        kind: ResolvedCommandStepKind::Allocation,
                    },
                ))
            }
        }
    }

    fn command_table_context(
        &self,
        table: &donat_metadata::QualifiedTable,
        session: &Session,
        path: &str,
    ) -> Result<TableCtx<'a>, PlanError> {
        self.command_table_ctx_by_name(table, &session.role)
            .ok_or_else(|| {
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

    fn resolve_any_command_row_set(
        &self,
        value: &CommandValue,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        argument_rows: Option<&ResolvedCommandRowSet>,
        path: &str,
    ) -> Result<ResolvedCommandRowSet, PlanError> {
        if matches!(value, CommandValue::Argument { .. }) {
            return argument_rows.cloned().ok_or_else(|| {
                PlanError::validation(
                    path,
                    "command argument row-set input was not resolved before use",
                )
            });
        }
        let CommandValue::Step {
            step,
            column: None,
            field,
            where_nonzero: _,
        } = value
        else {
            return Err(PlanError::validation(
                path,
                "command row-set input did not resolve to a complete prior step",
            ));
        };
        let resolved = previous_steps.get(step).ok_or_else(|| {
            PlanError::validation(path, "command row-set input was not resolved before use")
        })?;
        if let Some(field) = field {
            let returning = resolved.field_rows.get(field).cloned().ok_or_else(|| {
                PlanError::validation(path, "command row-set field was not resolved before use")
            })?;
            return Ok(ResolvedCommandRowSet {
                cte: format!("{}_{}", resolved.cte, field),
                columns: returning
                    .iter()
                    .cloned()
                    .map(|column| (column.name.clone(), column))
                    .collect(),
                guaranteed_non_empty: false,
            });
        }
        if !resolved.kind.is_row_set() {
            return Err(PlanError::validation(
                path,
                "command row-set input has an invalid resolved producer",
            ));
        }
        Ok(ResolvedCommandRowSet {
            cte: resolved.cte.clone(),
            columns: resolved.columns.clone(),
            guaranteed_non_empty: resolved.guaranteed_non_empty,
        })
    }

    fn resolve_command_named_values(
        &self,
        command: &CompiledCommand,
        values: &BTreeMap<String, CommandValue>,
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        item: Option<&CommandItemContext>,
        current: Option<&CommandCurrentContext>,
        path: &str,
    ) -> Result<Vec<CommandNamedValue>, PlanError> {
        values
            .iter()
            .map(|(name, value)| {
                let column = resolved_value_column(
                    command,
                    name,
                    value,
                    previous_steps,
                    item,
                    current,
                    path,
                )?;
                let value = self.resolve_command_value(
                    command,
                    value,
                    &column,
                    arguments,
                    previous_steps,
                    item,
                    current,
                    None,
                    path,
                )?;
                Ok(CommandNamedValue {
                    name: name.clone(),
                    column,
                    value,
                })
            })
            .collect()
    }

    fn resolve_fixed_row(
        &self,
        command: &CompiledCommand,
        row: &BTreeMap<String, CommandValue>,
        columns: &[CommandColumn],
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        path: &str,
    ) -> Result<Vec<CommandExecutionValue>, PlanError> {
        columns
            .iter()
            .map(|column| {
                self.resolve_command_value(
                    command,
                    row.get(&column.name)
                        .expect("the static compiler checks fixed row fields"),
                    column,
                    arguments,
                    previous_steps,
                    None,
                    None,
                    None,
                    path,
                )
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_command_decision(
        &self,
        command: &CompiledCommand,
        table_name: &str,
        inputs: &BTreeMap<String, CommandValue>,
        returning_names: &[String],
        arguments: &BTreeMap<String, Scalar>,
        previous_steps: &BTreeMap<String, ResolvedCommandStep>,
        item: Option<&CommandItemContext>,
        current: Option<&CommandCurrentContext>,
        path: &str,
    ) -> Result<(CommandDecision, Vec<CommandNamedValue>, Vec<CommandColumn>), PlanError> {
        let table = command.rules().decision_table(table_name).ok_or_else(|| {
            PlanError::validation(path, format!("unknown compiled decision '{table_name}'"))
        })?;
        let input = table
            .input_types()
            .map(|(name, type_)| {
                let column = CommandColumn {
                    name: name.clone(),
                    pg_type: command_rule_pg_type(type_).to_owned(),
                    logical_type: command_rule_pg_type(type_).to_owned(),
                    nullable: matches!(type_, RuleType::Nullable(_)),
                };
                let value = self.resolve_command_value(
                    command,
                    inputs
                        .get(name)
                        .expect("the static compiler checks decision inputs"),
                    &column,
                    arguments,
                    previous_steps,
                    item,
                    current,
                    None,
                    path,
                )?;
                Ok(CommandNamedValue {
                    name: name.clone(),
                    column,
                    value,
                })
            })
            .collect::<Result<Vec<_>, PlanError>>()?;
        let decision_bindings = SqlBindings::new(table.input_types().map(|(name, type_)| {
            (
                name.clone(),
                SqlBinding::expression(SqlExpression::column(
                    "_cmd_decision_input",
                    name,
                    type_.clone(),
                )),
            )
        }));
        let program = lower_postgres_decision(table, &decision_bindings)
            .map_err(|error| PlanError::validation(path, error.to_string()))?;
        let returning = returning_names
            .iter()
            .map(|name| {
                if let Some(output) = table.output_field(name) {
                    return Ok(CommandColumn {
                        name: name.clone(),
                        pg_type: command_rule_pg_type(&output.type_).to_owned(),
                        logical_type: command_rule_pg_type(&output.type_).to_owned(),
                        nullable: matches!(output.type_, RuleType::Nullable(_)),
                    });
                }
                current
                    .map(|current| &current.fields)
                    .or_else(|| item.map(|item| &item.fields))
                    .and_then(|fields| fields.get(name))
                    .cloned()
                    .ok_or_else(|| {
                        PlanError::validation(
                            path,
                            format!("decision returning field '{name}' was not resolved"),
                        )
                    })
            })
            .collect::<Result<Vec<_>, PlanError>>()?;
        Ok((
            CommandDecision {
                name: program.name,
                revision: program.revision,
                hit_policy: match program.hit_policy {
                    PostgresDecisionHitPolicy::First => CommandDecisionHitPolicy::First,
                    PostgresDecisionHitPolicy::Unique => CommandDecisionHitPolicy::Unique,
                },
                rows: program
                    .rows
                    .into_iter()
                    .map(|row| CommandDecisionRow {
                        id: row.id,
                        condition_sql: row.condition_sql,
                        output: row
                            .output
                            .into_iter()
                            .map(|(name, output)| CommandDecisionOutput {
                                column: CommandColumn {
                                    name: name.clone(),
                                    pg_type: command_rule_pg_type(&output.type_).to_owned(),
                                    logical_type: command_rule_pg_type(&output.type_).to_owned(),
                                    nullable: matches!(output.type_, RuleType::Nullable(_)),
                                },
                                name,
                                sql: output.sql,
                            })
                            .collect(),
                    })
                    .collect(),
            },
            input,
            returning,
        ))
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
        current: Option<&CommandCurrentContext>,
        session: &Session,
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
                    current,
                    Some(session),
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
        current: Option<&CommandCurrentContext>,
        session: Option<&Session>,
        path: &str,
    ) -> Result<CommandExecutionValue, PlanError> {
        match value {
            CommandValue::Argument { arg } => Ok(CommandExecutionValue::Scalar {
                value: command_argument(arguments, arg, path)?,
                pg_type: target.pg_type.clone(),
            }),
            CommandValue::Literal { literal, .. } => Ok(CommandExecutionValue::Scalar {
                value: Scalar::Json(literal.clone()),
                pg_type: target.pg_type.clone(),
            }),
            CommandValue::SessionVariable { session_variable } => {
                let session = session.ok_or_else(|| {
                    PlanError::validation(path, "session variable escaped its typed command target")
                })?;
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
                Ok(CommandExecutionValue::Scalar {
                    value: Scalar::Json(Json::String(value.to_owned())),
                    pg_type: target.pg_type.clone(),
                })
            }
            CommandValue::Step {
                step,
                column: Some(column),
                field: None,
                where_nonzero: None,
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
            CommandValue::CurrentColumn { current_column } => {
                let current = current.ok_or_else(|| {
                    PlanError::validation(
                        path,
                        "current_column escaped its resolved update_many scope",
                    )
                })?;
                let column = current.fields.get(current_column).cloned().ok_or_else(|| {
                    PlanError::validation(path, "current_column was not resolved before SQLgen")
                })?;
                Ok(CommandExecutionValue::CurrentColumn { column })
            }
            CommandValue::Rule { rule, bindings } => {
                let expression = self.lower_command_rule_expression(
                    command,
                    rule,
                    bindings,
                    arguments,
                    previous_steps,
                    item,
                    current,
                    path,
                )?;
                Ok(CommandExecutionValue::Rule {
                    sql: expression.into_sql(),
                    pg_type: target.pg_type.clone(),
                })
            }
            CommandValue::DatabaseTime { database_time } if database_time == "now" => {
                Ok(CommandExecutionValue::DatabaseTime {
                    function: CommandDatabaseTime::Now,
                    pg_type: target.pg_type.clone(),
                })
            }
            CommandValue::Step { .. } | CommandValue::DatabaseTime { .. } => Err(
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
        current: Option<&CommandCurrentContext>,
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
            current,
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
        current: Option<&CommandCurrentContext>,
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
                    current,
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
        current: Option<&CommandCurrentContext>,
        path: &str,
    ) -> Result<SqlBinding, PlanError> {
        match value {
            CommandValue::Argument { arg } => Ok(SqlBinding::literal(
                command_argument(arguments, arg, path)?.as_json().clone(),
            )),
            CommandValue::Literal { literal, .. } => Ok(SqlBinding::literal(literal.clone())),
            CommandValue::Step {
                step,
                column: Some(column),
                field: None,
                where_nonzero: None,
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
                    item.alias,
                    field,
                    expected_type.clone(),
                )))
            }
            CommandValue::CurrentColumn { current_column } => {
                let current = current.ok_or_else(|| {
                    PlanError::validation(path, "command rule current_column escaped update_many")
                })?;
                if !current.fields.contains_key(current_column) {
                    return Err(PlanError::validation(
                        path,
                        "command rule references an unresolved current column",
                    ));
                }
                Ok(SqlBinding::expression(SqlExpression::column(
                    current.alias,
                    current_column,
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
                    current,
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
            CommandValue::Step { .. } | CommandValue::DatabaseTime { .. } => Err(
                PlanError::validation(path, "value is not legal in a compiled Rule binding"),
            ),
        }
    }

    fn resolve_command_result_value(
        &self,
        command: &CompiledCommand,
        result_name: &str,
        value: &MetadataCommandResultValue,
        arguments: &BTreeMap<String, Scalar>,
        steps: &BTreeMap<String, ResolvedCommandStep>,
        path: &str,
    ) -> Result<CommandResultValue, PlanError> {
        match value {
            MetadataCommandResultValue::Step {
                step,
                column: None,
                field: None,
                maximum_items: None,
                ..
            } => {
                let step = steps.get(step).ok_or_else(|| {
                    PlanError::validation(path, "command result references an unresolved step")
                })?;
                Ok(CommandResultValue::StepRow {
                    cte: step.cte.clone(),
                    many: step.many,
                    columns: step.returning.clone(),
                })
            }
            MetadataCommandResultValue::Step {
                step,
                column: Some(column),
                field: None,
                ..
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
            MetadataCommandResultValue::Step {
                step,
                column: None,
                field: Some(field),
                maximum_items,
                ..
            } => {
                let step = steps.get(step).ok_or_else(|| {
                    PlanError::validation(path, "command result references an unresolved step")
                })?;
                let columns = step.field_rows.get(field).cloned().ok_or_else(|| {
                    PlanError::validation(
                        path,
                        "command result references an unresolved row-set field",
                    )
                })?;
                Ok(CommandResultValue::ProjectedRows {
                    cte: format!("{}_{}", step.cte, field),
                    many: true,
                    columns: columns
                        .into_iter()
                        .map(|source| CommandResultProjection {
                            name: source.name.clone(),
                            source,
                        })
                        .collect(),
                    maximum_items: maximum_items.unwrap_or(256),
                })
            }
            MetadataCommandResultValue::Step {
                step,
                column: None,
                field: None,
                maximum_items: Some(maximum_items),
                ..
            } => {
                let step = steps.get(step).ok_or_else(|| {
                    PlanError::validation(path, "command result references an unresolved step")
                })?;
                Ok(CommandResultValue::ProjectedRows {
                    cte: step.cte.clone(),
                    many: step.many,
                    columns: step
                        .returning
                        .iter()
                        .cloned()
                        .map(|source| CommandResultProjection {
                            name: source.name.clone(),
                            source,
                        })
                        .collect(),
                    maximum_items: *maximum_items,
                })
            }
            MetadataCommandResultValue::ProjectedStep {
                step,
                project,
                maximum_items,
            } => {
                let step = steps.get(step).ok_or_else(|| {
                    PlanError::validation(path, "command result references an unresolved step")
                })?;
                let columns = project
                    .iter()
                    .map(|(alias, source)| {
                        let source = step.columns.get(source).cloned().ok_or_else(|| {
                            PlanError::validation(
                                path,
                                "projected command result references an unresolved field",
                            )
                        })?;
                        Ok(CommandResultProjection {
                            name: alias.clone(),
                            source,
                        })
                    })
                    .collect::<Result<Vec<_>, PlanError>>()?;
                Ok(CommandResultValue::ProjectedRows {
                    cte: step.cte.clone(),
                    many: step.many,
                    columns,
                    maximum_items: *maximum_items,
                })
            }
            MetadataCommandResultValue::Argument { arg } => Ok(CommandResultValue::Scalar {
                value: command_argument(arguments, arg, path)?,
                pg_type: command_argument_pg_type(command.definition(), arg, path)?.to_owned(),
            }),
            MetadataCommandResultValue::Literal { literal, .. } => {
                let contract = command
                    .descriptor()
                    .result
                    .roots
                    .get(result_name)
                    .ok_or_else(|| {
                        PlanError::validation(
                            path,
                            format!("command result contract has no field '{result_name}'"),
                        )
                    })?;
                Ok(CommandResultValue::Scalar {
                    value: Scalar::Json(literal.clone()),
                    pg_type: command_contract_pg_type(&contract.type_ref),
                })
            }
            MetadataCommandResultValue::Rule { rule, bindings } => {
                let compiled = command.rules().rule(rule).ok_or_else(|| {
                    PlanError::validation(path, format!("unknown compiled rule '{rule}'"))
                })?;
                let expression = self.lower_command_rule_expression(
                    command, rule, bindings, arguments, steps, None, None, path,
                )?;
                Ok(CommandResultValue::Rule {
                    sql: expression.into_sql(),
                    pg_type: command_rule_pg_type(&compiled.result).to_owned(),
                })
            }
            MetadataCommandResultValue::Array(values) => Ok(CommandResultValue::Array {
                value: Scalar::Json(Json::Array(values.clone())),
                maximum_items: u32::try_from(values.len()).unwrap_or(u32::MAX),
            }),
            MetadataCommandResultValue::SessionVariable { .. }
            | MetadataCommandResultValue::CurrentColumn { .. }
            | MetadataCommandResultValue::Step {
                column: Some(_),
                field: Some(_),
                ..
            } => Err(PlanError::validation(
                path,
                "command result did not lower to a declared result producer",
            )),
        }
    }

    fn resolve_command_idempotency(
        &self,
        command: &CompiledCommand,
        arguments: &BTreeMap<String, Scalar>,
        steps: &BTreeMap<String, ResolvedCommandStep>,
        session: &Session,
        path: &str,
    ) -> Result<Option<CommandIdempotency>, PlanError> {
        let definition = command.definition();
        let Some(idempotency) = &definition.idempotency else {
            return Ok(None);
        };
        let CommandIdempotencyKey::Argument { argument } = &idempotency.key;
        let key = command_argument(arguments, argument, path)?;
        let scope = match &idempotency.scope {
            CommandIdempotencyScopeSpec::Command(_) => Vec::new(),
            CommandIdempotencyScopeSpec::Values(parts) => parts
                .iter()
                .map(|part| match part {
                    CommandIdempotencyScope::Argument { argument } => {
                        Ok(CommandExecutionValue::Scalar {
                            value: command_argument(arguments, argument, path)?,
                            pg_type: command_argument_pg_type(definition, argument, path)?
                                .to_owned(),
                        })
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
                        Ok(CommandExecutionValue::Scalar {
                            value: Scalar::Json(Json::String(value.to_owned())),
                            pg_type: command
                                .descriptor()
                                .required_session_variables
                                .get(&session.role)
                                .and_then(|required| {
                                    required.get(&session_variable.to_ascii_lowercase())
                                })
                                .map(session_contract_pg_type)
                                .transpose()?
                                .unwrap_or_else(|| "text".to_owned()),
                        })
                    }
                    CommandIdempotencyScope::Step { step, column } => {
                        let step = steps.get(step).ok_or_else(|| {
                            PlanError::validation(path, "idempotency scope step was not resolved")
                        })?;
                        let column = step.columns.get(column).cloned().ok_or_else(|| {
                            PlanError::validation(
                                path,
                                "idempotency scope step field was not resolved",
                            )
                        })?;
                        Ok(CommandExecutionValue::StepColumn {
                            cte: step.cte.clone(),
                            column,
                        })
                    }
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
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
                    logical_type: command_rule_item_pg_type(expected_type).to_owned(),
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

    pub(crate) fn command_is_permitted(&self, command: &Command, session: &Session) -> bool {
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
                .command_table_ctx_by_name(table, &session.role)
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
                CommandStepOperation::SelectMany { select_many } => {
                    require_select(
                        select_many
                            .by
                            .keys()
                            .chain(select_many.order_by.iter())
                            .chain(select_many.returning.iter())
                            .collect(),
                    )?;
                }
                CommandStepOperation::Insert { insert } => {
                    let permission = self
                        .resolve_command_role_perm(
                            &entry.command_insert_permissions,
                            &entry.insert_permissions,
                            &session.role,
                            |permission| !permission.backend_only || session.backend_request,
                        )
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
                        .resolve_command_role_perm(
                            &entry.command_insert_permissions,
                            &entry.insert_permissions,
                            &session.role,
                            |permission| !permission.backend_only || session.backend_request,
                        )
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
                CommandStepOperation::InsertWhen { insert_when } => {
                    let permission = self
                        .resolve_command_role_perm(
                            &entry.command_insert_permissions,
                            &entry.insert_permissions,
                            &session.role,
                            |permission| !permission.backend_only || session.backend_request,
                        )
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
                        insert_when.object.keys(),
                        "insert",
                        &session.role,
                        info,
                        &step_path,
                    )?;
                    require_select(insert_when.returning.iter().collect())?;
                }
                CommandStepOperation::Update { update } => {
                    let permission = self
                        .resolve_command_role_perm(
                            &entry.command_update_permissions,
                            &entry.update_permissions,
                            &session.role,
                            |_| true,
                        )
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
                CommandStepOperation::UpdateMany { update_many } => {
                    let permission = self
                        .resolve_command_role_perm(
                            &entry.command_update_permissions,
                            &entry.update_permissions,
                            &session.role,
                            |_| true,
                        )
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
                        update_many.set.keys(),
                        "update",
                        &session.role,
                        info,
                        &step_path,
                    )?;
                    let mut read_columns = update_many
                        .by
                        .keys()
                        .chain(update_many.returning.iter())
                        .collect::<Vec<_>>();
                    collect_runtime_current_columns(update_many.set.values(), &mut read_columns);
                    if let Some(check) = &update_many.check {
                        collect_runtime_current_columns(check.bindings.values(), &mut read_columns);
                    }
                    require_select(read_columns)?;
                }
                CommandStepOperation::UpdateWhen { update_when } => {
                    let permission = self
                        .resolve_command_role_perm(
                            &entry.command_update_permissions,
                            &entry.update_permissions,
                            &session.role,
                            |_| true,
                        )
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
                        update_when.set.keys(),
                        "update",
                        &session.role,
                        info,
                        &step_path,
                    )?;
                    require_select(
                        update_when
                            .predicate
                            .keys()
                            .chain(update_when.returning.iter())
                            .collect(),
                    )?;
                }
                CommandStepOperation::Delete { delete } => {
                    self.resolve_command_role_perm(
                        &entry.command_delete_permissions,
                        &entry.delete_permissions,
                        &session.role,
                        |_| true,
                    )
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
                CommandStepOperation::Aggregate { .. }
                | CommandStepOperation::Assert { .. }
                | CommandStepOperation::AssertWhen { .. }
                | CommandStepOperation::Decision { .. }
                | CommandStepOperation::DecisionMany { .. }
                | CommandStepOperation::Project { .. }
                | CommandStepOperation::ProjectMany { .. }
                | CommandStepOperation::FixedRows { .. }
                | CommandStepOperation::AllocateMany { .. } => {
                    unreachable!("table-free steps were skipped")
                }
            }
        }
        Ok(())
    }

    fn parse_command_arguments(
        &self,
        command: &CompiledCommand,
        field: &GqlField<'static, String>,
        vars: &JsonMap<String, Json>,
        path: &str,
    ) -> Result<BTreeMap<String, Scalar>, PlanError> {
        let definition = command.definition();
        let mut arguments = BTreeMap::new();
        for (name, value) in &field.arguments {
            let argument = definition
                .arguments
                .iter()
                .find(|argument| argument.name == *name)
                .ok_or_else(|| unexpected_arg(path, name))?;
            let value = value_to_json(value, vars, path)?;
            let value = coerce_command_argument_value(
                self.metadata(),
                command.rules(),
                &argument.type_,
                &value,
                &format!("{path}.args.{name}"),
            )?;
            arguments.insert(name.clone(), Scalar::Json(value));
        }
        for argument in &definition.arguments {
            if !arguments.contains_key(&argument.name) {
                if argument.type_.ends_with('!') {
                    return Err(PlanError::validation(
                        path,
                        format!("missing required field argument: \"{}\"", argument.name),
                    ));
                }
                arguments.insert(argument.name.clone(), Scalar::Json(Json::Null));
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
                    MetadataCommandResultValue::Step {
                        step,
                        column: None,
                        field: None,
                        as_,
                        ..
                    } => {
                        let step = command
                            .steps
                            .iter()
                            .find(|candidate| candidate.name == *step)
                            .expect("the static compiler retains result steps");
                        let selections = self.plan_command_row_selection(
                            command,
                            step,
                            None,
                            as_.as_deref(),
                            selected,
                            fragments,
                            vars,
                            path,
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
                    MetadataCommandResultValue::ProjectedStep { step, project, .. } => {
                        let step = command
                            .steps
                            .iter()
                            .find(|candidate| candidate.name == *step)
                            .expect("the static compiler retains result steps");
                        let selections = self.plan_command_row_selection(
                            command,
                            step,
                            Some(project),
                            None,
                            selected,
                            fragments,
                            vars,
                            path,
                        )?;
                        Ok(CommandResultSelection::List {
                            alias,
                            field: selected.name.clone(),
                            selections,
                        })
                    }
                    MetadataCommandResultValue::Step {
                        step,
                        column: None,
                        field: Some(row_field),
                        as_,
                        ..
                    } => {
                        let step = command
                            .steps
                            .iter()
                            .find(|candidate| candidate.name == *step)
                            .expect("the static compiler retains result steps");
                        let CommandStepOperation::AllocateMany { allocate_many } = &step.operation
                        else {
                            return Err(PlanError::validation(
                                path,
                                "only allocate_many exposes named row-set fields",
                            ));
                        };
                        let returning = match row_field.as_str() {
                            "groups" => &allocate_many.returning.groups,
                            "lines" => &allocate_many.returning.lines,
                            "backorders" => &allocate_many.returning.backorders,
                            _ => {
                                return Err(PlanError::validation(
                                    path,
                                    "allocate_many result references an unknown row-set field",
                                ));
                            }
                        };
                        let projection = returning
                            .iter()
                            .map(|name| (name.clone(), name.clone()))
                            .collect::<BTreeMap<_, _>>();
                        let selections = self.plan_command_row_selection(
                            command,
                            step,
                            Some(&projection),
                            as_.as_deref(),
                            selected,
                            fragments,
                            vars,
                            path,
                        )?;
                        Ok(CommandResultSelection::List {
                            alias,
                            field: selected.name.clone(),
                            selections,
                        })
                    }
                    MetadataCommandResultValue::Step {
                        column: Some(..), ..
                    }
                    | MetadataCommandResultValue::Literal { .. }
                    | MetadataCommandResultValue::Argument { .. }
                    | MetadataCommandResultValue::Rule { .. }
                    | MetadataCommandResultValue::Array(_) => {
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
                    MetadataCommandResultValue::SessionVariable { .. }
                    | MetadataCommandResultValue::CurrentColumn { .. } => {
                        unreachable!("the static command compiler limits result values")
                    }
                }
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_command_row_selection(
        &self,
        command: &Command,
        step: &CommandStep,
        projection: Option<&BTreeMap<String, String>>,
        type_override: Option<&str>,
        field: &GqlField<'static, String>,
        fragments: &Fragments,
        vars: &JsonMap<String, Json>,
        path: &str,
    ) -> Result<Vec<CommandResultSelection>, PlanError> {
        let row_type = type_override.map(str::to_owned).unwrap_or_else(|| {
            format!(
                "{}{}Row",
                command_pascal_case(&command.name),
                command_pascal_case(&step.name)
            )
        });
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
                if projection.is_some_and(|projection| !projection.contains_key(&selected.name))
                    || projection.is_none() && !command_step_exposes(step, &selected.name)
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

fn validate_closed_process_session(
    command: &CompiledCommand,
    session: &Session,
    path: &str,
) -> Result<(), PlanError> {
    let required = command
        .descriptor()
        .required_session_variables
        .get(&session.role)
        .map(|variables| {
            variables
                .keys()
                .map(|name| name.to_ascii_lowercase())
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    let actual = session
        .vars
        .keys()
        .map(|name| name.to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    if actual != required {
        let missing = required.difference(&actual).cloned().collect::<Vec<_>>();
        let extra = actual.difference(&required).cloned().collect::<Vec<_>>();
        return Err(PlanError::validation(
            path,
            format!(
                "Process command session variables are not closed (missing: [{}], extra: [{}])",
                missing.join(", "),
                extra.join(", ")
            ),
        ));
    }
    Ok(())
}

fn same_finalized_effect_identities(
    left: &[FinalizedCommandEffect],
    right: &[FinalizedCommandEffect],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (FinalizedCommandEffect::Start(left), FinalizedCommandEffect::Start(right)) => {
                    left.source == right.source
                        && left.process_name == right.process_name
                        && left.process_revision == right.process_revision
                        && left.start_policy == right.start_policy
                        && left.effect_position == right.effect_position
                }
                (FinalizedCommandEffect::Signal(left), FinalizedCommandEffect::Signal(right)) => {
                    left.source == right.source
                        && left.process_name == right.process_name
                        && left.process_revision == right.process_revision
                        && left.signal_name == right.signal_name
                        && left.compatible_revisions == right.compatible_revisions
                        && left.effect_position == right.effect_position
                }
                _ => false,
            })
}

fn complete_process_command_selection(
    descriptor: &crate::commands::CommandDescriptor,
    path: &str,
) -> Result<Vec<CommandResultSelection>, PlanError> {
    descriptor
        .result
        .roots
        .iter()
        .map(|(name, field)| {
            complete_process_command_field(name, &field.type_ref, &descriptor.result, path, 0)
        })
        .collect()
}

fn complete_process_command_field(
    name: &str,
    type_ref: &TypeRef,
    catalog: &ValueContractCatalog,
    path: &str,
    depth: usize,
) -> Result<CommandResultSelection, PlanError> {
    if depth > 64 {
        return Err(PlanError::new(
            path,
            "unexpected",
            "command result contract recursion escaped deployment validation",
        ));
    }
    let object_fields = match &type_ref.value_type {
        ValueType::Object { fields } => Some(fields),
        ValueType::Ref { name } => Some(
            &catalog
                .named_objects
                .get(name)
                .ok_or_else(|| {
                    PlanError::new(
                        path,
                        "unexpected",
                        format!("command result references absent object contract `{name}`"),
                    )
                })?
                .fields,
        ),
        _ => None,
    };
    if let Some(fields) = object_fields {
        let selections = fields
            .iter()
            .map(|(field_name, field)| {
                complete_process_command_field(
                    field_name,
                    &field.type_ref,
                    catalog,
                    path,
                    depth + 1,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(CommandResultSelection::Object {
            alias: name.to_owned(),
            field: name.to_owned(),
            selections,
        });
    }
    if let ValueType::List { element } = &type_ref.value_type {
        let element_object = match &element.value_type {
            ValueType::Object { fields } => Some(fields),
            ValueType::Ref { name } => Some(
                &catalog
                    .named_objects
                    .get(name)
                    .ok_or_else(|| {
                        PlanError::new(
                            path,
                            "unexpected",
                            format!("command result references absent object contract `{name}`"),
                        )
                    })?
                    .fields,
            ),
            _ => None,
        };
        if let Some(fields) = element_object {
            let selections = fields
                .iter()
                .map(|(field_name, field)| {
                    complete_process_command_field(
                        field_name,
                        &field.type_ref,
                        catalog,
                        path,
                        depth + 1,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(CommandResultSelection::List {
                alias: name.to_owned(),
                field: name.to_owned(),
                selections,
            });
        }
    }
    Ok(CommandResultSelection::Scalar {
        alias: name.to_owned(),
        field: name.to_owned(),
    })
}

fn command_column(column: &donat_catalog::ColumnInfo) -> CommandColumn {
    CommandColumn {
        name: column.name.clone(),
        // A domain-typed column keeps its exact database type for casts and
        // CTE column lists, while every decision about the value's meaning
        // uses the base type the domain wraps.
        pg_type: column.sql_type().to_owned(),
        logical_type: column.pg_type.clone(),
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

fn command_argument_row_columns(
    command: &CompiledCommand,
    name: &str,
    path: &str,
) -> Result<Vec<CommandColumn>, PlanError> {
    let root = command
        .descriptor()
        .arguments
        .roots
        .get(name)
        .ok_or_else(|| PlanError::validation(path, format!("unknown command argument '{name}'")))?;
    let ValueType::List { element } = &root.type_ref.value_type else {
        return Err(PlanError::validation(
            path,
            "command argument row-set contract must be a list",
        ));
    };
    let fields = match &element.value_type {
        ValueType::Ref { name } => {
            &command
                .descriptor()
                .arguments
                .named_objects
                .get(name)
                .ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!("command argument row-set object '{name}' is unresolved"),
                    )
                })?
                .fields
        }
        ValueType::Object { fields } => fields,
        _ => {
            return Err(PlanError::validation(
                path,
                "command argument row-set items must be typed objects",
            ));
        }
    };
    fields
        .iter()
        .map(|(name, field)| {
            Ok(CommandColumn {
                name: name.clone(),
                pg_type: command_contract_pg_type(&field.type_ref),
                logical_type: command_contract_pg_type(&field.type_ref),
                nullable: !field.required || field.type_ref.nullable,
            })
        })
        .collect()
}

fn command_contract_pg_type(type_ref: &TypeRef) -> String {
    match &type_ref.value_type {
        ValueType::Scalar { scalar } => match scalar {
            ValueScalar::Boolean => "bool".to_owned(),
            ValueScalar::String => "text".to_owned(),
            ValueScalar::Int32 => "int4".to_owned(),
            ValueScalar::Int64 => "int8".to_owned(),
            ValueScalar::UInt64 | ValueScalar::Decimal => "numeric".to_owned(),
            ValueScalar::Uuid => "uuid".to_owned(),
            ValueScalar::Date => "date".to_owned(),
            ValueScalar::Timestamp => "timestamp".to_owned(),
            ValueScalar::TimestampTz => "timestamptz".to_owned(),
            ValueScalar::Json => "jsonb".to_owned(),
            ValueScalar::Custom { name } => name.clone(),
        },
        ValueType::Enum { .. } => "text".to_owned(),
        ValueType::Object { .. } | ValueType::List { .. } | ValueType::Ref { .. } => {
            "jsonb".to_owned()
        }
    }
}

fn resolved_value_column(
    command: &CompiledCommand,
    name: &str,
    value: &CommandValue,
    previous_steps: &BTreeMap<String, ResolvedCommandStep>,
    item: Option<&CommandItemContext>,
    current: Option<&CommandCurrentContext>,
    path: &str,
) -> Result<CommandColumn, PlanError> {
    let (pg_type, nullable) = match value {
        CommandValue::Argument { arg } => (
            command_argument_pg_type(command.definition(), arg, path)?.to_owned(),
            command_argument_nullable(command.definition(), arg, path)?,
        ),
        CommandValue::Literal { literal, as_ } => (
            command_value_literal_pg_type(command, literal, as_.as_deref(), path)?,
            as_.as_deref().is_some_and(|type_| !type_.ends_with('!'))
                || (as_.is_none() && literal.is_null()),
        ),
        CommandValue::Step {
            step,
            column: Some(column),
            field: None,
            where_nonzero: None,
        } => {
            let column = previous_steps
                .get(step)
                .and_then(|step| step.columns.get(column))
                .ok_or_else(|| {
                    PlanError::validation(path, "projected step column was not resolved")
                })?;
            (column.pg_type.clone(), column.nullable)
        }
        CommandValue::Item { item: field } => {
            let column = item
                .and_then(|item| item.fields.get(field))
                .ok_or_else(|| PlanError::validation(path, "projected item was not resolved"))?;
            (column.pg_type.clone(), column.nullable)
        }
        CommandValue::CurrentColumn { current_column } => {
            let column = current
                .and_then(|current| current.fields.get(current_column))
                .ok_or_else(|| {
                    PlanError::validation(path, "projected current row was not resolved")
                })?;
            (column.pg_type.clone(), column.nullable)
        }
        CommandValue::Rule { rule, .. } => {
            let compiled = command.rules().rule(rule).ok_or_else(|| {
                PlanError::validation(path, format!("unknown compiled rule '{rule}'"))
            })?;
            (
                command_rule_pg_type(&compiled.result).to_owned(),
                matches!(compiled.result, RuleType::Nullable(_)),
            )
        }
        CommandValue::DatabaseTime { database_time } if database_time == "now" => {
            ("timestamptz".to_owned(), false)
        }
        _ => {
            return Err(PlanError::validation(
                path,
                "projection value did not resolve to one scalar",
            ));
        }
    };
    Ok(CommandColumn {
        name: name.to_owned(),
        logical_type: pg_type.clone(),
        pg_type,
        nullable,
    })
}

fn command_argument_definition<'a>(
    command: &'a Command,
    name: &str,
    path: &str,
) -> Result<&'a donat_metadata::CommandArgument, PlanError> {
    command
        .arguments
        .iter()
        .find(|argument| argument.name == name)
        .ok_or_else(|| PlanError::validation(path, format!("unknown command argument '{name}'")))
}

fn command_argument_pg_type<'a>(
    command: &'a Command,
    name: &str,
    path: &str,
) -> Result<&'a str, PlanError> {
    let type_ = command_argument_definition(command, name, path)?
        .type_
        .trim_end_matches('!');
    if type_.starts_with('[') {
        return Ok("jsonb");
    }
    Ok(match type_ {
        "Boolean" | "bool" => "bool",
        "String" | "string" | "ID" => "text",
        "Int" | "int" => "int4",
        "bigint" => "int8",
        "Float" | "float" | "decimal" => "numeric",
        "uuid" => "uuid",
        "date" => "date",
        "timestamp" => "timestamp",
        "timestamptz" => "timestamptz",
        "json" | "jsonb" => "jsonb",
        _ => "jsonb",
    })
}

fn command_argument_nullable(command: &Command, name: &str, path: &str) -> Result<bool, PlanError> {
    Ok(!command_argument_definition(command, name, path)?
        .type_
        .ends_with('!'))
}

fn resolve_effect_idempotency_key(
    command: &CompiledCommand,
    key: &CommandIdempotencyKey,
    arguments: &BTreeMap<String, Scalar>,
    path: &str,
) -> Result<CommandExecutionValue, PlanError> {
    let CommandIdempotencyKey::Argument { argument } = key;
    Ok(CommandExecutionValue::Scalar {
        value: command_argument(arguments, argument, path)?,
        pg_type: command_argument_pg_type(command.definition(), argument, path)?.to_owned(),
    })
}

fn session_contract_pg_type(contract: &TypeRef) -> Result<String, PlanError> {
    let pg_type = match &contract.value_type {
        ValueType::Scalar { scalar } => match scalar {
            ValueScalar::Boolean => "bool".to_owned(),
            ValueScalar::String => "text".to_owned(),
            ValueScalar::Int32 => "int4".to_owned(),
            ValueScalar::Int64 => "int8".to_owned(),
            ValueScalar::UInt64 | ValueScalar::Decimal => "numeric".to_owned(),
            ValueScalar::Uuid => "uuid".to_owned(),
            ValueScalar::Date => "date".to_owned(),
            ValueScalar::Timestamp => "timestamp".to_owned(),
            ValueScalar::TimestampTz => "timestamptz".to_owned(),
            ValueScalar::Json => "jsonb".to_owned(),
            ValueScalar::Custom { name } => name.clone(),
        },
        ValueType::Enum { .. } => "text".to_owned(),
        ValueType::Object { .. } | ValueType::List { .. } | ValueType::Ref { .. } => {
            return Err(PlanError::validation(
                "$",
                "command session contract must resolve to one scalar value",
            ));
        }
    };
    Ok(pg_type)
}

fn command_rule_pg_type(type_: &RuleType) -> &'static str {
    match type_ {
        RuleType::Bool => "bool",
        RuleType::String | RuleType::Enum { .. } => "text",
        RuleType::Int => "int4",
        RuleType::Int64 => "int8",
        RuleType::Decimal => "numeric",
        RuleType::Uuid => "uuid",
        RuleType::Date => "date",
        RuleType::Timestamp => "timestamptz",
        RuleType::List(_) | RuleType::Object { .. } | RuleType::OpaqueJson { .. } => "jsonb",
        RuleType::Nullable(inner) => command_rule_pg_type(inner),
    }
}

fn resolve_command_condition(
    condition: &MetadataCommandCondition,
    arguments: &BTreeMap<String, Scalar>,
    command: &CompiledCommand,
    path: &str,
) -> Result<CommandCondition, PlanError> {
    match condition {
        MetadataCommandCondition::ArgumentEquals { argument_equals } => {
            Ok(CommandCondition::ArgumentEquals {
                argument: command_argument(arguments, &argument_equals.argument, path)?,
                expected: Scalar::Json(argument_equals.value.clone()),
                pg_type: command_argument_pg_type(
                    command.definition(),
                    &argument_equals.argument,
                    path,
                )?
                .to_owned(),
            })
        }
    }
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

fn command_value_literal_pg_type(
    command: &CompiledCommand,
    value: &Json,
    annotation: Option<&str>,
    path: &str,
) -> Result<String, PlanError> {
    let Some(annotation) = annotation else {
        return Ok(command_result_literal_type(value).to_owned());
    };
    let name = annotation.trim_end_matches('!');
    let builtin = match name {
        "Boolean" | "bool" => Some("bool"),
        "String" | "string" | "ID" => Some("text"),
        "Int" | "int" => Some("int4"),
        "bigint" => Some("int8"),
        "Float" | "float" | "decimal" => Some("numeric"),
        "uuid" => Some("uuid"),
        "date" => Some("date"),
        "timestamp" => Some("timestamp"),
        "timestamptz" => Some("timestamptz"),
        "json" | "jsonb" => Some("jsonb"),
        _ => None,
    };
    if let Some(pg_type) = builtin {
        return Ok(pg_type.to_owned());
    }
    if let Some(type_) = command.rules().declared_type(name) {
        return Ok(command_rule_pg_type(type_).to_owned());
    }
    command
        .literal_annotation_pg_type(name)
        .map(str::to_owned)
        .ok_or_else(|| {
            PlanError::validation(
                path,
                format!("command literal annotation '{annotation}' escaped deployment validation"),
            )
        })
}

fn resolve_command_aggregate(
    name: &str,
    aggregate: &CommandAggregate,
    row_set: &ResolvedCommandRowSet,
    path: &str,
) -> Result<CommandAggregateIr, PlanError> {
    let input_column = |name: &str| {
        row_set.columns.get(name).cloned().ok_or_else(|| {
            PlanError::validation(
                path,
                format!("aggregate input column '{name}' was not resolved before SQLgen"),
            )
        })
    };
    let count_output = || CommandColumn {
        name: name.to_owned(),
        pg_type: "int8".to_owned(),
        logical_type: "int8".to_owned(),
        nullable: false,
    };
    match aggregate {
        CommandAggregate::Count { .. } => Ok(CommandAggregateIr::Count {
            output: count_output(),
        }),
        CommandAggregate::Sum { sum } => {
            let input = input_column(command_aggregate_selector(sum, path)?)?;
            let pg_type = match input.logical_type.as_str() {
                "int2" | "int4" | "serial" => "int8",
                "int8" | "bigint" | "bigserial" => "int8",
                "float4" => "float4",
                "float8" => "float8",
                "numeric" | "decimal" => "numeric",
                other => {
                    return Err(PlanError::validation(
                        path,
                        format!("sum input has unsupported resolved type '{other}'"),
                    ));
                }
            };
            Ok(CommandAggregateIr::Sum {
                output: CommandColumn {
                    name: name.to_owned(),
                    pg_type: pg_type.to_owned(),
                    logical_type: pg_type.to_owned(),
                    nullable: !row_set.guaranteed_non_empty || input.nullable,
                },
                input,
            })
        }
        CommandAggregate::Min { min } => {
            let input = input_column(command_aggregate_selector(min, path)?)?;
            Ok(CommandAggregateIr::Min {
                output: CommandColumn {
                    name: name.to_owned(),
                    pg_type: input.pg_type.clone(),
                    logical_type: input.pg_type.clone(),
                    nullable: !row_set.guaranteed_non_empty || input.nullable,
                },
                input,
            })
        }
        CommandAggregate::Max { max } => {
            let input = input_column(command_aggregate_selector(max, path)?)?;
            Ok(CommandAggregateIr::Max {
                output: CommandColumn {
                    name: name.to_owned(),
                    pg_type: input.pg_type.clone(),
                    logical_type: input.pg_type.clone(),
                    nullable: !row_set.guaranteed_non_empty || input.nullable,
                },
                input,
            })
        }
        CommandAggregate::CountDistinct { count_distinct } => {
            Ok(CommandAggregateIr::CountDistinct {
                output: count_output(),
                input: input_column(command_aggregate_selector(count_distinct, path)?)?,
            })
        }
    }
}

fn command_aggregate_selector<'a>(
    aggregate: &'a donat_metadata::ColumnCommandAggregate,
    path: &str,
) -> Result<&'a str, PlanError> {
    aggregate
        .column
        .as_deref()
        .or(aggregate.field.as_deref())
        .ok_or_else(|| PlanError::validation(path, "aggregate selector was not resolved"))
}

fn direct_command_item(value: &CommandValue) -> Option<&str> {
    match value {
        CommandValue::Item { item } => Some(item),
        _ => None,
    }
}

fn collect_runtime_current_columns<'a>(
    values: impl IntoIterator<Item = &'a CommandValue>,
    columns: &mut Vec<&'a String>,
) {
    let mut pending = values.into_iter().collect::<Vec<_>>();
    while let Some(value) = pending.pop() {
        match value {
            CommandValue::CurrentColumn { current_column } => columns.push(current_column),
            CommandValue::Rule { bindings, .. } => pending.extend(bindings.values()),
            _ => {}
        }
    }
}

fn command_rule_item_pg_type(type_: &RuleType) -> &'static str {
    match type_ {
        RuleType::Bool => "boolean",
        RuleType::String | RuleType::Enum { .. } => "text",
        RuleType::Int => "int4",
        RuleType::Int64 => "int8",
        RuleType::Decimal => "numeric",
        RuleType::Uuid => "uuid",
        RuleType::Date => "date",
        RuleType::Timestamp => "timestamptz",
        RuleType::List(_) | RuleType::Object { .. } | RuleType::OpaqueJson { .. } => "jsonb",
        RuleType::Nullable(inner) => command_rule_item_pg_type(inner),
    }
}

fn command_rule_item_nullable(type_: &RuleType) -> bool {
    matches!(type_, RuleType::Nullable(_))
}

fn command_step_table(step: &CommandStep) -> Option<&donat_metadata::QualifiedTable> {
    match &step.operation {
        CommandStepOperation::SelectOne { select_one } => Some(&select_one.table),
        CommandStepOperation::SelectMany { select_many } => Some(&select_many.table),
        CommandStepOperation::Insert { insert } => Some(&insert.table),
        CommandStepOperation::InsertMany { insert_many } => Some(&insert_many.table),
        CommandStepOperation::Update { update } => Some(&update.table),
        CommandStepOperation::UpdateMany { update_many } => Some(&update_many.table),
        CommandStepOperation::UpdateWhen { update_when } => Some(&update_when.table),
        CommandStepOperation::InsertWhen { insert_when } => Some(&insert_when.table),
        CommandStepOperation::Delete { delete } => Some(&delete.table),
        CommandStepOperation::Aggregate { .. }
        | CommandStepOperation::Assert { .. }
        | CommandStepOperation::AssertWhen { .. }
        | CommandStepOperation::Decision { .. }
        | CommandStepOperation::DecisionMany { .. }
        | CommandStepOperation::Project { .. }
        | CommandStepOperation::ProjectMany { .. }
        | CommandStepOperation::FixedRows { .. }
        | CommandStepOperation::AllocateMany { .. } => None,
    }
}

fn command_step_returning(step: &CommandStep) -> &[String] {
    match &step.operation {
        CommandStepOperation::SelectOne { select_one } => &select_one.returning,
        CommandStepOperation::SelectMany { select_many } => &select_many.returning,
        CommandStepOperation::Insert { insert } => &insert.returning,
        CommandStepOperation::InsertMany { insert_many } => &insert_many.returning,
        CommandStepOperation::Update { update } => &update.returning,
        CommandStepOperation::UpdateMany { update_many } => &update_many.returning,
        CommandStepOperation::UpdateWhen { update_when } => &update_when.returning,
        CommandStepOperation::InsertWhen { insert_when } => &insert_when.returning,
        CommandStepOperation::Decision { decision } => &decision.returning,
        CommandStepOperation::DecisionMany { decision_many } => &decision_many.returning,
        CommandStepOperation::Delete { delete } => &delete.returning,
        CommandStepOperation::Aggregate { .. }
        | CommandStepOperation::Assert { .. }
        | CommandStepOperation::AssertWhen { .. }
        | CommandStepOperation::Project { .. }
        | CommandStepOperation::ProjectMany { .. }
        | CommandStepOperation::FixedRows { .. }
        | CommandStepOperation::AllocateMany { .. } => &[],
    }
}

fn command_step_is_many(step: &CommandStep) -> bool {
    matches!(
        step.operation,
        CommandStepOperation::SelectMany { .. }
            | CommandStepOperation::InsertMany { .. }
            | CommandStepOperation::UpdateMany { .. }
            | CommandStepOperation::ProjectMany { .. }
            | CommandStepOperation::FixedRows { .. }
            | CommandStepOperation::DecisionMany { .. }
    )
}

fn command_step_exposes(step: &CommandStep, field: &str) -> bool {
    match &step.operation {
        CommandStepOperation::Aggregate { aggregate } => aggregate.values.contains_key(field),
        CommandStepOperation::Project { project } => project.values.contains_key(field),
        CommandStepOperation::ProjectMany { project_many } => {
            project_many.values.contains_key(field)
        }
        CommandStepOperation::FixedRows { fixed_rows } => fixed_rows
            .rows
            .first()
            .is_some_and(|row| row.contains_key(field)),
        _ => command_step_returning(step)
            .iter()
            .any(|column| column == field),
    }
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

fn coerce_command_argument_value(
    metadata: &donat_metadata::Metadata,
    rules: &donat_rules::RuleCatalog,
    type_: &str,
    value: &Json,
    path: &str,
) -> Result<Json, PlanError> {
    let (type_, nullable) = match type_.strip_suffix('!') {
        Some(inner) => (inner, false),
        None => (type_, true),
    };
    if value.is_null() {
        return if nullable {
            Ok(Json::Null)
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
        let items = match value {
            Json::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    coerce_command_argument_value(
                        metadata,
                        rules,
                        inner,
                        item,
                        &format!("{path}[{index}]"),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
            item => vec![coerce_command_argument_value(
                metadata, rules, inner, item, path,
            )?],
        };
        return Ok(Json::Array(items));
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
        "bigint" => Some(
            value.as_i64().is_some()
                || value
                    .as_u64()
                    .is_some_and(|number| number <= i64::MAX as u64),
        ),
        "Float" | "float" | "decimal" => Some(value.is_number()),
        "json" | "jsonb" => Some(true),
        _ => None,
    };
    if let Some(valid) = builtin_valid {
        return if valid {
            Ok(value.clone())
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
        let mut coerced = object.clone();
        for field in &input.fields {
            match object.get(&field.name) {
                Some(value) => {
                    coerced.insert(
                        field.name.clone(),
                        coerce_command_argument_value(
                            metadata,
                            rules,
                            &field.type_,
                            value,
                            &format!("{path}.{}", field.name),
                        )?,
                    );
                }
                None if field.type_.ends_with('!') => {
                    return Err(PlanError::validation(
                        path,
                        format!("missing required input field: \"{}\"", field.name),
                    ));
                }
                None => {}
            }
        }
        return Ok(Json::Object(coerced));
    }
    if let Some(type_definition) = rules.declared_type(type_) {
        return coerce_rule_argument_value(type_definition, value, path);
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
            Ok(Json::String(value.to_string()))
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
            Ok(value.clone())
        };
    }
    Err(PlanError::validation(
        path,
        format!("unknown command argument type '{type_}'"),
    ))
}

fn coerce_rule_argument_value(
    type_: &RuleType,
    value: &Json,
    path: &str,
) -> Result<Json, PlanError> {
    if let RuleType::Nullable(inner) = type_ {
        return if value.is_null() {
            Ok(Json::Null)
        } else {
            coerce_rule_argument_value(inner, value, path)
        };
    }
    if value.is_null() {
        return Err(PlanError::validation(
            path,
            "null is not allowed for a non-null argument",
        ));
    }
    let valid = match type_ {
        RuleType::Bool => value.is_boolean(),
        RuleType::String | RuleType::Uuid | RuleType::Date | RuleType::Timestamp => {
            value.is_string()
        }
        RuleType::Int => {
            value
                .as_i64()
                .is_some_and(|number| (i32::MIN as i64..=i32::MAX as i64).contains(&number))
                || value
                    .as_u64()
                    .is_some_and(|number| number <= i32::MAX as u64)
        }
        RuleType::Int64 => {
            value.as_i64().is_some()
                || value
                    .as_u64()
                    .is_some_and(|number| number <= i64::MAX as u64)
        }
        RuleType::Decimal => value.is_number(),
        RuleType::Enum { symbols, .. } => value
            .as_str()
            .is_some_and(|value| symbols.iter().any(|symbol| symbol == value)),
        RuleType::OpaqueJson { .. } => true,
        RuleType::List(item) => {
            let values = value
                .as_array()
                .ok_or_else(|| PlanError::validation(path, "argument must be a list"))?;
            return values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    coerce_rule_argument_value(item, value, &format!("{path}[{index}]"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Json::Array);
        }
        RuleType::Object { name, fields } => {
            let values = value.as_object().ok_or_else(|| {
                PlanError::validation(path, format!("argument must be input object '{name}'"))
            })?;
            for field in values.keys() {
                if !fields.contains_key(field) {
                    return Err(PlanError::validation(
                        path,
                        format!("field '{field}' is not declared by input object '{name}'"),
                    ));
                }
            }
            let mut coerced = serde_json::Map::new();
            for (field, field_type) in fields {
                match values.get(field) {
                    Some(value) => {
                        coerced.insert(
                            field.clone(),
                            coerce_rule_argument_value(
                                field_type,
                                value,
                                &format!("{path}.{field}"),
                            )?,
                        );
                    }
                    None if !matches!(field_type, RuleType::Nullable(_)) => {
                        return Err(PlanError::validation(
                            path,
                            format!("missing required input field: \"{field}\""),
                        ));
                    }
                    None => {}
                }
            }
            return Ok(Json::Object(coerced));
        }
        RuleType::Nullable(_) => unreachable!("nullable Rule types are handled above"),
    };
    if valid {
        Ok(value.clone())
    } else {
        Err(PlanError::validation(
            path,
            "argument does not match its declared Rule type",
        ))
    }
}
