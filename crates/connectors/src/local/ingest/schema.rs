//! The declared schema, and the only rules by which a cell becomes a value.
//!
//! There is no inference here, and there is deliberately no place to put any:
//! [`IngestSchema`] is resolved from a declaration the deployment wrote, a
//! column the declaration does not name is never looked at, and a declared
//! column the file does not carry fails the whole operation before a row is
//! read (spec 020 §2).
//!
//! Coercion follows the metadata type system's own scalars rather than a
//! second, reader-shaped set of rules. Three of them exist because reading a
//! spreadsheet corrupts data quietly otherwise:
//!
//! *A number is not a string.* A `String` column given a numeric cell is
//! rejected rather than stringified, because `12.5` and `12.50` are the same
//! number and different SKUs, and `00123` is not `123`. A text cell keeps
//! exactly the bytes it had.
//!
//! *A serial is a date only where a date system says what it means.* Excel
//! stores a date as a day count from an epoch the *workbook* chooses, so the
//! same serial is two different dates in the two systems. The epoch is read
//! from the workbook and applied; a CSV, which has no workbook and therefore no
//! epoch, refuses a bare number in a date column outright.
//!
//! *A decimal is text.* It arrives as the digits the file carried and leaves as
//! the same digits. A float that cannot be written back exactly is rejected
//! rather than rounded, which is the same rule the spreadsheet *writer* applies
//! in the other direction (spec 019 §5).

use std::collections::BTreeSet;

use serde_json::Value as JsonValue;

use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};

/// What a declared schema reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IngestKind {
    #[default]
    Spreadsheet,
    Csv,
}

impl IngestKind {
    /// The operation of `local.ingest` that reads this kind.
    pub const fn operation(self) -> &'static str {
        match self {
            Self::Spreadsheet => "spreadsheet.read",
            Self::Csv => "csv.read",
        }
    }
}

/// What a row that does not parse does to the operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RowErrorPolicy {
    /// Return the rows that parsed and a typed list of the ones that did not.
    #[default]
    Collect,
    /// Refuse the file whole. Nothing partial is ever returned either way; the
    /// difference is whether the process gets to decide.
    Fail,
}

/// One declared column, as a deployment writes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestColumnSpec {
    /// The header text this column is found under. Matching is exact after
    /// trimming, because a header matched loosely is a column mapped by luck.
    pub header: String,
    /// The output field the coerced value lands in.
    pub field: String,
    /// A type from the metadata type system, in its own spelling (`Decimal!`).
    pub declared: String,
    /// Whether surrounding whitespace is removed before coercion.
    pub trim: bool,
    /// Inclusive bounds, compared at the declared type's own scale.
    pub min: Option<String>,
    pub max: Option<String>,
}

/// One declared schema, as a deployment writes it.
///
/// This is the wire between `donat-metadata`, which owns the YAML, and this
/// crate, which owns the reading — the same seam `DocumentTemplateSpec` sits on,
/// and for the same reason: neither crate depends on the other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestSchemaSpec {
    pub name: String,
    pub kind: IngestKind,
    pub columns: Vec<IngestColumnSpec>,
    /// Which sheet to read. A name, an index, or neither — in which case the
    /// workbook's first sheet is read.
    pub sheet_by_name: Option<String>,
    pub sheet_by_index: Option<usize>,
    /// The 1-based row the header sits on.
    pub header_row: u32,
    /// The CSV field separator, one ASCII byte. `,` when absent.
    pub delimiter: Option<char>,
    pub on_row_error: RowErrorPolicy,

    // -- the narrowings, every one optional and none of them able to widen ---
    pub max_rows: Option<u64>,
    pub max_columns: Option<u64>,
    pub max_cell_bytes: Option<u64>,
    pub max_source_bytes: Option<u64>,
    pub max_archive_entries: Option<u64>,
    pub max_uncompressed_bytes: Option<u64>,
    pub max_compression_ratio: Option<u64>,
    pub max_working_bytes: Option<u64>,
    pub max_rejections: Option<u64>,
}

