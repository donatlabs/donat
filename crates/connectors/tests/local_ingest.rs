//! Spec 020 — `local.ingest`: reading a spreadsheet or a CSV a user uploaded.
//!
//! Every fixture in this file is authored here, byte by byte, including the
//! adversarial ones. That is deliberate: a zip bomb downloaded from somewhere
//! proves that somebody else's file is refused, while one written here proves
//! *which* bound refused it and *when* — before decompression, during
//! streaming, or at the row ceiling. The workbooks are hand-written OOXML for
//! the same reason: the 1904 date system, a serial date with no date style, a
//! formula whose cached value disagrees with its own formula, and a
//! numeric-looking string are all things no writer library will produce on
//! request.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{Cursor, Write};

use donat_connectors::local::ingest::{
    ArchiveLimits, IngestColumnSpec, IngestKind, IngestSchemaSet, IngestSchemaSpec, RowErrorPolicy,
    SourceFile, archive,
};
use donat_connectors::local::{LocalContext, LocalProduct, StopSignal, capability};
use donat_connectors::sdk::ConnectorFailure;
use donat_connectors::sdk::errors::ConnectorErrorClass;
use serde_json::{Value as JsonValue, json};
use zip::write::SimpleFileOptions;

// ---------------------------------------------------------------------------
// A per-thread allocation counter
// ---------------------------------------------------------------------------
//
// The same instrument spec 022's image suite uses, and here for the same
// reason: "the extent is checked before a range is materialized" is a statement
// about *memory*, and the only honest way to assert it is to measure the peak.
// A refusal code alone would still pass if the reader had allocated the grid
// first and refused afterwards.

struct Counting;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

/// Charge or refund, without allocating: both cells are `const`-initialized, so
/// touching them from inside the allocator cannot recurse into it.
fn charge_bytes(delta: isize) {
    let _ = LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            charge_bytes(layout.size() as isize);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        charge_bytes(-(layout.size() as isize));
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            charge_bytes(layout.size() as isize);
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            charge_bytes(new_size as isize - layout.size() as isize);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The most bytes this thread had live at once while `body` ran.
fn peak_bytes<T>(body: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(before));
    let value = body();
    let peak = PEAK.with(Cell::get);
    (value, (peak - before).max(0) as usize)
}

// ---------------------------------------------------------------------------
// Running one read
// ---------------------------------------------------------------------------

/// Execute one ingest operation over one declared schema and one stored file.
fn read(
    operation: &str,
    schema: IngestSchemaSpec,
    source: SourceFile,
) -> Result<JsonValue, ConnectorFailure> {
    let handle = source.handle().to_owned();
    let name = schema.name.clone();
    let schemas = IngestSchemaSet::resolve([schema]).expect("the test schema resolves");
    let context = LocalContext::default()
        .with_ingest_schemas(schemas)
        .with_source(source);
    let capability = capability("local.ingest").expect("local.ingest is compiled into this binary");
    let operation = capability
        .admit_operation(operation)
        .expect("the operation is declared and executable");
    match operation.execute(
        &json!({ "schema": name, "source": handle }),
        &context,
        None,
        &StopSignal::new(),
    )? {
        LocalProduct::Value(value) => Ok(value),
        LocalProduct::Artifact { .. } => panic!("an ingest operation returns typed rows"),
    }
}

/// Execute one ingest operation over an input written out in full — for the
/// cases where what the input *names* and what the deployment *declared* have
/// to differ.
fn execute(
    operation: &str,
    input: &JsonValue,
    schema: IngestSchemaSpec,
    source: SourceFile,
) -> Result<JsonValue, ConnectorFailure> {
    let schemas = IngestSchemaSet::resolve([schema]).expect("the test schema resolves");
    let context = LocalContext::default()
        .with_ingest_schemas(schemas)
        .with_source(source);
    let capability = capability("local.ingest").expect("local.ingest is compiled into this binary");
    match capability
        .admit_operation(operation)
        .expect("the operation is declared and executable")
        .execute(input, &context, None, &StopSignal::new())?
    {
        LocalProduct::Value(value) => Ok(value),
        LocalProduct::Artifact { .. } => panic!("an ingest operation returns typed rows"),
    }
}

/// The price list every test declares, in its spreadsheet spelling.
fn price_list() -> IngestSchemaSpec {
    IngestSchemaSpec {
        name: "price_list".to_owned(),
        kind: IngestKind::Spreadsheet,
        columns: vec![
            column("SKU", "sku", "String!"),
            column("Price", "price", "Decimal!"),
            column("Valid from", "valid_from", "Date"),
        ],
        sheet_by_name: Some("Prices".to_owned()),
        header_row: 1,
        on_row_error: RowErrorPolicy::Collect,
        max_rows: Some(64),
        max_columns: Some(16),
        max_cell_bytes: Some(64),
        max_source_bytes: Some(256 * 1_024),
        max_archive_entries: Some(32),
        max_uncompressed_bytes: Some(1_024 * 1_024),
        max_compression_ratio: Some(50),
        ..IngestSchemaSpec::default()
    }
}

fn column(header: &str, field: &str, declared: &str) -> IngestColumnSpec {
    IngestColumnSpec {
        header: header.to_owned(),
        field: field.to_owned(),
        declared: declared.to_owned(),
        trim: true,
        ..IngestColumnSpec::default()
    }
}

