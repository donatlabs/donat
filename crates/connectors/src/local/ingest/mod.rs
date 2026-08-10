//! `local.ingest` — reading a spreadsheet or a CSV somebody uploaded
//! (spec 020).
//!
//! | Operation | Backend | Product |
//! |---|---|---|
//! | `spreadsheet.read` | `calamine` over a [guarded archive](archive) | typed rows + typed rejections |
//! | `csv.read` | `csv` | typed rows + typed rejections |
//!
//! This is the reverse direction of spec 019 §5, and the difference between the
//! two directions is the whole design. A renderer's input is data the
//! deployment computed; a reader's input is a file a stranger chose. Three
//! things follow, and each is structural here rather than reviewed.
//!
//! **The input is hostile from the first byte to the last.** The bounds of spec
//! 020 §3 run in a fixed order — the stored file's size before it is opened,
//! the archive's declared entry count and expansion before anything is
//! decompressed, the real expansion while it streams, then sheet, row, column
//! and cell ceilings, then working memory and the deadline. The order is the
//! property: a ratio checked after decompression is not a check. See
//! [`archive`].
//!
//! **There is no schema inference.** A schema is deployment metadata, reached
//! through the [`LocalContext`](crate::local::LocalContext) exactly as a
//! document template is, and input names one by name. A column the schema does
//! not declare is ignored; a declared column the header does not carry fails the
//! whole operation before a row is read. Nothing about the answer's shape
//! depends on what the file happened to contain.
//!
//! **Nothing is written.** The product is a value: the rows that parsed and a
//! bounded, typed list of the ones that did not. No artifact, no column, no
//! role, no statement — the process decides what to do with an import, and it
//! decides after seeing what came back.

use std::sync::LazyLock;

use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::local::capability::{LocalCapability, LocalInvocation};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};

pub mod archive;
pub mod csv;
pub mod schema;
pub mod spreadsheet;

pub use archive::{ArchiveEntry, ArchiveLimits};
pub use schema::{
    Cell, CellRejection, DateSystem, IngestColumn, IngestColumnSpec, IngestKind, IngestRejection,
    IngestSchema, IngestSchemaSet, IngestSchemaSpec, RowErrorPolicy, Scalar,
};

/// The connector name every ingest operation is reached through.
pub const INGEST_CAPABILITY: &str = "local.ingest";

// ---------------------------------------------------------------------------
// The ceilings no schema can widen
// ---------------------------------------------------------------------------

/// The largest stored file either operation will open.
pub const MAX_SOURCE_BYTES: u64 = 32 * 1_024 * 1_024;
/// The most members an office document has any reason to carry.
pub const MAX_ARCHIVE_ENTRIES: u64 = 1_024;
/// The most an archive may expand to, declared or actual.
pub const MAX_UNCOMPRESSED_BYTES: u64 = 256 * 1_024 * 1_024;
/// The largest expansion factor an honest document claims.
pub const MAX_COMPRESSION_RATIO: u64 = 200;
/// The most rows either operation reads.
pub const MAX_ROWS: u64 = 1_000_000;
/// The most columns a header may carry.
pub const MAX_COLUMNS: u64 = 1_024;
/// The most bytes one cell may hold.
pub const MAX_CELL_BYTES: u64 = 32 * 1_024;
/// The most rejections the answer carries. The count is exact either way; what
/// is bounded is the list, because a hostile file's whole purpose can be to
/// make the *answer* the denial of service.
pub const MAX_REJECTIONS: u64 = 1_000;
/// Working memory for one read.
pub const MAX_WORKING_BYTES: usize = 320 * 1_024 * 1_024;

/// The capability's declaration, built once by the table in
/// [`crate::local::capabilities`].
pub fn capability() -> LocalCapability {
    LocalCapability::declare(INGEST_CAPABILITY, "1.0.0")
        .operation(spreadsheet::operation())
        .operation(csv::operation())
        .build()
        .expect("the ingest capability declaration is static and complete")
}

// ---------------------------------------------------------------------------
// The stored file
// ---------------------------------------------------------------------------

/// One stored attachment, as a reading sees it.
///
/// It travels in the [`LocalContext`](crate::local::LocalContext) beside the
/// input and never inside it. The reason is the one ADR 050 already gave for a
/// template: bytes in the input would be inside the value the bounds measure,
/// the journal retains, and the determinism probe hashes. Input names a stored
/// file by its handle; the dispatcher is what resolves that handle to bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    handle: String,
    file_name: String,
    media_type: String,
    bytes: Vec<u8>,
}

