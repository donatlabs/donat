//! Deploy-time compilation of declarative command metadata.
//!
//! The compiler is intentionally SQL-free and side-effect-free. It accepts
//! only the already compiled Rules catalog and immutable Postgres catalog
//! snapshots, so serving can consume its output without parsing YAML or
//! consulting mutable command definitions.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use donat_catalog::{Catalog, ColumnInfo, RelationKind, TableInfo};
use donat_ir::{
    TypeRef, ValueContractCatalog, ValueContractField, ValueScalar, ValueType,
    compile_value_contract_catalog,
};
use donat_metadata::{
    Columns, Command, CommandAggregate, CommandCondition, CommandEffect, CommandIdempotencyKey,
    CommandIdempotencyScope, CommandIdempotencyScopeSpec,
    CommandResultValue as MetadataCommandResultValue, CommandStepOperation, CommandValue, Metadata,
    QualifiedTable, Source, SourceKind, TableEntry, action_visible_to_role,
};
use donat_rules::{RuleCatalog, RuleType};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::introspection::build_schema_json;
use crate::naming::{command_pascal_case, table_base_name};
use crate::plan::{MutationKind, PlanError, Planner, Session, TableCtx};
use crate::predicate::PermissionSessionOperand;

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

    /// Iteration is intentionally source-local and deterministic. Request
    /// planning consumes this immutable snapshot; it never reads YAML.
    pub(crate) fn commands(&self) -> impl Iterator<Item = &CompiledCommand> {
        self.commands.values()
    }
}

/// A command definition accepted by the static compiler.
#[derive(Debug, Clone)]
pub struct CompiledCommand {
    source: String,
    definition: Command,
    descriptor: CommandDescriptor,
    rules: Arc<RuleCatalog>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub source: String,
    pub name: String,
    pub arguments: ValueContractCatalog,
    pub result: ValueContractCatalog,
    pub allowed_roles: BTreeSet<String>,
    pub required_session_variables: BTreeMap<String, BTreeMap<String, TypeRef>>,
    pub definition_fingerprint: String,
}

impl CompiledCommand {
    /// The source that owned this definition when the immutable catalog was
    /// compiled. The raw command name is only source-local.
    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    /// The trusted, immutable source definition. Request paths receive only a
    /// shared reference; metadata mutations never update this snapshot.
    pub fn definition(&self) -> &Command {
        &self.definition
    }