fn stored(name: &str, media_type: &str, bytes: Vec<u8>) -> SourceFile {
    SourceFile::new("upload", name, media_type, bytes)
}

const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

// ---------------------------------------------------------------------------
// §4 The required proofs
// ---------------------------------------------------------------------------

/// `ingest_requires_declared_schema`.
///
/// A file with no matching declared schema is rejected, and there is no other
/// way in: the operation reads its columns from the declaration or it reads
/// nothing. The input cannot carry a schema, and no header is ever inferred.
#[test]
fn ingest_requires_declared_schema() {
    let workbook = workbook(WorkbookFixture::default());

    // A schema this deployment did not declare: the set carries `price_list`
    // and the activity asks for something else.
    let failure = execute(
        "spreadsheet.read",
        &json!({ "schema": "other", "source": "upload" }),
        price_list(),
        stored("prices.xlsx", XLSX, workbook.clone()),
    )
    .expect_err("a schema the deployment did not declare is refused");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "ingest_schema_unknown");

    // A schema declared for the other operation.
    let failure = read(
        "csv.read",
        price_list(),
        stored("prices.xlsx", XLSX, workbook.clone()),
    )
    .expect_err("a spreadsheet schema does not read a CSV");
    assert_eq!(failure.code(), "ingest_schema_wrong_kind");

    // And an input that tries to carry its own columns is not a schema: the
    // key is ignored, and the declared schema still decides the shape.
    let value = execute(
        "spreadsheet.read",
        &json!({
            "schema": "price_list",
            "source": "upload",
            "columns": [{ "header": "SKU", "field": "sku", "type": "String!" }]
        }),
        price_list(),
        stored("prices.xlsx", XLSX, workbook),
    )
    .expect("the declared schema reads the file");
    assert_eq!(
        value["rows"][0]
            .as_object()
            .expect("a row is an object")
            .len(),
        3,
        "the declared schema decides the columns, not the input"
    );
}

/// `ingest_missing_column_fails_early`.
///
/// A declared column absent from the header fails the whole operation, before
/// any row is parsed — the point being that a partially applied import is
/// worse than no import at all.
#[test]
fn ingest_missing_column_fails_early() {
    let narrow = workbook(WorkbookFixture {
        header: &["SKU", "Price"],
        ..WorkbookFixture::default()
    });
    let failure = read(
        "spreadsheet.read",
        price_list(),
        stored("prices.xlsx", XLSX, narrow),
    )
    .expect_err("a declared column missing from the header fails the file");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "ingest_header_column_missing");

    // An *undeclared* column in the file is the other half of the same rule:
    // it is ignored, never inferred into the output.
    let wider = workbook(WorkbookFixture {
        header: &["SKU", "Price", "Valid from", "Internal note"],
        rows: &[&["A-1", "12.50", "2026-01-31", "leave me out"]],
        ..WorkbookFixture::default()
    });
    let value = read(
        "spreadsheet.read",
        price_list(),
        stored("prices.xlsx", XLSX, wider),
    )
    .expect("an undeclared column is ignored");
    assert_eq!(
        value["rows"],
        json!([{ "sku": "A-1", "price": "12.50", "valid_from": "2026-01-31" }])
    );
}

/// `ingest_bounds_precede_decompression`.
///
/// An archive whose *declared* expansion exceeds the ratio ceiling is refused
/// from the central directory alone. The proof that nothing was decompressed is
/// the working-memory charge: the streaming pass is the only thing that
/// allocates, so a refusal with nothing charged is a refusal taken before a
/// byte was expanded.
#[test]
fn ingest_bounds_precede_decompression() {
    // 400 KiB of zeros deflates to well under 1% of that.
    let bomb = zip_of(&[("xl/worksheets/sheet1.xml", vec![b'0'; 400 * 1_024])]);
    let ratio = 400 * 1_024 / bomb.len() as u64;
    assert!(
        ratio > 50,
        "the fixture must actually be a bomb: ratio {ratio}"
    );

    let failure = read(
        "spreadsheet.read",
        price_list(),
        stored("bomb.xlsx", XLSX, bomb.clone()),
    )
    .expect_err("an archive over the ratio ceiling is refused");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "ingest_compression_ratio_exceeded");

    // That nothing was decompressed is structural rather than timed: the
    // refusal comes out of `admit_declared`, which is handed the archive's
    // central directory and no way to expand an entry. The streaming pass is a
    // different function, it is the only one that can expand anything, and it
    // is never reached.
    assert_eq!(
        archive::admit_declared(
            &bomb,
            &ArchiveLimits {
                max_entries: 32,
                max_uncompressed_bytes: 1_024 * 1_024,
                max_compression_ratio: 50,
            },
        )
        .expect_err("the declared pass answers on its own")
        .code(),
        "ingest_compression_ratio_exceeded"
    );

    // The entry count is the other half of the same pass.
    let many: Vec<(String, Vec<u8>)> = (0..64)
        .map(|index| (format!("xl/worksheets/sheet{index}.xml"), b"x".to_vec()))
        .collect();
    let failure = read(
        "spreadsheet.read",
        price_list(),
        stored("many.xlsx", XLSX, zip_parts(&many)),
    )
    .expect_err("more entries than the ceiling admits is refused");
    assert_eq!(failure.code(), "ingest_archive_entries_exceeded");

    // And the declared total, for an archive whose per-entry ratio is honest
    // but whose sum is not.
    let wide: Vec<(&str, Vec<u8>)> = (0..8)
        .map(|index| {
            (
                ENTRY_NAMES[index],
                (0..200_u32 * 1_024).map(|byte| byte as u8).collect(),
            )
        })
        .collect();
    let failure = read(
        "spreadsheet.read",
        price_list(),
        stored("wide.xlsx", XLSX, zip_of(&wide)),
    )
    .expect_err("a declared expansion over the ceiling is refused");
    assert_eq!(failure.code(), "ingest_archive_expansion_exceeded");
}

