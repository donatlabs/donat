//! `spreadsheet.render` — a declared sheet layout and one typed row list into
//! an `.xlsx` artifact.
//!
//! The point over a CSV export is that numbers, dates, and money keep their
//! type (spec 019 §5): a total is a number with a currency format, not a string
//! that looks like one, and a date sorts as a date.
//!
//! **A cell is never a formula.** There is no formula field in the layout, and a
//! value that begins `=`, `+`, `-`, or `@` is written as a text cell with a
//! defensive `'` prefix. This is not a nicety: a spreadsheet application opening
//! an export treats a leading `=` as code, so an export that carries one is a
//! code-execution vector in the *recipient's* application — pointed at whoever
//! the report was for, using data whoever filled the row supplied. It has its
//! own test.
//!
//! Determinism needs one more thing than the other renderers: an `.xlsx` is a
//! ZIP whose `docProps/core.xml` carries a creation time, and the writer's
//! default is the wall clock. It is taken from declared input here, exactly as
//! the PDF's timestamp is.

use std::time::Duration;

use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
use serde_json::{Value as JsonValue, json};

use super::{
    DocumentKind, DocumentTemplate, contract, refuse, required_text, select_template, text,
};
use crate::local::bounds::LocalBounds;
use crate::local::capability::{LocalArtifact, LocalInvocation, LocalOperation, LocalProduct};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::ConnectorFailure;

/// The template the registration probe renders. Compiled in, not declared.
pub const PROBE_TEMPLATE: &str = "donat.probe.spreadsheet";

pub const PROBE_SOURCE: &str = r#"{"sheet":"Probe",
"columns":[{"header":"Subject","field":"value","type":"text"}]}"#;

/// The media type of an Office Open XML workbook.
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// The characters a spreadsheet application reads as the start of a formula.
///
/// `\t`, `\r`, and `\n` are here with the four the spec names because a leading
/// control character is stripped on paste and can leave the payload leading the
/// cell again.
const FORMULA_LEADS: &[char] = &['=', '+', '-', '@', '\t', '\r', '\n'];

fn bounds() -> LocalBounds {
    LocalBounds::declare(
        Duration::from_secs(20),
        4 * 1_024 * 1_024,
        16 * 1_024 * 1_024,
        64 * 1_024 * 1_024,
        "cells",
        250_000,
    )
    .expect("the spreadsheet bounds are static and complete")
}

pub fn operation() -> LocalOperation {
    LocalOperation::declare("spreadsheet.render", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "template": PROBE_TEMPLATE,
                    "rows": [{ "value": "donat" }],
                    "document_timestamp": "2026-01-01T00:00:00Z",
                    "attachment": "public.document.file",
                    "claim_role": "app",
                    "file_name": "probe.xlsx"
                }),
                "the output is the declared layout filled with the declared rows, with the \
                 workbook's creation time taken from declared input; no clock, no random \
                 seed, no environment, no locale",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(bounds())
        // Cells are charged exactly, once the row list has been read and
        // before a single one is written.
        .units(|_| 0)
        .run(run)
        .build()
        .expect("spreadsheet.render is deterministic")
}

/// The one input key a spreadsheet's row list is bound to.
///
/// It is fixed rather than named by the layout so that the row list is a
/// declared template input like any other — the metadata typecheck of spec 019
/// §7 then covers it without a second rule.
const ROWS: &str = "rows";

// ---------------------------------------------------------------------------
// The layout
// ---------------------------------------------------------------------------

/// One declared column.
struct Column {
    header: String,
    field: String,
    kind: ColumnKind,
    format: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColumnKind {
    Text,
    Integer,
    Decimal,
    Date,
    DateTime,
    Boolean,
}

impl ColumnKind {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "text" => Self::Text,
            "integer" => Self::Integer,
            "decimal" => Self::Decimal,
            "date" => Self::Date,
            "datetime" => Self::DateTime,
            "boolean" => Self::Boolean,
            _ => return None,
        })
    }

    /// The number format a column gets when the layout declares none.
    const fn default_format(self) -> Option<&'static str> {
        match self {
            Self::Decimal => Some("#,##0.00"),
            Self::Date => Some("yyyy-mm-dd"),
            Self::DateTime => Some("yyyy-mm-dd hh:mm:ss"),
            Self::Text | Self::Integer | Self::Boolean => None,
        }
    }
}