    pub fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    /// The immutable Rule catalog compiled with this command snapshot. It is
    /// available only to the request planner so SQLgen never reparses a rule
    /// name or source expression.
    pub(crate) fn rules(&self) -> &RuleCatalog {
        self.rules.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StaticType {
    Scalar(String),
    Object {
        name: String,
        fields: BTreeMap<String, StaticType>,
    },
    /// A back-edge to an input object already being expanded during
    /// deployment validation. It never leaves this static type checker.
    ObjectRef {
        name: String,
    },
    List(Box<StaticType>),
    Row(BTreeMap<String, StaticType>),
    Rows(BTreeMap<String, StaticType>),
    Nullable(Box<StaticType>),
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
    fn nullable(inner: Self) -> Self {
        if matches!(inner, Self::Nullable(_)) {
            inner
        } else {
            Self::Nullable(Box::new(inner))
        }
    }

    fn is_scalar(&self) -> bool {
        match self {
            Self::Scalar(_) => true,
            Self::Nullable(inner) => inner.is_scalar(),
            _ => false,
        }
    }

    fn scalar_name(&self) -> Option<&str> {
        match self {
            Self::Scalar(name) => Some(name),
            Self::Nullable(inner) => inner.scalar_name(),
            _ => None,
        }
    }

    fn display_name(&self) -> String {
        match self {
            Self::Scalar(name) => name.clone(),
            Self::Object { name, .. } | Self::ObjectRef { name } => format!("object {name}"),
            Self::List(item) => format!("list<{}>", item.display_name()),
            Self::Row(_) => "row".to_string(),
            Self::Rows(_) => "list<row>".to_string(),
            Self::Nullable(inner) => format!("nullable {}", inner.display_name()),
        }
    }
}

#[derive(Clone)]
struct StepOutput {
    fields: BTreeMap<String, StaticType>,
    many: bool,
    may_be_absent: bool,
    kind: StepOutputKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepOutputKind {
    Scalar,
    SelectMany,
    Aggregate,
    UpdateMany,
    ProjectMany,
    FixedRows,
    DecisionMany,
    Allocation,
}

#[derive(Clone, Copy)]
struct ValueContext<'a> {
    metadata: &'a Metadata,
    command: &'a Command,
    rules: &'a RuleCatalog,
    steps: &'a BTreeMap<String, StepOutput>,
    declared_steps: &'a HashSet<String>,
    item: Option<&'a BTreeMap<String, StaticType>>,
    current: Option<&'a BTreeMap<String, StaticType>>,
}

#[derive(Clone, Copy)]
enum ValueUse {
    Data,
    RuleBinding,
    Effect,
}

const MAX_COMMAND_ROWS: u32 = 256;

/// One generated GraphQL object name and the metadata declaration that owns
/// it. Runtime schema generation remains the single producer of fields;
/// deploy-time namespace validation must nevertheless retain every owner,
/// including declarations with equal structural shapes.
#[derive(Debug, Clone)]
struct GeneratedCommandType {
    name: String,
    path: String,
    origin: String,
}

struct ValidatedCommand<'a> {
    index: usize,
    source: &'a str,
    command: &'a Command,
}

/// A type already emitted into a role schema before command output types are
/// added. Diagnostics retain its metadata origin rather than exposing a
/// misleading generic GraphQL validation error later in introspection.
#[derive(Debug, Clone)]
struct ExistingGraphqlTypeOwner {
    origin: String,
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

pub fn compile_command_source_catalog(
    metadata: &Metadata,
    source_name: &str,
    catalog: &Catalog,
    rules: &RuleCatalog,
    infer_function_permissions: bool,
) -> Result<CompiledSourceCommandCatalog, PlanError> {
    let source = metadata
        .sources
        .iter()
        .find(|source| source.name == source_name)
        .ok_or_else(|| {
            PlanError::validation(
                "commands",
                format!("command source '{source_name}' does not exist"),
            )
        })?;
    if source.kind != SourceKind::Postgres {
        return Err(PlanError::validation(
            "commands",
            format!("command source '{source_name}' requires a Postgres source"),
        ));
    }
    let catalogs = HashMap::from([(source_name.to_owned(), catalog.clone())]);
    let rules = Arc::new(rules.clone());
    let context = CommandCompileContext {
        metadata,
        catalogs: &catalogs,
        rules: &rules,
        infer_function_permissions,
    };
    let mut commands = BTreeMap::new();
    for (index, command) in metadata.commands.iter().enumerate() {
        if command.source != source_name {
            continue;
        }
        let path = format!("commands[{index}]");
        if commands.contains_key(&command.name) {
            return Err(PlanError::validation(
                &path,
                format!(
                    "duplicate command name '{}' for source '{source_name}'",
                    command.name
                ),
            ));
        }
        let compiled = compile_one_command(&context, source, catalog, command, index)?;
        commands.insert(command.name.clone(), compiled);
    }
    Ok(CompiledSourceCommandCatalog { commands })
}

/// Both public compiler shapes use this primitive. The aggregate compiler
/// cannot call `compile_command_source_catalog` directly because that API
/// deliberately returns the first source-local error, while deployment
/// validation must retain independent diagnostics and cross-source namespace
/// collisions. Sharing the per-command primitive keeps descriptor bytes
/// identical without weakening aggregate diagnostics.
struct CommandCompileContext<'a> {
    metadata: &'a Metadata,
    catalogs: &'a HashMap<String, Catalog>,
    rules: &'a Arc<RuleCatalog>,
    infer_function_permissions: bool,
}

fn compile_one_command(
    context: &CommandCompileContext<'_>,
    source: &Source,
    catalog: &Catalog,
    command: &Command,
    command_index: usize,
) -> Result<CompiledCommand, PlanError> {
    validate_command(
        context.metadata,
        context.catalogs,
        source,
        context.rules.as_ref(),
        context.infer_function_permissions,
        command,
        command_index,
    )?;
    let descriptor = build_command_descriptor(
        context.metadata,
        source,
        catalog,
        context.rules.as_ref(),
        command,
        command_index,
        context.infer_function_permissions,
    )?;
    Ok(CompiledCommand {
        source: source.name.clone(),
        definition: command.clone(),
        descriptor,
        rules: context.rules.clone(),
    })
}

fn build_command_descriptor(
    metadata: &Metadata,
    source: &Source,
    catalog: &Catalog,
    rules: &RuleCatalog,
    command: &Command,
    command_index: usize,
    infer_function_permissions: bool,
) -> Result<CommandDescriptor, PlanError> {
    let path = format!("commands[{command_index}]");
    let argument_fields = command
        .arguments
        .iter()
        .map(|argument| (argument.name.clone(), argument.type_.clone()))
        .collect();
    let arguments = compile_value_contract_catalog(metadata, &argument_fields)
        .map_err(|error| PlanError::validation(&path, error.to_string()))?;

    let roles = command
        .permissions
        .iter()
        .map(|permission| permission.role.as_str())
        .collect::<HashSet<_>>();
    let declared_steps = command
        .steps
        .iter()
        .map(|step| step.name.clone())
        .collect::<HashSet<_>>();
    let mut steps = BTreeMap::new();
    for (index, step) in command.steps.iter().enumerate() {
        let context = ValueContext {
            metadata,
            command,
            rules,
            steps: &steps,
            declared_steps: &declared_steps,
            item: None,
            current: None,
        };
        let output = validate_step(
            source,
            catalog,
            &roles,
            step,
            &context,
            &format!("{path}.steps[{index}]"),
        )?;
        steps.insert(step.name.clone(), output);
    }
    let context = ValueContext {
        metadata,
        command,
        rules,
        steps: &steps,
        declared_steps: &declared_steps,
        item: None,
        current: None,
    };
    let mut result_roots = BTreeMap::new();
    for field in &command.result.fields {
        let static_type = result_value_type(&field.value, &context, &path)?;
        result_roots.insert(
            field.name.clone(),
            ValueContractField {
                required: true,
                type_ref: contract_type_from_static(&static_type, &path)?,
            },
        );
    }
    let result = ValueContractCatalog {
        roots: result_roots,
        named_objects: BTreeMap::new(),
    };
    let allowed_roles = command
        .permissions
        .iter()
        .map(|permission| permission.role.clone())
        .collect::<BTreeSet<_>>();
    let required_session_variables = collect_required_session_variables(
        metadata,
        source,
        catalog,
        command,
        &path,
        infer_function_permissions,
    )?;
    let definition_fingerprint = command_descriptor_fingerprint(
        source,
        command,
        rules,
        &arguments,
        &result,
        &allowed_roles,
        &required_session_variables,
    );
    Ok(CommandDescriptor {
        source: source.name.clone(),
        name: command.name.clone(),
        arguments,
        result,
        allowed_roles,
        required_session_variables,
        definition_fingerprint,
    })
}

fn contract_type_from_static(type_: &StaticType, path: &str) -> Result<TypeRef, PlanError> {
    let parsed = match type_ {
        StaticType::Scalar(name) => TypeRef::parse(name).map(|mut type_ref| {
            type_ref.nullable = false;
            type_ref
        }),
        StaticType::Object { name, .. } | StaticType::ObjectRef { name } => Ok(TypeRef {
            nullable: false,
            value_type: ValueType::Ref { name: name.clone() },
        }),
        StaticType::List(element) => Ok(TypeRef {
            nullable: false,
            value_type: ValueType::List {
                element: Box::new(contract_type_from_static(element, path)?),
            },
        }),
        StaticType::Row(fields) => Ok(TypeRef {
            nullable: false,
            value_type: ValueType::Object {
                fields: contract_fields_from_static(fields, path)?,
            },
        }),
        StaticType::Rows(fields) => Ok(TypeRef {
            nullable: false,
            value_type: ValueType::List {
                element: Box::new(TypeRef {
                    nullable: false,
                    value_type: ValueType::Object {
                        fields: contract_fields_from_static(fields, path)?,
                    },
                }),
            },
        }),
        StaticType::Nullable(inner) => {
            let mut type_ref = contract_type_from_static(inner, path)?;
            type_ref.nullable = true;
            Ok(type_ref)
        }
    };
    parsed.map_err(|error| PlanError::validation(path, error.to_string()))
}

fn contract_fields_from_static(
    fields: &BTreeMap<String, StaticType>,
    path: &str,
) -> Result<BTreeMap<String, ValueContractField>, PlanError> {
    fields
        .iter()
        .map(|(name, type_)| {
            Ok((
                name.clone(),
                ValueContractField {
                    required: true,
                    type_ref: contract_type_from_static(type_, path)?,
                },
            ))
        })
        .collect()
}

fn collect_required_session_variables(
    metadata: &Metadata,
    source: &Source,
    catalog: &Catalog,
    command: &Command,
    path: &str,
    infer_function_permissions: bool,
) -> Result<BTreeMap<String, BTreeMap<String, TypeRef>>, PlanError> {
    let mut planner = Planner::for_source(metadata, source, catalog);
    let declared_custom_scalars = metadata
        .custom_types
        .scalars
        .iter()
        .map(|scalar| scalar.name.as_str())
        .collect::<BTreeSet<_>>();
    // Command effects are table-only, so function-permission inference cannot
    // change these session contracts. Retain the caller's planner mode so the
    // descriptor path never constructs a differently configured snapshot.
    planner.infer_function_permissions = infer_function_permissions;
    let mut by_role = BTreeMap::new();

    for permission in &command.permissions {
        let role = permission.role.as_str();
        let mut required = BTreeMap::new();
        for step in &command.steps {
            let table = match &step.operation {
                CommandStepOperation::SelectOne { select_one } => &select_one.table,
                CommandStepOperation::SelectMany { select_many } => &select_many.table,
                CommandStepOperation::Insert { insert } => &insert.table,
                CommandStepOperation::InsertMany { insert_many } => &insert_many.table,
                CommandStepOperation::Update { update } => &update.table,
                CommandStepOperation::UpdateMany { update_many } => &update_many.table,
                CommandStepOperation::Delete { delete } => &delete.table,
                CommandStepOperation::UpdateWhen { update_when } => &update_when.table,
                CommandStepOperation::InsertWhen { insert_when } => &insert_when.table,
                CommandStepOperation::Aggregate { .. }
                | CommandStepOperation::Assert { .. }
                | CommandStepOperation::AssertWhen { .. }
                | CommandStepOperation::Decision { .. }
                | CommandStepOperation::DecisionMany { .. }
                | CommandStepOperation::Project { .. }
                | CommandStepOperation::ProjectMany { .. }
                | CommandStepOperation::FixedRows { .. }
                | CommandStepOperation::AllocateMany { .. } => continue,
            };
            let (entry, info) =
                if matches!(&step.operation, CommandStepOperation::SelectMany { .. }) {
                    command_read_target(source, catalog, table, path)?
                } else {
                    command_target(source, catalog, table, path)?
                };
            let filter_context = TableCtx {
                entry,
                info,
                perms: Vec::new(),
                type_name: table_base_name(entry),
            };
            if let Some(context) = planner.table_ctx_by_name(&entry.table, role) {
                for permission in context.perms {
                    collect_sessions_from_predicate(
                        &planner,
                        &permission.filter,
                        &filter_context,
                        role,
                        &mut required,
                        &declared_custom_scalars,
                        path,
                    )?;
                }
            }
            match &step.operation {
                CommandStepOperation::Insert { .. } | CommandStepOperation::InsertMany { .. } => {
                    if let Some(permission) =
                        planner.resolve_role_perm(&entry.insert_permissions, role, |permission| {
                            !permission.backend_only
                        })
                    {
                        collect_sessions_from_predicate(
                            &planner,
                            &permission.check,
                            &filter_context,
                            role,
                            &mut required,
                            &declared_custom_scalars,
                            path,
                        )?;
                        for (column, value) in &permission.set {
                            collect_session_preset(
                                value,
                                info.column(column),
                                role,
                                &mut required,
                                &declared_custom_scalars,
                                path,
                            )?;
                        }
                    }
                }
                CommandStepOperation::Update { .. }
                | CommandStepOperation::UpdateMany { .. }
                | CommandStepOperation::UpdateWhen { .. } => {
                    if let Some(permission) =
                        planner.resolve_role_perm(&entry.update_permissions, role, |_| true)
                    {
                        collect_sessions_from_predicate(
                            &planner,
                            &permission.filter,
                            &filter_context,
                            role,
                            &mut required,
                            &declared_custom_scalars,
                            path,
                        )?;
                        if let Some(check) = &permission.check {
                            collect_sessions_from_predicate(
                                &planner,
                                check,
                                &filter_context,
                                role,
                                &mut required,
                                &declared_custom_scalars,
                                path,
                            )?;
                        }
                        for (column, value) in &permission.set {
                            collect_session_preset(
                                value,
                                info.column(column),
                                role,
                                &mut required,
                                &declared_custom_scalars,
                                path,
                            )?;
                        }
                    }
                }
                CommandStepOperation::Delete { .. } => {
                    if let Some(permission) =
                        planner.resolve_role_perm(&entry.delete_permissions, role, |_| true)
                    {
                        collect_sessions_from_predicate(
                            &planner,
                            &permission.filter,
                            &filter_context,
                            role,
                            &mut required,
                            &declared_custom_scalars,
                            path,
                        )?;
                    }
                }
                CommandStepOperation::InsertWhen { .. } => {
                    if let Some(permission) =
                        planner.resolve_role_perm(&entry.insert_permissions, role, |permission| {
                            !permission.backend_only
                        })
                    {
                        collect_sessions_from_predicate(
                            &planner,
                            &permission.check,
                            &filter_context,
                            role,
                            &mut required,
                            &declared_custom_scalars,
                            path,
                        )?;
                        for (column, value) in &permission.set {
                            collect_session_preset(
                                value,
                                info.column(column),
                                role,
                                &mut required,
                                &declared_custom_scalars,
                                path,
                            )?;
                        }
                    }
                }
                CommandStepOperation::SelectOne { .. }
                | CommandStepOperation::SelectMany { .. }
                | CommandStepOperation::Aggregate { .. }
                | CommandStepOperation::Assert { .. }
                | CommandStepOperation::AssertWhen { .. }
                | CommandStepOperation::Decision { .. }
                | CommandStepOperation::DecisionMany { .. }
                | CommandStepOperation::Project { .. }
                | CommandStepOperation::ProjectMany { .. }
                | CommandStepOperation::FixedRows { .. }
                | CommandStepOperation::AllocateMany { .. } => {}
            }
        }

        if let Some(idempotency) = &command.idempotency {
            if let CommandIdempotencyScopeSpec::Values(scopes) = &idempotency.scope {
                for scope in scopes {
                    if let CommandIdempotencyScope::SessionVariable { session_variable } = scope {
                        insert_unconstrained_session_contract(&mut required, session_variable);
                    }
                }
            }
        }
        for effect in &command.effects {
            let values = match effect {
                CommandEffect::StartProcess { start_process } => {
                    start_process.input.values().collect::<Vec<_>>()
                }
                CommandEffect::SignalProcess { signal_process } => signal_process
                    .correlate
                    .values()
                    .chain(signal_process.payload.values())
                    .collect(),
            };
            let mut pending = values;
            while let Some(value) = pending.pop() {
                match value {
                    CommandValue::SessionVariable { session_variable } => {
                        insert_unconstrained_session_contract(&mut required, session_variable);
                    }
                    CommandValue::Rule { bindings, .. } => pending.extend(bindings.values()),
                    CommandValue::Argument { .. }
                    | CommandValue::Item { .. }
                    | CommandValue::Step { .. }
                    | CommandValue::Literal { .. }
                    | CommandValue::CurrentColumn { .. }
                    | CommandValue::DatabaseTime { .. } => {}
                }
            }
        }
        by_role.insert(permission.role.clone(), required);
    }
    Ok(by_role)
}

fn collect_sessions_from_predicate(
    planner: &Planner<'_>,
    value: &serde_json::Value,
    context: &TableCtx<'_>,
    role: &str,
    required: &mut BTreeMap<String, TypeRef>,
    declared_custom_scalars: &BTreeSet<&str>,
    path: &str,
) -> Result<(), PlanError> {
    for session_use in planner.collect_permission_session_uses(value, context, path)? {
        let contract = match session_use.operand {
            PermissionSessionOperand::Scalar(column) => {
                required_column_contract(&column, declared_custom_scalars, path)?
            }
            PermissionSessionOperand::List(column) => TypeRef {
                nullable: false,
                value_type: ValueType::List {
                    element: Box::new(required_column_contract(
                        &column,
                        declared_custom_scalars,
                        path,
                    )?),
                },
            },
            PermissionSessionOperand::Boolean => required_boolean_contract(),
            PermissionSessionOperand::String => required_string_contract(),
            PermissionSessionOperand::StringList => TypeRef {
                nullable: false,
                value_type: ValueType::List {
                    element: Box::new(required_string_contract()),
                },
            },
            PermissionSessionOperand::Decimal => required_decimal_contract(),
        };
        insert_session_contract(required, role, &session_use.name, contract, path)?;
    }
    Ok(())
}

fn collect_session_preset(
    value: &serde_json::Value,
    column: Option<&ColumnInfo>,
    role: &str,
    required: &mut BTreeMap<String, TypeRef>,
    declared_custom_scalars: &BTreeSet<&str>,
    path: &str,
) -> Result<(), PlanError> {
    let serde_json::Value::String(name) = value else {
        return Ok(());
    };
    if !session_variable_name(name) {
        return Ok(());
    }
    let column = column
        .ok_or_else(|| PlanError::validation(path, "permission preset names an unknown column"))?;
    insert_session_contract(
        required,
        role,
        name,
        required_column_contract(column, declared_custom_scalars, path)?,
        path,
    )
}

fn required_column_contract(
    column: &ColumnInfo,
    declared_custom_scalars: &BTreeSet<&str>,
    path: &str,
) -> Result<TypeRef, PlanError> {
    let scalar = match column.pg_type.as_str() {
        "bool" => ValueScalar::Boolean,
        "int2" | "int4" | "serial" => ValueScalar::Int32,
        "int8" | "bigint" | "bigserial" => ValueScalar::Int64,
        "float4" | "float8" | "numeric" | "decimal" => ValueScalar::Decimal,
        "uuid" => ValueScalar::Uuid,
        "date" => ValueScalar::Date,
        "timestamp" | "timestamp without time zone" => ValueScalar::Timestamp,
        "timestamptz" | "timestamp with time zone" => ValueScalar::TimestampTz,
        "json" | "jsonb" => ValueScalar::Json,
        "text" | "varchar" | "bpchar" | "name" | "citext" => ValueScalar::String,
        name if declared_custom_scalars.contains(name) => ValueScalar::Custom {
            name: name.to_owned(),
        },
        name => {
            return Err(PlanError::validation(
                path,
                format!("column scalar '{name}' has no closed session-variable contract"),
            ));
        }
    };
    Ok(TypeRef {
        nullable: false,
        value_type: ValueType::Scalar { scalar },
    })
}

fn required_boolean_contract() -> TypeRef {
    TypeRef {
        nullable: false,
        value_type: ValueType::Scalar {
            scalar: ValueScalar::Boolean,
        },
    }
}

fn required_decimal_contract() -> TypeRef {
    TypeRef {
        nullable: false,
        value_type: ValueType::Scalar {
            scalar: ValueScalar::Decimal,
        },
    }
}

fn insert_session_contract(
    required: &mut BTreeMap<String, TypeRef>,
    role: &str,
    name: &str,
    contract: TypeRef,
    path: &str,
) -> Result<(), PlanError> {
    let name = name.to_ascii_lowercase();
    if let Some(existing) = required.get(&name)
        && existing != &contract
    {
        return Err(PlanError::validation(
            path,
            format!("session variable '{name}' has incompatible contracts for role '{role}'"),
        ));
    }
    required.insert(name, contract);
    Ok(())
}

fn insert_unconstrained_session_contract(required: &mut BTreeMap<String, TypeRef>, name: &str) {
    let name = name.to_ascii_lowercase();
    required
        .entry(name)
        .or_insert_with(required_string_contract);
}

fn required_string_contract() -> TypeRef {
    TypeRef {
        nullable: false,
        value_type: ValueType::Scalar {
            scalar: ValueScalar::String,
        },
    }
}

fn session_variable_name(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.starts_with("x-donat-") || value.starts_with("x-hasura-")
}

fn command_descriptor_fingerprint(
    source: &Source,
    command: &Command,
    rules: &RuleCatalog,
    arguments: &ValueContractCatalog,
    result: &ValueContractCatalog,
    allowed_roles: &BTreeSet<String>,
    required_session_variables: &BTreeMap<String, BTreeMap<String, TypeRef>>,
) -> String {
    let rule_hashes = referenced_rule_hashes(command, rules);
    let required_sessions = required_session_variables
        .iter()
        .map(|(role, values)| {
            (
                role.clone(),
                values
                    .iter()
                    .map(|(name, type_ref)| (name.clone(), type_tokens(type_ref)))
                    .collect::<BTreeMap<_, _>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let canonical = serde_json::json!({
        "format": "donat.command-descriptor.v1",
        "source": source.name,
        "name": command.name,
        "arguments": contract_records(arguments),
        "result": contract_records(result),
        "allowed_roles": allowed_roles,
        "required_session_variables": required_sessions,
        "guards": command.guards,
        "steps": command.steps,
        "rule_artifact_hashes": rule_hashes,
        "idempotency": command.idempotency,
        "effects": command.effects,
    });
    let canonical = canonicalize_fingerprint_json(canonical);
    let bytes = serde_json::to_vec(&canonical).expect("canonical command record serializes");
    format!("{:x}", Sha256::digest(bytes))
}

fn canonicalize_fingerprint_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(canonicalize_fingerprint_json)
                .collect(),
        ),
        serde_json::Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(name, value)| (name, canonicalize_fingerprint_json(value)))
                    .collect(),
            )
        }
        scalar => scalar,
    }
}