/// `ingest_lying_header_is_caught`.
///
/// An entry that understates its uncompressed size in both the local header and
/// the central directory still fails, because the streaming pass counts the
/// bytes that actually come out.
#[test]
fn ingest_lying_header_is_caught() {
    let honest = zip_of(&[(
        "xl/worksheets/sheet1.xml",
        (0..64_u32 * 1_024).map(|byte| byte as u8).collect(),
    )]);
    let liar = understate_sizes(&honest, 64 * 1_024, 16);
    assert_ne!(honest, liar, "the fixture must actually have been patched");

    let failure = read(
        "spreadsheet.read",
        price_list(),
        stored("liar.xlsx", XLSX, liar),
    )
    .expect_err("an entry that lies about its size is refused while it streams");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "ingest_uncompressed_overflow");
}

/// `ingest_rejects_active_content`.
///
/// External workbook links, remote data connections, embedded objects, and
/// macro-enabled parts are refused before parsing, from the archive's own part
/// names — no decompression, no XML, no heuristics on content.
#[test]
fn ingest_rejects_active_content() {
    for part in [
        "xl/externalLinks/externalLink1.xml",
        "xl/connections.xml",
        "xl/embeddings/oleObject1.bin",
        "xl/vbaProject.bin",
        "xl/macrosheets/sheet1.xml",
        "xl/activeX/activeX1.xml",
    ] {
        let mut parts = workbook_parts(&WorkbookFixture::default());
        parts.push((part.to_owned(), b"<x/>".to_vec()));
        let failure = read(
            "spreadsheet.read",
            price_list(),
            stored("active.xlsx", XLSX, zip_parts(&parts)),
        )
        .unwrap_err();
        assert_eq!(
            failure.class(),
            ConnectorErrorClass::Validation,
            "{part} must be refused"
        );
        assert_eq!(failure.code(), "ingest_active_content", "{part}");
    }
}

/// `ingest_never_evaluates_formulas`.
///
/// The fixture's formula cell says `=1+1` and carries a cached value of `5`.
/// A reader that evaluated would produce `2`; this one produces `5`, because
/// what it reads is the cached value the writer stored. A formula with no
/// cached value at all is a typed rejection rather than a computation.
#[test]
fn ingest_never_evaluates_formulas() {
    let value = read(
        "spreadsheet.read",
        price_list(),
        stored("formulas.xlsx", XLSX, workbook(FORMULA_FIXTURE)),
    )
    .expect("a formula's cached value is read");
    assert_eq!(value["rows"][0]["price"], json!("5"));
    assert_eq!(value["rejected"][0]["row"], json!(3));
    assert_eq!(
        value["rejected"][0]["reason"],
        json!("formula_without_cached_value")
    );
}

/// `ingest_date_systems_are_exact`.
///
/// The three quiet corruptions, each with its own hand-written fixture: the
/// same serial under the 1900 and the 1904 date system, a serial date that
/// carries no date style at all, and a string that looks like a number.
#[test]
fn ingest_date_systems_are_exact() {
    // The same bytes, the same serial, one flag apart.
    let nineteen_hundred = read(
        "spreadsheet.read",
        price_list(),
        stored("dates.xlsx", XLSX, workbook(SERIAL_FIXTURE)),
    )
    .expect("a serial date under the 1900 system reads");
    let nineteen_four = read(
        "spreadsheet.read",
        price_list(),
        stored(
            "dates.xlsx",
            XLSX,
            workbook(WorkbookFixture {
                date1904: true,
                ..SERIAL_FIXTURE
            }),
        ),
    )
    .expect("a serial date under the 1904 system reads");

    // Serial 39448 is 2008-01-01 in the 1900 system. The 1904 epoch is 1462
    // days later, so the same serial is 1462 days later too.
    assert_eq!(
        nineteen_hundred["rows"][0]["valid_from"],
        json!("2008-01-01")
    );
    assert_eq!(nineteen_four["rows"][0]["valid_from"], json!("2012-01-02"));
    // Both rows carry a serial with a date style and one without: a number in a
    // date column is a date, and it is the same date either way.
    assert_eq!(
        nineteen_hundred["rows"][1]["valid_from"],
        json!("2008-01-01")
    );
    assert_eq!(nineteen_four["rows"][1]["valid_from"], json!("2012-01-02"));

    // A string that looks numeric stays the string it was: `00123` is a SKU,
    // not the number 123, and no rounding, no exponent, no leading-zero loss
    // happens on the way through.
    assert_eq!(nineteen_hundred["rows"][0]["sku"], json!("00123"));
    assert_eq!(nineteen_hundred["rows"][1]["sku"], json!("1.20E+02"));
    // And a *number* in a string column is refused rather than stringified,
    // because "12.5" and "12.50" are the same number and different SKUs.
    assert_eq!(nineteen_hundred["rejected"][0]["field"], json!("sku"));
    assert_eq!(
        nineteen_hundred["rejected"][0]["reason"],
        json!("cell_type_mismatch")
    );

    // A CSV has no date system, so a serial number in a date column is exactly
    // what it looks like: not a date.
    let value = read(
        "csv.read",
        IngestSchemaSpec {
            kind: IngestKind::Csv,
            sheet_by_name: None,
            ..price_list()
        },
        stored(
            "prices.csv",
            "text/csv",
            b"SKU,Price,Valid from\n00123,12.50,39448\n".to_vec(),
        ),
    )
    .expect("a CSV reads");
    assert_eq!(value["rows"], json!([]));
    assert_eq!(value["rejected"][0]["reason"], json!("cell_not_a_date"));
    // ... and the numeric-looking string is still a string here too.
    let value = read(
        "csv.read",
        IngestSchemaSpec {
            kind: IngestKind::Csv,
            sheet_by_name: None,
            columns: vec![column("SKU", "sku", "String!")],
            ..price_list()
        },
        stored("prices.csv", "text/csv", b"SKU\n00123\n1.20E+02\n".to_vec()),
    )
    .expect("a CSV reads");
    assert_eq!(
        value["rows"],
        json!([{ "sku": "00123" }, { "sku": "1.20E+02" }])
    );
}