/// One refusal of a declaration, named by the schema it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestRejection {
    pub schema: String,
    pub message: String,
}

impl IngestRejection {
    fn new(schema: &str, message: impl Into<String>) -> Self {
        Self {
            schema: schema.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for IngestRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.schema, self.message)
    }
}

/// The scalars a cell may become.
///
/// They are the metadata type system's built-in scalars, minus `Html`: a value
/// out of a file a stranger uploaded is never markup a renderer may trust
/// (spec 019 §4 decides that from the declaration, and this is a declaration
/// nobody but the uploader filled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    String,
    Id,
    Int,
    Float,
    Boolean,
    Decimal,
    Date,
    DateTime,
}

impl Scalar {
    /// The type system's own name, and the lowercase spelling spec 020 §2 uses.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "String" | "string" => Self::String,
            "ID" | "id" => Self::Id,
            "Int" | "int" | "integer" => Self::Int,
            "Float" | "float" => Self::Float,
            "Boolean" | "boolean" | "bool" => Self::Boolean,
            "Decimal" | "decimal" => Self::Decimal,
            "Date" | "date" => Self::Date,
            "DateTime" | "datetime" => Self::DateTime,
            _ => return None,
        })
    }
}

/// One resolved column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestColumn {
    pub header: String,
    pub field: String,
    pub scalar: Scalar,
    pub required: bool,
    pub trim: bool,
    pub min: Option<String>,
    pub max: Option<String>,
}

/// One resolved schema: what the reader actually consults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestSchema {
    name: String,
    kind: IngestKind,
    columns: Vec<IngestColumn>,
    sheet_by_name: Option<String>,
    sheet_by_index: Option<usize>,
    header_row: u32,
    delimiter: u8,
    on_row_error: RowErrorPolicy,
    spec: IngestSchemaSpec,
}

impl IngestSchema {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> IngestKind {
        self.kind
    }

    pub fn columns(&self) -> &[IngestColumn] {
        &self.columns
    }

    pub fn sheet_by_name(&self) -> Option<&str> {
        self.sheet_by_name.as_deref()
    }

    pub const fn sheet_by_index(&self) -> Option<usize> {
        self.sheet_by_index
    }

    /// The 1-based header row.
    pub const fn header_row(&self) -> u32 {
        self.header_row
    }

    pub const fn delimiter(&self) -> u8 {
        self.delimiter
    }

    pub const fn on_row_error(&self) -> RowErrorPolicy {
        self.on_row_error
    }

    /// A declared narrowing of one ceiling, which may only ever make it
    /// smaller: `None` and a larger number both leave the operation's own.
    pub fn narrowed(&self, declared: Option<u64>, ceiling: u64) -> u64 {
        declared.map_or(ceiling, |declared| declared.min(ceiling))
    }

    pub const fn spec(&self) -> &IngestSchemaSpec {
        &self.spec
    }

