//! Ingest schemas as deployment metadata (spec 020 §2).
//!
//! `local.ingest` reads a file a user uploaded. The columns it reads it with are
//! the one part of that transaction the uploader does not control, so they are a
//! declaration: `ingest.yaml` names a schema, its columns, their types, and its
//! bounds, and a process activity selects one *by name*.
//!
//! **There is no inference, and there is nowhere to put any.** A column the
//! schema does not declare is ignored; a declared column the file's header does
//! not carry fails the whole read before a row is parsed. Neither rule is a
//! runtime heuristic — the first is what "the declaration decides the output
//! shape" means, and the second is what stops a half-mapped import.
//!
//! Two things are settled here rather than in the reader.
//!
//! *A column's type is the metadata type system's.* A declared type resolves
//! through the same [`resolve_type`](crate::documents::resolve_type) every other
//! declaration uses, and is then narrowed to the scalars a cell can honestly
//! become. `Html` is refused on purpose: a value out of a stranger's
//! spreadsheet is never markup a renderer may trust.
//!
//! *The schema is pinned into the process revision.* The loader stamps
//! `<schema>@<sha256>` onto every `local.ingest` request activity, exactly as it
//! does for a document template — because editing a column's type changes what
//! every import means, and a process whose behaviour changed should have a new
//! revision saying so.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::documents::resolve_type;
use crate::types::{
    CustomTypes, Metadata, Process, ProcessForEachState, ProcessStateOperation, ProcessValue,
};

/// The connector name every ingest operation is reached through.
pub const INGEST_CAPABILITY: &str = "local.ingest";

/// The keys of a `local.ingest` activity input. There are two, and a third is a
/// declaration the runtime would ignore (ADR 034).
pub const INGEST_INPUT_KEYS: &[&str] = &["schema", "source"];

/// What a declared schema reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestSchemaKind {
    #[default]
    Spreadsheet,
    Csv,
}

impl IngestSchemaKind {
    /// The operation of `local.ingest` that reads this kind.
    pub const fn operation(self) -> &'static str {
        match self {
            Self::Spreadsheet => "spreadsheet.read",
            Self::Csv => "csv.read",
        }
    }
}

/// What a row that does not parse does to the read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestRowErrorPolicy {
    /// Return the rows that parsed and a typed list of the ones that did not.
    #[default]
    Collect,
    /// Refuse the file whole.
    Fail,
}

/// Which sheet of a workbook a schema reads. Absent means the first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IngestSheetSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_index: Option<u32>,
}

impl IngestSheetSelector {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// One declared column.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IngestColumn {
    /// The header text this column is found under, matched exactly after
    /// trimming: a header matched loosely is a column mapped by luck.
    pub header: String,
    /// The output field the coerced value lands in.
    pub field: String,
    /// A type from the metadata type system.
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trim: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
}

/// The per-schema narrowings. Every one is optional, and every one can only
/// make the capability's own ceiling smaller.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IngestBounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_columns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cell_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_source_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_archive_entries: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uncompressed_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_compression_ratio: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_working_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rejections: Option<u64>,
}

impl IngestBounds {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

fn one() -> u32 {
    1
}

/// One declared schema.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IngestSchema {
    pub name: String,
    #[serde(default)]
    pub kind: IngestSchemaKind,
    pub columns: Vec<IngestColumn>,
    #[serde(default, skip_serializing_if = "IngestSheetSelector::is_empty")]
    pub sheet: IngestSheetSelector,
    /// The 1-based row the header sits on.
    #[serde(default = "one")]
    pub header_row: u32,
    /// The CSV field separator. `,` when absent, and refused for a spreadsheet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delimiter: Option<String>,
    #[serde(default)]
    pub on_row_error: IngestRowErrorPolicy,
    #[serde(default, skip_serializing_if = "IngestBounds::is_empty")]
    pub bounds: IngestBounds,
}

/// One refusal, naming the exact metadata path that earned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestSchemaError {
    pub path: String,
    pub message: String,
}

impl IngestSchemaError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for IngestSchemaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

/// The scalars a cell can honestly become.
///
/// They are the type system's built-in scalars minus `Html`, and minus every
/// object, enum, and custom scalar: a cell is one value, and a declaration that
/// implied otherwise would have to be interpreted rather than applied.
pub const INGEST_SCALARS: &[&str] = &[
    "String", "ID", "Int", "Float", "Boolean", "Decimal", "Date", "DateTime",
];

