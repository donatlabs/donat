//! `spreadsheet.read` — one declared schema and one uploaded `.xlsx`, into
//! typed rows.
//!
//! The order of what happens here is the deliverable of spec 020 §3, and it
//! reads top to bottom in [`run`]:
//!
//! | # | Bound | Where |
//! |---|---|---|
//! | 1 | the stored file's size, before it is opened | [`begin`](super::begin) |
//! | 2 | archive entry count, declared expansion, compression ratio | [`archive::admit_declared`] |
//! | — | external links, connections, embedded objects, macro parts | [`archive::admit_active_content`] |
//! | 3 | real uncompressed bytes, per entry and in total | [`archive::verify_streamed`] |
//! | 4 | sheet, row, column, and per-cell ceilings | here |
//! | 5 | working memory and the cpu deadline | throughout |
//!
//! Only after 1–3 does a parser see a byte. That is not tidiness: a ratio
//! checked after decompression has already paid for the decompression, which is
//! the entire cost a zip bomb was trying to impose.
//!
//! **A sheet is never materialized as a grid.** `calamine`'s `Range` is dense:
//! it allocates one slot per cell of the bounding box of the populated cells,
//! so a sheet holding `A1` and `XFD1048576` — under a kilobyte of XML, and
//! nothing the archive bounds can see — asks for 16384 × 1048576 slots and
//! aborts the process on the allocation failure. The extent is therefore
//! answered before any range exists, and it is answered twice for the reason
//! ADR 052 gives about the central directory: the sheet's own `dimension`
//! record is believed first, because believing it is free, and then every cell
//! that actually arrives is checked as it is read, because the record was
//! written by whoever wrote the file. [`scan`] holds what it reads sparsely,
//! so the bounding box is a number rather than an allocation.
//!
//! **Formulas are never evaluated.** What a cell yields is the value the
//! *writer* cached in the file. The formula text is read for exactly one
//! purpose — telling "this cell is empty" apart from "this cell is a formula
//! nobody cached a value for" — and the second of those is a typed rejection
//! rather than a computation.
//!
//! **The date system belongs to the workbook.** Serial 39448 is 2008-01-01
//! under the 1900 epoch and 2012-01-02 under the 1904 one. Which epoch applies
//! is read from the workbook, never assumed, and a bare number in a declared
//! date column is converted through it (spec 020 §3's last paragraph).

use std::collections::HashMap;
use std::io::Cursor;
use std::time::Duration;

use calamine::{Data, DataRef, Dimensions, Reader, Xlsx};
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use super::schema::{Cell, DateSystem, IngestKind};
use super::{MAX_COLUMNS, MAX_ROWS, Reading, Rows, archive, begin, charge, refuse};
use crate::local::bounds::LocalBounds;
use crate::local::capability::{LocalInvocation, LocalOperation, LocalProduct};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::ConnectorFailure;

/// The schema and the stored file the registration probe reads. Compiled in.
pub const PROBE_SCHEMA: &str = "donat.probe.ingest.spreadsheet";
pub const PROBE_SOURCE: &str = "donat.probe.source.xlsx";

/// The most sheets a workbook may carry. A schema reads one of them; a
/// thousand of them is a file whose shape is the attack.
pub const MAX_SHEETS: usize = 256;

/// What one held cell is charged as working memory, beside its own text.
///
/// It is the size of the map entry plus its key and a slot's worth of table
/// overhead — deliberately generous, because the charge is what stands between
/// a sheet of millions of tiny cells and the working-memory ceiling.
const CELL_OVERHEAD_BYTES: usize = 64;

/// How many cells are read between two deadline checks.
const CELLS_PER_CHECKPOINT: u64 = 4_096;

fn bounds() -> LocalBounds {
    LocalBounds::declare(
        Duration::from_secs(30),
        // The input is a schema name and a stored file's handle. The bytes are
        // not in it, and this ceiling is what says so.
        4 * 1_024,
        64 * 1_024 * 1_024,
        super::MAX_WORKING_BYTES,
        "cells",
        4_000_000,
    )
    .expect("the spreadsheet ingest bounds are static and complete")
}