struct Layout {
    sheet: String,
    columns: Vec<Column>,
}

fn layout(template: &DocumentTemplate) -> Result<Layout, ConnectorFailure> {
    let defect = || {
        refuse(
            "local_template_defect",
            "the selected spreadsheet template's layout is not a declared sheet",
        )
    };
    let source = template.file(template.entry()).ok_or_else(defect)?;
    let parsed: JsonValue = serde_json::from_str(source).map_err(|_| defect())?;
    let sheet = parsed
        .get("sheet")
        .and_then(JsonValue::as_str)
        .ok_or_else(defect)?
        .to_owned();
    let declared = parsed
        .get("columns")
        .and_then(JsonValue::as_array)
        .ok_or_else(defect)?;
    if declared.is_empty() {
        return Err(defect());
    }
    let mut columns = Vec::with_capacity(declared.len());
    for column in declared {
        // A `formula` key is refused rather than ignored: a layout that
        // declares one is asking for exactly what this operation does not do.
        if column.get("formula").is_some() {
            return Err(refuse(
                "local_template_formula_declared",
                "a spreadsheet template declares no formula column",
            ));
        }
        let kind = column
            .get("type")
            .and_then(JsonValue::as_str)
            .and_then(ColumnKind::parse)
            .ok_or_else(defect)?;
        columns.push(Column {
            header: column
                .get("header")
                .and_then(JsonValue::as_str)
                .ok_or_else(defect)?
                .to_owned(),
            field: column
                .get("field")
                .and_then(JsonValue::as_str)
                .ok_or_else(defect)?
                .to_owned(),
            kind,
            format: column
                .get("format")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
                .or_else(|| kind.default_format().map(str::to_owned)),
        });
    }
    Ok(Layout { sheet, columns })
}

// ---------------------------------------------------------------------------
// The render
// ---------------------------------------------------------------------------

fn run(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let input = invocation.input();
    let template = select_template(invocation, DocumentKind::Spreadsheet)?;
    let attachment = required_text(input, "attachment")?;
    let claim_role = required_text(input, "claim_role")?;
    let file_name = required_text(input, "file_name")?;
    let created = excel_timestamp(required_text(input, "document_timestamp")?)?;
    let layout = layout(template)?;

    let JsonValue::Array(rows) = input.get(ROWS).unwrap_or(&JsonValue::Null) else {
        return Err(refuse(
            "local_template_input_missing",
            "a spreadsheet template's declared row list is not bound to a list",
        ));
    };
    invocation.charge_units((rows.len() * layout.columns.len()) as u64)?;
    invocation.reserve(rows.len() * layout.columns.len() * 32)?;

    let mut workbook = Workbook::new();
    // The one thing that would otherwise make two renders of one input differ.
    workbook.set_properties(
        &rust_xlsxwriter::DocProperties::new()
            .set_creation_datetime(&created)
            .set_author("donat"),
    );
    let header_format = Format::new().set_bold();
    let formats: Vec<Option<Format>> = layout
        .columns
        .iter()
        .map(|column| {
            column
                .format
                .as_ref()
                .map(|format| Format::new().set_num_format(format.clone()))
        })
        .collect();

    let sheet = workbook.add_worksheet();
    sheet.set_name(&layout.sheet).map_err(|_| {
        refuse(
            "local_template_defect",
            "a spreadsheet template's sheet name is not one Excel accepts",
        )
    })?;
    for (index, column) in layout.columns.iter().enumerate() {
        sheet
            .write_string_with_format(0, index as u16, &column.header, &header_format)
            .map_err(|_| write_failed())?;
    }

    for (row_index, row) in rows.iter().enumerate() {
        invocation.checkpoint()?;
        let row_number = row_index as u32 + 1;
        for (index, column) in layout.columns.iter().enumerate() {
            let value = row.get(&column.field).unwrap_or(&JsonValue::Null);
            write_cell(
                sheet,
                row_number,
                index as u16,
                column,
                formats[index].as_ref(),
                value,
            )?;
        }
    }

    let bytes = workbook.save_to_buffer().map_err(|_| write_failed())?;
    if let Some(declared) = template.max_output_bytes()
        && bytes.len() as u64 > declared
    {
        return Err(refuse(
            "local_output_too_large",
            "local capability output exceeds the operation's declared output ceiling",
        ));
    }

    Ok(LocalProduct::Artifact {
        artifact: LocalArtifact::new(attachment, claim_role, file_name, XLSX, bytes)?
            .claimed_by_session(text(input, "claim_session_key"))?,
        metadata: json!({
            "rows": rows.len(),
            "columns": layout.columns.len(),
            "cells": rows.len() * layout.columns.len(),
            "template": template.name(),
            "template_hash": template.content_hash(),
        }),
    })
}