/// `ingest_row_errors_are_typed_and_bounded`.
///
/// Every rejection names its row, its column, its field, and a reason from a
/// closed set — never the cell's own content, which is the attacker's text.
/// The list is bounded, and `on_row_error: fail` refuses the file whole.
#[test]
fn ingest_row_errors_are_typed_and_bounded() {
    let rows: Vec<[String; 3]> = (0..60)
        .map(|index| {
            [
                format!("A-{index}"),
                "not a decimal".to_owned(),
                "2026-01-31".to_owned(),
            ]
        })
        .collect();
    let borrowed: Vec<[&str; 3]> = rows
        .iter()
        .map(|row| [row[0].as_str(), row[1].as_str(), row[2].as_str()])
        .collect();
    let cells: Vec<&[&str]> = borrowed.iter().map(|row| &row[..]).collect();
    let workbook = workbook(WorkbookFixture {
        rows: &cells,
        ..WorkbookFixture::default()
    });

    let value = read(
        "spreadsheet.read",
        IngestSchemaSpec {
            max_rejections: Some(8),
            ..price_list()
        },
        stored("prices.xlsx", XLSX, workbook.clone()),
    )
    .expect("collected row errors are not a failure");
    assert_eq!(value["rows"], json!([]));
    assert_eq!(value["rejected_count"], json!(60));
    assert_eq!(
        value["rejected"]
            .as_array()
            .expect("the rejection list is a list")
            .len(),
        8,
        "the rejection list is bounded"
    );
    assert_eq!(value["rejected_truncated"], json!(true));
    assert_eq!(
        value["rejected"][0],
        json!({ "row": 2, "column": "Price", "field": "price", "reason": "cell_not_a_decimal" })
    );
    let rendered = value["rejected"].to_string();
    assert!(
        !rendered.contains("not a decimal"),
        "a rejection carries a reason, never the cell's own text: {rendered}"
    );

    // `fail` refuses the file whole rather than returning what parsed.
    let failure = read(
        "spreadsheet.read",
        IngestSchemaSpec {
            on_row_error: RowErrorPolicy::Fail,
            ..price_list()
        },
        stored("prices.xlsx", XLSX, workbook),
    )
    .expect_err("`fail` rejects the file whole");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "ingest_row_rejected");
}

/// `ingest_writes_nothing`.
///
/// The capability returns typed values and nothing else: no artifact, no file,
/// no column, no role. There is no branch of either operation that produces a
/// [`LocalProduct::Artifact`], which is the only way bytes or a database write
/// could leave a local capability at all.
#[test]
fn ingest_writes_nothing() {
    let value = read(
        "spreadsheet.read",
        price_list(),
        stored("prices.xlsx", XLSX, workbook(WorkbookFixture::default())),
    )
    .expect("a well-formed file reads");
    assert!(value.get("file").is_none());
    assert!(value.get("attachment").is_none());
    assert!(value.get("claim_role").is_none());

    // And the whole surface says so: an ingest operation's product is a value.
    let capability = capability("local.ingest").expect("local.ingest is compiled");
    for operation in capability.operations() {
        let evidence = operation
            .effect()
            .determinism_evidence()
            .expect("a pure operation carries its probe");
        let product = operation
            .execute(
                evidence.probe(),
                LocalContext::builtin(),
                None,
                &StopSignal::new(),
            )
            .expect("the declared probe reads");
        assert!(
            matches!(product, LocalProduct::Value(_)),
            "{} produces a value, never a file",
            operation.id()
        );
    }
}