pub fn operation() -> LocalOperation {
    LocalOperation::declare("spreadsheet.read", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({ "schema": PROBE_SCHEMA, "source": PROBE_SOURCE }),
                "the output is the declared schema applied to the stored bytes: no clock, no \
                 locale, no environment, and no formula is ever evaluated",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(bounds())
        // Cells are charged exactly, once the sheet's extent is known and
        // before a single one is coerced.
        .units(|_| 0)
        .run(run)
        .build()
        .expect("spreadsheet.read is deterministic")
}

fn run(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    // BOUND 1, and the two selections that precede it.
    let reading = begin(invocation, IngestKind::Spreadsheet)?;
    let bytes = reading.source.bytes();
    let limits = reading.archive_limits();

    // BOUND 2: the central directory, before anything is decompressed.
    let entries = archive::admit_declared(bytes, &limits)?;
    // Active content, from part names, still before anything is decompressed.
    archive::admit_active_content(&entries)?;
    // BOUND 3: the bytes that actually come out, per entry and in total.
    archive::verify_streamed(
        bytes,
        &entries,
        &limits,
        reading.max_working_bytes,
        invocation,
    )?;
    invocation.checkpoint()?;

    // Only now is there a workbook.
    let mut workbook: Xlsx<_> = Xlsx::new(Cursor::new(bytes)).map_err(|_| {
        refuse(
            "ingest_not_a_workbook",
            "the stored file is an archive but not a workbook this reader can open",
        )
    })?;
    let names = workbook.sheet_names();
    if names.len() > MAX_SHEETS {
        return Err(refuse(
            "ingest_sheet_count_exceeded",
            "the stored workbook carries more sheets than this reader admits",
        ));
    }
    let sheet = select_sheet(&reading, &names)?;
    // The workbook's own epoch, read from the workbook and never assumed.
    let dates = if workbook.has_1904_epoch() {
        DateSystem::Nineteen04
    } else {
        DateSystem::Nineteen00
    };

    // BOUND 4, before anything the size of the sheet's bounding box exists.
    let scanned = scan(invocation, &reading, &mut workbook, &sheet)?;

    read_sheet(invocation, &reading, &scanned, dates, &sheet)
}

/// The populated cells of one sheet, held sparsely.
///
/// Sparsely is the whole point. `calamine` hands back a `Range`, which is a
/// dense `Vec` over the bounding box of whatever cells it found; this holds one
/// entry per cell that exists, so a sheet's *extent* costs a comparison rather
/// than an allocation and a file can no longer choose how much memory reading
/// it takes.
struct Sheet {
    values: HashMap<(u32, u32), Data>,
    /// The formula text of the cells that carry one, read to tell an empty cell
    /// apart from a formula nobody cached a value for, and for nothing else.
    formulas: HashMap<(u32, u32), String>,
    /// The bottom-right of the populated *value* cells, which is what the
    /// header extent and the row loop are read from.
    end: Option<(u32, u32)>,
}

impl Sheet {
    fn value(&self, position: (u32, u32)) -> Option<&Data> {
        self.values.get(&position)
    }

    fn formula(&self, position: (u32, u32)) -> Option<&String> {
        self.formulas.get(&position)
    }
}

/// BOUND 4, as the only function that ever sees a cell position.
///
/// It runs twice over one sheet, in the order ADR 052 states for the archive
/// and for the same reason. First the sheet's own `dimension` record, against
/// the absolute ceilings: it costs one comparison, it is answered before a cell
/// is parsed, and it is what refuses a grid-sized bounding box for free.
/// Then every cell that actually arrives, against the schema's own — narrower —
/// ceilings, because the record is written by whoever wrote the file and may be
/// absent or a lie.
fn scan(
    invocation: &LocalInvocation<'_>,
    reading: &Reading<'_>,
    workbook: &mut Xlsx<Cursor<&[u8]>>,
    sheet: &str,
) -> Result<Sheet, ConnectorFailure> {
    let header_row = reading.schema.header_row() - 1;
    let mut values: HashMap<(u32, u32), Data> = HashMap::new();
    let mut formulas: HashMap<(u32, u32), String> = HashMap::new();
    let mut end: Option<(u32, u32)> = None;
    let mut seen = 0_u64;

    {
        let mut reader = workbook
            .worksheet_cells_reader(sheet)
            .map_err(|_| unreadable())?;
        admit_declared_extent(header_row, reader.dimensions())?;
        while let Some(cell) = reader.next_cell().map_err(|_| unreadable())? {
            let value = cell.get_value();
            if matches!(value, DataRef::Empty) {
                continue;
            }
            let (row, column) = cell.get_position();
            admit_extent(reading, header_row, row, column)?;
            charge(
                invocation,
                reading.max_working_bytes,
                CELL_OVERHEAD_BYTES + held_bytes(value),
            )?;
            checkpoint_every(invocation, &mut seen)?;
            end = Some(match end {
                Some((last_row, last_column)) => (last_row.max(row), last_column.max(column)),
                None => (row, column),
            });
            values.insert((row, column), Data::from(value.clone()));
        }
    }

    {
        // The formulas are a second pass over the same XML, bounded by the same
        // extent: a far cell carrying a formula and no cached value is a cell
        // the pass above never sees.
        let mut reader = workbook
            .worksheet_cells_reader(sheet)
            .map_err(|_| unreadable())?;
        admit_declared_extent(header_row, reader.dimensions())?;
        while let Some(cell) = reader.next_formula().map_err(|_| unreadable())? {
            if cell.get_value().is_empty() {
                continue;
            }
            let (row, column) = cell.get_position();
            admit_extent(reading, header_row, row, column)?;
            charge(
                invocation,
                reading.max_working_bytes,
                CELL_OVERHEAD_BYTES + cell.get_value().len(),
            )?;
            checkpoint_every(invocation, &mut seen)?;
            formulas.insert((row, column), cell.get_value().clone());
        }
    }

    Ok(Sheet {
        values,
        formulas,
        end,
    })
}

/// The extent the sheet claims for itself, against the ceilings no schema can
/// widen.
///
/// The absolute ceilings rather than the schema's, deliberately: the record is
/// the *used* range as some writer computed it, which legitimately runs past
/// the rows a schema expects to import, and a cheap early refusal that fires on
/// honest files would be a bug of its own. What it does catch is the shape no
/// honest writer produces — a bounding box the size of the grid — which is the
/// only thing the dense range could not survive.
fn admit_declared_extent(header_row: u32, declared: Dimensions) -> Result<(), ConnectorFailure> {
    let (row, column) = declared.end;
    if u64::from(column) + 1 > MAX_COLUMNS {
        return Err(columns_exceeded());
    }
    if u64::from(row.saturating_sub(header_row)) > MAX_ROWS {
        return Err(rows_exceeded());
    }
    Ok(())
}

/// One cell's position, against the ceilings the schema declares.
fn admit_extent(
    reading: &Reading<'_>,
    header_row: u32,
    row: u32,
    column: u32,
) -> Result<(), ConnectorFailure> {
    if u64::from(column) + 1 > reading.max_columns {
        return Err(columns_exceeded());
    }
    if u64::from(row.saturating_sub(header_row)) > reading.max_rows {
        return Err(rows_exceeded());
    }
    Ok(())
}

/// What holding one cell's value costs beyond the map entry itself.
fn held_bytes(value: &DataRef<'_>) -> usize {
    match value {
        DataRef::String(text) => text.len(),
        DataRef::SharedString(text) => text.len(),
        DataRef::DateTimeIso(text) | DataRef::DurationIso(text) => text.len(),
        _ => 0,
    }
}

fn checkpoint_every(
    invocation: &LocalInvocation<'_>,
    seen: &mut u64,
) -> Result<(), ConnectorFailure> {
    *seen += 1;
    if (*seen).is_multiple_of(CELLS_PER_CHECKPOINT) {
        invocation.checkpoint()?;
    }
    Ok(())
}

fn unreadable() -> ConnectorFailure {
    refuse(
        "ingest_sheet_unreadable",
        "the selected sheet could not be read from the stored workbook",
    )
}

fn columns_exceeded() -> ConnectorFailure {
    refuse(
        "ingest_columns_exceeded",
        "the stored sheet is wider than the schema admits",
    )
}

fn rows_exceeded() -> ConnectorFailure {
    refuse(
        "ingest_rows_exceeded",
        "the stored sheet carries more rows than the schema admits",
    )
}

/// Which sheet the schema names, out of the ones the workbook carries.
fn select_sheet(reading: &Reading<'_>, names: &[String]) -> Result<String, ConnectorFailure> {
    let unknown = || {
        refuse(
            "ingest_sheet_unknown",
            "the sheet the schema selects is not in the stored workbook",
        )
    };
    if let Some(name) = reading.schema.sheet_by_name() {
        return names
            .iter()
            .find(|candidate| candidate.as_str() == name)
            .cloned()
            .ok_or_else(unknown);
    }
    if let Some(index) = reading.schema.sheet_by_index() {
        return names.get(index).cloned().ok_or_else(unknown);
    }
    names.first().cloned().ok_or_else(unknown)
}

/// The coercion the whole capability exists for.
///
/// Every position it reads was admitted by [`scan`], so the extent arithmetic
/// here is a unit count rather than a bound.
fn read_sheet(
    invocation: &LocalInvocation<'_>,
    reading: &Reading<'_>,
    sheet_cells: &Sheet,
    dates: DateSystem,
    sheet: &str,
) -> Result<LocalProduct, ConnectorFailure> {
    let Some((last_row, last_column)) = sheet_cells.end else {
        return Err(refuse(
            "ingest_header_column_missing",
            "the stored sheet is empty, so the columns the schema declares are not in it",
        ));
    };
    let header_row = reading.schema.header_row() - 1;

    let width = u64::from(last_column) + 1;
    let rows = u64::from(last_row.saturating_sub(header_row));
    invocation.charge_units(rows.saturating_mul(width))?;

    let header: Vec<String> = (0..=last_column)
        .map(|column| match sheet_cells.value((header_row, column)) {
            Some(Data::String(text)) => text.clone(),
            Some(Data::Empty) | None => String::new(),
            // A header cell that is not text is not the header the schema
            // names, and is left as the empty string rather than rendered.
            Some(_) => String::new(),
        })
        .collect();
    // The whole file fails here, before a row is read, if a declared column is
    // not in the header.
    let bound = reading.schema.bind_header(&header)?;

    let mut answer = Rows::new(reading);
    for row in (header_row + 1)..=last_row {
        invocation.checkpoint()?;
        let mut values = JsonMap::new();
        let mut rejected = false;
        for (column, index) in reading.schema.columns().iter().zip(&bound) {
            let position = (row, *index as u32);
            let cell = cell(sheet_cells.value(position), sheet_cells.formula(position));
            match super::schema::coerce(column, &cell, reading.max_cell_bytes, dates) {
                Ok(value) => {
                    values.insert(column.field.clone(), value);
                }
                Err(reason) => {
                    // The file's own 1-based row number, so an operator can
                    // open the file and look at the row.
                    answer.reject(u64::from(row) + 1, column, reason)?;
                    rejected = true;
                    break;
                }
            }
        }
        if !rejected {
            // A row of nothing but empty cells is the trailing whitespace every
            // spreadsheet carries, not a row.
            if values.values().all(JsonValue::is_null) {
                continue;
            }
            answer.accept(values);
        }
    }
    let product = answer.finish(reading, Some(sheet));
    charge(
        invocation,
        reading.max_working_bytes,
        crate::local::canonical_bytes(&product).len(),
    )?;
    Ok(LocalProduct::Value(product))
}

/// One cell, as this reader is willing to describe it.
///
/// The only place a formula is consulted: a cell with no cached value and a
/// formula behind it is a formula nobody evaluated, which is a rejection. It is
/// never a value, and nothing here computes one.
///
/// A cell the file itself typed as a date is only turned into one when its
/// serial is inside the range a date can be named in. `calamine`'s conversion
/// has no guard of its own — it saturates through `.floor() as u64` and then
/// adds an epoch offset, so `<v>1e400</v>` (which parses to `f64::INFINITY`)
/// overflows: a panic in debug and a wrapped, truncated year in release. The
/// same clamp [`super::schema::from_serial`] applies to a bare number applies
/// here, and a serial outside it is a typed rejection rather than a date
/// nobody can explain.
fn cell(value: Option<&Data>, formula: Option<&String>) -> Cell {
    let has_formula = formula.is_some_and(|formula| !formula.is_empty());
    match value {
        None | Some(Data::Empty) => {
            if has_formula {
                Cell::FormulaWithoutValue
            } else {
                Cell::Empty
            }
        }
        Some(Data::String(text)) => Cell::Text(text.clone()),
        Some(Data::DateTimeIso(text)) | Some(Data::DurationIso(text)) => Cell::Text(text.clone()),
        Some(Data::Int(number)) => Cell::Number(*number as f64),
        Some(Data::Float(number)) => Cell::Number(*number),
        Some(Data::Bool(flag)) => Cell::Boolean(*flag),
        Some(Data::DateTime(stamp))
            if stamp.is_datetime() && !super::schema::is_representable_serial(stamp.as_f64()) =>
        {
            Cell::UnrepresentableDate
        }
        Some(Data::DateTime(stamp)) if stamp.is_datetime() => {
            let (year, month, day, hour, minute, second, _milli) = stamp.to_ymd_hms_milli();
            Cell::Instant(
                i32::from(year),
                u32::from(month),
                u32::from(day),
                u32::from(hour),
                u32::from(minute),
                u32::from(second),
            )
        }
        Some(Data::DateTime(duration)) => Cell::Number(duration.as_f64()),
        Some(Data::Error(_)) => Cell::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classification of a formula cell, which is the whole of "formulas
    /// are never evaluated" at the unit it is decided in.
    #[test]
    fn a_formula_is_a_cached_value_or_a_rejection() {
        // A cached value is what comes back — the formula beside it changes
        // nothing, because nothing here reads it as an expression.
        assert_eq!(
            cell(Some(&Data::Float(5.0)), Some(&"1+1".to_owned())),
            Cell::Number(5.0)
        );
        // No cached value, and a formula: a rejection, not a computation.
        assert_eq!(
            cell(Some(&Data::Empty), Some(&"1+1".to_owned())),
            Cell::FormulaWithoutValue
        );
        // No cached value and no formula: an empty cell.
        assert_eq!(cell(Some(&Data::Empty), None), Cell::Empty);
        assert_eq!(cell(None, Some(&String::new())), Cell::Empty);
    }
}
