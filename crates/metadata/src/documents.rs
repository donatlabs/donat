//! Document templates as deployment metadata (spec 019 §2).
//!
//! A template is not something a request supplies. It lives in the metadata
//! directory, is read and resolved at boot, and is pinned into the process
//! definition revision by the hash of its bytes. Input selects a template only
//! by its declared name, from the set this deployment enabled — so "which
//! template" is a deploy-time decision and "what goes in it" is the only thing
//! a running process gets to choose.
//!
//! Three things are settled here rather than in the renderer.
//!
//! *The file set is frozen.* A template's source, and every file it declares
//! as an include, are read at load into a map of virtual paths, rooted at the
//! template's own directory. Nothing else is reachable, and there is no
//! filesystem access left to do at render time. A declared include that
//! escapes the template's directory is refused while it is still readable.
//!
//! *The declaration typechecks against what binds it.* A template input the
//! process never binds, and a bound value the template never declares, are
//! both refused (`knowledgebase/declarative-saas/decisions/034-*`). Every
//! declared type resolves against the metadata type system, and a literal
//! binding is checked against it.
//!
//! *The bytes are pinned.* Each template carries the SHA-256 of the material
//! it was loaded from, and the loader stamps `<name>@<hash>` onto every
//! request activity that selects it. The process definition fingerprint is
//! taken over the serialized process, so editing a template file changes the
//! revision of every process that renders with it — without the renderer, the
//! process compiler, or the operator having to remember.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::types::{
    CustomTypes, Metadata, Process, ProcessForEachState, ProcessStateOperation, ProcessValue,
};

/// The connector name every document operation is reached through.
pub const DOCUMENT_CAPABILITY: &str = "local.document";

/// Keys of a `local.document` activity input that belong to the capability
/// rather than to the template.
///
/// They are reserved in one place so a template input can never be spelled the
/// same as one of them: a collision would make the renderer read a file name
/// where a process meant a customer name.
pub const RESERVED_INPUT_KEYS: &[&str] = &[
    "template",
    "attachment",
    "claim_role",
    "file_name",
    "document_id",
    "document_timestamp",
];

/// What a template renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentTemplateKind {
    Pdf,
    Email,
    Spreadsheet,
    Calendar,
}

impl DocumentTemplateKind {
    /// The operation of `local.document` that renders this kind.
    pub const fn operation(self) -> &'static str {
        match self {
            Self::Pdf => "pdf.render",
            Self::Email => "email.render",
            Self::Spreadsheet => "spreadsheet.render",
            Self::Calendar => "calendar.render",
        }
    }

    /// The extension the source file must carry. A `.mjml` file declared as a
    /// PDF is a mistake worth catching at load rather than at the first render.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "typ",
            Self::Email => "mjml",
            Self::Spreadsheet | Self::Calendar => "yaml",
        }
    }

    /// Whether the source is a layout document the loader normalizes to JSON.
    ///
    /// The renderer parses spreadsheet and calendar layouts, and it has no YAML
    /// parser — deliberately, because the metadata crate is where YAML is
    /// read. Normalizing here keeps exactly one YAML reader in the workspace.
    pub const fn is_layout(self) -> bool {
        matches!(self, Self::Spreadsheet | Self::Calendar)
    }
}

/// One declared template.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentTemplate {
    pub name: String,
    pub kind: DocumentTemplateKind,
    /// The source file, relative to the metadata directory.
    pub source: String,
    /// Further files the template may resolve, each relative to the metadata
    /// directory and inside the source file's own directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
    /// Declared input names and their types from the metadata type system.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "DocumentTemplateBounds::is_empty")]
    pub bounds: DocumentTemplateBounds,

    // --- derived at load; never written by an operator -------------------
    /// The frozen file set: virtual path (rooted at the template's own
    /// directory) to file text. `skip_deserializing` is what makes it derived
    /// rather than declared — with `deny_unknown_fields`, a metadata file that
    /// tries to supply one is refused.
    #[serde(default, skip_deserializing, skip_serializing)]
    pub files: BTreeMap<String, String>,
    /// The virtual path of the source file inside [`Self::files`].
    #[serde(default, skip_deserializing, skip_serializing)]
    pub entry: String,
    /// SHA-256 over the whole frozen set, in path order.
    #[serde(default, skip_deserializing, skip_serializing_if = "String::is_empty")]
    pub content_hash: String,
    /// Declared input paths (dotted, from the root of an input) whose value is
    /// HTML and must not be escaped when an email template interpolates it.
    #[serde(default, skip_deserializing, skip_serializing)]
    pub html_paths: BTreeSet<String>,
}

