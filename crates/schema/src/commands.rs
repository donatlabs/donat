//! Deploy-time compilation of declarative command metadata.
//!
//! The compiler is intentionally SQL-free and side-effect-free. It accepts
//! only the already compiled Rules catalog and immutable Postgres catalog
//! snapshots, so serving can consume its output without parsing YAML or
//! consulting mutable command definitions.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use donat_catalog::{Catalog, ColumnInfo, RelationKind, TableInfo};
use donat_metadata::{
    Columns, Command, CommandEffect, CommandIdempotencyKey, CommandIdempotencyScope,
    CommandStepOperation, CommandValue, Metadata, QualifiedTable, Source, SourceKind, TableEntry,
};
use donat_rules::{RuleCatalog, RuleType};
use uuid::Uuid;

use crate::plan::{MutationKind, PlanError, Planner};

/// Immutable command definitions grouped by their Postgres source.
#[derive(Debug, Clone)]
pub struct CompiledCommandCatalog {
    sources: BTreeMap<String, CompiledSourceCommandCatalog>,
}

impl CompiledCommandCatalog {
    pub(crate) fn empty() -> Self {
        Self {
            sources: BTreeMap::new(),
        }
    }

    /// The already validated commands belonging to one source.
    pub fn source(&self, source: &str) -> Option<&CompiledSourceCommandCatalog> {
        self.sources.get(source)
    }
}

/// Immutable commands for one Postgres source.
#[derive(Debug, Clone, Default)]
pub struct CompiledSourceCommandCatalog {
    commands: BTreeMap<String, CompiledCommand>,
}

impl CompiledSourceCommandCatalog {
    /// Look up a command by its deployment-time name.
    pub fn command(&self, name: &str) -> Option<&CompiledCommand> {
        self.commands.get(name)
    }
}

/// A command definition accepted by the static compiler.
#[derive(Debug, Clone)]
pub struct CompiledCommand {
    definition: Command,
}