impl SourceFile {
    pub fn new(
        handle: impl Into<String>,
        file_name: impl Into<String>,
        media_type: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            handle: handle.into(),
            file_name: file_name.into(),
            media_type: media_type.into(),
            bytes,
        }
    }

    /// The same file, under the handle an input actually wrote.
    ///
    /// A capability finds its file by the literal string its input carries, so
    /// whoever resolves that string to bytes is the one that has to key the
    /// result by it. A store that keys by the canonical form of an identifier
    /// instead answers a differently-spelled name with nothing — after the
    /// download, and after the deadline it spent.
    #[must_use]
    pub fn under_handle(mut self, handle: impl Into<String>) -> Self {
        self.handle = handle.into();
        self
    }

    pub fn handle(&self) -> &str {
        &self.handle
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn byte_size(&self) -> u64 {
        self.bytes.len() as u64
    }
}

// ---------------------------------------------------------------------------
// One reading
// ---------------------------------------------------------------------------

/// Everything one read is allowed to do, resolved before it starts.
pub(crate) struct Reading<'a> {
    pub schema: &'a IngestSchema,
    pub source: &'a SourceFile,
    pub max_rows: u64,
    pub max_columns: u64,
    pub max_cell_bytes: u64,
    pub max_rejections: u64,
    pub max_working_bytes: u64,
}

impl<'a> Reading<'a> {
    /// The archive ceilings, narrowed by the schema and by nothing else.
    pub fn archive_limits(&self) -> ArchiveLimits {
        let spec = self.schema.spec();
        ArchiveLimits {
            max_entries: self
                .schema
                .narrowed(spec.max_archive_entries, MAX_ARCHIVE_ENTRIES),
            max_uncompressed_bytes: self
                .schema
                .narrowed(spec.max_uncompressed_bytes, MAX_UNCOMPRESSED_BYTES),
            max_compression_ratio: self
                .schema
                .narrowed(spec.max_compression_ratio, MAX_COMPRESSION_RATIO),
        }
    }
}

/// Bounds 1 and the two selections that precede it.
///
/// In order: the schema the input names (there is no other way to get one), the
/// stored file the input names, and then — before the file is opened, parsed,
/// sniffed, or decompressed — its size.
pub(crate) fn begin<'a>(
    invocation: &'a LocalInvocation<'a>,
    kind: IngestKind,
) -> Result<Reading<'a>, ConnectorFailure> {
    let input = invocation.input();
    let name = input
        .get("schema")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            refuse(
                "ingest_schema_required",
                "an ingest activity names the schema it reads with; there is no inference",
            )
        })?;
    let schema = invocation
        .context()
        .ingest_schemas()
        .get(name)
        .ok_or_else(|| {
            refuse(
                "ingest_schema_unknown",
                "the selected ingest schema is not declared by this deployment",
            )
        })?;
    if schema.kind() != kind {
        return Err(refuse(
            "ingest_schema_wrong_kind",
            "the selected ingest schema is read through another operation",
        ));
    }

    let handle = input
        .get("source")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            refuse(
                "ingest_source_required",
                "an ingest activity names the stored file it reads",
            )
        })?;
    let source = invocation.context().source(handle).ok_or_else(|| {
        refuse(
            "ingest_source_missing",
            "the stored file this activity names was not resolved for this execution",
        )
    })?;

    // BOUND 1. Before the file is opened at all.
    let spec = schema.spec();
    if source.byte_size() > schema.narrowed(spec.max_source_bytes, MAX_SOURCE_BYTES) {
        return Err(refuse(
            "ingest_source_too_large",
            "the stored file is larger than its schema admits, and is not opened",
        ));
    }

    Ok(Reading {
        schema,
        source,
        max_rows: schema.narrowed(spec.max_rows, MAX_ROWS),
        max_columns: schema.narrowed(spec.max_columns, MAX_COLUMNS),
        max_cell_bytes: schema.narrowed(spec.max_cell_bytes, MAX_CELL_BYTES),
        max_rejections: schema.narrowed(spec.max_rejections, MAX_REJECTIONS),
        max_working_bytes: schema.narrowed(spec.max_working_bytes, MAX_WORKING_BYTES as u64),
    })
}