    /// Resolve one declaration, or say exactly what is wrong with it.
    pub fn resolve(spec: IngestSchemaSpec) -> Result<Self, Vec<IngestRejection>> {
        let mut errors = Vec::new();
        if spec.name.is_empty() {
            errors.push(IngestRejection::new("", "an ingest schema needs a name"));
        }
        if spec.columns.is_empty() {
            errors.push(IngestRejection::new(
                &spec.name,
                "an ingest schema declares at least one column; there is no inference",
            ));
        }
        if spec.header_row == 0 {
            errors.push(IngestRejection::new(
                &spec.name,
                "a header row is 1-based, so zero is not a row",
            ));
        }
        if spec.kind == IngestKind::Csv
            && (spec.sheet_by_name.is_some() || spec.sheet_by_index.is_some())
        {
            errors.push(IngestRejection::new(
                &spec.name,
                "a CSV has no sheets, so a CSV schema selects none",
            ));
        }
        let delimiter = match spec.delimiter {
            None => b',',
            Some(delimiter) if delimiter.is_ascii() && !delimiter.is_ascii_control() => {
                delimiter as u8
            }
            Some(_) => {
                errors.push(IngestRejection::new(
                    &spec.name,
                    "a CSV delimiter is one printable ASCII character",
                ));
                b','
            }
        };

        let mut headers = BTreeSet::new();
        let mut fields = BTreeSet::new();
        let mut columns = Vec::with_capacity(spec.columns.len());
        for column in &spec.columns {
            let header = column.header.trim();
            if header.is_empty() {
                errors.push(IngestRejection::new(
                    &spec.name,
                    "an ingest column names the header it is found under",
                ));
            }
            if !headers.insert(header.to_owned()) {
                errors.push(IngestRejection::new(
                    &spec.name,
                    format!("column header `{header}` is declared twice"),
                ));
            }
            if column.field.is_empty() || !fields.insert(column.field.clone()) {
                errors.push(IngestRejection::new(
                    &spec.name,
                    format!("column field `{}` is empty or declared twice", column.field),
                ));
            }
            let declared = column.declared.trim();
            let (base, required) = match declared.strip_suffix('!') {
                Some(base) => (base, true),
                None => (declared, false),
            };
            let Some(scalar) = Scalar::parse(base) else {
                errors.push(IngestRejection::new(
                    &spec.name,
                    format!(
                        "`{declared}` is not a scalar an ingested cell can become; a column is \
                         one of String, ID, Int, Float, Boolean, Decimal, Date, DateTime"
                    ),
                ));
                continue;
            };
            columns.push(IngestColumn {
                header: header.to_owned(),
                field: column.field.clone(),
                scalar,
                required,
                trim: column.trim,
                min: column.min.clone(),
                max: column.max.clone(),
            });
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Self {
            name: spec.name.clone(),
            kind: spec.kind,
            columns,
            sheet_by_name: spec.sheet_by_name.clone(),
            sheet_by_index: spec.sheet_by_index,
            header_row: spec.header_row,
            delimiter,
            on_row_error: spec.on_row_error,
            spec,
        })
    }

    /// Map every declared column onto the index it sits at in this header.
    ///
    /// A declared column the header does not carry fails here — before a single
    /// row is read — because a half-mapped import is worse than none. A header
    /// column the schema does not declare is simply not in the answer.
    pub fn bind_header(&self, header: &[String]) -> Result<Vec<usize>, ConnectorFailure> {
        let mut bound = Vec::with_capacity(self.columns.len());
        for column in &self.columns {
            let index = header
                .iter()
                .position(|candidate| candidate.trim() == column.header)
                .ok_or_else(|| {
                    refuse(
                        "ingest_header_column_missing",
                        "a column the schema declares is not in the file's header row",
                    )
                })?;
            bound.push(index);
        }
        Ok(bound)
    }
}

/// The resolved set of one deployment's schemas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestSchemaSet {
    schemas: std::collections::BTreeMap<String, IngestSchema>,
}

impl IngestSchemaSet {
    pub fn resolve(
        specs: impl IntoIterator<Item = IngestSchemaSpec>,
    ) -> Result<Self, Vec<IngestRejection>> {
        let mut schemas = std::collections::BTreeMap::new();
        let mut errors = Vec::new();
        for spec in specs {
            let name = spec.name.clone();
            match IngestSchema::resolve(spec) {
                Ok(schema) => {
                    if schemas.insert(name.clone(), schema).is_some() {
                        errors.push(IngestRejection::new(
                            &name,
                            "an ingest schema is declared twice",
                        ));
                    }
                }
                Err(rejections) => errors.extend(rejections),
            }
        }
        if errors.is_empty() {
            Ok(Self { schemas })
        } else {
            Err(errors)
        }
    }