fn contract_records(catalog: &ValueContractCatalog) -> Vec<serde_json::Value> {
    let mut records = Vec::new();
    for (name, field) in &catalog.roots {
        records.push(serde_json::json!({
            "scope": "root",
            "name": name,
            "required": field.required,
            "type": type_tokens(&field.type_ref),
        }));
    }
    for (object_name, object) in &catalog.named_objects {
        for (name, field) in &object.fields {
            records.push(serde_json::json!({
                "scope": "named_object",
                "object": object_name,
                "name": name,
                "required": field.required,
                "type": type_tokens(&field.type_ref),
            }));
        }
    }
    records
}

fn type_tokens(root: &TypeRef) -> Vec<String> {
    enum Event<'a> {
        Type(&'a TypeRef),
        Field(&'a str, &'a ValueContractField),
    }
    let mut tokens = Vec::new();
    let mut pending = vec![Event::Type(root)];
    while let Some(event) = pending.pop() {
        match event {
            Event::Field(name, field) => {
                tokens.push(String::from("field"));
                tokens.push(name.to_owned());
                tokens.push(
                    if field.required {
                        "required"
                    } else {
                        "optional"
                    }
                    .to_owned(),
                );
                pending.push(Event::Type(&field.type_ref));
            }
            Event::Type(type_ref) => {
                tokens.push(
                    if type_ref.nullable {
                        "nullable"
                    } else {
                        "non_null"
                    }
                    .to_owned(),
                );
                match &type_ref.value_type {
                    ValueType::Scalar { scalar } => match scalar {
                        ValueScalar::Custom { name } => {
                            tokens.push(String::from("custom"));
                            tokens.push(name.clone());
                        }
                        scalar => {
                            tokens.push(String::from("scalar"));
                            tokens.push(value_scalar_token(scalar).to_owned());
                        }
                    },
                    ValueType::Enum { name, values } => {
                        tokens.push(String::from("enum"));
                        tokens.push(name.clone());
                        tokens.push(values.len().to_string());
                        tokens.extend(values.iter().cloned());
                    }
                    ValueType::Object { fields } => {
                        tokens.push(String::from("object"));
                        tokens.push(fields.len().to_string());
                        for (name, field) in fields.iter().rev() {
                            pending.push(Event::Field(name, field));
                        }
                    }
                    ValueType::List { element } => {
                        tokens.push(String::from("list"));
                        pending.push(Event::Type(element));
                    }
                    ValueType::Ref { name } => {
                        tokens.push(String::from("ref"));
                        tokens.push(name.clone());
                    }
                }
            }
        }
    }
    tokens
}

fn value_scalar_token(scalar: &ValueScalar) -> &'static str {
    match scalar {
        ValueScalar::Boolean => "boolean",
        ValueScalar::String => "string",
        ValueScalar::Int32 => "int32",
        ValueScalar::Int64 => "int64",
        ValueScalar::UInt64 => "uint64",
        ValueScalar::Decimal => "decimal",
        ValueScalar::Uuid => "uuid",
        ValueScalar::Date => "date",
        ValueScalar::Timestamp => "timestamp",
        ValueScalar::TimestampTz => "timestamptz",
        ValueScalar::Json => "json",
        ValueScalar::Custom { .. } => {
            unreachable!("custom scalars retain their declared name as a separate token")
        }
    }
}

fn referenced_rule_hashes(command: &Command, rules: &RuleCatalog) -> BTreeMap<String, String> {
    let mut names = BTreeSet::new();
    let mut pending = Vec::new();
    for guard in &command.guards {
        names.insert(guard.rule.clone());
        pending.extend(guard.bindings.values());
    }
    for step in &command.steps {
        match &step.operation {
            CommandStepOperation::SelectOne { select_one } => {
                pending.extend(select_one.by.values());
            }
            CommandStepOperation::SelectMany { select_many } => {
                pending.extend(select_many.by.values());
            }
            CommandStepOperation::Insert { insert } => pending.extend(insert.object.values()),
            CommandStepOperation::InsertMany { insert_many } => {
                pending.push(&insert_many.for_each);
                pending.extend(insert_many.object.values());
            }
            CommandStepOperation::Update { update } => {
                pending.extend(update.predicate.values());
                pending.extend(update.set.values());
            }
            CommandStepOperation::UpdateMany { update_many } => {
                pending.push(&update_many.for_each);
                pending.extend(update_many.by.values());
                pending.extend(update_many.set.values());
                if let Some(check) = &update_many.check {
                    names.insert(check.rule.clone());
                    pending.extend(check.bindings.values());
                }
            }
            CommandStepOperation::Delete { delete } => pending.extend(delete.predicate.values()),
            CommandStepOperation::Aggregate { aggregate } => pending.push(&aggregate.from),
            CommandStepOperation::Assert { assert } => {
                names.insert(assert.rule.clone());
                pending.extend(assert.bindings.values());
            }
            CommandStepOperation::Project { project } => pending.extend(project.values.values()),
            CommandStepOperation::ProjectMany { project_many } => {
                pending.push(&project_many.from);
                pending.extend(project_many.values.values());
            }
            CommandStepOperation::FixedRows { fixed_rows } => {
                for row in &fixed_rows.rows {
                    pending.extend(row.values());
                }
            }
            CommandStepOperation::Decision { decision } => {
                pending.extend(decision.input.values());
            }
            CommandStepOperation::DecisionMany { decision_many } => {
                pending.push(&decision_many.from);
                pending.extend(decision_many.input.values());
            }
            CommandStepOperation::AssertWhen { assert_when } => {
                names.insert(assert_when.rule.clone());
                pending.extend(assert_when.bindings.values());
            }
            CommandStepOperation::UpdateWhen { update_when } => {
                pending.extend(update_when.predicate.values());
                pending.extend(update_when.set.values());
            }
            CommandStepOperation::InsertWhen { insert_when } => {
                pending.extend(insert_when.object.values());
            }
            CommandStepOperation::AllocateMany { allocate_many } => {
                pending.push(&allocate_many.from);
                pending.push(&allocate_many.request_id);
            }
        }
    }
    for field in &command.result.fields {
        match &field.value {
            MetadataCommandResultValue::Rule { rule, bindings } => {
                names.insert(rule.clone());
                pending.extend(bindings.values());
            }
            MetadataCommandResultValue::Argument { .. }
            | MetadataCommandResultValue::Literal { .. }
            | MetadataCommandResultValue::SessionVariable { .. }
            | MetadataCommandResultValue::CurrentColumn { .. }
            | MetadataCommandResultValue::Step { .. }
            | MetadataCommandResultValue::ProjectedStep { .. }
            | MetadataCommandResultValue::Array(_) => {}
        }
    }
    for effect in &command.effects {
        match effect {
            CommandEffect::StartProcess { start_process } => {
                pending.extend(start_process.input.values());
            }
            CommandEffect::SignalProcess { signal_process } => {
                pending.extend(signal_process.correlate.values());
                pending.extend(signal_process.payload.values());
            }
        }
    }
    while let Some(value) = pending.pop() {
        if let CommandValue::Rule { rule, bindings } = value {
            names.insert(rule.clone());
            pending.extend(bindings.values());
        }
    }
    names
        .into_iter()
        .filter_map(|name| {
            rules
                .rule(&name)
                .map(|rule| (name, rule.artifact.canonical_ast_sha256.clone()))
        })
        .collect()
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
    let rules_snapshot = Arc::new(rules.clone());
    let mut sources = BTreeMap::new();
    for source in &metadata.sources {
        if source.kind == SourceKind::Postgres {
            sources.insert(source.name.clone(), CompiledSourceCommandCatalog::default());
        }
    }

    let mut names_by_source: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut diagnostics = Vec::new();
    let mut validated = Vec::new();
    let context = CommandCompileContext {
        metadata,
        catalogs,
        rules: &rules_snapshot,
        infer_function_permissions,
    };
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
        let Some(catalog) = catalogs.get(&source.name) else {
            diagnostics.push(PlanError::validation(
                &path,
                format!("catalog for command source '{}' is missing", source.name),
            ));
            continue;
        };
        let compiled = match compile_one_command(&context, source, catalog, command, index) {
            Ok(compiled) => compiled,
            Err(error) => {
                diagnostics.push(error);
                continue;
            }
        };
        if !duplicate_name {
            sources
                .get_mut(&source.name)
                .expect("Postgres command source was initialized")
                .commands
                .insert(command.name.clone(), compiled);
            validated.push(ValidatedCommand {
                index,
                source: &source.name,
                command,
            });
        }
    }
    diagnostics.extend(validate_role_visible_command_collisions(
        metadata,
        &validated,
        catalogs,
        infer_function_permissions,
    ));
    (CompiledCommandCatalog { sources }, diagnostics)
}

