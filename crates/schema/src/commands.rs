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

/// PostgreSQL facts that are intentionally narrower than the GraphQL-facing
/// [`StaticType`]. This stays private to deploy-time command literal
/// validation: arguments, items, step outputs, Rules, and the generated
/// schema continue to use their existing static typing model.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandScalarDescriptor {
    Bool,
    SignedInteger {
        minimum: i128,
        maximum: i128,
    },
    Float32,
    Float64,
    Numeric {
        precision: Option<u16>,
        scale: Option<i16>,
    },
    Uuid,
    Date,
    Timestamp {
        with_time_zone: bool,
        fractional_precision: u8,
    },
    Text {
        maximum_characters: Option<usize>,
        maximum_bytes: Option<usize>,
    },
}

impl CommandScalarDescriptor {
    fn from_column(column: &ColumnInfo) -> Result<Self, String> {
        let unmodified = || {
            if column.pg_typmod == -1 {
                Ok(())
            } else {
                Err(format!(
                    "has unsupported type modifier {}",
                    column.pg_typmod
                ))
            }
        };
        match column.pg_type.as_str() {
            "bool" => {
                unmodified()?;
                Ok(Self::Bool)
            }
            "int2" => {
                unmodified()?;
                Ok(Self::SignedInteger {
                    minimum: i16::MIN.into(),
                    maximum: i16::MAX.into(),
                })
            }
            "int4" => {
                unmodified()?;
                Ok(Self::SignedInteger {
                    minimum: i32::MIN.into(),
                    maximum: i32::MAX.into(),
                })
            }
            "int8" => {
                unmodified()?;
                Ok(Self::SignedInteger {
                    minimum: i64::MIN.into(),
                    maximum: i64::MAX.into(),
                })
            }
            "float4" => {
                unmodified()?;
                Ok(Self::Float32)
            }
            "float8" => {
                unmodified()?;
                Ok(Self::Float64)
            }
            "numeric" | "decimal" => {
                let (precision, scale) = numeric_modifier(column.pg_typmod)?;
                Ok(Self::Numeric { precision, scale })
            }
            "uuid" => {
                unmodified()?;
                Ok(Self::Uuid)
            }
            "date" => {
                unmodified()?;
                Ok(Self::Date)
            }
            "timestamp" | "timestamp without time zone" => Ok(Self::Timestamp {
                with_time_zone: false,
                fractional_precision: timestamp_precision(column.pg_typmod)?,
            }),
            "timestamptz" | "timestamp with time zone" => Ok(Self::Timestamp {
                with_time_zone: true,
                fractional_precision: timestamp_precision(column.pg_typmod)?,
            }),
            "text" | "citext" => {
                unmodified()?;
                Ok(Self::Text {
                    maximum_characters: None,
                    maximum_bytes: None,
                })
            }
            "varchar" | "bpchar" => {
                let maximum_characters = match column.pg_typmod {
                    -1 => None,
                    modifier if modifier >= 4 => Some((modifier - 4) as usize),
                    modifier => {
                        return Err(format!("has malformed type modifier {modifier}"));
                    }
                };
                Ok(Self::Text {
                    maximum_characters,
                    maximum_bytes: None,
                })
            }
            "name" => {
                unmodified()?;
                Ok(Self::Text {
                    maximum_characters: None,
                    maximum_bytes: Some(63),
                })
            }
            _ => Err("is not a supported command literal type".to_string()),
        }
    }