/// `ingest_is_deterministic`.
///
/// The same file and the same schema produce identical output twice — which is
/// also the condition the capability was registered on, since the table's
/// double render runs over both operations' declared probes.
#[test]
fn ingest_is_deterministic() {
    for (operation, schema, source) in [
        (
            "spreadsheet.read",
            price_list(),
            stored("prices.xlsx", XLSX, workbook(WorkbookFixture::default())),
        ),
        (
            "csv.read",
            IngestSchemaSpec {
                kind: IngestKind::Csv,
                sheet_by_name: None,
                ..price_list()
            },
            stored(
                "prices.csv",
                "text/csv",
                b"SKU,Price,Valid from\nA-1,12.50,2026-01-31\n".to_vec(),
            ),
        ),
    ] {
        let first = read(operation, schema.clone(), source.clone()).expect("the file reads");
        let second = read(operation, schema, source).expect("the file reads again");
        assert_eq!(first, second, "{operation} is a function of its input");
    }
}

// ---------------------------------------------------------------------------
// §3 The bounds, in the order they are applied
// ---------------------------------------------------------------------------

/// Bound 1: the stored file's size, before it is opened.
#[test]
fn ingest_refuses_a_stored_file_over_its_ceiling() {
    let failure = read(
        "spreadsheet.read",
        IngestSchemaSpec {
            max_source_bytes: Some(64),
            ..price_list()
        },
        stored("prices.xlsx", XLSX, workbook(WorkbookFixture::default())),
    )
    .expect_err("a stored file over the declared ceiling is never opened");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "ingest_source_too_large");

    // The same ceiling applies to a CSV, which is not an archive at all.
    let failure = read(
        "csv.read",
        IngestSchemaSpec {
            kind: IngestKind::Csv,
            sheet_by_name: None,
            max_source_bytes: Some(4),
            ..price_list()
        },
        stored("prices.csv", "text/csv", b"SKU,Price,Valid from\n".to_vec()),
    )
    .expect_err("a stored file over the declared ceiling is never opened");
    assert_eq!(failure.code(), "ingest_source_too_large");
}

/// Bound 4: sheet, row, column, and per-cell ceilings, each on its own.
#[test]
fn ingest_sheet_row_column_and_cell_bounds_are_exact() {
    // A sheet the schema does not name.
    let failure = read(
        "spreadsheet.read",
        IngestSchemaSpec {
            sheet_by_name: Some("Absent".to_owned()),
            ..price_list()
        },
        stored("prices.xlsx", XLSX, workbook(WorkbookFixture::default())),
    )
    .expect_err("a sheet the file does not carry is refused");
    assert_eq!(failure.code(), "ingest_sheet_unknown");

    // Sheets. The workbook declares more of them than the reader admits, and
    // is refused before a range is read.
    let mut parts = workbook_parts(&WorkbookFixture::default());
    let sheets: String = (1..=300)
        .map(|index| format!(r#"<sheet name="S{index}" sheetId="{index}" r:id="rId1"/>"#))
        .collect();
    for part in &mut parts {
        if part.0 == "xl/workbook.xml" {
            part.1 = format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets>{sheets}</sheets></workbook>"#
            )
            .into_bytes();
        }
    }
    let failure = read(
        "spreadsheet.read",
        IngestSchemaSpec {
            sheet_by_name: Some("S1".to_owned()),
            ..price_list()
        },
        stored("many.xlsx", XLSX, zip_parts(&parts)),
    )
    .expect_err("a workbook with more sheets than the reader admits is refused");
    assert_eq!(failure.code(), "ingest_sheet_count_exceeded");

    // Rows.
    let failure = read(
        "spreadsheet.read",
        IngestSchemaSpec {
            max_rows: Some(1),
            ..price_list()
        },
        stored(
            "prices.xlsx",
            XLSX,
            workbook(WorkbookFixture {
                rows: &[
                    &["A-1", "12.50", "2026-01-31"],
                    &["A-2", "13.50", "2026-01-31"],
                ],
                ..WorkbookFixture::default()
            }),
        ),
    )
    .expect_err("one row over the ceiling is refused");
    assert_eq!(failure.code(), "ingest_rows_exceeded");

    // Columns.
    let failure = read(
        "spreadsheet.read",
        IngestSchemaSpec {
            max_columns: Some(2),
            ..price_list()
        },
        stored("prices.xlsx", XLSX, workbook(WorkbookFixture::default())),
    )
    .expect_err("a header wider than the ceiling is refused");
    assert_eq!(failure.code(), "ingest_columns_exceeded");

    // One cell, over its byte length. It is a row rejection rather than a file
    // refusal: one long cell is a bad row, not a hostile file.
    let long = "x".repeat(80);
    let value = read(
        "spreadsheet.read",
        price_list(),
        stored(
            "prices.xlsx",
            XLSX,
            workbook(WorkbookFixture {
                rows: &[&[&long, "12.50", "2026-01-31"]],
                ..WorkbookFixture::default()
            }),
        ),
    )
    .expect("a long cell is a rejected row");
    assert_eq!(value["rejected"][0]["reason"], json!("cell_too_large"));
}

/// Bound 4, as a statement about memory rather than about a code: a sheet
/// holding two far-apart cells is refused *before* anything the size of its
/// bounding box exists.
///
/// This is the spreadsheet twin of spec 022's
/// `image_dimensions_are_checked_before_allocation`, and the same class of
/// attack. `calamine`'s `Range::from_sparse` allocates one slot per cell of the
/// bounding box of the populated cells, so a few hundred bytes of XML holding
/// only `A1` and `XFD1048576` asks for 16384 × 1048576 slots — about half a
/// terabyte, an allocation failure, and an aborted process. The archive guards
/// cannot see it: uncompressed, the sheet is under a kilobyte.
///
/// Three fixtures, because the extent has to be refused from *both* the sheet's
/// own record and the cells that actually arrive, and because the formulas are
/// a second range with the same shape.
#[test]
fn ingest_sheet_extent_is_bounded_before_a_range_is_materialized() {
    const CEILING: usize = 8 * 1_024 * 1_024;

    // 1. The sheet's own `dimension` record says how wide it is. Believing it
    //    is free, and it refuses the bomb before a cell is parsed.
    let declared = far_cell_sheet(true, false);
    assert!(
        declared.len() < 1_024,
        "the bomb is a few hundred bytes of XML: {} bytes",
        declared.len()
    );
    let bytes = workbook_with_sheet(&declared);
    let (failure, peak) = peak_bytes(|| {
        read(
            "spreadsheet.read",
            price_list(),
            stored("bomb.xlsx", XLSX, bytes),
        )
        .expect_err("a sheet declaring more columns than the schema admits is refused")
    });
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "ingest_columns_exceeded");
    assert!(
        peak < CEILING,
        "the declared extent must be read before a range exists: {peak} bytes peaked"
    );

    // 2. The record is written by whoever wrote the file, so it may be absent.
    //    Then the cells themselves are the bound, and the refusal still lands
    //    before the bounding box is ever allocated.
    let undeclared = far_cell_sheet(false, false);
    let bytes = workbook_with_sheet(&undeclared);
    let (failure, peak) = peak_bytes(|| {
        read(
            "spreadsheet.read",
            price_list(),
            stored("bomb.xlsx", XLSX, bytes),
        )
        .expect_err("a cell outside the schema's columns is refused as it is read")
    });
    assert_eq!(failure.code(), "ingest_columns_exceeded");
    assert!(
        peak < CEILING,
        "no bounding box may be materialized for a sheet that is refused: {peak} bytes peaked"
    );

    // 3. The formulas are a second range over the same sheet, built the same
    //    way. A far cell carrying only a formula is invisible to the value
    //    pass and would otherwise blow up the formula pass on its own.
    let formula_only = far_cell_sheet(false, true);
    let bytes = workbook_with_sheet(&formula_only);
    let (failure, peak) = peak_bytes(|| {
        read(
            "spreadsheet.read",
            price_list(),
            stored("bomb.xlsx", XLSX, bytes),
        )
        .expect_err("a far formula cell is refused like a far value cell")
    });
    assert_eq!(failure.code(), "ingest_columns_exceeded");
    assert!(
        peak < CEILING,
        "the formula pass is bounded by the same extent: {peak} bytes peaked"
    );

    // And the other direction: a sheet whose declared record is honest and
    // inside the ceilings still reads.
    let honest = read(
        "spreadsheet.read",
        price_list(),
        stored(
            "prices.xlsx",
            XLSX,
            workbook_with_sheet(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:C2"/><sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>SKU</t></is></c><c r="B1" t="inlineStr"><is><t>Price</t></is></c><c r="C1" t="inlineStr"><is><t>Valid from</t></is></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>A-1</t></is></c><c r="B2" t="inlineStr"><is><t>12.50</t></is></c><c r="C2" t="inlineStr"><is><t>2026-01-31</t></is></c></row>
</sheetData></worksheet>"#,
            ),
        ),
    )
    .expect("a sheet that declares its own honest extent reads");
    assert_eq!(honest["row_count"], json!(1));
    assert_eq!(honest["rows"][0]["sku"], json!("A-1"));
}