/// Validate command-only names after the individual command compiler has
/// accepted their source-local definitions. Unlike the source-independent
/// composite ownership index, these checks project each command through the
/// roles that can actually see it. That keeps a candidate deployment honest
/// without rejecting two source-local commands that live in disjoint schemas.
fn validate_role_visible_command_collisions(
    metadata: &Metadata,
    commands: &[ValidatedCommand<'_>],
    catalogs: &HashMap<String, Catalog>,
    infer_function_permissions: bool,
) -> Vec<PlanError> {
    let mut diagnostics = Vec::new();
    let mut roots: HashMap<(String, String), &ValidatedCommand<'_>> = HashMap::new();
    let mut generated_types: HashMap<(String, String), GeneratedCommandType> = HashMap::new();
    let mut existing_types_by_role = HashMap::new();

    for command in commands {
        let generated = generated_command_types(command);
        for permission in &command.command.permissions {
            let role = permission.role.clone();
            let existing_types = existing_types_by_role
                .entry(role.clone())
                .or_insert_with(|| {
                    role_visible_existing_type_owners(
                        metadata,
                        commands,
                        catalogs,
                        &role,
                        infer_function_permissions,
                    )
                });
            let root_key = (role.clone(), command.command.name.clone());
            if let Some(existing) = roots.get(&root_key)
                && existing.source != command.source
            {
                let path = format!("commands[{}]", command.index);
                diagnostics.push(PlanError::validation(
                    &path,
                    format!(
                        "command root '{}' is visible to role '{}' in both commands[{}] (source '{}') and commands[{}] (source '{}')",
                        command.command.name,
                        role,
                        existing.index,
                        existing.source,
                        command.index,
                        command.source,
                    ),
                ));
            } else {
                roots.insert(root_key, command);
            }

            for generated_type in &generated {
                if !is_graphql_name(&generated_type.name) {
                    diagnostics.push(PlanError::validation(
                        &generated_type.path,
                        format!(
                            "generated command type '{}' for role '{}' at {} is not a valid GraphQL name",
                            generated_type.name, role, generated_type.origin,
                        ),
                    ));
                    continue;
                }
                let type_key = (role.clone(), generated_type.name.clone());
                if let Some(existing) = generated_types.get(&type_key) {
                    diagnostics.push(PlanError::validation(
                        &generated_type.path,
                        format!(
                            "generated command type '{}' is visible to role '{}' in both {} and {}",
                            generated_type.name, role, existing.origin, generated_type.origin,
                        ),
                    ));
                } else if let Some(existing) = existing_types.get(&generated_type.name) {
                    diagnostics.push(PlanError::validation(
                        &generated_type.path,
                        format!(
                            "generated command type '{}' is visible to role '{}' in {} and {}",
                            generated_type.name, role, generated_type.origin, existing.origin,
                        ),
                    ));
                } else {
                    generated_types.insert(type_key, generated_type.clone());
                }
            }
        }
    }

    diagnostics
}

fn generated_command_types(command: &ValidatedCommand<'_>) -> Vec<GeneratedCommandType> {
    let mut types = vec![GeneratedCommandType {
        name: format!("{}Result", command_pascal_case(&command.command.name)),
        path: format!("commands[{}]", command.index),
        origin: format!("commands[{}] (source '{}')", command.index, command.source),
    }];
    let mut emitted_steps = BTreeSet::new();
    for field in &command.command.result.fields {
        let step = match &field.value {
            MetadataCommandResultValue::Step {
                step,
                column: None,
                field: None,
                ..
            }
            | MetadataCommandResultValue::ProjectedStep { step, .. } => step,
            _ => continue,
        };
        let (step_index, step) = command
            .command
            .steps
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.name == *step)
            .expect("the static command compiler retains only declared result steps");
        if !emitted_steps.insert(step_index) {
            continue;
        }
        let path = format!("commands[{}].steps[{step_index}]", command.index);
        types.push(GeneratedCommandType {
            name: format!(
                "{}{}Row",
                command_pascal_case(&command.command.name),
                command_pascal_case(&step.name)
            ),
            path: path.clone(),
            origin: format!("{path} (step '{}')", step.name),
        });
    }
    types.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    types
}