/// The per-template tightenings of spec 019 §2.
///
/// Every one of them is optional and can only ever *narrow* the operation's own
/// declared bound: a template cannot buy itself more time, more pages, or a
/// larger file than the capability admits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentTemplateBounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pages: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_deadline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<String>,
}

impl DocumentTemplateBounds {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// The declared deadline in milliseconds, if any.
    pub fn cpu_deadline_ms(&self) -> Option<u64> {
        self.cpu_deadline.as_deref().and_then(parse_duration_ms)
    }

    /// The declared output ceiling in bytes, if any.
    pub fn max_output_bytes_value(&self) -> Option<u64> {
        self.max_output_bytes.as_deref().and_then(parse_byte_size)
    }
}

/// One refusal, naming the metadata path that earned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentTemplateError {
    pub path: String,
    pub message: String,
}

impl DocumentTemplateError {
    pub(crate) fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DocumentTemplateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

/// `<name>@<content hash>`: the pin the loader stamps onto every activity that
/// selects this template.
pub fn template_pin(template: &DocumentTemplate) -> String {
    format!("{}@{}", template.name, template.content_hash)
}

/// The SHA-256 of one template: its declaration and its frozen file set.
///
/// Both halves decide what a render does. The files are the program, and the
/// declaration is how the program is executed — an input's declared type is
/// what says whether its value is escaped, and the bounds are what the renderer
/// is held to. A hash over the bytes alone would let `inputs: { order: String }`
/// become `inputs: { order: Html }` under an unchanged pin and an identical
/// recorded process revision, which is the guarantee the pin exists to make.
/// `ingest::content_hash` hashes its whole declaration for the same reason.
///
/// The declaration is hashed as its serialized form with the hash field itself
/// cleared, so the value never depends on a previous one. The files follow in
/// path order, so the hash does not depend on the order the loader happened to
/// read them in.
pub fn content_hash(template: &DocumentTemplate) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"donat.document.template.v2\0");
    let declaration = DocumentTemplate {
        content_hash: String::new(),
        ..template.clone()
    };
    hasher.update(
        serde_json::to_vec(&declaration).expect("a template declaration always serializes"),
    );
    for (path, text) in &template.files {
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((text.len() as u64).to_be_bytes());
        hasher.update(text.as_bytes());
    }
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

/// Every template rule, applied to one deployment's metadata.
pub fn validate_document_templates(metadata: &Metadata) -> Vec<DocumentTemplateError> {
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, template) in metadata.templates.iter().enumerate() {
        let path = format!("templates[{index}]");
        if !seen.insert(template.name.as_str()) {
            errors.push(DocumentTemplateError::new(
                format!("{path}.name"),
                format!("document template `{}` is declared twice", template.name),
            ));
        }
        validate_template(template, &path, &metadata.custom_types, &mut errors);
    }
    let by_name: BTreeMap<&str, &DocumentTemplate> = metadata
        .templates
        .iter()
        .map(|template| (template.name.as_str(), template))
        .collect();
    for process in &metadata.processes {
        validate_process(process, &by_name, &metadata.custom_types, &mut errors);
    }
    errors
}