/// A date-styled cell whose serial is not a date this reader can name is a
/// rejected row, not a panic.
///
/// `1e400` parses to `f64::INFINITY`; `calamine`'s serial conversion saturates
/// it through `.floor() as u64` and then adds the epoch offset, which overflows
/// — a panic in debug and a truncated, wrapped year in release. The clamp
/// `schema::from_serial` already applies to a *bare* number has to apply to a
/// cell the file itself typed as a date as well.
#[test]
fn ingest_refuses_a_serial_date_outside_the_representable_range() {
    let value = read(
        "spreadsheet.read",
        price_list(),
        stored(
            "prices.xlsx",
            XLSX,
            workbook(WorkbookFixture {
                rows: &[],
                raw_rows: &[
                    r#"<row r="2"><c r="A2" t="inlineStr"><is><t>A-1</t></is></c><c r="B2" t="inlineStr"><is><t>12.50</t></is></c><c r="C2" s="1"><v>1e400</v></c></row>"#,
                    r#"<row r="3"><c r="A3" t="inlineStr"><is><t>A-2</t></is></c><c r="B3" t="inlineStr"><is><t>12.50</t></is></c><c r="C3" s="1"><v>1e18</v></c></row>"#,
                    r#"<row r="4"><c r="A4" t="inlineStr"><is><t>A-3</t></is></c><c r="B4" t="inlineStr"><is><t>12.50</t></is></c><c r="C4" s="1"><v>-1</v></c></row>"#,
                    // The boundary itself: the last serial the reader admits.
                    r#"<row r="5"><c r="A5" t="inlineStr"><is><t>A-4</t></is></c><c r="B5" t="inlineStr"><is><t>12.50</t></is></c><c r="C5" s="1"><v>2958465</v></c></row>"#,
                ],
                ..WorkbookFixture::default()
            }),
        ),
    )
    .expect("an unrepresentable date is a rejected row, not a failed file");

    assert_eq!(value["rejected_count"], json!(3));
    for index in 0..3 {
        assert_eq!(
            value["rejected"][index]["reason"],
            json!("cell_not_a_date"),
            "rejection {index} names the date, not a panic"
        );
    }
    assert_eq!(value["row_count"], json!(1));
    assert_eq!(value["rows"][0]["valid_from"], json!("9999-12-31"));
}