/// Collect the pre-command type namespace for one role. The command-free
/// schema build is deploy-time only: it consumes immutable metadata and
/// catalog snapshots, never a request, a database connection, or command
/// definitions. This keeps table visibility exactly aligned with runtime
/// introspection, including inherited permissions and backend capabilities.
fn role_visible_existing_type_owners(
    metadata: &Metadata,
    commands: &[ValidatedCommand<'_>],
    catalogs: &HashMap<String, Catalog>,
    role: &str,
    infer_function_permissions: bool,
) -> BTreeMap<String, ExistingGraphqlTypeOwner> {
    let mut owners = BTreeMap::new();
    let session = Session {
        role: role.to_string(),
        vars: HashMap::new(),
        backend_request: false,
    };

    for (source_index, source) in metadata.sources.iter().enumerate() {
        let Some(catalog) = catalogs.get(&source.name) else {
            continue;
        };
        let mut planner = Planner::for_source(metadata, source, catalog);
        planner.infer_function_permissions = infer_function_permissions;
        let schema = build_schema_json(&planner, &session);
        for type_ in schema["types"].as_array().into_iter().flatten() {
            let Some(name) = type_["name"].as_str() else {
                continue;
            };
            owners
                .entry(name.to_string())
                .or_insert_with(|| ExistingGraphqlTypeOwner {
                    origin: source_type_owner(source_index, source, &planner, role, name),
                });
        }
    }

    add_visible_custom_type_owners(metadata, commands, role, &mut owners);
    owners
}

fn source_type_owner(
    source_index: usize,
    source: &Source,
    planner: &Planner,
    role: &str,
    name: &str,
) -> String {
    if let Some((table_index, _)) = source.tables.iter().enumerate().find(|(index, table)| {
        planner.table_ctx(*index, role).is_some() && table_base_name(table) == name
    }) {
        return format!(
            "sources[{source_index}].tables[{table_index}] (source '{}')",
            source.name
        );
    }
    format!("sources[{source_index}] (source '{}')", source.name)
}

/// Custom types are added by command/action roots, not by the ordinary table
/// schema builder. Follow only references reachable from roots visible to this
/// role, so two genuinely disjoint role schemas remain independent.
fn add_visible_custom_type_owners(
    metadata: &Metadata,
    commands: &[ValidatedCommand<'_>],
    role: &str,
    owners: &mut BTreeMap<String, ExistingGraphqlTypeOwner>,
) {
    let mut pending = Vec::new();
    for command in commands {
        if command
            .command
            .permissions
            .iter()
            .any(|permission| permission.role == role)
        {
            pending.extend(
                command
                    .command
                    .arguments
                    .iter()
                    .map(|argument| (argument.type_.as_str(), None)),
            );
        }
    }
    for (action_index, action) in metadata.actions.iter().enumerate() {
        if action_visible_to_role(action, role) {
            let action_origin = format!("actions[{action_index}] (action '{}')", action.name);
            pending.extend(
                action
                    .definition
                    .arguments
                    .iter()
                    .map(|argument| (argument.type_.as_str(), Some(action_origin.clone()))),
            );
            pending.push((action.definition.output_type.as_str(), Some(action_origin)));
        }
    }

    let mut seen = HashSet::new();
    while let Some((type_, root_origin)) = pending.pop() {
        let name = graphql_named_type_name(type_);
        if !seen.insert(name.to_string()) {
            continue;
        }
        if let Some((index, input)) = metadata
            .custom_types
            .input_objects
            .iter()
            .enumerate()
            .find(|(_, input)| input.name == name)
        {
            owners
                .entry(name.to_string())
                .or_insert_with(|| ExistingGraphqlTypeOwner {
                    origin: custom_type_owner_origin(
                        root_origin.as_deref(),
                        format!("custom_types.input_objects[{index}]"),
                    ),
                });
            pending.extend(
                input
                    .fields
                    .iter()
                    .map(|field| (field.type_.as_str(), root_origin.clone())),
            );
            continue;
        }
        if let Some((index, object)) = metadata
            .custom_types
            .objects
            .iter()
            .enumerate()
            .find(|(_, object)| object.name == name)
        {
            owners
                .entry(name.to_string())
                .or_insert_with(|| ExistingGraphqlTypeOwner {
                    origin: custom_type_owner_origin(
                        root_origin.as_deref(),
                        format!("custom_types.objects[{index}]"),
                    ),
                });
            pending.extend(
                object
                    .fields
                    .iter()
                    .map(|field| (field.type_.as_str(), root_origin.clone())),
            );
            continue;
        }
        if let Some((index, _)) = metadata
            .custom_types
            .enums
            .iter()
            .enumerate()
            .find(|(_, enum_)| enum_.name == name)
        {
            owners
                .entry(name.to_string())
                .or_insert_with(|| ExistingGraphqlTypeOwner {
                    origin: custom_type_owner_origin(
                        root_origin.as_deref(),
                        format!("custom_types.enums[{index}]"),
                    ),
                });
            continue;
        }
        if let Some((index, _)) = metadata
            .custom_types
            .scalars
            .iter()
            .enumerate()
            .find(|(_, scalar)| scalar.name == name)
        {
            owners
                .entry(name.to_string())
                .or_insert_with(|| ExistingGraphqlTypeOwner {
                    origin: custom_type_owner_origin(
                        root_origin.as_deref(),
                        format!("custom_types.scalars[{index}]"),
                    ),
                });
        }
    }
}

fn custom_type_owner_origin(root_origin: Option<&str>, type_origin: String) -> String {
    root_origin
        .map(|origin| format!("{origin} -> {type_origin}"))
        .unwrap_or(type_origin)
}

fn graphql_named_type_name(type_: &str) -> &str {
    let type_ = type_.strip_suffix('!').unwrap_or(type_);
    type_
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .map(graphql_named_type_name)
        .unwrap_or(type_)
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
            current: None,
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
        current: None,
    };
    validate_idempotency_step_scopes(command, &context, &path)?;
    for (index, guard) in command.guards.iter().enumerate() {
        validate_guard_precondition_bindings(&guard.bindings, &format!("{path}.guards[{index}]"))?;
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

fn validate_idempotency_step_scopes(
    command: &Command,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<(), PlanError> {
    let Some(idempotency) = &command.idempotency else {
        return Ok(());
    };
    let CommandIdempotencyScopeSpec::Values(scopes) = &idempotency.scope else {
        return Ok(());
    };
    for scope in scopes {
        let CommandIdempotencyScope::Step { step, column } = scope else {
            continue;
        };
        let output = context.steps.get(step).ok_or_else(|| {
            PlanError::validation(path, format!("unknown idempotency scope step '{step}'"))
        })?;
        if output.many {
            return Err(PlanError::validation(
                path,
                "idempotency step scope must reference one scalar row",
            ));
        }
        let type_ = output.fields.get(column).ok_or_else(|| {
            PlanError::validation(
                path,
                format!("idempotency scope step '{step}' has no field '{column}'"),
            )
        })?;
        if !type_.is_scalar() {
            return Err(PlanError::validation(
                path,
                "idempotency step scope must reference one scalar field",
            ));
        }
    }
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
                may_be_absent: false,
                kind: StepOutputKind::Scalar,
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
                may_be_absent: !select_one.require_found,
                kind: StepOutputKind::Scalar,
            })
        }
        CommandStepOperation::SelectMany { select_many } => {
            let (entry, info) = command_read_target(source, catalog, &select_many.table, path)?;
            if select_many.by.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "select_many requires at least one equality binding",
                ));
            }
            validate_object(&select_many.by, info, context, path)?;
            if select_many.order_by.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "select_many requires a non-empty declared total order",
                ));
            }
            let mut seen_order = HashSet::new();
            for column in &select_many.order_by {
                if !seen_order.insert(column) {
                    return Err(PlanError::validation(
                        path,
                        format!("duplicate order column '{column}' in select_many"),
                    ));
                }
                if info.column(column).is_none() {
                    return Err(PlanError::validation(
                        path,
                        format!("unknown order column '{column}' on select_many target"),
                    ));
                }
            }
            let returning = returning_columns(&select_many.returning, info, path)?;
            require_select_permissions(
                &planner,
                entry,
                info,
                roles,
                select_many.by.keys().chain(select_many.order_by.iter()),
                returning.keys(),
                path,
            )?;
            Ok(StepOutput {
                fields: returning,
                many: true,
                may_be_absent: false,
                kind: StepOutputKind::SelectMany,
            })
        }
        CommandStepOperation::Aggregate { aggregate } => {
            let input = prior_select_many_output(&aggregate.from, context, "aggregate", path)?;
            if aggregate.values.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "aggregate must declare at least one output value",
                ));
            }
            let fields = validate_command_aggregates(&aggregate.values, &input.fields, path)?;
            Ok(StepOutput {
                fields,
                many: false,
                may_be_absent: false,
                kind: StepOutputKind::Aggregate,
            })
        }
        CommandStepOperation::Insert { insert } => {
            let (entry, info) = command_target(source, catalog, &insert.table, path)?;
            if insert.object.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "insert object must contain at least one column assignment",
                ));
            }
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
                may_be_absent: false,
                kind: StepOutputKind::Scalar,
            })
        }
        CommandStepOperation::InsertMany { insert_many } => {
            let (entry, info) = command_target(source, catalog, &insert_many.table, path)?;
            if insert_many.object.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "insert_many object must contain at least one column assignment",
                ));
            }
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
                may_be_absent: false,
                kind: StepOutputKind::Scalar,
            })
        }
        CommandStepOperation::Update { update } => {
            let (entry, info) = command_target(source, catalog, &update.table, path)?;
            validate_primary_key_predicate(&update.predicate, info, context, path)?;
            if update.set.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "update set must contain at least one column assignment",
                ));
            }
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
                may_be_absent: !update.require_affected,
                kind: StepOutputKind::Scalar,
            })
        }
        CommandStepOperation::UpdateMany { update_many } => {
            let (entry, info) = command_read_target(source, catalog, &update_many.table, path)?;
            if info.relation_kind != RelationKind::Table {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "update_many target '{}.{}' must be an ordinary table, not {:?}",
                        info.schema, info.name, info.relation_kind
                    ),
                ));
            }
            if info.primary_key.is_empty() {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "update_many target '{}.{}' requires a primary key",
                        info.schema, info.name
                    ),
                ));
            }
            let input_fields = update_many_item_fields(&update_many.for_each, context, path)?;
            let supplied = update_many.by.keys().collect::<BTreeSet<_>>();
            let required = info.primary_key.iter().collect::<BTreeSet<_>>();
            if supplied != required {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "update_many requires every primary-key column ({})",
                        info.primary_key.join(", ")
                    ),
                ));
            }
            let mut input_keys = HashSet::new();
            for (target, value) in &update_many.by {
                let CommandValue::Item { item } = value else {
                    return Err(PlanError::validation(
                        path,
                        "update_many primary-key assignments must use current input item fields",
                    ));
                };
                if !input_keys.insert(item) {
                    return Err(PlanError::validation(
                        path,
                        format!("duplicate input key '{item}' in update_many primary-key mapping"),
                    ));
                }
                let actual = input_fields.get(item).ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!("update_many input does not expose item field '{item}'"),
                    )
                })?;
                let target = info
                    .column(target)
                    .expect("complete primary-key names came from the catalog");
                let expected = column_type(target);
                if !assignable(actual, &expected) {
                    return Err(PlanError::validation(
                        path,
                        format!(
                            "{} is not assignable to primary-key column '{}' ({})",
                            actual.display_name(),
                            target.name,
                            expected.display_name()
                        ),
                    ));
                }
            }
            if update_many.set.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "update_many set must contain at least one column assignment",
                ));
            }
            let current_fields = info
                .columns
                .iter()
                .map(|column| (column.name.clone(), column_type(column)))
                .collect::<BTreeMap<_, _>>();
            let update_context = ValueContext {
                item: Some(&input_fields),
                current: Some(&current_fields),
                ..*context
            };
            validate_object(&update_many.set, info, &update_context, path)?;
            if let Some(check) = &update_many.check {
                validate_rule(
                    &check.rule,
                    &check.bindings,
                    &update_context,
                    path,
                    Some(&StaticType::Scalar("Boolean".to_owned())),
                )?;
            }
            let returning = returning_columns(&update_many.returning, info, path)?;
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
                    update_many.set.keys(),
                    role,
                    "update",
                    info,
                    path,
                )?;
            }
            let mut read_columns = update_many.by.keys().cloned().collect::<BTreeSet<_>>();
            collect_current_columns_from_values(update_many.set.values(), &mut read_columns);
            if let Some(check) = &update_many.check {
                collect_current_columns_from_values(check.bindings.values(), &mut read_columns);
            }
            require_select_permissions(
                &planner,
                entry,
                info,
                roles,
                read_columns.iter(),
                returning.keys(),
                path,
            )?;
            Ok(StepOutput {
                fields: returning,
                many: true,
                may_be_absent: false,
                kind: StepOutputKind::UpdateMany,
            })
        }
        CommandStepOperation::Project { project } => {
            let fields = validate_projection_values(&project.values, context, path)?;
            Ok(StepOutput {
                fields,
                many: false,
                may_be_absent: false,
                kind: StepOutputKind::Scalar,
            })
        }
        CommandStepOperation::ProjectMany { project_many } => {
            validate_row_bound(project_many.maximum_rows, "project_many", path)?;
            let input = prior_row_set_output(&project_many.from, context, "project_many", path)?;
            let item_context = ValueContext {
                item: Some(&input.fields),
                ..*context
            };
            let fields = validate_projection_values(&project_many.values, &item_context, path)?;
            Ok(StepOutput {
                fields,
                many: true,
                may_be_absent: false,
                kind: StepOutputKind::ProjectMany,
            })
        }
        CommandStepOperation::FixedRows { fixed_rows } => {
            validate_row_bound(fixed_rows.maximum_rows, "fixed_rows", path)?;
            if fixed_rows.rows.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "fixed_rows must declare at least one row",
                ));
            }
            if fixed_rows.rows.len() > fixed_rows.maximum_rows as usize {
                return Err(PlanError::validation(
                    path,
                    "fixed_rows row count exceeds maximum_rows",
                ));
            }
            let fields = validate_fixed_rows(&fixed_rows.rows, context, path)?;
            Ok(StepOutput {
                fields,
                many: true,
                may_be_absent: false,
                kind: StepOutputKind::FixedRows,
            })
        }
        CommandStepOperation::Decision { decision } => {
            let fields = validate_decision(
                &decision.decision_table,
                &decision.input,
                &decision.returning,
                context,
                path,
            )?;
            Ok(StepOutput {
                fields,
                many: false,
                may_be_absent: false,
                kind: StepOutputKind::Scalar,
            })
        }
        CommandStepOperation::DecisionMany { decision_many } => {
            let input = prior_row_set_output(&decision_many.from, context, "decision_many", path)?;
            let item_context = ValueContext {
                item: Some(&input.fields),
                ..*context
            };
            let fields = validate_decision(
                &decision_many.decision_table,
                &decision_many.input,
                &decision_many.returning,
                &item_context,
                path,
            )?;
            validate_total_output_order(&decision_many.order_by, &fields, "decision_many", path)?;
            Ok(StepOutput {
                fields,
                many: true,
                may_be_absent: false,
                kind: StepOutputKind::DecisionMany,
            })
        }
        CommandStepOperation::AssertWhen { assert_when } => {
            validate_condition(&assert_when.when, context, path)?;
            validate_rule(
                &assert_when.rule,
                &assert_when.bindings,
                context,
                path,
                Some(&StaticType::Scalar("Boolean".to_owned())),
            )?;
            Ok(StepOutput {
                fields: BTreeMap::new(),
                many: false,
                may_be_absent: false,
                kind: StepOutputKind::Scalar,
            })
        }
        CommandStepOperation::UpdateWhen { update_when } => {
            validate_condition(&update_when.when, context, path)?;
            let (entry, info) = command_target(source, catalog, &update_when.table, path)?;
            validate_primary_key_predicate(&update_when.predicate, info, context, path)?;
            if update_when.set.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "update_when set must contain at least one column assignment",
                ));
            }
            validate_object(&update_when.set, info, context, path)?;
            let returning = returning_columns(&update_when.returning, info, path)?;
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
                    update_when.set.keys(),
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
                update_when.predicate.keys(),
                returning.keys(),
                path,
            )?;
            Ok(StepOutput {
                fields: returning,
                many: false,
                may_be_absent: !update_when.require_affected,
                kind: StepOutputKind::Scalar,
            })
        }
        CommandStepOperation::InsertWhen { insert_when } => {
            validate_condition(&insert_when.when, context, path)?;
            let (entry, info) = command_target(source, catalog, &insert_when.table, path)?;
            if insert_when.object.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "insert_when object must contain at least one column assignment",
                ));
            }
            validate_object(&insert_when.object, info, context, path)?;
            let returning = returning_columns(&insert_when.returning, info, path)?;
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
                    insert_when.object.keys(),
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
                may_be_absent: true,
                kind: StepOutputKind::Scalar,
            })
        }
        CommandStepOperation::AllocateMany { allocate_many } => {
            let input = prior_row_set_output(&allocate_many.from, context, "allocate_many", path)?;
            if allocate_many.group_key.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "allocate_many group_key must not be empty",
                ));
            }
            let required = |name: &str| {
                input.fields.get(name).cloned().ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!("allocate_many input is missing required field '{name}'"),
                    )
                })
            };
            let requested = required(&allocate_many.exact_quantity_columns.requested)?;
            let available = required(&allocate_many.exact_quantity_columns.available)?;
            if !matches!(
                requested.scalar_name(),
                Some("Int" | "int2" | "int4" | "int8" | "numeric" | "decimal")
            ) || !assignable(&available, &requested)
            {
                return Err(PlanError::validation(
                    path,
                    "allocate_many requested and available quantities must share one numeric type",
                ));
            }
            required("order_line_id")?;
            for key in &allocate_many.group_key {
                required(key)?;
            }
            value_type(
                &allocate_many.request_id,
                context,
                None,
                ValueUse::Data,
                path,
            )?;
            let output_type = |name: &str| -> Result<StaticType, PlanError> {
                match name {
                    "allocation_id" => Ok(StaticType::Scalar("uuid".to_owned())),
                    "first_line_sequence" => required("line_sequence"),
                    "items" => Ok(StaticType::Scalar("jsonb".to_owned())),
                    name if name == allocate_many.exact_quantity_columns.allocated => {
                        Ok(requested.clone())
                    }
                    name if name == allocate_many.exact_quantity_columns.backordered => {
                        Ok(requested.clone())
                    }
                    name => required(name),
                }
            };
            let output = |names: &[String]| {
                names
                    .iter()
                    .map(|name| Ok((name.clone(), output_type(name)?)))
                    .collect::<Result<BTreeMap<_, _>, PlanError>>()
            };
            let groups = output(&allocate_many.returning.groups)?;
            let lines = output(&allocate_many.returning.lines)?;
            let backorders = output(&allocate_many.returning.backorders)?;
            validate_total_output_order(
                &allocate_many.group_order_by,
                &groups,
                "allocate_many groups",
                path,
            )?;
            validate_total_output_order(
                &allocate_many.line_order_by,
                &lines,
                "allocate_many lines",
                path,
            )?;
            Ok(StepOutput {
                fields: BTreeMap::from([
                    ("groups".to_owned(), StaticType::Rows(groups)),
                    ("lines".to_owned(), StaticType::Rows(lines)),
                    ("backorders".to_owned(), StaticType::Rows(backorders)),
                ]),
                many: false,
                may_be_absent: false,
                kind: StepOutputKind::Allocation,
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
                may_be_absent: !delete.require_affected,
                kind: StepOutputKind::Scalar,
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

fn command_read_target<'a>(
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
    match info.relation_kind {
        RelationKind::Table | RelationKind::View | RelationKind::MaterializedView => {
            Ok((entry, info))
        }
        kind => Err(PlanError::validation(
            path,
            format!(
                "select_many target '{}.{}' must be a table or view, not {kind:?}",
                table.schema(),
                table.name()
            ),
        )),
    }
}

fn prior_select_many_output<'a>(
    value: &CommandValue,
    context: &'a ValueContext<'_>,
    operation: &str,
    path: &str,
) -> Result<&'a StepOutput, PlanError> {
    let CommandValue::Step {
        step,
        column: None,
        field: None,
        where_nonzero: None,
    } = value
    else {
        return Err(PlanError::validation(
            path,
            format!("{operation} input must be a prior select_many row set"),
        ));
    };
    let output = context.steps.get(step).ok_or_else(|| {
        let message = if context.declared_steps.contains(step) {
            format!("step reference '{step}' must reference an earlier step")
        } else {
            format!("unknown step reference '{step}'")
        };
        PlanError::validation(path, message)
    })?;
    if output.kind != StepOutputKind::SelectMany {
        return Err(PlanError::validation(
            path,
            format!("{operation} input must be a prior select_many row set"),
        ));
    }
    Ok(output)
}