fn validate_template(
    template: &DocumentTemplate,
    path: &str,
    types: &CustomTypes,
    errors: &mut Vec<DocumentTemplateError>,
) {
    if template.name.is_empty()
        || !template
            .name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        errors.push(DocumentTemplateError::new(
            format!("{path}.name"),
            "a document template name is alphanumeric with `_` or `-`",
        ));
    }
    if !template
        .source
        .ends_with(&format!(".{}", template.kind.extension()))
    {
        errors.push(DocumentTemplateError::new(
            format!("{path}.source"),
            format!(
                "a {:?} template's source is a `.{}` file, but `{}` is not",
                template.kind,
                template.kind.extension(),
                template.source
            ),
        ));
    }
    for (input, declared) in &template.inputs {
        if RESERVED_INPUT_KEYS.contains(&input.as_str()) {
            errors.push(DocumentTemplateError::new(
                format!("{path}.inputs.{input}"),
                format!(
                    "`{input}` is reserved for the capability's own input; a template input \
                     cannot be named after one"
                ),
            ));
        }
        if resolve_type(types, declared).is_none() {
            errors.push(DocumentTemplateError::new(
                format!("{path}.inputs.{input}"),
                format!("`{declared}` is not a type this deployment declares"),
            ));
        }
    }
    if let Some(deadline) = &template.bounds.cpu_deadline
        && parse_duration_ms(deadline).is_none()
    {
        errors.push(DocumentTemplateError::new(
            format!("{path}.bounds.cpu_deadline"),
            format!("`{deadline}` is not a duration (for example `15s`)"),
        ));
    }
    if let Some(size) = &template.bounds.max_output_bytes
        && parse_byte_size(size).is_none()
    {
        errors.push(DocumentTemplateError::new(
            format!("{path}.bounds.max_output_bytes"),
            format!("`{size}` is not a byte size (for example `8MiB`)"),
        ));
    }
    if template.bounds.max_pages == Some(0) {
        errors.push(DocumentTemplateError::new(
            format!("{path}.bounds.max_pages"),
            "a page ceiling of zero admits no document",
        ));
    }
}