/// Charge working memory against the operation's ceiling and the schema's
/// narrowing of it, with one code either way: which of the two was reached is
/// not something an operator can act on differently.
pub(crate) fn charge(
    invocation: &LocalInvocation<'_>,
    ceiling: u64,
    bytes: usize,
) -> Result<(), ConnectorFailure> {
    invocation.reserve(bytes)?;
    if invocation.intermediate_used() as u64 > ceiling {
        return Err(ConnectorFailure::new(
            ConnectorErrorClass::Validation,
            "local_intermediate_too_large",
            "local capability execution exceeds the operation's declared working-memory ceiling",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The answer
// ---------------------------------------------------------------------------

/// The rows that parsed and the ones that did not, assembled as they are read.
pub(crate) struct Rows {
    rows: Vec<JsonValue>,
    rejected: Vec<JsonValue>,
    rejected_count: u64,
    max_rejections: u64,
    policy: RowErrorPolicy,
}

impl Rows {
    pub fn new(reading: &Reading<'_>) -> Self {
        Self {
            rows: Vec::new(),
            rejected: Vec::new(),
            rejected_count: 0,
            max_rejections: reading.max_rejections,
            policy: reading.schema.on_row_error(),
        }
    }

    pub fn accept(&mut self, row: JsonMap<String, JsonValue>) {
        self.rows.push(JsonValue::Object(row));
    }

    /// Record one rejected cell. `row` is the file's own 1-based row number, so
    /// an operator can open the file and look at it.
    pub fn reject(
        &mut self,
        row: u64,
        column: &IngestColumn,
        reason: CellRejection,
    ) -> Result<(), ConnectorFailure> {
        if self.policy == RowErrorPolicy::Fail {
            return Err(refuse(
                "ingest_row_rejected",
                "a row did not parse and the schema declares `on_row_error: fail`",
            ));
        }
        self.rejected_count += 1;
        if self.rejected.len() as u64 >= self.max_rejections {
            return Ok(());
        }
        self.rejected.push(json!({
            "row": row,
            "column": column.header,
            "field": column.field,
            "reason": reason.reason(),
        }));
        Ok(())
    }

    /// The typed answer. Key order is fixed here, so two reads of one file
    /// serialize identically.
    pub fn finish(self, reading: &Reading<'_>, sheet: Option<&str>) -> JsonValue {
        let row_count = self.rows.len();
        let truncated = self.rejected_count > self.rejected.len() as u64;
        json!({
            "schema": reading.schema.name(),
            "source": reading.source.handle(),
            "file_name": reading.source.file_name(),
            "sheet": sheet,
            "rows": self.rows,
            "row_count": row_count,
            "rejected": self.rejected,
            "rejected_count": self.rejected_count,
            "rejected_truncated": truncated,
        })
    }
}

pub(crate) fn refuse(code: &'static str, message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(ConnectorErrorClass::Validation, code, message)
}

// ---------------------------------------------------------------------------
// The registration probes
// ---------------------------------------------------------------------------

/// The schemas compiled into this binary.
///
/// They exist for the reason the built-in document templates do: ADR 044 makes
/// determinism a registration condition, and a registration that depended on a
/// deployment's declarations would admit an operation on one deployment and
/// refuse it on another.
pub fn builtin_schemas() -> IngestSchemaSet {
    let probe = |name: &str, kind: IngestKind, sheet: Option<&str>| IngestSchemaSpec {
        name: name.to_owned(),
        kind,
        columns: vec![IngestColumnSpec {
            header: "Value".to_owned(),
            field: "value".to_owned(),
            declared: "String!".to_owned(),
            trim: true,
            ..IngestColumnSpec::default()
        }],
        sheet_by_name: sheet.map(str::to_owned),
        header_row: 1,
        ..IngestSchemaSpec::default()
    };
    IngestSchemaSet::resolve([
        probe(
            spreadsheet::PROBE_SCHEMA,
            IngestKind::Spreadsheet,
            Some("Probe"),
        ),
        probe(csv::PROBE_SCHEMA, IngestKind::Csv, None),
    ])
    .expect("the built-in probe schemas are static and complete")
}

/// The stored files compiled into this binary, for the same reason.
pub fn builtin_sources() -> Vec<SourceFile> {
    vec![
        SourceFile::new(
            csv::PROBE_SOURCE,
            "probe.csv",
            "text/csv",
            b"Value\ndonat\n".to_vec(),
        ),
        SourceFile::new(
            spreadsheet::PROBE_SOURCE,
            "probe.xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            probe_workbook().clone(),
        ),
    ]
}

/// The probe workbook, written once.
///
/// It is assembled here rather than checked in as a binary blob so that what
/// the registration proof reads is visible as XML in this file — and so that a
/// reviewer can see there is no macro part, no external link, and no formula in
/// the one workbook this binary is built to be able to read.
fn probe_workbook() -> &'static Vec<u8> {
    static WORKBOOK: LazyLock<Vec<u8>> = LazyLock::new(|| {
        use std::io::Write;

        const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#;
        const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;
        const WORKBOOK_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<workbookPr date1904="0"/><sheets><sheet name="Probe" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;
        const SHEET: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>Value</t></is></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>donat</t></is></c></row>
</sheetData></worksheet>"#;

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        // A fixed modification time, because the probe's bytes are compared
        // byte for byte at registration and a clock would make them differ.
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(
                zip::DateTime::from_date_and_time(2026, 1, 1, 0, 0, 0)
                    .expect("a fixed archive timestamp is valid"),
            );
        for (name, body) in [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("xl/workbook.xml", WORKBOOK_XML),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/worksheets/sheet1.xml", SHEET),
        ] {
            writer
                .start_file(name, options)
                .expect("the probe workbook's parts are static");
            writer
                .write_all(body.as_bytes())
                .expect("the probe workbook's parts are static");
        }
        writer
            .finish()
            .expect("the probe workbook is written")
            .into_inner()
    });
    &WORKBOOK
}