    pub fn get(&self, name: &str) -> Option<&IngestSchema> {
        self.schemas.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

/// One cell, in the only two shapes a reader can honestly report: the text the
/// file carried, or a number the file stored as a number.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Empty,
    /// Text exactly as stored — never re-rendered from a number.
    Text(String),
    /// A number stored as a number.
    Number(f64),
    Boolean(bool),
    /// A date the file itself typed, already resolved through the workbook's
    /// own date system: (year, month, day, hour, minute, second).
    Instant(i32, u32, u32, u32, u32, u32),
    /// A cell the file typed as a date whose serial names no date this reader
    /// can report — negative, past year 9999, or not finite at all.
    UnrepresentableDate,
    /// A cell the application itself recorded as an error (`#REF!`, `#DIV/0!`).
    Error,
    /// A formula cell with no cached value. Nothing is computed for it.
    FormulaWithoutValue,
}

/// Why one cell did not become a value. A closed set, and never the cell's own
/// text: the content is the uploader's, and an error message is read by an
/// operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellRejection {
    Missing,
    TypeMismatch,
    NotAnInteger,
    NotADecimal,
    NotANumber,
    NotADate,
    NotABoolean,
    TooLarge,
    CellError,
    FormulaWithoutCachedValue,
    BelowMinimum,
    AboveMaximum,
    DecimalNotExact,
}

impl CellRejection {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Missing => "cell_missing",
            Self::TypeMismatch => "cell_type_mismatch",
            Self::NotAnInteger => "cell_not_an_integer",
            Self::NotADecimal => "cell_not_a_decimal",
            Self::NotANumber => "cell_not_a_number",
            Self::NotADate => "cell_not_a_date",
            Self::NotABoolean => "cell_not_a_boolean",
            Self::TooLarge => "cell_too_large",
            Self::CellError => "cell_error",
            Self::FormulaWithoutCachedValue => "formula_without_cached_value",
            Self::BelowMinimum => "cell_below_minimum",
            Self::AboveMaximum => "cell_above_maximum",
            Self::DecimalNotExact => "cell_decimal_not_exact",
        }
    }
}

/// Whether a bare number in a date column means anything, and if so, from which
/// epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateSystem {
    /// The workbook says 1900 (Excel's default, leap-year bug included).
    Nineteen00,
    /// The workbook says 1904.
    Nineteen04,
    /// There is no workbook, so there is no epoch: a CSV.
    None,
}