/// The lowercase spelling spec 020 §2 writes, mapped onto the type system's own.
pub fn canonical_scalar(name: &str) -> Option<&'static str> {
    Some(match name {
        "String" | "string" => "String",
        "ID" | "id" => "ID",
        "Int" | "int" | "integer" => "Int",
        "Float" | "float" => "Float",
        "Boolean" | "boolean" | "bool" => "Boolean",
        "Decimal" | "decimal" => "Decimal",
        "Date" | "date" => "Date",
        "DateTime" | "datetime" => "DateTime",
        _ => return None,
    })
}

/// `<name>@<content hash>`: the pin the loader stamps onto every activity that
/// selects this schema.
pub fn schema_pin(schema: &IngestSchema) -> String {
    format!("{}@{}", schema.name, content_hash(schema))
}

/// The SHA-256 of one schema's declaration.
///
/// It is taken over the serialized declaration rather than over a file, because
/// unlike a template a schema *is* its declaration — there are no bytes on disk
/// beside it that could change without the declaration changing.
pub fn content_hash(schema: &IngestSchema) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"donat.ingest.schema.v1\0");
    hasher.update(serde_json::to_vec(schema).expect("a schema always serializes"));
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Every ingest rule, applied to one deployment's metadata.
pub fn validate_ingest_schemas(metadata: &Metadata) -> Vec<IngestSchemaError> {
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, schema) in metadata.ingest_schemas.iter().enumerate() {
        let path = format!("schemas[{index}]");
        if !seen.insert(schema.name.as_str()) {
            errors.push(IngestSchemaError::new(
                format!("{path}.name"),
                format!("ingest schema `{}` is declared twice", schema.name),
            ));
        }
        validate_schema(schema, &path, &metadata.custom_types, &mut errors);
    }
    let by_name: BTreeMap<&str, &IngestSchema> = metadata
        .ingest_schemas
        .iter()
        .map(|schema| (schema.name.as_str(), schema))
        .collect();
    for process in &metadata.processes {
        validate_process(process, &by_name, &mut errors);
    }
    errors
}

fn validate_schema(
    schema: &IngestSchema,
    path: &str,
    types: &CustomTypes,
    errors: &mut Vec<IngestSchemaError>,
) {
    if schema.name.is_empty()
        || !schema
            .name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        errors.push(IngestSchemaError::new(
            format!("{path}.name"),
            "an ingest schema name is alphanumeric with `_` or `-`",
        ));
    }
    if schema.columns.is_empty() {
        errors.push(IngestSchemaError::new(
            format!("{path}.columns"),
            "an ingest schema declares at least one column; there is no inference, so a schema \
             with no columns reads nothing",
        ));
    }
    if schema.header_row == 0 {
        errors.push(IngestSchemaError::new(
            format!("{path}.header_row"),
            "a header row is 1-based, so zero is not a row",
        ));
    }
    match schema.kind {
        IngestSchemaKind::Csv if !schema.sheet.is_empty() => {
            errors.push(IngestSchemaError::new(
                format!("{path}.sheet"),
                "a CSV has no sheets, so a CSV schema selects none",
            ));
        }
        IngestSchemaKind::Spreadsheet if schema.delimiter.is_some() => {
            errors.push(IngestSchemaError::new(
                format!("{path}.delimiter"),
                "a workbook has no field separator; `delimiter` belongs to a CSV schema",
            ));
        }
        _ => {}
    }
    if schema.sheet.by_name.is_some() && schema.sheet.by_index.is_some() {
        errors.push(IngestSchemaError::new(
            format!("{path}.sheet"),
            "a sheet is selected by name or by index, not by both",
        ));
    }
    if let Some(delimiter) = &schema.delimiter
        && (delimiter.chars().count() != 1
            || delimiter
                .chars()
                .next()
                .is_some_and(|character| !character.is_ascii() || character.is_ascii_control()))
    {
        errors.push(IngestSchemaError::new(
            format!("{path}.delimiter"),
            format!("`{delimiter}` is not one printable ASCII character"),
        ));
    }
    for bound in [
        ("max_rows", schema.bounds.max_rows),
        ("max_columns", schema.bounds.max_columns),
        ("max_cell_bytes", schema.bounds.max_cell_bytes),
        ("max_source_bytes", schema.bounds.max_source_bytes),
        ("max_archive_entries", schema.bounds.max_archive_entries),
        (
            "max_uncompressed_bytes",
            schema.bounds.max_uncompressed_bytes,
        ),
        ("max_compression_ratio", schema.bounds.max_compression_ratio),
        ("max_working_bytes", schema.bounds.max_working_bytes),
    ] {
        if bound.1 == Some(0) {
            errors.push(IngestSchemaError::new(
                format!("{path}.bounds.{}", bound.0),
                "a ceiling of zero admits no file",
            ));
        }
    }

    let mut headers = BTreeSet::new();
    let mut fields = BTreeSet::new();
    for (index, column) in schema.columns.iter().enumerate() {
        let path = format!("{path}.columns[{index}]");
        if column.header.trim().is_empty() {
            errors.push(IngestSchemaError::new(
                format!("{path}.header"),
                "an ingest column names the header it is found under",
            ));
        } else if !headers.insert(column.header.trim()) {
            errors.push(IngestSchemaError::new(
                format!("{path}.header"),
                format!("column header `{}` is declared twice", column.header),
            ));
        }
        if column.field.is_empty() {
            errors.push(IngestSchemaError::new(
                format!("{path}.field"),
                "an ingest column names the field it lands in",
            ));
        } else if !fields.insert(column.field.as_str()) {
            errors.push(IngestSchemaError::new(
                format!("{path}.field"),
                format!("column field `{}` is declared twice", column.field),
            ));
        }
        validate_column_type(&column.type_, &path, types, errors);
    }
}