fn prior_row_set_output<'a>(
    value: &CommandValue,
    context: &'a ValueContext<'_>,
    operation: &str,
    path: &str,
) -> Result<&'a StepOutput, PlanError> {
    let CommandValue::Step {
        step,
        column: None,
        field: None,
        where_nonzero: None,
    } = value
    else {
        return Err(PlanError::validation(
            path,
            format!("{operation} input must be a prior row-set step"),
        ));
    };
    let output = context.steps.get(step).ok_or_else(|| {
        let message = if context.declared_steps.contains(step) {
            format!("step reference '{step}' must reference an earlier step")
        } else {
            format!("unknown step reference '{step}'")
        };
        PlanError::validation(path, message)
    })?;
    if !output.many {
        return Err(PlanError::validation(
            path,
            format!("{operation} input must be a prior row-set step"),
        ));
    }
    Ok(output)
}

fn validate_row_bound(bound: u32, operation: &str, path: &str) -> Result<(), PlanError> {
    if !(1..=MAX_COMMAND_ROWS).contains(&bound) {
        return Err(PlanError::validation(
            path,
            format!("{operation} maximum_rows must be between 1 and {MAX_COMMAND_ROWS}"),
        ));
    }
    Ok(())
}

fn validate_projection_values(
    values: &BTreeMap<String, CommandValue>,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<BTreeMap<String, StaticType>, PlanError> {
    if values.is_empty() {
        return Err(PlanError::validation(
            path,
            "projection must declare at least one value",
        ));
    }
    values
        .iter()
        .map(|(name, value)| {
            if !is_graphql_name(name) {
                return Err(PlanError::validation(
                    path,
                    format!("projection field '{name}' must be a valid GraphQL name"),
                ));
            }
            let type_ = value_type(value, context, None, ValueUse::Data, path)?;
            if !type_.is_scalar() {
                return Err(PlanError::validation(
                    path,
                    format!("projection field '{name}' must resolve to one scalar value"),
                ));
            }
            Ok((name.clone(), type_))
        })
        .collect()
}

fn validate_fixed_rows(
    rows: &[BTreeMap<String, CommandValue>],
    context: &ValueContext<'_>,
    path: &str,
) -> Result<BTreeMap<String, StaticType>, PlanError> {
    let first = rows
        .first()
        .expect("the caller rejected an empty fixed_rows declaration");
    let fields = validate_projection_values(first, context, path)?;
    let expected_names = first.keys().collect::<BTreeSet<_>>();
    for row in rows.iter().skip(1) {
        if row.keys().collect::<BTreeSet<_>>() != expected_names {
            return Err(PlanError::validation(
                path,
                "every fixed_rows row must declare the same fields",
            ));
        }
        for (name, value) in row {
            let expected = fields.get(name).expect("fixed row names were checked");
            let actual = value_type(value, context, Some(expected), ValueUse::Data, path)?;
            if !assignable(&actual, expected) {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "{} is not assignable to fixed_rows field '{}' ({})",
                        actual.display_name(),
                        name,
                        expected.display_name()
                    ),
                ));
            }
        }
    }
    Ok(fields)
}

fn validate_decision(
    table_name: &str,
    input: &BTreeMap<String, CommandValue>,
    returning: &[String],
    context: &ValueContext<'_>,
    path: &str,
) -> Result<BTreeMap<String, StaticType>, PlanError> {
    let table = context.rules.decision_table(table_name).ok_or_else(|| {
        PlanError::validation(path, format!("unknown decision table '{table_name}'"))
    })?;
    let expected_names = table
        .input_types()
        .map(|(name, _)| name)
        .collect::<BTreeSet<_>>();
    if input.keys().collect::<BTreeSet<_>>() != expected_names {
        return Err(PlanError::validation(
            path,
            format!("decision table '{table_name}' must bind every declared input exactly once"),
        ));
    }
    for (name, expected) in table.input_types() {
        let expected = rule_type(expected);
        let actual = value_type(
            input.get(name).expect("decision input names were checked"),
            context,
            Some(&expected),
            ValueUse::RuleBinding,
            path,
        )?;
        if !assignable(&actual, &expected) {
            return Err(PlanError::validation(
                path,
                format!(
                    "{} is not assignable to decision input '{}' ({})",
                    actual.display_name(),
                    name,
                    expected.display_name()
                ),
            ));
        }
    }
    if returning.is_empty() {
        return Err(PlanError::validation(
            path,
            "decision must declare at least one returning field",
        ));
    }
    let mut seen = HashSet::new();
    returning
        .iter()
        .map(|name| {
            if !seen.insert(name) {
                return Err(PlanError::validation(
                    path,
                    format!("duplicate decision returning field '{name}'"),
                ));
            }
            if let Some(field) = table.output_field(name) {
                return Ok((name.clone(), rule_type(&field.type_)));
            }
            context
                .item
                .and_then(|fields| fields.get(name))
                .cloned()
                .map(|field| (name.clone(), field))
                .ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!("decision table '{table_name}' has no output field '{name}'"),
                    )
                })
        })
        .collect()
}

fn validate_total_output_order(
    order_by: &[String],
    output: &BTreeMap<String, StaticType>,
    operation: &str,
    path: &str,
) -> Result<(), PlanError> {
    if order_by.is_empty() {
        return Err(PlanError::validation(
            path,
            format!("{operation} requires a non-empty declared total order"),
        ));
    }
    let mut seen = HashSet::new();
    for name in order_by {
        if !seen.insert(name) {
            return Err(PlanError::validation(
                path,
                format!("duplicate {operation} order field '{name}'"),
            ));
        }
        let type_ = output.get(name).ok_or_else(|| {
            PlanError::validation(
                path,
                format!("{operation} order field '{name}' is not returned"),
            )
        })?;
        if !type_.is_scalar() {
            return Err(PlanError::validation(
                path,
                format!("{operation} order field '{name}' must be scalar"),
            ));
        }
    }
    Ok(())
}

fn validate_condition(
    condition: &CommandCondition,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<(), PlanError> {
    match condition {
        CommandCondition::ArgumentEquals { argument_equals } => {
            let expected = command_argument_type_by_name(
                context.metadata,
                context.command,
                &argument_equals.argument,
                path,
            )
            .map_err(|_| {
                PlanError::validation(
                    path,
                    format!(
                        "unknown argument '{}' in command condition",
                        argument_equals.argument
                    ),
                )
            })?;
            if !expected.is_scalar() {
                return Err(PlanError::validation(
                    path,
                    "command conditions require a scalar argument",
                ));
            }
            let actual = literal_type(&argument_equals.value, Some(&expected), path)?;
            if !assignable(&actual, &expected) {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "{} is not assignable to command condition argument '{}' ({})",
                        actual.display_name(),
                        argument_equals.argument,
                        expected.display_name()
                    ),
                ));
            }
            Ok(())
        }
    }
}

fn validate_command_aggregates(
    values: &BTreeMap<String, CommandAggregate>,
    input: &BTreeMap<String, StaticType>,
    path: &str,
) -> Result<BTreeMap<String, StaticType>, PlanError> {
    let mut output = BTreeMap::new();
    for (name, aggregate) in values {
        if !is_graphql_name(name) {
            return Err(PlanError::validation(
                path,
                format!("aggregate output name '{name}' must be a valid GraphQL name"),
            ));
        }
        let type_ = match aggregate {
            CommandAggregate::Count { .. } | CommandAggregate::CountDistinct { .. } => {
                StaticType::Scalar("int8".to_owned())
            }
            CommandAggregate::Sum { sum } => {
                let input_type = aggregate_input_type(aggregate_selector(sum, path)?, input, path)?;
                let scalar = input_type
                    .scalar_name()
                    .ok_or_else(|| PlanError::validation(path, "sum requires a numeric column"))?;
                let output = match scalar {
                    "Int" => "int8",
                    "Float" => "float8",
                    "int2" | "int4" | "serial" => "int8",
                    "int8" | "bigint" | "bigserial" | "numeric" | "decimal" => "numeric",
                    "float4" => "float4",
                    "float8" => "float8",
                    _ => {
                        return Err(PlanError::validation(
                            path,
                            format!("sum requires a numeric column, got '{scalar}'"),
                        ));
                    }
                };
                StaticType::nullable(StaticType::Scalar(output.to_owned()))
            }
            CommandAggregate::Min { min } | CommandAggregate::Max { max: min } => {
                let input_type = aggregate_input_type(aggregate_selector(min, path)?, input, path)?;
                let scalar = input_type.scalar_name().ok_or_else(|| {
                    PlanError::validation(path, "min/max requires an orderable column")
                })?;
                if !aggregate_orderable_scalar(scalar) {
                    let operation = if matches!(aggregate, CommandAggregate::Min { .. }) {
                        "min"
                    } else {
                        "max"
                    };
                    return Err(PlanError::validation(
                        path,
                        format!("{operation} requires an orderable column, got '{scalar}'"),
                    ));
                }
                StaticType::nullable(StaticType::Scalar(scalar.to_owned()))
            }
        };
        if let CommandAggregate::CountDistinct { count_distinct } = aggregate {
            let input_type =
                aggregate_input_type(aggregate_selector(count_distinct, path)?, input, path)?;
            let scalar = input_type.scalar_name().ok_or_else(|| {
                PlanError::validation(path, "count_distinct requires a scalar column")
            })?;
            if matches!(scalar, "json" | "jsonb") {
                return Err(PlanError::validation(
                    path,
                    "count_distinct requires a comparable scalar column",
                ));
            }
        }
        output.insert(name.clone(), type_);
    }
    Ok(output)
}