fn write_cell(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    column_index: u16,
    column: &Column,
    format: Option<&Format>,
    value: &JsonValue,
) -> Result<(), ConnectorFailure> {
    if value.is_null() {
        return Ok(());
    }
    match column.kind {
        ColumnKind::Text => {
            let text = match value {
                JsonValue::String(text) => text.clone(),
                JsonValue::Number(number) => number.to_string(),
                JsonValue::Bool(flag) => flag.to_string(),
                _ => return Err(kind_mismatch()),
            };
            write_text(sheet, row, column_index, format, &text)
        }
        ColumnKind::Integer => {
            let number = value
                .as_i64()
                .or_else(|| value.as_str()?.parse::<i64>().ok())
                .ok_or_else(kind_mismatch)?;
            write_number(sheet, row, column_index, format, number as f64)
        }
        ColumnKind::Decimal => {
            // A decimal arrives as a string, because that is the only JSON
            // spelling that has not already been through a float. It is
            // refused rather than rounded when a spreadsheet cell — which is
            // an IEEE double — cannot hold it exactly.
            let text = value.as_str().ok_or_else(kind_mismatch)?;
            let number = text.parse::<f64>().map_err(|_| kind_mismatch())?;
            if !decimal_is_exact(text, number) {
                return Err(refuse(
                    "local_decimal_not_representable",
                    "a decimal that a spreadsheet cell cannot hold exactly is refused rather \
                     than silently rounded",
                ));
            }
            write_number(sheet, row, column_index, format, number)
        }
        ColumnKind::Date | ColumnKind::DateTime => {
            let text = value.as_str().ok_or_else(kind_mismatch)?;
            let datetime = excel_timestamp(text).or_else(|_| excel_date(text))?;
            let format = format.ok_or_else(write_failed)?;
            sheet
                .write_datetime_with_format(row, column_index, &datetime, format)
                .map(|_| ())
                .map_err(|_| write_failed())
        }
        ColumnKind::Boolean => {
            let flag = value.as_bool().ok_or_else(kind_mismatch)?;
            match format {
                Some(format) => sheet.write_boolean_with_format(row, column_index, flag, format),
                None => sheet.write_boolean(row, column_index, flag),
            }
            .map(|_| ())
            .map_err(|_| write_failed())
        }
    }
}

/// Write a text cell, defusing anything a spreadsheet application would treat
/// as the start of a formula.
///
/// Two things happen and both are needed. The cell is a *string* cell, which is
/// never evaluated; and the value is prefixed with `'`, which is what survives
/// a copy-paste or a save-as-CSV out of the recipient's application, where the
/// cell type does not.
fn write_text(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    column: u16,
    format: Option<&Format>,
    text: &str,
) -> Result<(), ConnectorFailure> {
    let defused = if text.starts_with(FORMULA_LEADS) {
        format!("'{text}")
    } else {
        text.to_owned()
    };
    match format {
        Some(format) => sheet.write_string_with_format(row, column, &defused, format),
        None => sheet.write_string(row, column, &defused),
    }
    .map(|_| ())
    .map_err(|_| write_failed())
}