/// A column's declared type, through the type system and then narrowed.
fn validate_column_type(
    declared: &str,
    path: &str,
    types: &CustomTypes,
    errors: &mut Vec<IngestSchemaError>,
) {
    let trimmed = declared.trim();
    let base = trimmed.strip_suffix('!').unwrap_or(trimmed);
    if base.starts_with('[') {
        errors.push(IngestSchemaError::new(
            format!("{path}.type"),
            format!("`{declared}` is a list, and one cell is one value"),
        ));
        return;
    }
    let Some(canonical) = canonical_scalar(base) else {
        // Not one of the spellings this reader knows. Say whether it is a type
        // at all, because "unknown type" and "known type a cell cannot become"
        // are different mistakes.
        let message = if resolve_type(types, trimmed).is_some() {
            format!(
                "`{declared}` is a type this deployment declares, but a cell becomes one of {}",
                INGEST_SCALARS.join(", ")
            )
        } else {
            format!("`{declared}` is not a type this deployment declares")
        };
        errors.push(IngestSchemaError::new(format!("{path}.type"), message));
        return;
    };
    debug_assert!(INGEST_SCALARS.contains(&canonical));
}

fn validate_process(
    process: &Process,
    schemas: &BTreeMap<&str, &IngestSchema>,
    errors: &mut Vec<IngestSchemaError>,
) {
    for (index, state) in process.states.iter().enumerate() {
        let path = format!("processes.{}.states[{index}]", process.name);
        match &state.operation {
            ProcessStateOperation::Request { request } => validate_request(
                &request.connector,
                &request.operation,
                &request.input,
                &format!("{path}.request"),
                schemas,
                errors,
            ),
            ProcessStateOperation::ForEach { for_each } => {
                if let ProcessForEachState::Request { request, .. } = for_each.as_ref() {
                    validate_request(
                        &request.connector,
                        &request.operation,
                        &request.input,
                        &format!("{path}.for_each.request"),
                        schemas,
                        errors,
                    );
                }
            }
            _ => {}
        }
    }
}