/// Bound 5: working memory and the cpu deadline, both reached through the
/// declared bounds rather than by anything the file can widen.
#[test]
fn ingest_working_memory_and_the_deadline_are_bounded() {
    let capability = capability("local.ingest").expect("local.ingest is compiled");
    for operation in capability.operations() {
        let bounds = operation.bounds();
        assert!(!bounds.cpu_deadline().is_zero());
        assert!(bounds.max_intermediate_bytes() > 0);
        assert!(bounds.max_units() > 0);
        assert_eq!(bounds.unit(), "cells");
    }

    // A file whose expansion is inside the archive ceiling but outside the
    // operation's working-memory charge is refused as working memory.
    let failure = read(
        "spreadsheet.read",
        IngestSchemaSpec {
            max_uncompressed_bytes: Some(512 * 1_024),
            max_working_bytes: Some(1_024),
            ..price_list()
        },
        stored(
            "prices.xlsx",
            XLSX,
            zip_of(&[("xl/worksheets/sheet1.xml", incompressible(64 * 1_024))]),
        ),
    )
    .expect_err("working memory is charged as the archive streams");
    assert_eq!(failure.code(), "local_intermediate_too_large");
}

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

/// Bytes that deflate to about their own size, so a fixture can be large
/// without being a bomb. A tiny linear congruential generator, written out so
/// the fixture is reproducible and owes nothing to a crate.
fn incompressible(bytes: usize) -> Vec<u8> {
    let mut state: u64 = 0x2026_0810;
    (0..bytes)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

const ENTRY_NAMES: &[&str] = &[
    "xl/worksheets/sheet1.xml",
    "xl/worksheets/sheet2.xml",
    "xl/worksheets/sheet3.xml",
    "xl/worksheets/sheet4.xml",
    "xl/worksheets/sheet5.xml",
    "xl/worksheets/sheet6.xml",
    "xl/worksheets/sheet7.xml",
    "xl/worksheets/sheet8.xml",
];

/// One hand-written workbook.
#[derive(Clone, Copy)]
struct WorkbookFixture<'a> {
    sheet: &'a str,
    header: &'a [&'a str],
    /// Rows of plain text cells, written as inline strings.
    rows: &'a [&'a [&'a str]],
    /// Rows written as raw `<c>` XML, for the cases a text cell cannot express.
    raw_rows: &'a [&'a str],
    date1904: bool,
    extra: &'a [(&'a str, &'a str)],
}

impl Default for WorkbookFixture<'_> {
    fn default() -> Self {
        Self {
            sheet: "Prices",
            header: &["SKU", "Price", "Valid from"],
            rows: &[&["A-1", "12.50", "2026-01-31"]],
            raw_rows: &[],
            date1904: false,
            extra: &[],
        }
    }
}

/// A formula cell whose cached value disagrees with its own formula: `=1+1`
/// cached as `5`. A reader that evaluated would say `2`.
const FORMULA_FIXTURE: WorkbookFixture<'static> = WorkbookFixture {
    sheet: "Prices",
    header: &["SKU", "Price", "Valid from"],
    rows: &[],
    raw_rows: &[
        r#"<row r="2"><c r="A2" t="inlineStr"><is><t>A-1</t></is></c><c r="B2"><f>1+1</f><v>5</v></c></row>"#,
        r#"<row r="3"><c r="A3" t="inlineStr"><is><t>A-2</t></is></c><c r="B3"><f>1+1</f></c></row>"#,
    ],
    date1904: false,
    extra: &[],
};

/// Serial dates with and without a date style, and two strings that look like
/// numbers, and one number in a string column.
const SERIAL_FIXTURE: WorkbookFixture<'static> = WorkbookFixture {
    sheet: "Prices",
    header: &["SKU", "Price", "Valid from"],
    rows: &[],
    raw_rows: &[
        // A styled serial (numFmtId 14) and a text SKU with leading zeros.
        r#"<row r="2"><c r="A2" t="inlineStr"><is><t>00123</t></is></c><c r="B2" t="inlineStr"><is><t>12.50</t></is></c><c r="C2" s="1"><v>39448</v></c></row>"#,
        // A bare serial with no date style at all, and a string that looks like
        // an exponent.
        r#"<row r="3"><c r="A3" t="inlineStr"><is><t>1.20E+02</t></is></c><c r="B3" t="inlineStr"><is><t>12.50</t></is></c><c r="C3"><v>39448</v></c></row>"#,
        // A number where a string was declared.
        r#"<row r="4"><c r="A4"><v>123</v></c><c r="B4" t="inlineStr"><is><t>12.50</t></is></c></row>"#,
    ],
    date1904: false,
    extra: &[],
};