fn write_number(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    column: u16,
    format: Option<&Format>,
    number: f64,
) -> Result<(), ConnectorFailure> {
    match format {
        Some(format) => sheet.write_number_with_format(row, column, number, format),
        None => sheet.write_number(row, column, number),
    }
    .map(|_| ())
    .map_err(|_| write_failed())
}

/// Whether a decimal string survives the trip through an IEEE double.
///
/// The comparison is made at the input's own scale: `12.50` and `12.5` are the
/// same decimal, while `123456789012345678.99` is not the number a double
/// would store.
fn decimal_is_exact(text: &str, number: f64) -> bool {
    let scale = text
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let rendered = format!("{number:.scale$}");
    let normalize = |value: &str| {
        let value = value.trim_start_matches('+');
        let (sign, digits) = match value.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", value),
        };
        format!("{sign}{}", digits.trim_start_matches('0'))
    };
    normalize(&rendered) == normalize(text)
}

fn excel_timestamp(source: &str) -> Result<ExcelDateTime, ConnectorFailure> {
    let invalid = || contract("a timestamp is spelled `YYYY-MM-DDTHH:MM:SSZ`");
    let (date, rest) = source.split_once('T').ok_or_else(invalid)?;
    let time = rest.strip_suffix('Z').unwrap_or(rest);
    let mut clock = time.split(':');
    let hour = clock
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(invalid)?;
    let minute = clock
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(invalid)?;
    let second: f64 = clock
        .next()
        .and_then(|part| part.parse().ok())
        .unwrap_or(0.0);
    excel_date(date)?
        .and_hms(hour, minute, second)
        .map_err(|_| invalid())
}

fn excel_date(source: &str) -> Result<ExcelDateTime, ConnectorFailure> {
    let invalid = || contract("a date is spelled `YYYY-MM-DD`");
    let mut parts = source.split('-');
    let year = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(invalid)?;
    let month = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(invalid)?;
    let day = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(invalid)?;
    if parts.next().is_some() {
        return Err(invalid());
    }
    ExcelDateTime::from_ymd(year, month, day).map_err(|_| invalid())
}

fn kind_mismatch() -> ConnectorFailure {
    refuse(
        "local_template_input_kind",
        "a spreadsheet cell's value does not match the type its column declares",
    )
}

fn write_failed() -> ConnectorFailure {
    ConnectorFailure::new(
        crate::sdk::errors::ConnectorErrorClass::Invariant,
        "local_template_export_failed",
        "the workbook could not be written",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every lead character a spreadsheet application reads as code is defused,
    /// and an ordinary value is left exactly as it was.
    #[test]
    fn every_formula_lead_is_defused_and_nothing_else_is_touched() {
        for payload in ["=1+1", "+1", "-1", "@SUM(A1)", "\t=1", "\r=1"] {
            assert!(
                payload.starts_with(FORMULA_LEADS),
                "`{payload}` must be treated as a formula lead"
            );
        }
        for ordinary in ["A-1", "12.50", "customer@example.test", " =1"] {
            assert!(
                !ordinary.starts_with(FORMULA_LEADS),
                "`{ordinary}` is not a formula lead"
            );
        }
    }

    /// The decimal gate: what a cell can hold exactly, and what it refuses.
    #[test]
    fn a_decimal_is_written_only_when_a_cell_can_hold_it() {
        for exact in ["12.50", "0.1", "1234567.89", "-42", "1000000"] {
            let number = exact.parse::<f64>().expect("a decimal parses");
            assert!(decimal_is_exact(exact, number), "`{exact}` is exact");
        }
        for lossy in ["123456789012345678.99", "0.12345678901234567890"] {
            let number = lossy.parse::<f64>().expect("a decimal parses");
            assert!(!decimal_is_exact(lossy, number), "`{lossy}` is not exact");
        }
    }
}