/// Coerce one cell into the value its column declares.
pub fn coerce(
    column: &IngestColumn,
    cell: &Cell,
    max_cell_bytes: u64,
    dates: DateSystem,
) -> Result<JsonValue, CellRejection> {
    let cell = match cell {
        Cell::Text(text) if column.trim => Cell::Text(text.trim().to_owned()),
        other => other.clone(),
    };
    match &cell {
        Cell::Error => return Err(CellRejection::CellError),
        Cell::FormulaWithoutValue => return Err(CellRejection::FormulaWithoutCachedValue),
        // A date-typed cell nobody can name is not a date, whatever the column
        // declared: refusing it here is what keeps every branch below working
        // on values that exist.
        Cell::UnrepresentableDate => return Err(CellRejection::NotADate),
        Cell::Text(text) if text.len() as u64 > max_cell_bytes => {
            return Err(CellRejection::TooLarge);
        }
        Cell::Empty | Cell::Text(_) | Cell::Number(_) | Cell::Boolean(_) | Cell::Instant(..) => {}
    }
    let empty =
        matches!(&cell, Cell::Empty) || matches!(&cell, Cell::Text(text) if text.is_empty());
    if empty {
        return if column.required {
            Err(CellRejection::Missing)
        } else {
            Ok(JsonValue::Null)
        };
    }

    let value = match column.scalar {
        Scalar::String | Scalar::Id => match &cell {
            // A number is refused rather than rendered as text: rendering is
            // where `00123` becomes `123` and `12.50` becomes `12.5`.
            Cell::Text(text) => JsonValue::String(text.clone()),
            _ => return Err(CellRejection::TypeMismatch),
        },
        Scalar::Int => match &cell {
            Cell::Number(number) if number.fract() == 0.0 && number.abs() < 9e15 => {
                JsonValue::from(*number as i64)
            }
            Cell::Number(_) => return Err(CellRejection::NotAnInteger),
            Cell::Text(text) => match text.parse::<i64>() {
                Ok(number) => JsonValue::from(number),
                Err(_) => return Err(CellRejection::NotAnInteger),
            },
            _ => return Err(CellRejection::TypeMismatch),
        },
        Scalar::Float => match &cell {
            Cell::Number(number) => JsonValue::from(*number),
            Cell::Text(text) => match text.parse::<f64>() {
                Ok(number) if number.is_finite() => JsonValue::from(number),
                _ => return Err(CellRejection::NotANumber),
            },
            _ => return Err(CellRejection::TypeMismatch),
        },
        Scalar::Decimal => match &cell {
            // A decimal leaves as the digits it arrived as. It is a string in
            // JSON for the same reason it is one on the way out (spec 019 §5):
            // that is the only spelling that has not been through a float.
            Cell::Text(text) => match decimal(text) {
                Some(decimal) => JsonValue::String(decimal),
                None => return Err(CellRejection::NotADecimal),
            },
            Cell::Number(number) => match exact_decimal(*number) {
                Some(decimal) => JsonValue::String(decimal),
                None => return Err(CellRejection::DecimalNotExact),
            },
            _ => return Err(CellRejection::TypeMismatch),
        },
        Scalar::Boolean => match &cell {
            Cell::Boolean(flag) => JsonValue::Bool(*flag),
            Cell::Text(text) => match text.to_ascii_lowercase().as_str() {
                "true" => JsonValue::Bool(true),
                "false" => JsonValue::Bool(false),
                _ => return Err(CellRejection::NotABoolean),
            },
            _ => return Err(CellRejection::NotABoolean),
        },
        Scalar::Date | Scalar::DateTime => {
            let (year, month, day, hour, minute, second) = match &cell {
                Cell::Instant(year, month, day, hour, minute, second) => {
                    (*year, *month, *day, *hour, *minute, *second)
                }
                // A bare number is a serial date, and a serial date means
                // nothing without the workbook that declared its epoch.
                Cell::Number(serial) => match dates {
                    DateSystem::None => return Err(CellRejection::NotADate),
                    system => match from_serial(*serial, system) {
                        Some(parts) => parts,
                        None => return Err(CellRejection::NotADate),
                    },
                },
                Cell::Text(text) => match parse_iso(text) {
                    Some(parts) => parts,
                    None => return Err(CellRejection::NotADate),
                },
                _ => return Err(CellRejection::TypeMismatch),
            };
            if column.scalar == Scalar::Date {
                JsonValue::String(format!("{year:04}-{month:02}-{day:02}"))
            } else {
                JsonValue::String(format!(
                    "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
                ))
            }
        }
    };
    admit_range(column, &value)?;
    Ok(value)
}

/// The declared inclusive bounds, compared at the declared type's own scale.
fn admit_range(column: &IngestColumn, value: &JsonValue) -> Result<(), CellRejection> {
    let compare = |bound: &str| -> Option<std::cmp::Ordering> {
        match (column.scalar, value) {
            (Scalar::Int | Scalar::Float | Scalar::Decimal, _) => {
                let left = match value {
                    JsonValue::String(text) => text.parse::<f64>().ok()?,
                    other => other.as_f64()?,
                };
                left.partial_cmp(&bound.parse::<f64>().ok()?)
            }
            (
                Scalar::Date | Scalar::DateTime | Scalar::String | Scalar::Id,
                JsonValue::String(text),
            ) => Some(text.as_str().cmp(bound)),
            _ => None,
        }
    };
    if let Some(min) = &column.min
        && compare(min) == Some(std::cmp::Ordering::Less)
    {
        return Err(CellRejection::BelowMinimum);
    }
    if let Some(max) = &column.max
        && compare(max) == Some(std::cmp::Ordering::Greater)
    {
        return Err(CellRejection::AboveMaximum);
    }
    Ok(())
}

