//! `csv.read` — one declared schema and one uploaded CSV, into typed rows.
//!
//! Everything the spreadsheet reader does with an archive is absent here, and
//! everything it does with a schema is identical. What is different is one
//! thing, and it is a correctness rule rather than an omission:
//!
//! **A CSV has no date system.** An `.xlsx` stores a date as a day count from
//! an epoch its own workbook declares, which is why the same serial is two
//! different dates in the two systems. A CSV declares nothing, so a bare number
//! in a declared date column is refused rather than guessed at — a guess here
//! is a year or four years of silent error, and the file gives no way to tell
//! which. Dates in a CSV are ISO text or they are rejections.
//!
//! The rest is the same discipline: the stored file's size before it is read,
//! then row, column, and per-cell ceilings, then working memory and the
//! deadline. There is no decompression step to bound because there is no
//! archive — a `.csv.gz` is not a thing this operation accepts.

use std::time::Duration;

use serde_json::{Map as JsonMap, Value as JsonValue, json};

use super::schema::{Cell, DateSystem, IngestKind};
use super::{Rows, begin, charge, refuse};
use crate::local::bounds::LocalBounds;
use crate::local::capability::{LocalInvocation, LocalOperation, LocalProduct};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::ConnectorFailure;

/// The schema and the stored file the registration probe reads. Compiled in.
pub const PROBE_SCHEMA: &str = "donat.probe.ingest.csv";
pub const PROBE_SOURCE: &str = "donat.probe.source.csv";

fn bounds() -> LocalBounds {
    LocalBounds::declare(
        Duration::from_secs(30),
        4 * 1_024,
        64 * 1_024 * 1_024,
        super::MAX_WORKING_BYTES,
        "cells",
        4_000_000,
    )
    .expect("the csv ingest bounds are static and complete")
}

pub fn operation() -> LocalOperation {
    LocalOperation::declare("csv.read", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({ "schema": PROBE_SCHEMA, "source": PROBE_SOURCE }),
                "the output is the declared schema applied to the stored bytes: no clock, no \
                 locale, no environment, and no date system to guess at",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(bounds())
        .units(|_| 0)
        .run(run)
        .build()
        .expect("csv.read is deterministic")
}

fn run(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    // BOUND 1, and the two selections that precede it.
    let reading = begin(invocation, IngestKind::Csv)?;
    let bytes = reading.source.bytes();
    // BOUND 5: the text is held once, and charged before it is walked.
    charge(invocation, reading.max_working_bytes, bytes.len())?;

    // A UTF-8 BOM is a byte-order mark, not the first character of the first
    // header. Removing it is the one normalization this reader performs.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let mut reader = ::csv::ReaderBuilder::new()
        .delimiter(reading.schema.delimiter())
        .has_headers(false)
        // A short or long row is a row error, not a file error: the schema
        // decides what a missing cell means, and it decides per row.
        .flexible(true)
        .from_reader(bytes);

    let not_text = || {
        refuse(
            "ingest_not_text",
            "the stored file is not the UTF-8 text its schema expects",
        )
    };

    let mut records = reader.records();
    // Everything above the header row is skipped without being looked at.
    for _ in 1..reading.schema.header_row() {
        invocation.checkpoint()?;
        records.next().transpose().map_err(|_| not_text())?;
    }
    let header = records
        .next()
        .transpose()
        .map_err(|_| not_text())?
        .ok_or_else(|| {
            refuse(
                "ingest_header_column_missing",
                "the stored file has no header row, so the columns the schema declares are not \
                 in it",
            )
        })?;
    if header.len() as u64 > reading.max_columns {
        return Err(refuse(
            "ingest_columns_exceeded",
            "the stored file is wider than the schema admits",
        ));
    }
    let header: Vec<String> = header.iter().map(str::to_owned).collect();
    // The whole file fails here, before a row is read.
    let bound = reading.schema.bind_header(&header)?;

    let mut answer = Rows::new(&reading);
    let mut read: u64 = 0;
    for (index, record) in records.enumerate() {
        invocation.checkpoint()?;
        let record = record.map_err(|_| not_text())?;
        read += 1;
        if read > reading.max_rows {
            return Err(refuse(
                "ingest_rows_exceeded",
                "the stored file carries more rows than the schema admits",
            ));
        }
        if record.iter().all(|field| field.trim().is_empty()) {
            continue;
        }
        invocation.charge_units(reading.schema.columns().len() as u64)?;

        let number = u64::from(reading.schema.header_row()) + index as u64 + 1;
        let mut values = JsonMap::new();
        let mut rejected = false;
        for (column, position) in reading.schema.columns().iter().zip(&bound) {
            let cell = match record.get(*position) {
                None => Cell::Empty,
                Some(text) => Cell::Text(text.to_owned()),
            };
            // `DateSystem::None`: a CSV declares no epoch, so a number in a
            // date column is a number.
            match super::schema::coerce(column, &cell, reading.max_cell_bytes, DateSystem::None) {
                Ok(value) => {
                    values.insert(column.field.clone(), value);
                }
                Err(reason) => {
                    answer.reject(number, column, reason)?;
                    rejected = true;
                    break;
                }
            }
        }
        if !rejected && !values.values().all(JsonValue::is_null) {
            answer.accept(values);
        }
    }

    let product = answer.finish(&reading, None);
    charge(
        invocation,
        reading.max_working_bytes,
        crate::local::canonical_bytes(&product).len(),
    )?;
    Ok(LocalProduct::Value(product))
}