fn validate_process(
    process: &Process,
    templates: &BTreeMap<&str, &DocumentTemplate>,
    types: &CustomTypes,
    errors: &mut Vec<DocumentTemplateError>,
) {
    for (index, state) in process.states.iter().enumerate() {
        let path = format!("processes.{}.states[{index}]", process.name);
        match &state.operation {
            ProcessStateOperation::Request { request } => {
                validate_request(
                    &request.connector,
                    &request.operation,
                    &request.input,
                    &format!("{path}.request"),
                    templates,
                    types,
                    errors,
                );
            }
            ProcessStateOperation::ForEach { for_each } => {
                if let ProcessForEachState::Request { request, .. } = for_each.as_ref() {
                    validate_request(
                        &request.connector,
                        &request.operation,
                        &request.input,
                        &format!("{path}.for_each.request"),
                        templates,
                        types,
                        errors,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Spec 019 §7 `templates_typecheck_against_process_data`, in one place.
#[allow(clippy::too_many_arguments)]
fn validate_request(
    connector: &str,
    operation: &str,
    input: &BTreeMap<String, ProcessValue>,
    path: &str,
    templates: &BTreeMap<&str, &DocumentTemplate>,
    types: &CustomTypes,
    errors: &mut Vec<DocumentTemplateError>,
) {
    if connector != DOCUMENT_CAPABILITY {
        return;
    }
    let Some(selected) = input.get("template") else {
        errors.push(DocumentTemplateError::new(
            format!("{path}.input.template"),
            "a document activity selects its template by name",
        ));
        return;
    };
    // The template is a deploy-time decision, so it is a literal and never a
    // value the run computes: a process that could choose a template at runtime
    // would be supplying one by another name.
    let ProcessValue::Literal {
        literal: JsonValue::String(name),
    } = selected
    else {
        errors.push(DocumentTemplateError::new(
            format!("{path}.input.template"),
            "a document activity's `template` is a literal name from the deployment's \
             declared templates, not a computed value",
        ));
        return;
    };
    let Some(template) = templates.get(name.as_str()) else {
        errors.push(DocumentTemplateError::new(
            format!("{path}.input.template"),
            format!("document template `{name}` is not declared by this deployment"),
        ));
        return;
    };
    if template.kind.operation() != operation {
        errors.push(DocumentTemplateError::new(
            format!("{path}.operation"),
            format!(
                "document template `{name}` is a {:?} template and renders through `{}`, not `{}`",
                template.kind,
                template.kind.operation(),
                operation
            ),
        ));
    }

    // Both directions, because either one is a declaration nothing reads: an
    // unbound template input renders a hole, and a bound value the template
    // never names is silently dropped (ADR 034).
    let bound: BTreeSet<&str> = input
        .keys()
        .map(String::as_str)
        .filter(|key| !RESERVED_INPUT_KEYS.contains(key))
        .collect();
    for declared in template.inputs.keys() {
        if !bound.contains(declared.as_str()) {
            errors.push(DocumentTemplateError::new(
                format!("{path}.input"),
                format!(
                    "document template `{name}` declares input `{declared}`, which this \
                     activity does not bind"
                ),
            ));
        }
    }
    for key in &bound {
        if !template.inputs.contains_key(*key) {
            errors.push(DocumentTemplateError::new(
                format!("{path}.input.{key}"),
                format!("document template `{name}` declares no input `{key}`"),
            ));
        }
    }

    // A literal binding is the one case where the value itself is visible here,
    // so it is the one case that can be checked against the declared type
    // without the process compiler's contract catalog.
    for (key, value) in input {
        let Some(declared) = template.inputs.get(key) else {
            continue;
        };
        if let ProcessValue::Literal { literal } = value
            && let Err(message) = literal_matches(types, declared, literal)
        {
            errors.push(DocumentTemplateError::new(
                format!("{path}.input.{key}"),
                format!("document template `{name}` input `{key}` is `{declared}`, but {message}"),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// The type system, as a template reads it
// ---------------------------------------------------------------------------

/// A declared type reference: a base name plus its list and nullability
/// modifiers, in the `[Type!]!` spelling the rest of the metadata uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    pub base: String,
    pub list: bool,
    pub required: bool,
    pub item_required: bool,
}

/// Parse a type reference, returning `None` when the base name is not declared.
pub fn resolve_type(types: &CustomTypes, declared: &str) -> Option<TypeRef> {
    let reference = parse_type(declared)?;
    if is_builtin_scalar(&reference.base)
        || types
            .scalars
            .iter()
            .any(|scalar| scalar.name == reference.base)
        || types.enums.iter().any(|enum_| enum_.name == reference.base)
        || types
            .objects
            .iter()
            .any(|object| object.name == reference.base)
        || types
            .input_objects
            .iter()
            .any(|object| object.name == reference.base)
    {
        return Some(reference);
    }
    None
}

fn parse_type(declared: &str) -> Option<TypeRef> {
    let text = declared.trim();
    let (text, required) = match text.strip_suffix('!') {
        Some(inner) => (inner, true),
        None => (text, false),
    };
    if let Some(inner) = text.strip_prefix('[') {
        let inner = inner.strip_suffix(']')?;
        let (base, item_required) = match inner.trim().strip_suffix('!') {
            Some(base) => (base.trim(), true),
            None => (inner.trim(), false),
        };
        if base.is_empty() || !base.chars().all(is_type_character) {
            return None;
        }
        return Some(TypeRef {
            base: base.to_owned(),
            list: true,
            required,
            item_required,
        });
    }
    if text.is_empty() || !text.chars().all(is_type_character) {
        return None;
    }
    Some(TypeRef {
        base: text.to_owned(),
        list: false,
        required,
        item_required: false,
    })
}

fn is_type_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// The scalar names the metadata type system provides without a declaration.
///
/// `Html` is one of them, and it is the whole of spec 019 §4's escaping
/// contract: a string is escaped when an email template interpolates it, unless
/// its declared type says it is already markup.
pub fn is_builtin_scalar(name: &str) -> bool {
    matches!(
        name,
        "String" | "Int" | "Float" | "Boolean" | "ID" | "Html" | "Date" | "DateTime" | "Decimal"
    )
}

/// The scalar whose values are inserted into an email without escaping.
pub const HTML_SCALAR: &str = "Html";

/// Every dotted path under `declared` whose value is [`HTML_SCALAR`].
///
/// Resolved at load and frozen onto the template, so the renderer never has to
/// know the type system: it holds a set of paths and escapes everything else.
pub fn html_paths(types: &CustomTypes, root: &str, declared: &str) -> BTreeSet<String> {
    fn walk(
        types: &CustomTypes,
        prefix: &str,
        declared: &str,
        ancestors: &mut BTreeSet<String>,
        out: &mut BTreeSet<String>,
    ) {
        let Some(reference) = parse_type(declared) else {
            return;
        };
        if reference.base == HTML_SCALAR {
            out.insert(prefix.to_owned());
            return;
        }
        if !ancestors.insert(reference.base.clone()) {
            return;
        }
        let fields = types
            .objects
            .iter()
            .find(|object| object.name == reference.base)
            .map(|object| &object.fields)
            .or_else(|| {
                types
                    .input_objects
                    .iter()
                    .find(|object| object.name == reference.base)
                    .map(|object| &object.fields)
            });
        if let Some(fields) = fields {
            for field in fields {
                walk(
                    types,
                    &format!("{prefix}.{}", field.name),
                    &field.type_,
                    ancestors,
                    out,
                );
            }
        }
        ancestors.remove(&reference.base);
    }

    let mut out = BTreeSet::new();
    walk(types, root, declared, &mut BTreeSet::new(), &mut out);
    out
}

/// Check a literal against a declared type, shape first.
fn literal_matches(types: &CustomTypes, declared: &str, value: &JsonValue) -> Result<(), String> {
    let Some(reference) = parse_type(declared) else {
        return Err(format!("`{declared}` is not a type reference"));
    };
    if value.is_null() {
        return if reference.required {
            Err("the bound literal is null".to_owned())
        } else {
            Ok(())
        };
    }
    if reference.list {
        let JsonValue::Array(items) = value else {
            return Err("the bound literal is not a list".to_owned());
        };
        for item in items {
            if item.is_null() {
                if reference.item_required {
                    return Err("the bound literal has a null item".to_owned());
                }
                continue;
            }
            scalar_or_object_matches(types, &reference.base, item)?;
        }
        return Ok(());
    }
    scalar_or_object_matches(types, &reference.base, value)
}

fn scalar_or_object_matches(
    types: &CustomTypes,
    base: &str,
    value: &JsonValue,
) -> Result<(), String> {
    let ok = match base {
        "String" | "ID" | "Html" | "Date" | "DateTime" | "Decimal" => value.is_string(),
        "Int" => value.is_i64() || value.is_u64(),
        "Float" => value.is_number(),
        "Boolean" => value.is_boolean(),
        _ => {
            if let Some(enum_) = types.enums.iter().find(|enum_| enum_.name == base) {
                return match value.as_str() {
                    Some(text) if enum_.values.iter().any(|entry| entry.value == text) => Ok(()),
                    _ => Err(format!("the bound literal is not a `{base}` value")),
                };
            }
            let fields = types
                .objects
                .iter()
                .find(|object| object.name == base)
                .map(|object| &object.fields)
                .or_else(|| {
                    types
                        .input_objects
                        .iter()
                        .find(|object| object.name == base)
                        .map(|object| &object.fields)
                });
            let Some(fields) = fields else {
                // A declared scalar this crate knows nothing about admits any
                // JSON: the deployment named the type, not its shape.
                return Ok(());
            };
            let JsonValue::Object(object) = value else {
                return Err("the bound literal is not an object".to_owned());
            };
            for field in fields {
                match object.get(&field.name) {
                    Some(bound) => literal_matches(types, &field.type_, bound)
                        .map_err(|message| format!("field `{}`: {message}", field.name))?,
                    None if parse_type(&field.type_).is_some_and(|kind| kind.required) => {
                        return Err(format!("the bound literal has no `{}`", field.name));
                    }
                    None => {}
                }
            }
            for key in object.keys() {
                if !fields.iter().any(|field| &field.name == key) {
                    return Err(format!("the bound literal has an undeclared field `{key}`"));
                }
            }
            true
        }
    };
    if ok {
        Ok(())
    } else {
        Err(format!("the bound literal is not a `{base}`"))
    }
}

// ---------------------------------------------------------------------------
// Small grammars
// ---------------------------------------------------------------------------

/// `15s`, `500ms`, `2m`: the duration grammar process metadata already uses.
pub fn parse_duration_ms(source: &str) -> Option<u64> {
    let (digits, multiplier) = [("ms", 1_u64), ("s", 1_000), ("m", 60_000), ("h", 3_600_000)]
        .into_iter()
        .find_map(|(suffix, multiplier)| Some((source.strip_suffix(suffix)?, multiplier)))?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(multiplier)
}

/// `8MiB`, `512KiB`, `4096`: binary multiples only, because a document ceiling
/// that means 8_000_000 to the operator and 8_388_608 to the engine is a
/// ceiling nobody can reason about.
pub fn parse_byte_size(source: &str) -> Option<u64> {
    let (digits, multiplier) = [("KiB", 1_024_u64), ("MiB", 1_048_576), ("B", 1)]
        .into_iter()
        .find_map(|(suffix, multiplier)| Some((source.strip_suffix(suffix)?, multiplier)))
        .unwrap_or((source, 1));
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = digits.parse::<u64>().ok()?.checked_mul(multiplier)?;
    (value > 0).then_some(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn metadata(value: JsonValue) -> Metadata {
        serde_json::from_value(value).expect("test metadata deserializes")
    }

    fn messages(value: JsonValue) -> String {
        validate_document_templates(&metadata(value))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn deployment(inputs: JsonValue, bound: JsonValue) -> JsonValue {
        json!({
            "version": 3,
            "custom_types": {
                "objects": [
                    { "name": "order_document", "fields": [
                        { "name": "number", "type": "String!" },
                        { "name": "total", "type": "Decimal!" },
                        { "name": "note", "type": "Html" }
                    ] }
                ]
            },
            "templates": [{
                "name": "invoice",
                "kind": "pdf",
                "source": "templates/invoice.typ",
                "inputs": inputs
            }],
            "connectors": [{
                "name": "local.document",
                "module": "local.document",
                "operations": [{ "name": "pdf.render" }]
            }],
            "processes": [{
                "name": "bill", "kind": "process", "version": 1, "source": "default",
                "start_at": "render",
                "states": [{
                    "id": "render",
                    "request": {
                        "connector": "local.document",
                        "operation": "pdf.render",
                        "input": bound,
                        "timeout": { "schedule_to_start": "10s", "start_to_close": "30s" },
                        "retry": { "retry_on": ["timeout"], "max_attempts": 1, "initial_interval": "1s", "max_interval": "5s", "jitter": "1s" },
                        "next": "done"
                    }
                }]
            }]
        })
    }

    /// Spec 019 §7 `templates_typecheck_against_process_data`.
    ///
    /// A declared input the activity does not bind, a bound value the template
    /// does not declare, an undeclared type, and a literal that does not match
    /// its declared type: each is refused, on its own path, at `validate`.
    #[test]
    fn templates_typecheck_against_process_data() {
        // The declaration a deployment is allowed to write.
        assert_eq!(
            messages(deployment(
                json!({ "order": "order_document!" }),
                json!({
                    "template": { "literal": "invoice" },
                    "attachment": { "literal": "public.invoice.file" },
                    "claim_role": { "literal": "app" },
                    "order": { "state": "fetch", "field": "order" }
                })
            )),
            "",
            "a template whose declared inputs are exactly what the activity binds is valid"
        );

        // A declared input nothing binds: the template renders a hole.
        assert!(
            messages(deployment(
                json!({ "order": "order_document!" }),
                json!({ "template": { "literal": "invoice" } })
            ))
            .contains("declares input `order`, which this activity does not bind")
        );

        // A bound value the template never names: a declaration the runtime
        // ignores (ADR 034).
        assert!(
            messages(deployment(
                json!({ "order": "order_document!" }),
                json!({
                    "template": { "literal": "invoice" },
                    "order": { "state": "fetch", "field": "order" },
                    "customer": { "state": "fetch", "field": "customer" }
                })
            ))
            .contains("declares no input `customer`")
        );

        // A type this deployment never declared.
        assert!(
            messages(deployment(
                json!({ "order": "shipping_label!" }),
                json!({
                    "template": { "literal": "invoice" },
                    "order": { "state": "fetch", "field": "order" }
                })
            ))
            .contains("`shipping_label!` is not a type this deployment declares")
        );

        // A literal that does not match the declared type, at every depth the
        // literal makes visible.
        let refused = messages(deployment(
            json!({ "order": "order_document!" }),
            json!({
                "template": { "literal": "invoice" },
                "order": { "literal": { "number": 7, "total": "12.50" } }
            }),
        ));
        assert!(
            refused.contains("field `number`: the bound literal is not a `String`"),
            "a mistyped field is named: {refused}"
        );
        let refused = messages(deployment(
            json!({ "order": "order_document!" }),
            json!({
                "template": { "literal": "invoice" },
                "order": { "literal": { "number": "A-1", "total": "12.50", "shipping": "x" } }
            }),
        ));
        assert!(refused.contains("undeclared field `shipping`"), "{refused}");
        let refused = messages(deployment(
            json!({ "order": "order_document!" }),
            json!({
                "template": { "literal": "invoice" },
                "order": { "literal": { "number": "A-1" } }
            }),
        ));
        assert!(refused.contains("has no `total`"), "{refused}");

        // And the template a run picks for itself is not a template: it is a
        // way to supply one.
        assert!(
            messages(deployment(
                json!({}),
                json!({ "template": { "state": "choose", "field": "template" } })
            ))
            .contains("is a literal name from the deployment's declared templates")
        );
        assert!(
            messages(deployment(
                json!({}),
                json!({ "template": { "literal": "receipt" } })
            ))
            .contains("document template `receipt` is not declared by this deployment")
        );
    }

    /// A template declaration is refused for its own reasons too: a duplicate
    /// name, a source whose extension does not match its kind, an input named
    /// after a reserved key, and an unparseable bound.
    #[test]
    fn a_template_declaration_is_checked_on_its_own() {
        let refused = messages(json!({
            "version": 3,
            "templates": [
                { "name": "invoice", "kind": "pdf", "source": "templates/invoice.typ" },
                { "name": "invoice", "kind": "email", "source": "templates/invoice.typ" },
                {
                    "name": "receipt", "kind": "pdf", "source": "templates/receipt.typ",
                    "inputs": { "file_name": "String!" },
                    "bounds": { "cpu_deadline": "soon", "max_output_bytes": "8 megabytes", "max_pages": 0 }
                }
            ]
        }));
        assert!(refused.contains("`invoice` is declared twice"), "{refused}");
        assert!(
            refused.contains("template's source is a `.mjml` file"),
            "{refused}"
        );
        assert!(refused.contains("`file_name` is reserved"), "{refused}");
        assert!(refused.contains("`soon` is not a duration"), "{refused}");
        assert!(
            refused.contains("`8 megabytes` is not a byte size"),
            "{refused}"
        );
        assert!(refused.contains("page ceiling of zero"), "{refused}");
    }

    /// The kind and the operation are one decision: a PDF template cannot be
    /// rendered through the email operation.
    #[test]
    fn a_template_renders_only_through_its_own_operation() {
        let mut value = deployment(json!({}), json!({ "template": { "literal": "invoice" } }));
        value["processes"][0]["states"][0]["request"]["operation"] =
            JsonValue::String("email.render".to_owned());
        assert!(
            messages(value).contains("renders through `pdf.render`, not `email.render`"),
            "a kind and an operation are the same decision"
        );
    }

    /// The escaping contract of spec 019 §4 is resolved from the type system at
    /// load: a field declared `Html` is a path the renderer will not escape,
    /// and every other field is one it will.
    #[test]
    fn html_fields_are_resolved_from_the_declared_type() {
        let types: CustomTypes = serde_json::from_value(json!({
            "objects": [
                { "name": "order_document", "fields": [
                    { "name": "number", "type": "String!" },
                    { "name": "note", "type": "Html" },
                    { "name": "customer", "type": "customer_document!" }
                ] },
                { "name": "customer_document", "fields": [
                    { "name": "name", "type": "String!" },
                    { "name": "signature", "type": "Html" }
                ] }
            ]
        }))
        .expect("test types deserialize");
        assert_eq!(
            html_paths(&types, "order", "order_document!"),
            BTreeSet::from([
                "order.note".to_owned(),
                "order.customer.signature".to_owned()
            ])
        );
        assert_eq!(
            html_paths(&types, "body", "Html!"),
            BTreeSet::from(["body".to_owned()])
        );
        assert!(html_paths(&types, "name", "String!").is_empty());
    }

    /// The two grammars, including what they refuse.
    #[test]
    fn bounds_are_parsed_or_refused() {
        assert_eq!(parse_duration_ms("15s"), Some(15_000));
        assert_eq!(parse_duration_ms("500ms"), Some(500));
        assert_eq!(parse_duration_ms("2m"), Some(120_000));
        assert_eq!(parse_duration_ms("2 s"), None);
        assert_eq!(parse_duration_ms("s"), None);
        assert_eq!(parse_byte_size("8MiB"), Some(8 * 1_048_576));
        assert_eq!(parse_byte_size("512KiB"), Some(512 * 1_024));
        assert_eq!(parse_byte_size("4096"), Some(4_096));
        assert_eq!(parse_byte_size("8MB"), None);
        assert_eq!(parse_byte_size("0"), None);
    }

    /// One template, frozen the way the loader leaves it.
    fn template(files: BTreeMap<String, String>) -> DocumentTemplate {
        DocumentTemplate {
            name: "invoice".to_owned(),
            kind: DocumentTemplateKind::Pdf,
            source: "templates/invoice.typ".to_owned(),
            includes: vec![],
            inputs: BTreeMap::from([("order".to_owned(), "String".to_owned())]),
            bounds: DocumentTemplateBounds::default(),
            entry: "/invoice.typ".to_owned(),
            content_hash: String::new(),
            html_paths: BTreeSet::new(),
            files,
        }
    }

    /// The hash is over the whole frozen set and does not depend on read order.
    #[test]
    fn the_content_hash_covers_every_file_in_the_set() {
        let one = template(BTreeMap::from([
            ("/invoice.typ".to_owned(), "= Invoice".to_owned()),
            (
                "/partials/totals.typ".to_owned(),
                "#let total = 1".to_owned(),
            ),
        ]));
        let mut two = one.clone();
        assert_eq!(content_hash(&one), content_hash(&two));
        two.files.insert(
            "/partials/totals.typ".to_owned(),
            "#let total = 2".to_owned(),
        );
        assert_ne!(
            content_hash(&one),
            content_hash(&two),
            "an edit to an included file is an edit to the template"
        );
        // And the path is hashed with the text, so moving content between files
        // is a change too.
        let three = template(BTreeMap::from([(
            "/other.typ".to_owned(),
            "= Invoice".to_owned(),
        )]));
        assert_ne!(
            content_hash(&template(BTreeMap::from([(
                "/invoice.typ".to_owned(),
                "= Invoice".to_owned()
            )]))),
            content_hash(&three)
        );
    }

    /// And over the declaration that says how the set is executed: the input
    /// types that decide what is escaped, and the bounds the renderer is held
    /// to. Neither changes a byte on disk, and both change what a render does.
    #[test]
    fn the_content_hash_covers_the_declaration_too() {
        let base = template(BTreeMap::from([(
            "/invoice.typ".to_owned(),
            "= Invoice".to_owned(),
        )]));

        let mut escaped = base.clone();
        escaped.inputs.insert("order".to_owned(), "Html".to_owned());
        assert_ne!(
            content_hash(&base),
            content_hash(&escaped),
            "an input's declared type is what decides whether its value is escaped"
        );

        let mut bounded = base.clone();
        bounded.bounds.max_pages = Some(4);
        assert_ne!(content_hash(&base), content_hash(&bounded));

        // The recorded hash of a previous load is not part of the material, so
        // hashing a template twice is the same answer.
        let mut rehashed = base.clone();
        rehashed.content_hash = content_hash(&base);
        assert_eq!(content_hash(&base), content_hash(&rehashed));
    }
}