/// The complete part list of one fixture workbook.
fn workbook_parts(fixture: &WorkbookFixture<'_>) -> Vec<(String, Vec<u8>)> {
    let mut sheet = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
    );
    if !fixture.header.is_empty() {
        sheet.push_str(r#"<row r="1">"#);
        for (index, header) in fixture.header.iter().enumerate() {
            sheet.push_str(&format!(
                r#"<c r="{}1" t="inlineStr"><is><t>{header}</t></is></c>"#,
                column_letter(index)
            ));
        }
        sheet.push_str("</row>");
    }
    for (index, row) in fixture.rows.iter().enumerate() {
        let number = index + 2;
        sheet.push_str(&format!(r#"<row r="{number}">"#));
        for (cell, text) in row.iter().enumerate() {
            sheet.push_str(&format!(
                r#"<c r="{}{number}" t="inlineStr"><is><t>{}</t></is></c>"#,
                column_letter(cell),
                text.replace('&', "&amp;").replace('<', "&lt;")
            ));
        }
        sheet.push_str("</row>");
    }
    for row in fixture.raw_rows {
        sheet.push_str(row);
    }
    sheet.push_str("</sheetData></worksheet>");

    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<workbookPr date1904="{}"/>
<sheets><sheet name="{}" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#,
        u8::from(fixture.date1904),
        fixture.sheet
    );

    let mut parts = vec![
        (
            "[Content_Types].xml".to_owned(),
            CONTENT_TYPES.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_owned(), ROOT_RELS.as_bytes().to_vec()),
        ("xl/workbook.xml".to_owned(), workbook.into_bytes()),
        (
            "xl/_rels/workbook.xml.rels".to_owned(),
            WORKBOOK_RELS.as_bytes().to_vec(),
        ),
        ("xl/styles.xml".to_owned(), STYLES.as_bytes().to_vec()),
        ("xl/worksheets/sheet1.xml".to_owned(), sheet.into_bytes()),
    ];
    for (name, body) in fixture.extra {
        parts.push(((*name).to_owned(), body.as_bytes().to_vec()));
    }
    parts
}

fn workbook(fixture: WorkbookFixture<'_>) -> Vec<u8> {
    zip_parts(&workbook_parts(&fixture))
}

/// The default fixture workbook with its one sheet replaced by exactly this
/// XML. The workbook part still names the sheet `Prices`, so `price_list()`
/// selects it.
fn workbook_with_sheet(sheet: &str) -> Vec<u8> {
    let mut parts = workbook_parts(&WorkbookFixture::default());
    for part in &mut parts {
        if part.0 == "xl/worksheets/sheet1.xml" {
            part.1 = sheet.as_bytes().to_vec();
        }
    }
    zip_parts(&parts)
}

/// A sheet holding the declared header and one cell in the last cell Excel has:
/// `XFD1048576`, column 16384 of row 1,048,576.
///
/// The bounding box of those two cells is the whole grid. `declare` writes the
/// `dimension` record that says so; `formula_only` puts a formula with no
/// cached value there instead of a value, which is a cell the value pass never
/// sees and the formula pass does.
fn far_cell_sheet(declare: bool, formula_only: bool) -> String {
    let dimension = if declare {
        r#"<dimension ref="A1:XFD1048576"/>"#
    } else {
        ""
    };
    let far = if formula_only {
        r#"<c r="XFD1048576"><f>1+1</f></c>"#
    } else {
        r#"<c r="XFD1048576" t="inlineStr"><is><t>x</t></is></c>"#
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">{dimension}<sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>SKU</t></is></c><c r="B1" t="inlineStr"><is><t>Price</t></is></c><c r="C1" t="inlineStr"><is><t>Valid from</t></is></c></row>
<row r="1048576">{far}</row>
</sheetData></worksheet>"#
    )
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

/// Two cell formats: the default, and `numFmtId="14"` — the built-in short date.
const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<fonts count="1"><font/></fonts>
<fills count="1"><fill/></fills>
<borders count="1"><border/></borders>
<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
<cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/></cellXfs>
</styleSheet>"#;

fn column_letter(index: usize) -> char {
    char::from(b'A' + index as u8)
}

/// A deflated archive of the given parts.
fn zip_parts(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in parts {
        writer.start_file(name, options).expect("a part starts");
        writer.write_all(body).expect("a part is written");
    }
    writer
        .finish()
        .expect("the archive is finished")
        .into_inner()
}

fn zip_of(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    zip_parts(
        &parts
            .iter()
            .map(|(name, body)| ((*name).to_owned(), body.clone()))
            .collect::<Vec<_>>(),
    )
}

/// Patch an archive so every occurrence of `declared` as an uncompressed size —
/// in the local header and in the central directory — reads `lie` instead.
///
/// This is the fixture the streaming bound exists for. No writer will produce
/// it, because no writer lies about what it wrote.
fn understate_sizes(archive: &[u8], declared: u64, lie: u32) -> Vec<u8> {
    let mut patched = archive.to_vec();
    let truth = (declared as u32).to_le_bytes();
    let replacement = lie.to_le_bytes();
    let mut index = 0;
    while index + 4 <= patched.len() {
        let signature = &patched[index..index + 4];
        let offset = match signature {
            [0x50, 0x4b, 0x03, 0x04] => Some(22), // local header: uncompressed size
            [0x50, 0x4b, 0x01, 0x02] => Some(24), // central directory: the same
            _ => None,
        };
        if let Some(offset) = offset
            && index + offset + 4 <= patched.len()
            && patched[index + offset..index + offset + 4] == truth
        {
            patched[index + offset..index + offset + 4].copy_from_slice(&replacement);
        }
        index += 1;
    }
    patched
}