impl CompiledCommand {
    /// The trusted, immutable source definition. Request paths receive only a
    /// shared reference; metadata mutations never update this snapshot.
    pub fn definition(&self) -> &Command {
        &self.definition
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StaticType {
    Scalar(String),
    Object {
        name: String,
        fields: BTreeMap<String, StaticType>,
    },
    List(Box<StaticType>),
    Row(BTreeMap<String, StaticType>),
    Rows(BTreeMap<String, StaticType>),
}

impl StaticType {
    fn is_scalar(&self) -> bool {
        matches!(self, Self::Scalar(_))
    }

    fn display_name(&self) -> String {
        match self {
            Self::Scalar(name) => name.clone(),
            Self::Object { name, .. } => format!("object {name}"),
            Self::List(item) => format!("list<{}>", item.display_name()),
            Self::Row(_) => "row".to_string(),
            Self::Rows(_) => "list<row>".to_string(),
        }
    }
}

#[derive(Clone)]
struct StepOutput {
    fields: BTreeMap<String, StaticType>,
    many: bool,
}

#[derive(Clone, Copy)]
struct ValueContext<'a> {
    metadata: &'a Metadata,
    command: &'a Command,
    rules: &'a RuleCatalog,
    steps: &'a BTreeMap<String, StepOutput>,
    declared_steps: &'a HashSet<String>,
    item: Option<&'a BTreeMap<String, StaticType>>,
}

#[derive(Clone, Copy)]
enum ValueUse {
    Data,
    RuleBinding,
    Effect,
}

/// Compile every command against the supplied immutable catalogs. A caller
/// must build the Rules catalog before this function; commands never parse
/// expressions or duplicate the Rules type checker.
pub fn compile_command_catalog(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    rules: &RuleCatalog,
    infer_function_permissions: bool,
) -> Result<CompiledCommandCatalog, PlanError> {
    let mut sources = BTreeMap::new();
    for source in &metadata.sources {
        if source.kind == SourceKind::Postgres {
            sources.insert(source.name.clone(), CompiledSourceCommandCatalog::default());
        }
    }

    let mut names_by_source: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (index, command) in metadata.commands.iter().enumerate() {
        let path = format!("commands[{index}]");
        let source = metadata
            .sources
            .iter()
            .find(|source| source.name == command.source)
            .ok_or_else(|| {
                PlanError::validation(
                    &path,
                    format!("command source '{}' does not exist", command.source),
                )
            })?;
        if source.kind != SourceKind::Postgres {
            return Err(PlanError::validation(
                &path,
                format!(
                    "command source '{}' requires a Postgres source",
                    command.source
                ),
            ));
        }
        let seen = names_by_source.entry(source.name.as_str()).or_default();
        if !seen.insert(command.name.as_str()) {
            return Err(PlanError::validation(
                &path,
                format!(
                    "duplicate command name '{}' for source '{}'",
                    command.name, source.name
                ),
            ));
        }
        let catalog = catalogs.get(&source.name).ok_or_else(|| {
            PlanError::validation(
                &path,
                format!("catalog for command source '{}' is missing", source.name),
            )
        })?;
        validate_command(
            metadata,
            catalogs,
            source,
            catalog,
            rules,
            infer_function_permissions,
            command,
            index,
        )?;
        sources
            .get_mut(&source.name)
            .expect("Postgres command source was initialized")
            .commands
            .insert(
                command.name.clone(),
                CompiledCommand {
                    definition: command.clone(),
                },
            );
    }
    Ok(CompiledCommandCatalog { sources })
}

fn validate_command(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    source: &Source,
    catalog: &Catalog,
    rules: &RuleCatalog,
    infer_function_permissions: bool,
    command: &Command,
    command_index: usize,
) -> Result<(), PlanError> {
    let path = format!("commands[{command_index}]");
    if command.name.is_empty() {
        return Err(PlanError::validation(&path, "command name cannot be empty"));
    }
    if !is_graphql_name(&command.name) {
        return Err(PlanError::validation(
            &path,
            format!(
                "command name '{}' must be a valid GraphQL name",
                command.name
            ),
        ));
    }
    if command.permissions.is_empty() {
        return Err(PlanError::validation(
            &path,
            "command must declare at least one explicit role",
        ));
    }
    let mut roles = HashSet::new();
    for permission in &command.permissions {
        if permission.role.is_empty() || !roles.insert(permission.role.as_str()) {
            return Err(PlanError::validation(
                &path,
                "command permissions must contain unique explicit roles",
            ));
        }
    }
    validate_mutation_root_collisions(
        metadata,
        catalogs,
        command,
        &roles,
        infer_function_permissions,
        &path,
    )?;

    let mut arguments = HashMap::new();
    for (index, argument) in command.arguments.iter().enumerate() {
        let argument_path = format!("{path}.arguments[{index}]");
        if !is_graphql_name(&argument.name) {
            return Err(PlanError::validation(
                &argument_path,
                format!(
                    "command argument name '{}' must be a valid GraphQL name",
                    argument.name
                ),
            ));
        }
        if argument.name.is_empty() || arguments.insert(argument.name.as_str(), argument).is_some()
        {
            return Err(PlanError::validation(
                &argument_path,
                format!("duplicate or empty command argument '{}'", argument.name),
            ));
        }
        command_argument_type(metadata, argument, &argument_path)?;
    }

    validate_idempotency(metadata, command, &path)?;

    let declared_steps = command
        .steps
        .iter()
        .map(|step| step.name.clone())
        .collect::<HashSet<_>>();
    if declared_steps.len() != command.steps.len()
        || declared_steps.iter().any(|name| name.is_empty())
    {
        return Err(PlanError::validation(
            &path,
            "command steps must have unique non-empty names",
        ));
    }
    let mut steps = BTreeMap::new();
    for (index, step) in command.steps.iter().enumerate() {
        let step_path = format!("{path}.steps[{index}]");
        let context = ValueContext {
            metadata,
            command,
            rules,
            steps: &steps,
            declared_steps: &declared_steps,
            item: None,
        };
        let output = validate_step(source, catalog, &roles, step, &context, &step_path)?;
        steps.insert(step.name.clone(), output);
    }

    let context = ValueContext {
        metadata,
        command,
        rules,
        steps: &steps,
        declared_steps: &declared_steps,
        item: None,
    };
    for (index, guard) in command.guards.iter().enumerate() {
        validate_rule(
            &guard.rule,
            &guard.bindings,
            &context,
            &format!("{path}.guards[{index}]"),
            Some(&StaticType::Scalar("Boolean".to_string())),
        )?;
    }
    validate_result(command, &context, &path)?;
    validate_effects(command, &context, &path)?;
    Ok(())
}

fn validate_step(
    source: &Source,
    catalog: &Catalog,
    roles: &HashSet<&str>,
    step: &donat_metadata::CommandStep,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<StepOutput, PlanError> {
    let planner = Planner::for_source(context.metadata, source, catalog);
    match &step.operation {
        CommandStepOperation::Assert { assert } => {
            validate_rule(
                &assert.rule,
                &assert.bindings,
                context,
                path,
                Some(&StaticType::Scalar("Boolean".to_string())),
            )?;
            Ok(StepOutput {
                fields: BTreeMap::new(),
                many: false,
            })
        }
        CommandStepOperation::SelectOne { select_one } => {
            let (entry, info) = command_target(source, catalog, &select_one.table, path)?;
            validate_primary_key_predicate(&select_one.by, info, context, path)?;
            let returning = returning_columns(&select_one.returning, info, path)?;
            require_select_permissions(
                &planner,
                entry,
                info,
                roles,
                select_one.by.keys(),
                returning.keys(),
                path,
            )?;
            Ok(StepOutput {
                fields: returning,
                many: false,
            })
        }
        CommandStepOperation::Insert { insert } => {
            let (entry, info) = command_target(source, catalog, &insert.table, path)?;
            validate_object(&insert.object, info, context, path)?;
            let returning = returning_columns(&insert.returning, info, path)?;
            for role in roles {
                let permission = planner
                    .resolve_role_perm(&entry.insert_permissions, role, |permission| {
                        !permission.backend_only
                    })
                    .ok_or_else(|| {
                        PlanError::validation(
                            path,
                            format!(
                                "role '{role}' lacks insert permission on table '{}.{}'",
                                info.schema, info.name
                            ),
                        )
                    })?;
                require_columns(
                    &permission.columns,
                    insert.object.keys(),
                    role,
                    "insert",
                    info,
                    path,
                )?;
            }
            require_select_permissions(
                &planner,
                entry,
                info,
                roles,
                [].iter(),
                returning.keys(),
                path,
            )?;
            Ok(StepOutput {
                fields: returning,
                many: false,
            })
        }
        CommandStepOperation::InsertMany { insert_many } => {
            let (entry, info) = command_target(source, catalog, &insert_many.table, path)?;
            let item_fields = insert_many_item_fields(&insert_many.for_each, context, path)?;
            let item_context = ValueContext {
                item: Some(&item_fields),
                ..*context
            };
            validate_object(&insert_many.object, info, &item_context, path)?;
            let returning = returning_columns(&insert_many.returning, info, path)?;
            for role in roles {
                let permission = planner
                    .resolve_role_perm(&entry.insert_permissions, role, |permission| {
                        !permission.backend_only
                    })
                    .ok_or_else(|| {
                        PlanError::validation(
                            path,
                            format!(
                                "role '{role}' lacks insert permission on table '{}.{}'",
                                info.schema, info.name
                            ),
                        )
                    })?;
                require_columns(
                    &permission.columns,
                    insert_many.object.keys(),
                    role,
                    "insert",
                    info,
                    path,
                )?;
            }
            require_select_permissions(
                &planner,
                entry,
                info,
                roles,
                [].iter(),
                returning.keys(),
                path,
            )?;
            Ok(StepOutput {
                fields: returning,
                many: true,
            })
        }
        CommandStepOperation::Update { update } => {
            let (entry, info) = command_target(source, catalog, &update.table, path)?;
            validate_primary_key_predicate(&update.predicate, info, context, path)?;
            validate_object(&update.set, info, context, path)?;
            let returning = returning_columns(&update.returning, info, path)?;
            for role in roles {
                let permission = planner
                    .resolve_role_perm(&entry.update_permissions, role, |_| true)
                    .ok_or_else(|| {
                        PlanError::validation(
                            path,
                            format!(
                                "role '{role}' lacks update permission on table '{}.{}'",
                                info.schema, info.name
                            ),
                        )
                    })?;
                require_columns(
                    &permission.columns,
                    update.set.keys(),
                    role,
                    "update",
                    info,
                    path,
                )?;
            }
            require_select_permissions(
                &planner,
                entry,
                info,
                roles,
                update.predicate.keys(),
                returning.keys(),
                path,
            )?;
            Ok(StepOutput {
                fields: returning,
                many: false,
            })
        }
        CommandStepOperation::Delete { delete } => {
            let (entry, info) = command_target(source, catalog, &delete.table, path)?;
            validate_primary_key_predicate(&delete.predicate, info, context, path)?;
            let returning = returning_columns(&delete.returning, info, path)?;
            for role in roles {
                if planner
                    .resolve_role_perm(&entry.delete_permissions, role, |_| true)
                    .is_none()
                {
                    return Err(PlanError::validation(
                        path,
                        format!(
                            "role '{role}' lacks delete permission on table '{}.{}'",
                            info.schema, info.name
                        ),
                    ));
                }
            }
            require_select_permissions(
                &planner,
                entry,
                info,
                roles,
                delete.predicate.keys(),
                returning.keys(),
                path,
            )?;
            Ok(StepOutput {
                fields: returning,
                many: false,
            })
        }
    }
}

fn command_target<'a>(
    source: &'a Source,
    catalog: &'a Catalog,
    table: &QualifiedTable,
    path: &str,
) -> Result<(&'a TableEntry, &'a TableInfo), PlanError> {
    let entry = source
        .tables
        .iter()
        .find(|entry| entry.table.schema() == table.schema() && entry.table.name() == table.name())
        .ok_or_else(|| {
            PlanError::validation(
                path,
                format!(
                    "command target '{}.{}' is not tracked",
                    table.schema(),
                    table.name()
                ),
            )
        })?;
    let info = catalog.table(table.schema(), table.name()).ok_or_else(|| {
        PlanError::validation(
            path,
            format!(
                "command target '{}.{}' does not exist in the catalog",
                table.schema(),
                table.name()
            ),
        )
    })?;
    if info.relation_kind != RelationKind::Table {
        return Err(PlanError::validation(
            path,
            format!(
                "command target '{}.{}' must be an ordinary table, not {:?}",
                table.schema(),
                table.name(),
                info.relation_kind
            ),
        ));
    }
    if info.primary_key.is_empty() {
        return Err(PlanError::validation(
            path,
            format!(
                "command target '{}.{}' requires a primary key",
                table.schema(),
                table.name()
            ),
        ));
    }
    Ok((entry, info))
}

fn validate_primary_key_predicate(
    predicate: &BTreeMap<String, CommandValue>,
    table: &TableInfo,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<(), PlanError> {
    let supplied = predicate.keys().collect::<BTreeSet<_>>();
    let required = table.primary_key.iter().collect::<BTreeSet<_>>();
    if supplied != required {
        return Err(PlanError::validation(
            path,
            format!(
                "update/delete/select_one requires every primary-key column ({})",
                table.primary_key.join(", ")
            ),
        ));
    }
    for (column, value) in predicate {
        let column_info = table.column(column).expect("primary key came from table");
        validate_value_against_column(value, column_info, context, path)?;
    }
    Ok(())
}

fn validate_object(
    object: &BTreeMap<String, CommandValue>,
    table: &TableInfo,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<(), PlanError> {
    for (column, value) in object {
        let column_info = table.column(column).ok_or_else(|| {
            PlanError::validation(path, format!("unknown column '{column}' on command target"))
        })?;
        validate_value_against_column(value, column_info, context, path)?;
    }
    Ok(())
}

fn validate_value_against_column(
    value: &CommandValue,
    column: &ColumnInfo,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<(), PlanError> {
    let expected = column_type(column);
    let actual = value_type(value, context, Some(&expected), ValueUse::Data, path)?;
    if !assignable(&actual, &expected) {
        return Err(PlanError::validation(
            path,
            format!(
                "{} is not assignable to column '{}' ({})",
                actual.display_name(),
                column.name,
                expected.display_name()
            ),
        ));
    }
    Ok(())
}

fn returning_columns(
    returning: &[String],
    table: &TableInfo,
    path: &str,
) -> Result<BTreeMap<String, StaticType>, PlanError> {
    let mut fields = BTreeMap::new();
    for column in returning {
        let column_info = table.column(column).ok_or_else(|| {
            PlanError::validation(path, format!("unknown column '{column}' on command target"))
        })?;
        if fields
            .insert(column.clone(), column_type(column_info))
            .is_some()
        {
            return Err(PlanError::validation(
                path,
                format!("duplicate returning column '{column}'"),
            ));
        }
    }
    Ok(fields)
}

fn require_select_permissions<'a>(
    planner: &Planner<'_>,
    entry: &TableEntry,
    info: &TableInfo,
    roles: &HashSet<&'a str>,
    predicate_columns: impl Iterator<Item = &'a String>,
    returning_columns: impl Iterator<Item = &'a String>,
    path: &str,
) -> Result<(), PlanError> {
    let columns = predicate_columns
        .chain(returning_columns)
        .collect::<BTreeSet<_>>();
    for role in roles {
        let context = planner
            .table_ctx_by_name(&entry.table, role)
            .ok_or_else(|| {
                PlanError::validation(
                    path,
                    format!(
                        "role '{role}' lacks select permission on table {}",
                        entry.table
                    ),
                )
            })?;
        for column in &columns {
            if !context.column_allowed(column) {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "role '{role}' lacks select permission for column '{column}' on table '{}.{}'",
                        info.schema, info.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn require_columns<'a>(
    allowed: &Columns,
    columns: impl IntoIterator<Item = &'a String>,
    role: &str,
    operation: &str,
    table: &TableInfo,
    path: &str,
) -> Result<(), PlanError> {
    for column in columns {
        let permitted = match allowed {
            Columns::Star => true,
            Columns::List(list) => list.iter().any(|allowed| allowed == column),
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

fn insert_many_item_fields(
    for_each: &CommandValue,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<BTreeMap<String, StaticType>, PlanError> {
    let CommandValue::Argument { arg } = for_each else {
        return Err(PlanError::validation(
            path,
            "insert_many for_each must bind one declared list argument",
        ));
    };
    let type_ = command_argument_type_by_name(context.metadata, context.command, arg, path)?;
    let StaticType::List(item) = type_ else {
        return Err(PlanError::validation(
            path,
            "insert_many for_each must bind one declared list argument",
        ));
    };
    let StaticType::Object { fields, .. } = *item else {
        return Err(PlanError::validation(
            path,
            "insert_many for_each items must be typed input objects",
        ));
    };
    Ok(fields)
}

fn validate_result(
    command: &Command,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<(), PlanError> {
    let mut names = HashSet::new();
    for field in &command.result.fields {
        if !is_graphql_name(&field.name) {
            return Err(PlanError::validation(
                path,
                format!(
                    "command result field '{}' must be a valid GraphQL name",
                    field.name
                ),
            ));
        }
        if field.name.is_empty() || !names.insert(field.name.as_str()) {
            return Err(PlanError::validation(
                path,
                "command result fields must have unique non-empty names",
            ));
        }
        match &field.value {
            CommandValue::Step { .. } | CommandValue::Literal { .. } => {
                value_type(&field.value, context, None, ValueUse::Data, path)?;
            }
            _ => {
                return Err(PlanError::validation(
                    path,
                    "command result fields must be step columns or literals, never mutable arguments",
                ));
            }
        }
    }
    Ok(())
}

fn validate_idempotency(
    metadata: &Metadata,
    command: &Command,
    path: &str,
) -> Result<(), PlanError> {
    let Some(idempotency) = &command.idempotency else {
        return Ok(());
    };
    validate_idempotency_key(metadata, &idempotency.key, command, path)?;
    for scope in &idempotency.scope {
        match scope {
            CommandIdempotencyScope::Argument { argument } => {
                let type_ = command_argument_type_by_name(metadata, command, argument, path)?;
                if !type_.is_scalar() {
                    return Err(PlanError::validation(
                        path,
                        "idempotency scope must be scalar and cannot use object or list arguments",
                    ));
                }
            }
            CommandIdempotencyScope::SessionVariable { session_variable } => {
                if secret_looking(session_variable) {
                    return Err(PlanError::validation(
                        path,
                        "idempotency scope cannot use a secret-looking session variable",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_idempotency_key(
    metadata: &Metadata,
    key: &CommandIdempotencyKey,
    command: &Command,
    path: &str,
) -> Result<(), PlanError> {
    let CommandIdempotencyKey::Argument { argument } = key;
    let type_ = command_argument_type_by_name(metadata, command, argument, path)?;
    if !type_.is_scalar() {
        return Err(PlanError::validation(
            path,
            "idempotency key must be a declared scalar argument",
        ));
    }
    Ok(())
}

fn validate_effects(
    command: &Command,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<(), PlanError> {
    if command.effects.is_empty() {
        return Ok(());
    }
    if command.idempotency.is_none() {
        return Err(PlanError::validation(
            path,
            "command effects require command idempotency",
        ));
    }
    for (index, effect) in command.effects.iter().enumerate() {
        let effect_path = format!("{path}.effects[{index}]");
        match effect {
            CommandEffect::StartProcess { start_process } => {
                let key = start_process.idempotency_key.as_ref().ok_or_else(|| {
                    PlanError::validation(
                        &effect_path,
                        "command effect requires an idempotency key",
                    )
                })?;
                validate_idempotency_key(context.metadata, key, command, &effect_path)?;
                validate_effect_bindings(&start_process.input, context, &effect_path)?;
            }
            CommandEffect::SignalProcess { signal_process } => {
                let key = signal_process.idempotency_key.as_ref().ok_or_else(|| {
                    PlanError::validation(
                        &effect_path,
                        "command effect requires an idempotency key",
                    )
                })?;
                validate_idempotency_key(context.metadata, key, command, &effect_path)?;
                validate_effect_bindings(&signal_process.correlate, context, &effect_path)?;
                validate_effect_bindings(&signal_process.payload, context, &effect_path)?;
            }
        }
    }
    Ok(())
}

fn validate_effect_bindings(
    bindings: &BTreeMap<String, CommandValue>,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<(), PlanError> {
    for value in bindings.values() {
        match value {
            CommandValue::Argument { .. }
            | CommandValue::Step { .. }
            | CommandValue::SessionVariable { .. } => {
                value_type(value, context, None, ValueUse::Effect, path)?;
            }
            _ => {
                return Err(PlanError::validation(
                    path,
                    "effect payload and correlation bindings must be local arguments, prior step values, or explicit session variables",
                ));
            }
        }
    }
    Ok(())
}

fn validate_rule(
    name: &str,
    bindings: &BTreeMap<String, CommandValue>,
    context: &ValueContext<'_>,
    path: &str,
    expected_result: Option<&StaticType>,
) -> Result<StaticType, PlanError> {
    let rule = context
        .rules
        .rule(name)
        .ok_or_else(|| PlanError::validation(path, format!("unknown rule '{name}'")))?;
    let expected_names = rule.bindings.keys().collect::<BTreeSet<_>>();
    let supplied_names = bindings.keys().collect::<BTreeSet<_>>();
    if expected_names != supplied_names {
        return Err(PlanError::validation(
            path,
            format!("rule '{name}' must bind every declared rule parameter exactly once"),
        ));
    }
    for (binding, expected) in &rule.bindings {
        let actual = value_type(
            bindings.get(binding).expect("binding names were checked"),
            context,
            Some(&rule_type(expected)),
            ValueUse::RuleBinding,
            path,
        )?;
        let expected = rule_type(expected);
        if !assignable(&actual, &expected) {
            return Err(PlanError::validation(
                path,
                format!(
                    "{} is not assignable to rule binding '{}' ({})",
                    actual.display_name(),
                    binding,
                    expected.display_name()
                ),
            ));
        }
    }
    let result = rule_type(&rule.result);
    if let Some(expected) = expected_result
        && !assignable(&result, expected)
    {
        return Err(PlanError::validation(
            path,
            format!("rule '{name}' must return {}", expected.display_name()),
        ));
    }
    Ok(result)
}

fn value_type(
    value: &CommandValue,
    context: &ValueContext<'_>,
    expected: Option<&StaticType>,
    use_: ValueUse,
    path: &str,
) -> Result<StaticType, PlanError> {
    match value {
        CommandValue::Argument { arg } => {
            command_argument_type_by_name(context.metadata, context.command, arg, path)
        }
        CommandValue::Item { item } => {
            let fields = context.item.ok_or_else(|| {
                PlanError::validation(path, "item values are allowed only inside insert_many")
            })?;
            fields.get(item).cloned().ok_or_else(|| {
                PlanError::validation(path, format!("unknown insert_many item field '{item}'"))
            })
        }
        CommandValue::Step { step, column } => {
            let output = context.steps.get(step).ok_or_else(|| {
                let message = if context.declared_steps.contains(step) {
                    format!("step reference '{step}' must reference an earlier step")
                } else {
                    format!("unknown step reference '{step}'")
                };
                PlanError::validation(path, message)
            })?;
            match column {
                Some(column) => {
                    let field = output.fields.get(column).cloned().ok_or_else(|| {
                        PlanError::validation(
                            path,
                            format!("step '{step}' does not return column '{column}'"),
                        )
                    })?;
                    if output.many {
                        Ok(StaticType::List(Box::new(field)))
                    } else {
                        Ok(field)
                    }
                }
                None if output.many => Ok(StaticType::Rows(output.fields.clone())),
                None => Ok(StaticType::Row(output.fields.clone())),
            }
        }
        CommandValue::Literal { literal } => literal_type(literal, expected, path),
        CommandValue::Rule { rule, bindings } => {
            validate_rule(rule, bindings, context, path, expected)
        }
        CommandValue::SessionVariable { session_variable } => {
            if !matches!(use_, ValueUse::Effect) {
                return Err(PlanError::validation(
                    path,
                    "session variables are allowed only in command effect bindings",
                ));
            }
            if secret_looking(session_variable) {
                return Err(PlanError::validation(
                    path,
                    "effect bindings cannot use a secret-looking session variable",
                ));
            }
            Ok(StaticType::Scalar("String".to_string()))
        }
    }
}

fn literal_type(
    literal: &serde_json::Value,
    expected: Option<&StaticType>,
    path: &str,
) -> Result<StaticType, PlanError> {
    let inferred = match literal {
        serde_json::Value::Bool(_) => StaticType::Scalar("Boolean".to_string()),
        serde_json::Value::Number(number) if number.is_i64() || number.is_u64() => {
            StaticType::Scalar("Int".to_string())
        }
        serde_json::Value::Number(_) => StaticType::Scalar("Float".to_string()),
        serde_json::Value::String(value) => {
            if let Some(expected) = expected {
                validate_string_literal(value, expected, path)?;
                expected.clone()
            } else {
                StaticType::Scalar("String".to_string())
            }
        }
        serde_json::Value::Null => expected.cloned().ok_or_else(|| {
            PlanError::validation(
                path,
                "null command literals require an explicit typed destination",
            )
        })?,
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            return Err(PlanError::validation(
                path,
                "command literals must be scalar values",
            ));
        }
    };
    Ok(inferred)
}

fn validate_string_literal(
    value: &str,
    expected: &StaticType,
    path: &str,
) -> Result<(), PlanError> {
    let StaticType::Scalar(scalar) = expected else {
        return Ok(());
    };
    let valid = match scalar.as_str() {
        "Boolean" => matches!(value, "true" | "false"),
        "Int" => value.parse::<i32>().is_ok(),
        "Float" => value.parse::<f64>().is_ok_and(|number| number.is_finite()),
        "uuid" => Uuid::parse_str(value).is_ok(),
        "date" => NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok(),
        "timestamp" => parse_timestamp(value).is_some(),
        "timestamptz" => DateTime::parse_from_rfc3339(value).is_ok(),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(PlanError::validation(
            path,
            format!("invalid literal for {scalar}"),
        ))
    }
}

fn parse_timestamp(value: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f"))
        .ok()
}

fn command_argument_type_by_name(
    metadata: &Metadata,
    command: &Command,
    name: &str,
    path: &str,
) -> Result<StaticType, PlanError> {
    let argument = command
        .arguments
        .iter()
        .find(|argument| argument.name == name)
        .ok_or_else(|| PlanError::validation(path, format!("unknown argument '{name}'")))?;
    command_argument_type(metadata, argument, path)
}

fn command_argument_type(
    metadata: &Metadata,
    argument: &donat_metadata::CommandArgument,
    path: &str,
) -> Result<StaticType, PlanError> {
    parse_command_type(metadata, &argument.type_, path)
}

fn parse_command_type(
    metadata: &Metadata,
    type_: &str,
    path: &str,
) -> Result<StaticType, PlanError> {
    parse_type_with_named(type_, path, |name| {
        if let Some(input) = metadata
            .custom_types
            .input_objects
            .iter()
            .find(|input| input.name == name)
        {
            let mut fields = BTreeMap::new();
            for field in &input.fields {
                fields.insert(
                    field.name.clone(),
                    parse_command_type(metadata, &field.type_, path)?,
                );
            }
            return Ok(Some(StaticType::Object {
                name: name.to_string(),
                fields,
            }));
        }
        if metadata
            .custom_types
            .enums
            .iter()
            .any(|value| value.name == name)
            || metadata
                .custom_types
                .scalars
                .iter()
                .any(|value| value.name == name)
        {
            return Ok(Some(StaticType::Scalar(name.to_string())));
        }
        Ok(None)
    })
}

fn parse_type_with_named(
    type_: &str,
    path: &str,
    named: impl Fn(&str) -> Result<Option<StaticType>, PlanError>,
) -> Result<StaticType, PlanError> {
    fn parse(
        source: &str,
        path: &str,
        named: &impl Fn(&str) -> Result<Option<StaticType>, PlanError>,
    ) -> Result<StaticType, PlanError> {
        let source = source.strip_suffix('!').unwrap_or(source);
        if let Some(inner) = source
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
        {
            return Ok(StaticType::List(Box::new(parse(inner, path, named)?)));
        }
        let builtin = match source {
            "Boolean" | "bool" => Some("Boolean"),
            "String" | "string" | "ID" => Some("String"),
            "Int" | "int" => Some("Int"),
            "Float" | "float" | "decimal" => Some("Float"),
            "uuid" | "date" | "timestamp" | "timestamptz" | "json" | "jsonb" => Some(source),
            _ => None,
        };
        if let Some(name) = builtin {
            return Ok(StaticType::Scalar(name.to_string()));
        }
        if let Some(type_) = named(source)? {
            return Ok(type_);
        }
        Err(PlanError::validation(
            path,
            format!("unknown command argument type '{source}'"),
        ))
    }
    parse(type_, path, &named)
}

fn column_type(column: &ColumnInfo) -> StaticType {
    let scalar = match column.pg_type.as_str() {
        "int2" | "int4" | "serial" => "Int",
        "float4" | "float8" | "numeric" | "decimal" => "Float",
        "text" | "varchar" | "bpchar" | "name" | "citext" => "String",
        "bool" => "Boolean",
        "timestamp" | "timestamp without time zone" => "timestamp",
        "timestamptz" | "timestamp with time zone" => "timestamptz",
        other => other,
    };
    StaticType::Scalar(scalar.to_string())
}

fn rule_type(type_: &RuleType) -> StaticType {
    match type_ {
        RuleType::Bool => StaticType::Scalar("Boolean".to_string()),
        RuleType::String => StaticType::Scalar("String".to_string()),
        RuleType::Int => StaticType::Scalar("Int".to_string()),
        RuleType::Decimal => StaticType::Scalar("Float".to_string()),
        RuleType::Uuid => StaticType::Scalar("uuid".to_string()),
        RuleType::Date => StaticType::Scalar("date".to_string()),
        RuleType::Timestamp => StaticType::Scalar("timestamp".to_string()),
        RuleType::Enum { name, .. } => StaticType::Scalar(name.clone()),
        RuleType::List(item) => StaticType::List(Box::new(rule_type(item))),
        RuleType::Object { name, fields } => StaticType::Object {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|(name, type_)| (name.clone(), rule_type(type_)))
                .collect(),
        },
        RuleType::Nullable(inner) => rule_type(inner),
    }
}

fn assignable(actual: &StaticType, expected: &StaticType) -> bool {
    actual == expected
}

fn secret_looking(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["secret", "token", "password", "credential", "api-key"]
        .iter()
        .any(|fragment| name.contains(fragment))
}

fn is_graphql_name(name: &str) -> bool {
    let mut characters = name.bytes();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && !name.starts_with("__")
        && characters.all(|character| character.is_ascii_alphanumeric() || character == b'_')
}

fn validate_mutation_root_collisions(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    command: &Command,
    roles: &HashSet<&str>,
    infer_function_permissions: bool,
    path: &str,
) -> Result<(), PlanError> {
    for role in roles {
        if metadata.actions.iter().any(|action| {
            action.name == command.name
                && action.definition.action_type.as_deref() != Some("query")
                && (action.permissions.is_empty()
                    || action
                        .permissions
                        .iter()
                        .any(|permission| permission.role == *role))
        }) {
            return Err(PlanError::validation(
                path,
                format!(
                    "command name '{}' collides with action mutation for role '{role}'",
                    command.name
                ),
            ));
        }

        for source in &metadata.sources {
            let Some(catalog) = catalogs.get(&source.name) else {
                continue;
            };
            let mut planner = Planner::for_source(metadata, source, catalog);
            planner.infer_function_permissions = infer_function_permissions;
            let command_is_mutation_root = planner
                .mutation_root_names()
                .any(|root| root == command.name);

            if command_is_mutation_root
                && source.functions.iter().any(|function| {
                    function
                        .configuration
                        .as_ref()
                        .and_then(|configuration| configuration.exposed_as.as_deref())
                        == Some("mutation")
                        && function_mutation_root_name(function) == command.name
                        && (infer_function_permissions
                            || role_or_parent_has_permission(
                                metadata,
                                role,
                                function
                                    .permissions
                                    .iter()
                                    .map(|permission| permission.role.as_str()),
                            ))
                })
            {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "command name '{}' collides with an existing mutation root field for role '{role}'",
                        command.name
                    ),
                ));
            }

            if table_mutation_root_visible_to_role(&planner, source, role, &command.name) {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "command name '{}' collides with an existing mutation root field for role '{role}'",
                        command.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn table_mutation_root_visible_to_role(
    planner: &Planner<'_>,
    source: &Source,
    role: &str,
    command_name: &str,
) -> bool {
    if !planner
        .mutation_root_names()
        .any(|root| root == command_name)
    {
        return false;
    }
    source.tables.iter().any(|entry| {
        table_mutation_roots(entry).into_iter().any(|(root, kind)| {
            root == command_name
                && match kind {
                    MutationKind::Insert | MutationKind::InsertOne => planner
                        .resolve_role_perm(&entry.insert_permissions, role, |permission| {
                            !permission.backend_only
                        })
                        .is_some(),
                    MutationKind::Update | MutationKind::UpdateByPk => planner
                        .resolve_role_perm(&entry.update_permissions, role, |_| true)
                        .is_some(),
                    MutationKind::Delete | MutationKind::DeleteByPk => planner
                        .resolve_role_perm(&entry.delete_permissions, role, |_| true)
                        .is_some(),
                }
        })
    })
}

fn table_mutation_roots(entry: &TableEntry) -> [(String, MutationKind); 6] {
    let base = crate::naming::table_base_name(entry);
    let custom = entry
        .configuration
        .as_ref()
        .map(|configuration| &configuration.custom_root_fields);
    let root = |key: &str, default: String| {
        custom
            .and_then(|roots| roots.get(key).cloned())
            .unwrap_or(default)
    };
    [
        (
            root("insert", format!("insert_{base}")),
            MutationKind::Insert,
        ),
        (
            root("insert_one", format!("insert_{base}_one")),
            MutationKind::InsertOne,
        ),
        (
            root("update", format!("update_{base}")),
            MutationKind::Update,
        ),
        (
            root("update_by_pk", format!("update_{base}_by_pk")),
            MutationKind::UpdateByPk,
        ),
        (
            root("delete", format!("delete_{base}")),
            MutationKind::Delete,
        ),
        (
            root("delete_by_pk", format!("delete_{base}_by_pk")),
            MutationKind::DeleteByPk,
        ),
    ]
}

fn function_mutation_root_name(function: &donat_metadata::FunctionEntry) -> String {
    function
        .configuration
        .as_ref()
        .and_then(|configuration| configuration.custom_name.clone())
        .unwrap_or_else(|| crate::naming::default_base_name(&function.function))
}

fn role_or_parent_has_permission<'a>(
    metadata: &Metadata,
    role: &str,
    permitted_roles: impl Iterator<Item = &'a str>,
) -> bool {
    let permitted_roles = permitted_roles.collect::<HashSet<_>>();
    let mut pending = vec![role];
    let mut seen = HashSet::new();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        if permitted_roles.contains(current) {
            return true;
        }
        if let Some(inherited) = metadata
            .inherited_roles
            .iter()
            .find(|inherited| inherited.role_name == current)
        {
            pending.extend(inherited.role_set.iter().map(String::as_str));
        }
    }
    false
}