fn aggregate_selector<'a>(
    aggregate: &'a donat_metadata::ColumnCommandAggregate,
    path: &str,
) -> Result<&'a str, PlanError> {
    match (&aggregate.column, &aggregate.field) {
        (Some(column), None) | (None, Some(column)) => Ok(column),
        _ => Err(PlanError::validation(
            path,
            "aggregate selector must declare exactly one of column or field",
        )),
    }
}

fn aggregate_input_type<'a>(
    column: &str,
    input: &'a BTreeMap<String, StaticType>,
    path: &str,
) -> Result<&'a StaticType, PlanError> {
    input.get(column).ok_or_else(|| {
        PlanError::validation(
            path,
            format!("aggregate input row set does not declare column '{column}'"),
        )
    })
}

fn aggregate_orderable_scalar(scalar: &str) -> bool {
    matches!(
        scalar,
        "Int"
            | "Float"
            | "String"
            | "int2"
            | "int4"
            | "int8"
            | "serial"
            | "bigint"
            | "bigserial"
            | "float4"
            | "float8"
            | "numeric"
            | "decimal"
            | "text"
            | "varchar"
            | "bpchar"
            | "name"
            | "citext"
            | "uuid"
            | "date"
            | "timestamp"
            | "timestamptz"
    )
}

fn collect_current_columns_from_values<'a>(
    values: impl IntoIterator<Item = &'a CommandValue>,
    columns: &mut BTreeSet<String>,
) {
    let mut pending = values.into_iter().collect::<Vec<_>>();
    while let Some(value) = pending.pop() {
        match value {
            CommandValue::CurrentColumn { current_column } => {
                columns.insert(current_column.clone());
            }
            CommandValue::Rule { bindings, .. } => pending.extend(bindings.values()),
            _ => {}
        }
    }
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
    let type_ = value_type(for_each, context, None, ValueUse::Data, path)?;
    match type_ {
        StaticType::Rows(fields) => Ok(fields),
        StaticType::List(item) => match *item {
            StaticType::Object { fields, .. } | StaticType::Row(fields) => Ok(fields),
            _ => Err(PlanError::validation(
                path,
                "insert_many for_each items must be typed objects or rows",
            )),
        },
        _ => Err(PlanError::validation(
            path,
            "insert_many for_each must bind one bounded list or row set",
        )),
    }
}

fn update_many_item_fields(
    for_each: &CommandValue,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<BTreeMap<String, StaticType>, PlanError> {
    match for_each {
        CommandValue::Step {
            field: None,
            column: None,
            where_nonzero: None,
            ..
        } => Ok(
            prior_select_many_output(for_each, context, "update_many", path)?
                .fields
                .clone(),
        ),
        CommandValue::Step {
            field: Some(_),
            column: None,
            ..
        } => insert_many_item_fields(for_each, context, path).map_err(|_| {
            PlanError::validation(
                path,
                "update_many input must be a prior select_many row set",
            )
        }),
        _ => Err(PlanError::validation(
            path,
            "update_many input must be a prior select_many row set",
        )),
    }
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
            MetadataCommandResultValue::Literal { literal }
                if literal.as_i64().is_some_and(|value| {
                    !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&value)
                }) || literal
                    .as_u64()
                    .is_some_and(|value| value > i32::MAX as u64) =>
            {
                return Err(PlanError::validation(
                    path,
                    "integral command result literal is outside the GraphQL Int range",
                ));
            }
            MetadataCommandResultValue::Argument { .. }
            | MetadataCommandResultValue::SessionVariable { .. }
            | MetadataCommandResultValue::CurrentColumn { .. } => {
                return Err(PlanError::validation(
                    path,
                    "command result fields must be step columns or literals",
                ));
            }
            _ => {}
        }
        result_value_type(&field.value, context, path)?;
    }
    Ok(())
}

fn result_value_type(
    value: &MetadataCommandResultValue,
    context: &ValueContext<'_>,
    path: &str,
) -> Result<StaticType, PlanError> {
    match value {
        MetadataCommandResultValue::Step {
            step,
            column,
            field,
            as_: _,
            maximum_items,
        } => {
            let output = context.steps.get(step).ok_or_else(|| {
                PlanError::validation(path, format!("unknown step reference '{step}'"))
            })?;
            if column.is_some() && field.is_some() {
                return Err(PlanError::validation(
                    path,
                    "command result step must declare at most one of column or field",
                ));
            }
            if let Some(column) = column.as_ref().or(field.as_ref()) {
                let field_type = output.fields.get(column).cloned().ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!("step '{step}' does not expose result field '{column}'"),
                    )
                })?;
                if matches!(field_type, StaticType::Rows(_)) {
                    let maximum = maximum_items.ok_or_else(|| {
                        PlanError::validation(path, "row-set result must declare maximum_items")
                    })?;
                    validate_result_bound(maximum, path)?;
                    return Ok(field_type);
                }
                if maximum_items.is_some() {
                    return Err(PlanError::validation(
                        path,
                        "scalar result fields must not declare maximum_items",
                    ));
                }
                if output.many {
                    return Err(PlanError::validation(
                        path,
                        "row-set result must reference the declared row object",
                    ));
                }
                if output.may_be_absent {
                    Ok(StaticType::nullable(field_type))
                } else {
                    Ok(field_type)
                }
            } else if output.many {
                if let Some(maximum) = maximum_items {
                    validate_result_bound(*maximum, path)?;
                }
                Ok(StaticType::Rows(output.fields.clone()))
            } else {
                if maximum_items.is_some() {
                    return Err(PlanError::validation(
                        path,
                        "scalar row results must not declare maximum_items",
                    ));
                }
                Ok(StaticType::Row(output.fields.clone()))
            }
        }
        MetadataCommandResultValue::ProjectedStep {
            step,
            project,
            maximum_items,
        } => {
            validate_result_bound(*maximum_items, path)?;
            let output = context.steps.get(step).ok_or_else(|| {
                PlanError::validation(path, format!("unknown step reference '{step}'"))
            })?;
            let mut fields = BTreeMap::new();
            for (alias, source) in project {
                if !is_graphql_name(alias) {
                    return Err(PlanError::validation(
                        path,
                        format!("result projection alias '{alias}' must be a valid GraphQL name"),
                    ));
                }
                let type_ = output.fields.get(source).cloned().ok_or_else(|| {
                    PlanError::validation(
                        path,
                        format!("step '{step}' does not expose result field '{source}'"),
                    )
                })?;
                fields.insert(alias.clone(), type_);
            }
            if project.is_empty() {
                return Err(PlanError::validation(
                    path,
                    "result projection must declare at least one field",
                ));
            }
            if output.many {
                Ok(StaticType::Rows(fields))
            } else {
                Ok(StaticType::Row(fields))
            }
        }
        MetadataCommandResultValue::Argument { arg } => {
            command_argument_type_by_name(context.metadata, context.command, arg, path)
        }
        MetadataCommandResultValue::Literal { literal } => literal_type(literal, None, path),
        MetadataCommandResultValue::Rule { rule, bindings } => {
            validate_rule(rule, bindings, context, path, None)
        }
        MetadataCommandResultValue::Array(values) => {
            if values.len() > MAX_COMMAND_ROWS as usize {
                return Err(PlanError::validation(
                    path,
                    format!("command result array exceeds {MAX_COMMAND_ROWS} items"),
                ));
            }
            let first = values.first().ok_or_else(|| {
                PlanError::validation(path, "command result arrays must not be empty")
            })?;
            let item = literal_type(first, None, path)?;
            if !item.is_scalar() {
                return Err(PlanError::validation(
                    path,
                    "command result arrays must contain scalar literals",
                ));
            }
            for value in values.iter().skip(1) {
                let actual = literal_type(value, Some(&item), path)?;
                if !assignable(&actual, &item) {
                    return Err(PlanError::validation(
                        path,
                        "command result array items must share one scalar type",
                    ));
                }
            }
            Ok(StaticType::List(Box::new(item)))
        }
        MetadataCommandResultValue::SessionVariable { .. }
        | MetadataCommandResultValue::CurrentColumn { .. } => Err(PlanError::validation(
            path,
            "command result fields cannot expose session or mutable row context values",
        )),
    }
}