fn validate_request(
    connector: &str,
    operation: &str,
    input: &BTreeMap<String, ProcessValue>,
    path: &str,
    schemas: &BTreeMap<&str, &IngestSchema>,
    errors: &mut Vec<IngestSchemaError>,
) {
    if connector != INGEST_CAPABILITY {
        return;
    }
    // The stored file is the one runtime value here: a process reads the file
    // its run is about.
    if !input.contains_key("source") {
        errors.push(IngestSchemaError::new(
            format!("{path}.input.source"),
            "an ingest activity names the stored file it reads",
        ));
    }
    for key in input.keys() {
        if !INGEST_INPUT_KEYS.contains(&key.as_str()) {
            errors.push(IngestSchemaError::new(
                format!("{path}.input.{key}"),
                format!(
                    "`local.ingest` reads a declared schema, so it takes {} and nothing else",
                    INGEST_INPUT_KEYS.join(" and ")
                ),
            ));
        }
    }

    let Some(selected) = input.get("schema") else {
        errors.push(IngestSchemaError::new(
            format!("{path}.input.schema"),
            "an ingest activity selects its schema by name; there is no inference",
        ));
        return;
    };
    // A schema is a deploy-time decision, so it is a literal: a process that
    // could choose one at runtime would be supplying one by another name.
    let ProcessValue::Literal {
        literal: JsonValue::String(name),
    } = selected
    else {
        errors.push(IngestSchemaError::new(
            format!("{path}.input.schema"),
            "an ingest activity's `schema` is a literal name from the deployment's declared \
             schemas, not a computed value",
        ));
        return;
    };
    let Some(schema) = schemas.get(name.as_str()) else {
        errors.push(IngestSchemaError::new(
            format!("{path}.input.schema"),
            format!("ingest schema `{name}` is not declared by this deployment"),
        ));
        return;
    };
    if schema.kind.operation() != operation {
        errors.push(IngestSchemaError::new(
            format!("{path}.operation"),
            format!(
                "ingest schema `{name}` is a {:?} schema and reads through `{}`, not `{}`",
                schema.kind,
                schema.kind.operation(),
                operation
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn metadata(value: JsonValue) -> Metadata {
        serde_json::from_value(value).expect("test metadata deserializes")
    }

    fn messages(value: JsonValue) -> String {
        validate_ingest_schemas(&metadata(value))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The declaration spec 020 §2 writes, loaded verbatim.
    #[test]
    fn the_declared_schema_loads_as_written() {
        let parsed: IngestSchema = serde_yaml::from_str(
            r#"
name: price_list
columns:
  - { header: "SKU",   field: sku,   type: "string!",  trim: true }
  - { header: "Price", field: price, type: "decimal!", min: "0" }
  - { header: "Valid from", field: valid_from, type: "date" }
sheet: { by_name: "Prices" }
header_row: 1
bounds: { max_rows: 50000, max_columns: 64, max_cell_bytes: 4096 }
on_row_error: collect
"#,
        )
        .expect("the spec's own declaration loads");
        assert_eq!(parsed.kind, IngestSchemaKind::Spreadsheet);
        assert_eq!(parsed.columns.len(), 3);
        assert_eq!(parsed.sheet.by_name.as_deref(), Some("Prices"));
        assert_eq!(parsed.on_row_error, IngestRowErrorPolicy::Collect);
        assert_eq!(parsed.bounds.max_rows, Some(50_000));
        assert!(
            validate_ingest_schemas(&Metadata {
                ingest_schemas: vec![parsed],
                ..serde_json::from_value(json!({ "version": 3 })).expect("empty metadata")
            })
            .is_empty()
        );
    }

    /// A schema declaration is checked on its own: a duplicate name, a column
    /// with no header, a type no cell can become, and a bound of zero.
    #[test]
    fn an_ingest_schema_declaration_is_checked_on_its_own() {
        let refused = messages(json!({
            "version": 3,
            "custom_types": { "objects": [{ "name": "order_document", "fields": [
                { "name": "number", "type": "String!" }
            ] }] },
            "ingest_schemas": [
                { "name": "prices", "columns": [{ "header": "SKU", "field": "sku", "type": "String!" }] },
                {
                    "name": "prices",
                    "columns": [
                        { "header": "", "field": "sku", "type": "String!" },
                        { "header": "SKU", "field": "sku", "type": "String!" },
                        { "header": "Note", "field": "note", "type": "Html" },
                        { "header": "Order", "field": "order", "type": "order_document!" },
                        { "header": "Tags", "field": "tags", "type": "[String!]" }
                    ],
                    "header_row": 0,
                    "bounds": { "max_rows": 0 }
                },
                { "name": "notes", "kind": "csv", "columns": [
                    { "header": "SKU", "field": "sku", "type": "String!" }
                ], "sheet": { "by_name": "Prices" } }
            ]
        }));
        assert!(refused.contains("`prices` is declared twice"), "{refused}");
        assert!(
            refused.contains("names the header it is found under"),
            "{refused}"
        );
        assert!(refused.contains("`sku` is declared twice"), "{refused}");
        assert!(
            refused.contains("`Html` is a type this deployment declares, but a cell becomes"),
            "a cell out of an uploaded file is never markup: {refused}"
        );
        assert!(
            refused.contains("`order_document!` is a type this deployment declares, but a cell"),
            "{refused}"
        );
        assert!(
            refused.contains("is a list, and one cell is one value"),
            "{refused}"
        );
        assert!(refused.contains("zero is not a row"), "{refused}");
        assert!(refused.contains("ceiling of zero"), "{refused}");
        assert!(refused.contains("a CSV has no sheets"), "{refused}");
    }

    /// An activity reads a declared schema through the operation that schema's
    /// kind names, and binds the two inputs the capability has.
    #[test]
    fn an_ingest_activity_names_a_declared_schema() {
        assert_eq!(
            messages(deployment(json!({
                "schema": { "literal": "prices" },
                "source": { "state": "upload", "field": "file_id" }
            }))),
            ""
        );

        assert!(
            messages(deployment(json!({ "schema": { "literal": "prices" } })))
                .contains("names the stored file it reads")
        );
        assert!(
            messages(deployment(json!({
                "source": { "state": "upload", "field": "file_id" }
            })))
            .contains("selects its schema by name")
        );
        assert!(
            messages(deployment(json!({
                "schema": { "state": "choose", "field": "schema" },
                "source": { "state": "upload", "field": "file_id" }
            })))
            .contains("is a literal name from the deployment's declared schemas")
        );
        assert!(
            messages(deployment(json!({
                "schema": { "literal": "absent" },
                "source": { "state": "upload", "field": "file_id" }
            })))
            .contains("is not declared by this deployment")
        );
        // A key the capability does not read is a declaration the runtime would
        // ignore (ADR 034) — including one that looks like a schema.
        assert!(
            messages(deployment(json!({
                "schema": { "literal": "prices" },
                "source": { "state": "upload", "field": "file_id" },
                "columns": { "literal": [] }
            })))
            .contains("takes schema and source and nothing else")
        );
    }

    /// A CSV schema does not read through the spreadsheet operation.
    #[test]
    fn a_schema_reads_only_through_its_own_operation() {
        let mut value = deployment(json!({
            "schema": { "literal": "prices" },
            "source": { "state": "upload", "field": "file_id" }
        }));
        value["ingest_schemas"][0]["kind"] = JsonValue::String("csv".to_owned());
        assert!(
            messages(value).contains("reads through `csv.read`, not `spreadsheet.read`"),
            "a kind and an operation are the same decision"
        );
    }

    /// The pin changes when the declaration does, and not otherwise.
    #[test]
    fn the_pin_covers_the_whole_declaration() {
        let schema: IngestSchema = serde_json::from_value(json!({
            "name": "prices",
            "columns": [{ "header": "SKU", "field": "sku", "type": "String!" }]
        }))
        .expect("a schema deserializes");
        let mut changed = schema.clone();
        assert_eq!(content_hash(&schema), content_hash(&changed));
        changed.columns[0].type_ = "Decimal!".to_owned();
        assert_ne!(
            content_hash(&schema),
            content_hash(&changed),
            "changing a column's type changes what every import means"
        );
        assert!(schema_pin(&schema).starts_with("prices@"));
    }

    fn deployment(input: JsonValue) -> JsonValue {
        json!({
            "version": 3,
            "ingest_schemas": [{
                "name": "prices",
                "columns": [{ "header": "SKU", "field": "sku", "type": "String!" }]
            }],
            "connectors": [{
                "name": "local.ingest",
                "module": "local.ingest",
                "operations": [{ "name": "spreadsheet.read" }]
            }],
            "processes": [{
                "name": "import", "kind": "process", "version": 1, "source": "default",
                "start_at": "read",
                "states": [{
                    "id": "read",
                    "request": {
                        "connector": "local.ingest",
                        "operation": "spreadsheet.read",
                        "input": input,
                        "timeout": { "schedule_to_start": "10s", "start_to_close": "60s" },
                        "retry": { "retry_on": ["timeout"], "max_attempts": 1, "initial_interval": "1s", "max_interval": "5s", "jitter": "1s" },
                        "next": "done"
                    }
                }]
            }]
        })
    }
}