    fn validate(&self, literal: &serde_json::Value, nullable: bool) -> Result<(), String> {
        if literal.is_null() {
            return if nullable {
                Ok(())
            } else {
                Err("null is not allowed for a non-nullable column".to_string())
            };
        }
        if matches!(
            literal,
            serde_json::Value::Array(_) | serde_json::Value::Object(_)
        ) {
            return Err("must be a scalar value, not an object or list".to_string());
        }

        match self {
            Self::Bool => {
                if literal.is_boolean() {
                    Ok(())
                } else {
                    Err("must be a JSON boolean".to_string())
                }
            }
            Self::SignedInteger { minimum, maximum } => {
                let number = parse_integral_literal(literal)?;
                if number < *minimum || number > *maximum {
                    Err("is out of range".to_string())
                } else {
                    Ok(())
                }
            }
            Self::Float32 => parse_finite_float32(literal),
            Self::Float64 => parse_finite_float64(literal),
            Self::Numeric { precision, scale } => {
                let decimal = parse_decimal_literal(literal)?;
                if let (Some(precision), Some(scale)) = (precision, scale) {
                    let rounded = round_decimal_to_scale(&decimal, *scale);
                    if rounded.len() > usize::from(*precision) {
                        return Err(format!(
                            "exceeds numeric({precision}, {scale}) precision after rounding"
                        ));
                    }
                }
                Ok(())
            }
            Self::Uuid => {
                let value = literal
                    .as_str()
                    .ok_or_else(|| "must be a UUID string".to_string())?;
                let parsed =
                    Uuid::parse_str(value).map_err(|_| "must be a canonical UUID".to_string())?;
                if parsed.hyphenated().to_string().eq_ignore_ascii_case(value) {
                    Ok(())
                } else {
                    Err("must be a canonical UUID".to_string())
                }
            }
            Self::Date => {
                let value = literal
                    .as_str()
                    .ok_or_else(|| "must be a YYYY-MM-DD date string".to_string())?;
                if is_iso_date(value) && NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok() {
                    Ok(())
                } else {
                    Err("must be a valid YYYY-MM-DD date".to_string())
                }
            }
            Self::Timestamp {
                with_time_zone,
                fractional_precision,
            } => {
                let value = literal
                    .as_str()
                    .ok_or_else(|| "must be a timestamp string".to_string())?;
                let valid = if *with_time_zone {
                    DateTime::parse_from_rfc3339(value).is_ok()
                } else {
                    parse_timestamp(value).is_some()
                };
                if !valid {
                    return Err(if *with_time_zone {
                        "must be an RFC 3339 timestamp with an offset".to_string()
                    } else {
                        "must be a local timestamp".to_string()
                    });
                }
                let digits = timestamp_fractional_digits(value, *with_time_zone)?;
                if digits > *fractional_precision {
                    Err(format!(
                        "has {digits} fractional-second digits but permits at most {fractional_precision}"
                    ))
                } else {
                    Ok(())
                }
            }
            Self::Text {
                maximum_characters,
                maximum_bytes,
            } => {
                let value = literal
                    .as_str()
                    .ok_or_else(|| "must be a string".to_string())?;
                if let Some(maximum) = maximum_characters
                    && value.chars().count() > *maximum
                {
                    return Err(format!("exceeds the {maximum}-character limit"));
                }
                if let Some(maximum) = maximum_bytes
                    && value.len() > *maximum
                {
                    return Err(format!("exceeds the {maximum}-byte limit"));
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
struct DecimalLiteral {
    digits: String,
    fractional_digits: usize,
}

fn numeric_modifier(pg_typmod: i32) -> Result<(Option<u16>, Option<i16>), String> {
    if pg_typmod == -1 {
        return Ok((None, None));
    }
    let modifier = pg_typmod
        .checked_sub(4)
        .ok_or_else(|| format!("has malformed type modifier {pg_typmod}"))?
        as u32;
    let low = modifier & 0xffff;
    if low & !0x07ff != 0 {
        return Err(format!("has malformed type modifier {pg_typmod}"));
    }
    let precision = (modifier >> 16) as u16;
    let raw_scale = (low & 0x07ff) as i16;
    let scale = if raw_scale & 0x0400 != 0 {
        raw_scale - 0x0800
    } else {
        raw_scale
    };
    if precision == 0 || precision > 1000 || !(-1000..=1000).contains(&scale) {
        return Err(format!("has malformed type modifier {pg_typmod}"));
    }
    Ok((Some(precision), Some(scale)))
}

fn timestamp_precision(pg_typmod: i32) -> Result<u8, String> {
    match pg_typmod {
        -1 => Ok(6),
        0..=6 => Ok(pg_typmod as u8),
        modifier => Err(format!("has malformed type modifier {modifier}")),
    }
}

fn parse_integral_literal(literal: &serde_json::Value) -> Result<i128, String> {
    let value = literal_text(literal, "must be an integral JSON number or string")?;
    if !is_integral_decimal(&value) {
        return Err("must be an integral JSON number or string".to_string());
    }
    value
        .parse::<i128>()
        .map_err(|_| "is out of range".to_string())
}

fn parse_finite_float32(literal: &serde_json::Value) -> Result<(), String> {
    let value = literal_text(literal, "must be a JSON number or numeric string")?;
    if value.trim() != value {
        return Err("must be a JSON number or numeric string".to_string());
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|number| number.is_finite())
        .map(|_| ())
        .ok_or_else(|| "must be a finite float4 value".to_string())
}

fn parse_finite_float64(literal: &serde_json::Value) -> Result<(), String> {
    let value = literal_text(literal, "must be a JSON number or numeric string")?;
    if value.trim() != value {
        return Err("must be a JSON number or numeric string".to_string());
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
        .map(|_| ())
        .ok_or_else(|| "must be a finite float8 value".to_string())
}

fn literal_text(literal: &serde_json::Value, expected: &str) -> Result<String, String> {
    match literal {
        serde_json::Value::String(value) => Ok(value.clone()),
        serde_json::Value::Number(number) => Ok(number.to_string()),
        _ => Err(expected.to_string()),
    }
}

fn parse_decimal_literal(literal: &serde_json::Value) -> Result<DecimalLiteral, String> {
    let value = literal_text(literal, "must be a decimal JSON number or string")?;
    let unsigned = value.strip_prefix('-').unwrap_or(&value);
    if unsigned.is_empty() {
        return Err("must use canonical decimal grammar".to_string());
    }
    let (integral, fractional) = match unsigned.split_once('.') {
        Some((integral, fractional)) => (integral, Some(fractional)),
        None => (unsigned, None),
    };
    if !is_unsigned_integral_decimal(integral)
        || fractional.is_some_and(|fractional| {
            fractional.is_empty() || !fractional.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err("must use canonical decimal grammar".to_string());
    }
    let fractional = fractional.unwrap_or_default();
    Ok(DecimalLiteral {
        digits: format!("{integral}{fractional}"),
        fractional_digits: fractional.len(),
    })
}

fn is_integral_decimal(value: &str) -> bool {
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    !unsigned.is_empty() && is_unsigned_integral_decimal(unsigned)
}

fn is_unsigned_integral_decimal(value: &str) -> bool {
    matches!(value.as_bytes(), [b'0'])
        || (value
            .as_bytes()
            .first()
            .is_some_and(|byte| *byte >= b'1' && *byte <= b'9')
            && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn round_decimal_to_scale(value: &DecimalLiteral, scale: i16) -> String {
    let shift = i32::from(scale) - value.fractional_digits as i32;
    if shift >= 0 {
        let mut digits = value.digits.clone();
        digits.extend(std::iter::repeat_n('0', shift as usize));
        return trim_leading_zeroes(digits);
    }

    let discarded = (-shift) as usize;
    let kept = value.digits.len().saturating_sub(discarded);
    let should_round = discarded <= value.digits.len()
        && value
            .digits
            .as_bytes()
            .get(kept)
            .is_some_and(|digit| *digit >= b'5');
    let mut rounded = if kept == 0 {
        "0".to_string()
    } else {
        value.digits[..kept].to_string()
    };
    if should_round {
        rounded = increment_decimal_digits(rounded);
    }
    trim_leading_zeroes(rounded)
}

fn increment_decimal_digits(digits: String) -> String {
    let mut bytes = digits.into_bytes();
    for index in (0..bytes.len()).rev() {
        if bytes[index] == b'9' {
            bytes[index] = b'0';
        } else {
            bytes[index] += 1;
            return String::from_utf8(bytes).expect("decimal digits remain UTF-8");
        }
    }
    bytes.insert(0, b'1');
    String::from_utf8(bytes).expect("decimal digits remain UTF-8")
}

fn trim_leading_zeroes(value: String) -> String {
    let trimmed = value.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn timestamp_fractional_digits(value: &str, with_time_zone: bool) -> Result<u8, String> {
    let separator = value
        .find('T')
        .or_else(|| (!with_time_zone).then(|| value.find(' ')).flatten())
        .ok_or_else(|| "must include a date-time separator".to_string())?;
    let time_and_offset = &value[separator + 1..];
    let time = if with_time_zone {
        let end = time_and_offset
            .find(['Z', '+', '-'])
            .unwrap_or(time_and_offset.len());
        &time_and_offset[..end]
    } else {
        time_and_offset
    };
    let Some((_, fraction)) = time.rsplit_once('.') else {
        return Ok(0);
    };
    if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("has malformed fractional seconds".to_string());
    }
    u8::try_from(fraction.len()).map_err(|_| "has too many fractional-second digits".to_string())
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
    let (catalog, diagnostics) = compile_command_catalog_with_diagnostics(
        metadata,
        catalogs,
        rules,
        infer_function_permissions,
    );
    match diagnostics.into_iter().next() {
        Some(diagnostic) => Err(diagnostic),
        None => Ok(catalog),
    }
}

/// Collect every deploy-time command diagnostic in one compiler traversal.
///
/// `compile_command_catalog` retains its fail-closed `Result` API for
/// candidate-engine construction. Deployment validation uses this companion
/// to report independent invalid command definitions together instead of
/// repeatedly compiling altered metadata subsets.
pub fn validate_command_catalog(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    rules: &RuleCatalog,
    infer_function_permissions: bool,
) -> Vec<PlanError> {
    compile_command_catalog_with_diagnostics(metadata, catalogs, rules, infer_function_permissions)
        .1
}

fn compile_command_catalog_with_diagnostics(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    rules: &RuleCatalog,
    infer_function_permissions: bool,
) -> (CompiledCommandCatalog, Vec<PlanError>) {
    let mut sources = BTreeMap::new();
    for source in &metadata.sources {
        if source.kind == SourceKind::Postgres {
            sources.insert(source.name.clone(), CompiledSourceCommandCatalog::default());
        }
    }

    let mut names_by_source: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut diagnostics = Vec::new();
    for (index, command) in metadata.commands.iter().enumerate() {
        let path = format!("commands[{index}]");
        let Some(source) = metadata
            .sources
            .iter()
            .find(|source| source.name == command.source)
        else {
            diagnostics.push(PlanError::validation(
                &path,
                format!("command source '{}' does not exist", command.source),
            ));
            continue;
        };
        if source.kind != SourceKind::Postgres {
            diagnostics.push(PlanError::validation(
                &path,
                format!(
                    "command source '{}' requires a Postgres source",
                    command.source
                ),
            ));
            continue;
        }
        let seen = names_by_source.entry(source.name.as_str()).or_default();
        let duplicate_name = !seen.insert(command.name.as_str());
        if duplicate_name {
            diagnostics.push(PlanError::validation(
                &path,
                format!(
                    "duplicate command name '{}' for source '{}'",
                    command.name, source.name
                ),
            ));
        }
        if let Err(error) = validate_command(
            metadata,
            catalogs,
            source,
            rules,
            infer_function_permissions,
            command,
            index,
        ) {
            diagnostics.push(error);
            continue;
        }
        if !duplicate_name {
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
    }
    (CompiledCommandCatalog { sources }, diagnostics)
}

fn validate_command(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    source: &Source,
    rules: &RuleCatalog,
    infer_function_permissions: bool,
    command: &Command,
    command_index: usize,
) -> Result<(), PlanError> {
    let path = format!("commands[{command_index}]");
    let catalog = catalogs.get(&source.name).ok_or_else(|| {
        PlanError::validation(
            &path,
            format!("catalog for command source '{}' is missing", source.name),
        )
    })?;
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
    let actual = match value {
        CommandValue::Literal { literal } => {
            validate_command_literal(literal, column, path)?;
            // A descriptor has already proven the metadata value satisfies the
            // concrete database column. Preserve the established StaticType
            // assignment check for the rest of command compilation without
            // leaking PostgreSQL widths or modifiers into that public model.
            expected.clone()
        }
        _ => value_type(value, context, Some(&expected), ValueUse::Data, path)?,
    };
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

fn validate_command_literal(
    literal: &serde_json::Value,
    column: &ColumnInfo,
    path: &str,
) -> Result<(), PlanError> {
    let descriptor = CommandScalarDescriptor::from_column(column).map_err(|reason| {
        PlanError::validation(
            path,
            format!(
                "invalid literal for column '{}' with PostgreSQL column type '{}': {reason}",
                column.name, column.pg_type
            ),
        )
    })?;
    descriptor
        .validate(literal, column.nullable)
        .map_err(|reason| {
            PlanError::validation(
                path,
                format!(
                    "invalid literal for column '{}' with PostgreSQL column type '{}': {reason}",
                    column.name, column.pg_type
                ),
            )
        })
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