fn validate_result_bound(bound: u32, path: &str) -> Result<(), PlanError> {
    if !(1..=MAX_COMMAND_ROWS).contains(&bound) {
        return Err(PlanError::validation(
            path,
            format!("command result maximum_items must be between 1 and {MAX_COMMAND_ROWS}"),
        ));
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
    if let Some(retention) = &idempotency.retention {
        command_retention_seconds(retention)
            .map_err(|message| PlanError::validation(path, message))?;
    }
    let CommandIdempotencyScopeSpec::Values(scopes) = &idempotency.scope else {
        return Ok(());
    };
    for scope in scopes {
        match scope {
            CommandIdempotencyScope::Argument { argument } => {
                let type_ = command_argument_type_by_name(metadata, command, argument, path)?;
                if !type_.is_scalar() {
                    return Err(PlanError::validation(
                        path,
                        "idempotency scope must be scalar and cannot use object or list arguments",
                    ));
                }
                if matches!(type_.scalar_name(), Some("json" | "jsonb")) {
                    return Err(PlanError::validation(
                        path,
                        "idempotency scope must not use json or jsonb arguments",
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
            CommandIdempotencyScope::Step { step, column } => {
                if step.is_empty() || column.is_empty() {
                    return Err(PlanError::validation(
                        path,
                        "idempotency step scope requires non-empty step and column names",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Parse the deliberately narrow command retention grammar at deployment
/// time, so SQLgen receives a typed duration rather than raw metadata text.
pub(crate) fn command_retention_seconds(value: &str) -> Result<u64, String> {
    let Some((amount, unit)) = value
        .strip_suffix('s')
        .map(|amount| (amount, 1_u64))
        .or_else(|| value.strip_suffix('m').map(|amount| (amount, 60)))
        .or_else(|| value.strip_suffix('h').map(|amount| (amount, 60 * 60)))
        .or_else(|| value.strip_suffix('d').map(|amount| (amount, 24 * 60 * 60)))
    else {
        return Err(
            "command idempotency retention must use a positive s, m, h, or d duration".to_string(),
        );
    };
    let amount = amount.parse::<u64>().map_err(|_| {
        "command idempotency retention must use a positive s, m, h, or d duration".to_string()
    })?;
    if amount == 0 {
        return Err(
            "command idempotency retention must use a positive s, m, h, or d duration".to_string(),
        );
    }
    amount.checked_mul(unit).ok_or_else(|| {
        "command idempotency retention exceeds the supported duration range".to_string()
    })
}

fn validate_idempotency_key(
    metadata: &Metadata,
    key: &CommandIdempotencyKey,
    command: &Command,
    path: &str,
) -> Result<(), PlanError> {
    let CommandIdempotencyKey::Argument { argument } = key;
    let declared = command
        .arguments
        .iter()
        .find(|candidate| candidate.name == *argument)
        .ok_or_else(|| PlanError::validation(path, format!("unknown argument '{argument}'")))?;
    let type_ = command_argument_type_by_name(metadata, command, argument, path)?;
    if !type_.is_scalar() {
        return Err(PlanError::validation(
            path,
            "idempotency key must be a declared scalar argument",
        ));
    }
    if matches!(type_.scalar_name(), Some("json" | "jsonb")) {
        return Err(PlanError::validation(
            path,
            "idempotency key must not use a json or jsonb argument",
        ));
    }
    if !declared.type_.ends_with('!') {
        return Err(PlanError::validation(
            path,
            "idempotency key must be a required scalar argument",
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

/// Guards are command preconditions. They must remain independent from every
/// command step so SQLgen can evaluate their materialized gate before any
/// writable CTE or idempotency claim is allowed to execute.
fn validate_guard_precondition_bindings(
    bindings: &BTreeMap<String, CommandValue>,
    path: &str,
) -> Result<(), PlanError> {
    for value in bindings.values() {
        match value {
            CommandValue::Step { step, .. } => {
                return Err(PlanError::validation(
                    path,
                    format!(
                        "command guards cannot reference step '{step}'; use an assert after the referenced step"
                    ),
                ));
            }
            CommandValue::Rule { bindings, .. } => {
                validate_guard_precondition_bindings(bindings, path)?;
            }
            CommandValue::Argument { .. }
            | CommandValue::Item { .. }
            | CommandValue::Literal { .. }
            | CommandValue::SessionVariable { .. }
            | CommandValue::CurrentColumn { .. }
            | CommandValue::DatabaseTime { .. } => {}
        }
    }
    Ok(())
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
                PlanError::validation(
                    path,
                    "item values are allowed only inside a relational item scope",
                )
            })?;
            fields.get(item).cloned().ok_or_else(|| {
                PlanError::validation(path, format!("unknown command item field '{item}'"))
            })
        }
        CommandValue::CurrentColumn { current_column } => {
            let fields = context.current.ok_or_else(|| {
                PlanError::validation(
                    path,
                    "current_column values are allowed only inside update_many set or check",
                )
            })?;
            fields.get(current_column).cloned().ok_or_else(|| {
                PlanError::validation(
                    path,
                    format!("unknown update_many current column '{current_column}'"),
                )
            })
        }
        CommandValue::Step {
            step,
            column,
            field,
            where_nonzero,
        } => {
            let output = context.steps.get(step).ok_or_else(|| {
                let message = if context.declared_steps.contains(step) {
                    format!("step reference '{step}' must reference an earlier step")
                } else {
                    format!("unknown step reference '{step}'")
                };
                PlanError::validation(path, message)
            })?;
            if column.is_some() && field.is_some() {
                return Err(PlanError::validation(
                    path,
                    "step values must declare at most one of column or field",
                ));
            }
            if where_nonzero.is_some() && field.is_none() {
                return Err(PlanError::validation(
                    path,
                    "where_nonzero requires a row-set field selection",
                ));
            }
            match column.as_ref().or(field.as_ref()) {
                Some(selected) => {
                    let selected_type = output.fields.get(selected).cloned().ok_or_else(|| {
                        PlanError::validation(
                            path,
                            format!("step '{step}' does not return field '{selected}'"),
                        )
                    })?;
                    if let Some(nonzero) = where_nonzero {
                        let rows = match &selected_type {
                            StaticType::Rows(fields) => fields,
                            StaticType::List(item) => match item.as_ref() {
                                StaticType::Row(fields) => fields,
                                _ => {
                                    return Err(PlanError::validation(
                                        path,
                                        "where_nonzero requires a projected row set",
                                    ));
                                }
                            },
                            _ => {
                                return Err(PlanError::validation(
                                    path,
                                    "where_nonzero requires a projected row set",
                                ));
                            }
                        };
                        let filter_type = rows.get(nonzero).ok_or_else(|| {
                            PlanError::validation(
                                path,
                                format!("where_nonzero field '{nonzero}' is not projected"),
                            )
                        })?;
                        if !matches!(
                            filter_type.scalar_name(),
                            Some(
                                "Int"
                                    | "Float"
                                    | "int2"
                                    | "int4"
                                    | "int8"
                                    | "numeric"
                                    | "decimal"
                                    | "float4"
                                    | "float8"
                            )
                        ) {
                            return Err(PlanError::validation(
                                path,
                                "where_nonzero requires a numeric projected field",
                            ));
                        }
                    }
                    if output.many {
                        Ok(StaticType::List(Box::new(selected_type)))
                    } else if output.may_be_absent {
                        Ok(StaticType::nullable(selected_type))
                    } else {
                        Ok(selected_type)
                    }
                }
                None if output.many => Ok(StaticType::Rows(output.fields.clone())),
                None => Ok(StaticType::Row(output.fields.clone())),
            }
        }
        CommandValue::Literal { literal } => literal_type(literal, expected, path),
        CommandValue::Rule { rule, bindings } => {
            let actual = validate_rule(rule, bindings, context, path, None)?;
            if let (Some(expected), StaticType::Nullable(actual_inner)) = (expected, &actual)
                && !matches!(expected, StaticType::Nullable(_))
                && assignable(actual_inner, expected)
            {
                return Err(PlanError::validation(
                    path,
                    format!("rule '{rule}' must return {}", expected.display_name()),
                ));
            }
            Ok(actual)
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
        CommandValue::DatabaseTime { database_time } => {
            if database_time != "now" {
                return Err(PlanError::validation(
                    path,
                    format!("unknown database_time function '{database_time}'"),
                ));
            }
            Ok(StaticType::Scalar("timestamptz".to_owned()))
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
        serde_json::Value::Null => match expected {
            Some(expected @ StaticType::Nullable(_)) => expected.clone(),
            Some(_) => {
                return Err(PlanError::validation(
                    path,
                    "null command literals require a nullable typed destination",
                ));
            }
            None => {
                return Err(PlanError::validation(
                    path,
                    "null command literals require an explicit typed destination",
                ));
            }
        },
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
    let Some(scalar) = expected.scalar_name() else {
        return Ok(());
    };
    let valid = match scalar {
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
    let mut active_inputs = HashSet::new();
    parse_command_type_with_active_inputs(metadata, type_, path, &mut active_inputs)
}

fn parse_command_type_with_active_inputs(
    metadata: &Metadata,
    type_: &str,
    path: &str,
    active_inputs: &mut HashSet<String>,
) -> Result<StaticType, PlanError> {
    parse_type_with_named(type_, path, |name| {
        if let Some(input) = metadata
            .custom_types
            .input_objects
            .iter()
            .find(|input| input.name == name)
        {
            if !active_inputs.insert(name.to_string()) {
                return Ok(Some(StaticType::ObjectRef {
                    name: name.to_string(),
                }));
            }
            let parsed = (|| {
                let mut fields = BTreeMap::new();
                for field in &input.fields {
                    fields.insert(
                        field.name.clone(),
                        parse_command_type_with_active_inputs(
                            metadata,
                            &field.type_,
                            path,
                            active_inputs,
                        )?,
                    );
                }
                Ok(StaticType::Object {
                    name: name.to_string(),
                    fields,
                })
            })();
            active_inputs.remove(name);
            return parsed.map(Some);
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
    mut named: impl FnMut(&str) -> Result<Option<StaticType>, PlanError>,
) -> Result<StaticType, PlanError> {
    fn parse<F>(source: &str, path: &str, named: &mut F) -> Result<StaticType, PlanError>
    where
        F: FnMut(&str) -> Result<Option<StaticType>, PlanError>,
    {
        let (source, required) = match source.strip_suffix('!') {
            Some(inner) => (inner, true),
            None => (source, false),
        };
        let parsed = if let Some(inner) = source
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
        {
            StaticType::List(Box::new(parse(inner, path, named)?))
        } else {
            let builtin = match source {
                "Boolean" | "bool" => Some("Boolean"),
                "String" | "string" | "ID" => Some("String"),
                "Int" | "int" => Some("Int"),
                "Float" | "float" | "decimal" => Some("Float"),
                "uuid" | "date" | "timestamp" | "timestamptz" | "json" | "jsonb" => Some(source),
                _ => None,
            };
            if let Some(name) = builtin {
                StaticType::Scalar(name.to_string())
            } else if let Some(type_) = named(source)? {
                type_
            } else {
                return Err(PlanError::validation(
                    path,
                    format!("unknown command argument type '{source}'"),
                ));
            }
        };
        Ok(if required {
            parsed
        } else {
            StaticType::nullable(parsed)
        })
    }
    parse(type_, path, &mut named)
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
    let scalar = StaticType::Scalar(scalar.to_string());
    if column.nullable {
        StaticType::nullable(scalar)
    } else {
        scalar
    }
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
        RuleType::OpaqueJson { .. } => StaticType::Scalar("jsonb".to_owned()),
        RuleType::Nullable(inner) => StaticType::nullable(rule_type(inner)),
    }
}

fn assignable(actual: &StaticType, expected: &StaticType) -> bool {
    match (actual, expected) {
        (StaticType::Nullable(actual), StaticType::Nullable(expected)) => {
            assignable(actual, expected)
        }
        (StaticType::Nullable(_), _) => false,
        (actual, StaticType::Nullable(expected)) => assignable(actual, expected),
        (StaticType::List(actual), StaticType::List(expected)) => assignable(actual, expected),
        _ => actual == expected,
    }
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
        if let Some((action_index, action)) =
            metadata.actions.iter().enumerate().find(|(_, action)| {
                action.name == command.name && action_visible_to_role(action, role)
            })
        {
            return Err(PlanError::validation(
                path,
                format!(
                    "command root '{}' is visible to role '{role}' in {path} (source '{}') and actions[{action_index}] (action '{}', type '{}')",
                    command.name,
                    command.source,
                    action.name,
                    action
                        .definition
                        .action_type
                        .as_deref()
                        .unwrap_or("mutation"),
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

#[cfg(test)]
mod descriptor_fingerprint_tests {
    use super::*;

    fn fingerprint_for_step_literal(literal: serde_json::Value) -> String {
        let metadata: Metadata = serde_json::from_value(serde_json::json!({
            "version": 3,
            "sources": [{
                "name": "default",
                "kind": "postgres",
                "configuration": {
                    "connection_info": { "database_url": "postgres://unused" }
                }
            }],
            "commands": [{
                "name": "canonical_literal",
                "source": "default",
                "steps": [{
                    "name": "write",
                    "insert": {
                        "table": { "schema": "public", "name": "orders" },
                        "object": {
                            "payload": { "literal": literal }
                        }
                    }
                }]
            }]
        }))
        .expect("fingerprint fixture deserializes");
        let empty_contract = ValueContractCatalog {
            roots: BTreeMap::new(),
            named_objects: BTreeMap::new(),
        };
        let rules = donat_rules::compile_catalog(&[], &[]).expect("empty rules compile");
        command_descriptor_fingerprint(
            &metadata.sources[0],
            &metadata.commands[0],
            &rules,
            &empty_contract,
            &empty_contract,
            &BTreeSet::new(),
            &BTreeMap::new(),
        )
    }

    fn nested_literal(reverse: bool) -> serde_json::Value {
        let mut nested = serde_json::Map::new();
        let mut root = serde_json::Map::new();
        if reverse {
            nested.insert("z".to_owned(), serde_json::json!(2));
            nested.insert("a".to_owned(), serde_json::json!(1));
            root.insert("second".to_owned(), serde_json::Value::Object(nested));
            root.insert("first".to_owned(), serde_json::json!(true));
        } else {
            nested.insert("a".to_owned(), serde_json::json!(1));
            nested.insert("z".to_owned(), serde_json::json!(2));
            root.insert("first".to_owned(), serde_json::json!(true));
            root.insert("second".to_owned(), serde_json::Value::Object(nested));
        }
        serde_json::Value::Object(root)
    }

    #[test]
    fn fingerprint_sorts_nested_json_step_literal_keys_recursively() {
        assert_eq!(
            fingerprint_for_step_literal(nested_literal(false)),
            fingerprint_for_step_literal(nested_literal(true)),
            "object insertion order is not command semantics"
        );
    }
}