/// A decimal, kept as the digits it was written with.
fn decimal(text: &str) -> Option<String> {
    let body = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = match body.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (body, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(whole) || fraction.is_some_and(|fraction| !digits(fraction)) {
        return None;
    }
    Some(text.to_owned())
}

/// A number stored as a number, written back only when the shortest form that
/// round-trips is a plain decimal. Anything needing an exponent, or more
/// precision than a double actually holds, is refused rather than rounded.
fn exact_decimal(number: f64) -> Option<String> {
    if !number.is_finite() || number.abs() >= 1e15 {
        return None;
    }
    let rendered = format!("{number}");
    decimal(&rendered)
}

/// `YYYY-MM-DD`, optionally with `THH:MM:SS` and a `Z`.
fn parse_iso(text: &str) -> Option<(i32, u32, u32, u32, u32, u32)> {
    let (date, time) = match text.split_once(['T', ' ']) {
        Some((date, time)) => (date, Some(time.trim_end_matches('Z'))),
        None => (text, None),
    };
    let mut parts = date.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if day > days_in_month(year, month) {
        return None;
    }
    let Some(time) = time else {
        return Some((year, month, day, 0, 0, 0));
    };
    let mut clock = time.split(':');
    let hour: u32 = clock.next()?.parse().ok()?;
    let minute: u32 = clock.next()?.parse().ok()?;
    let second: u32 = clock
        .next()
        .map_or(Ok(0), |second| {
            second.split('.').next().unwrap_or("0").parse()
        })
        .ok()?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((year, month, day, hour, minute, second))
}

/// Whether an Excel serial names a date `calamine`'s conversion can carry.
///
/// The upper end is 9999-12-31 and the lower end is the epoch itself.
/// `calamine` has no guard of its own: it takes `.floor() as u64`, which
/// saturates a non-finite or enormous value to `u64::MAX`, and then adds the
/// epoch offset — an overflow that panics in debug and wraps into a truncated
/// year in release. This is the one place the range is written down, so both
/// callers (a bare number, and a cell the file itself typed as a date) refuse
/// the same set.
pub(crate) fn is_representable_serial(serial: f64) -> bool {
    serial.is_finite() && (0.0..2_958_466.0).contains(&serial)
}

/// An Excel serial, through the workbook's own date system.
///
/// The conversion is `calamine`'s: it is the one place in the workspace that
/// knows both epochs *and* Excel's 1900 leap-year bug, and re-deriving it here
/// would be a second implementation to keep in agreement with the first.
fn from_serial(serial: f64, system: DateSystem) -> Option<(i32, u32, u32, u32, u32, u32)> {
    if !is_representable_serial(serial) {
        return None;
    }
    let value = calamine::ExcelDateTime::new(
        serial,
        calamine::ExcelDateTimeType::DateTime,
        system == DateSystem::Nineteen04,
    );
    let (year, month, day, hour, minute, second, _milli) = value.to_ymd_hms_milli();
    Some((
        i32::from(year),
        u32::from(month),
        u32::from(day),
        u32::from(hour),
        u32::from(minute),
        u32::from(second),
    ))
}

const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

pub(crate) fn refuse(code: &'static str, message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(ConnectorErrorClass::Validation, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(scalar: Scalar, required: bool) -> IngestColumn {
        IngestColumn {
            header: "H".to_owned(),
            field: "f".to_owned(),
            scalar,
            required,
            trim: true,
            min: None,
            max: None,
        }
    }

    /// The three quiet corruptions, at the unit the rule is written in.
    #[test]
    fn a_cell_becomes_only_what_its_column_declares() {
        let text = column(Scalar::String, true);
        // A numeric-looking string is the string it was.
        assert_eq!(
            coerce(&text, &Cell::Text("00123".to_owned()), 64, DateSystem::None),
            Ok(JsonValue::String("00123".to_owned()))
        );
        // A number in a string column is refused, not rendered.
        assert_eq!(
            coerce(&text, &Cell::Number(123.0), 64, DateSystem::None),
            Err(CellRejection::TypeMismatch)
        );

        // A serial date is a date only where an epoch says which one.
        let date = column(Scalar::Date, false);
        assert_eq!(
            coerce(&date, &Cell::Number(39448.0), 64, DateSystem::Nineteen00),
            Ok(JsonValue::String("2008-01-01".to_owned()))
        );
        assert_eq!(
            coerce(&date, &Cell::Number(39448.0), 64, DateSystem::Nineteen04),
            Ok(JsonValue::String("2012-01-02".to_owned()))
        );
        assert_eq!(
            coerce(&date, &Cell::Number(39448.0), 64, DateSystem::None),
            Err(CellRejection::NotADate)
        );

        // A decimal keeps its own digits; a float that cannot be written back
        // exactly is refused rather than rounded.
        let money = column(Scalar::Decimal, true);
        assert_eq!(
            coerce(
                &money,
                &Cell::Text("12.50".to_owned()),
                64,
                DateSystem::None
            ),
            Ok(JsonValue::String("12.50".to_owned()))
        );
        assert_eq!(
            coerce(&money, &Cell::Number(12.5), 64, DateSystem::None),
            Ok(JsonValue::String("12.5".to_owned()))
        );
        assert_eq!(
            coerce(&money, &Cell::Number(1e20), 64, DateSystem::None),
            Err(CellRejection::DecimalNotExact)
        );
        assert_eq!(
            coerce(
                &money,
                &Cell::Text("twelve".to_owned()),
                64,
                DateSystem::None
            ),
            Err(CellRejection::NotADecimal)
        );

        // And the two cells nothing is computed for.
        assert_eq!(
            coerce(&money, &Cell::FormulaWithoutValue, 64, DateSystem::None),
            Err(CellRejection::FormulaWithoutCachedValue)
        );
        assert_eq!(
            coerce(&money, &Cell::Error, 64, DateSystem::None),
            Err(CellRejection::CellError)
        );
    }

    /// A declaration is refused for its own reasons, before a file exists.
    #[test]
    fn a_schema_declaration_is_checked_on_its_own() {
        let base = IngestSchemaSpec {
            name: "prices".to_owned(),
            kind: IngestKind::Spreadsheet,
            columns: vec![IngestColumnSpec {
                header: "SKU".to_owned(),
                field: "sku".to_owned(),
                declared: "String!".to_owned(),
                trim: true,
                ..IngestColumnSpec::default()
            }],
            header_row: 1,
            ..IngestSchemaSpec::default()
        };
        assert!(IngestSchema::resolve(base.clone()).is_ok());
        assert!(
            IngestSchema::resolve(IngestSchemaSpec {
                columns: Vec::new(),
                ..base.clone()
            })
            .is_err(),
            "a schema with no columns would have to infer them"
        );
        assert!(
            IngestSchema::resolve(IngestSchemaSpec {
                header_row: 0,
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            IngestSchema::resolve(IngestSchemaSpec {
                columns: vec![IngestColumnSpec {
                    header: "Note".to_owned(),
                    field: "note".to_owned(),
                    declared: "Html".to_owned(),
                    ..IngestColumnSpec::default()
                }],
                ..base.clone()
            })
            .is_err(),
            "a cell out of an uploaded file is never markup"
        );
        assert!(
            IngestSchema::resolve(IngestSchemaSpec {
                kind: IngestKind::Csv,
                sheet_by_name: Some("Prices".to_owned()),
                ..base
            })
            .is_err()
        );
    }

    /// A declared ceiling narrows the operation's and never widens it.
    #[test]
    fn a_declared_ceiling_only_narrows() {
        let schema = IngestSchema::resolve(IngestSchemaSpec {
            name: "prices".to_owned(),
            columns: vec![IngestColumnSpec {
                header: "SKU".to_owned(),
                field: "sku".to_owned(),
                declared: "String".to_owned(),
                ..IngestColumnSpec::default()
            }],
            header_row: 1,
            ..IngestSchemaSpec::default()
        })
        .expect("a complete declaration resolves");
        assert_eq!(schema.narrowed(Some(10), 100), 10);
        assert_eq!(schema.narrowed(Some(1_000), 100), 100);
        assert_eq!(schema.narrowed(None, 100), 100);
    }
}
