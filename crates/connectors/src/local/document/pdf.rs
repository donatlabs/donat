//! `pdf.render` — a Typst template and declared data into one stored PDF.
//!
//! The sandbox is the design (spec 019 §3), and it is split in two on purpose.
//! [`super::template`] refuses a package import or an out-of-set path *at
//! load*, so a template that would reach outside its own files never becomes a
//! template; [`super::world`] then closes the same door at render time, because
//! a lexical check cannot see a path a template computes. Neither half is
//! sufficient alone and neither is redundant.
//!
//! Determinism has three sources here, and all three are declared input:
//!
//! * the **document id**, which becomes the PDF's `/ID` — `Smart::Auto` would
//!   derive it from title and author, which is stable, but a process that
//!   re-renders one invoice should say so itself;
//! * the **timestamp**, which becomes `/CreationDate` and Typst's
//!   `datetime.today()`. The renderer never reads a clock;
//! * the **fonts**, which are the two families compiled into this binary.
//!
//! A missing glyph is reported in the typed metadata rather than swallowed: it
//! renders as the font's own notdef, and the activity output says which
//! characters were not covered.

use std::time::Duration;

use serde_json::{Map as JsonMap, Value as JsonValue, json};
use typst::foundations::{Datetime, Dict, IntoValue, Smart, Value as TypstValue};
use typst_layout::PagedDocument;
use typst_pdf::{PdfOptions, PdfStandards, Timestamp};

use super::world::ClosedWorld;
use super::{DocumentKind, contract, fonts, refuse, required_text, select_template, template_data};
use crate::local::bounds::LocalBounds;
use crate::local::capability::{LocalArtifact, LocalInvocation, LocalOperation, LocalProduct};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::ConnectorFailure;

/// The template the registration probe renders. Compiled in, not declared.
pub const PROBE_TEMPLATE: &str = "donat.probe.pdf";

/// The probe's source: a page of the declared subject, and nothing else that
/// could vary between two renders.
pub const PROBE_SOURCE: &str = r#"#set page(width: 120pt, height: 80pt, margin: 8pt)
#set text(size: 9pt)
= #sys.inputs.subject
"#;

/// The absolute ceilings of the operation. A template may narrow any of them
/// and none of them may be widened, which is what keeps a bound a bound.
fn bounds() -> LocalBounds {
    LocalBounds::declare(
        Duration::from_secs(15),
        256 * 1_024,
        8 * 1_024 * 1_024,
        64 * 1_024 * 1_024,
        "pages",
        200,
    )
    .expect("the pdf bounds are static and complete")
}

pub fn operation() -> LocalOperation {
    LocalOperation::declare("pdf.render", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "template": PROBE_TEMPLATE,
                    "subject": "donat",
                    "document_id": "probe",
                    "document_timestamp": "2026-01-01T00:00:00Z",
                    "attachment": "public.document.file",
                    "claim_role": "app",
                    "file_name": "probe.pdf"
                }),
                "the output is the declared template rendered over the declared input, with \
                 the document id, the timestamp, and the fonts all taken from the declaration; \
                 no clock, no system font lookup, no filesystem, and no package registry",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(bounds())
        // Pages are not knowable before layout, so the pre-count is zero and
        // the real count is charged once the document exists.
        .units(|_| 0)
        .run(run)
        .build()
        .expect("pdf.render is deterministic")
}

fn run(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let input = invocation.input();
    let template = select_template(invocation, DocumentKind::Pdf)?;
    let attachment = required_text(input, "attachment")?;
    let claim_role = required_text(input, "claim_role")?;
    let file_name = required_text(input, "file_name")?;
    let document_id = required_text(input, "document_id")?;
    let timestamp = parse_timestamp(required_text(input, "document_timestamp")?)?;

    let mut inputs = Dict::new();
    for (name, value) in template_data(input, template)? {
        inputs.insert(name.into(), typst_value(&value));
    }
    // The template's own text is charged as working memory: the compiler holds
    // every source of the set at once, and a ceiling nothing charges against is
    // not a ceiling.
    invocation.reserve(template.files().values().map(String::len).sum())?;
    invocation.checkpoint()?;

    let world = ClosedWorld::new(template, inputs, Some(timestamp.0));
    let document = compile_bounded(invocation, world).map_err(|failure| match failure {
        Compilation::Refused(failure) => failure,
        Compilation::Diagnostics(detail) => {
            // The diagnostics carry template text, which a `ConnectorFailure`
            // does not: what an operator routes on is the code, and the
            // template is the deployment's own file. It is logged here and
            // named in the code.
            tracing_free_log("pdf template did not compile", template.name(), &detail);
            refuse(
                "local_template_compile_failed",
                "the selected document template did not compile",
            )
        }
    })?;
    invocation.checkpoint()?;

    // Pages, counted from the document and charged against both the declared
    // ceiling and whatever the template narrowed it to.
    let pages = document.pages().len() as u64;
    invocation.charge_units(pages)?;
    if let Some(declared) = template.max_pages()
        && pages > declared
    {
        return Err(pages_exceeded());
    }

    let bytes = typst_pdf::pdf(
        &document,
        &PdfOptions {
            // A stable identifier the caller supplied, so a re-render of one
            // input is the same document rather than a new one.
            ident: Smart::Custom(document_id.to_owned()),
            // `Smart::Auto` would write "Typst <version>", which is true but
            // makes the engine's own upgrade a diff in every stored invoice.
            creator: Smart::Custom(Some("donat".to_owned())),
            timestamp: Some(Timestamp::new_utc(timestamp.0)),
            page_ranges: None,
            standards: PdfStandards::default(),
            tagged: true,
            pretty: false,
        },
    )
    .map_err(|diagnostics| {
        tracing_free_log(
            "pdf template did not export",
            template.name(),
            &diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        );
        refuse(
            "local_template_export_failed",
            "the selected document template did not export to PDF",
        )
    })?;

    if let Some(declared) = template.max_output_bytes()
        && bytes.len() as u64 > declared
    {
        return Err(output_exceeded());
    }

    let uncovered = uncovered_characters(input, template);
    Ok(LocalProduct::Artifact {
        artifact: LocalArtifact::new(attachment, claim_role, file_name, "application/pdf", bytes)?
            .claimed_by_session(super::text(input, "claim_session_key"))?,
        metadata: json!({
            "pages": pages,
            "template": template.name(),
            "template_hash": template.content_hash(),
            "warnings": warnings(&uncovered),
        }),
    })
}

/// Why a compilation did not produce a document.
enum Compilation {
    /// The bound that stopped it: the cpu deadline, or the drain.
    Refused(ConnectorFailure),
    /// The template did not compile, with the diagnostics as one line of text.
    Diagnostics(String),
}

/// How often the waiter looks at the deadline and the stop signal.
const COMPILE_POLL: Duration = Duration::from_millis(25);

/// Compile on a thread of its own, and stop waiting when the budget is gone.
///
/// `typst::compile` is a black box to both of ADR 044's operational promises.
/// It takes no callback, its `Sink` is created inside it, and over a
/// forty-second compile it touches its `World` exactly once — so there is no
/// tracked method a `checkpoint` could live in, and "the work observes the
/// signal and ends" cannot be true of the typesetter itself. The observer
/// therefore has to be somebody who is not inside it.
///
/// What is left running when the wait ends is a thread holding a
/// [`ClosedWorld`] and nothing else: no runtime handle, no connection, no file,
/// no signal it could be needed to answer. Its product is dropped, and its
/// remaining work is CPU that ends on its own. That is a deliberately smaller
/// promise than "the work stops", and it is the one that can actually be kept
/// here — while the promise that matters to a Process, that an activity never
/// outlives the `start_to_close` it declared, becomes true instead of
/// aspirational.
fn compile_bounded(
    invocation: &LocalInvocation<'_>,
    world: ClosedWorld,
) -> Result<PagedDocument, Compilation> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("donat-pdf-render".to_owned())
        .spawn(move || {
            let compiled = typst::compile::<PagedDocument>(&world);
            // A closed receiver means the wait ended without us, which is the
            // ordinary outcome of a deadline: there is nobody to tell.
            let _ = sender.send(compiled.output.map_err(|diagnostics| {
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            }));
        })
        .map_err(|_| {
            Compilation::Refused(ConnectorFailure::new(
                crate::sdk::errors::ConnectorErrorClass::Timeout,
                "local_render_unavailable",
                "a local render could not be started on this replica",
            ))
        })?;

    loop {
        match receiver.recv_timeout(COMPILE_POLL) {
            Ok(Ok(document)) => return Ok(document),
            Ok(Err(detail)) => return Err(Compilation::Diagnostics(detail)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                invocation.checkpoint().map_err(Compilation::Refused)?;
            }
            // The renderer ended without answering, which is this binary's own
            // code failing rather than a template's.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Compilation::Refused(ConnectorFailure::new(
                    crate::sdk::errors::ConnectorErrorClass::Invariant,
                    "local_render_did_not_finish",
                    "a local render ended without producing a document or a diagnostic",
                )));
            }
        }
    }
}

/// The typed warnings of spec 019 §3.
///
/// A character with no glyph in either embedded family renders as the font's
/// own notdef box. It is visible in the PDF and invisible to whoever asked for
/// it, so it is named here instead.
fn warnings(uncovered: &[String]) -> JsonValue {
    if uncovered.is_empty() {
        return JsonValue::Array(Vec::new());
    }
    json!([{
        "code": "missing_glyphs",
        "characters": uncovered,
        "message": "the embedded fonts have no glyph for these characters; they render as the \
                    font's notdef",
    }])
}

fn uncovered_characters(input: &JsonValue, template: &super::DocumentTemplate) -> Vec<String> {
    let fonts = fonts::embedded();
    let mut uncovered = std::collections::BTreeSet::new();
    let walk = |value: &JsonValue| {
        fn strings(value: &JsonValue, out: &mut Vec<String>) {
            match value {
                JsonValue::String(text) => out.push(text.clone()),
                JsonValue::Array(items) => items.iter().for_each(|item| strings(item, out)),
                JsonValue::Object(object) => {
                    object.values().for_each(|item| strings(item, out));
                }
                _ => {}
            }
        }
        let mut collected = Vec::new();
        strings(value, &mut collected);
        collected
    };
    for name in template.inputs() {
        let Some(value) = input.get(name) else {
            continue;
        };
        for text in walk(value) {
            for character in text.chars() {
                if character.is_whitespace() || character.is_control() {
                    continue;
                }
                if !fonts.covers(&character.to_string()) {
                    uncovered.insert(character.to_string());
                }
            }
        }
    }
    uncovered.into_iter().collect()
}

/// A page ceiling and an output ceiling a template narrowed keep the operation's
/// own refusal codes: an operator reading a journal should not have to know
/// whether the limit came from the capability or the template.
fn pages_exceeded() -> ConnectorFailure {
    ConnectorFailure::new(
        crate::sdk::errors::ConnectorErrorClass::Validation,
        "local_units_exceeded",
        "local capability input exceeds the operation's declared unit ceiling",
    )
}

fn output_exceeded() -> ConnectorFailure {
    ConnectorFailure::new(
        crate::sdk::errors::ConnectorErrorClass::Validation,
        "local_output_too_large",
        "local capability output exceeds the operation's declared output ceiling",
    )
}

/// The declared timestamp, in the one spelling a process produces.
struct DocumentTimestamp(Datetime);

fn parse_timestamp(source: &str) -> Result<DocumentTimestamp, ConnectorFailure> {
    let invalid =
        || contract("`document_timestamp` is a UTC instant spelled `YYYY-MM-DDTHH:MM:SSZ`");
    let (date, rest) = source.split_once('T').ok_or_else(invalid)?;
    let time = rest.strip_suffix('Z').ok_or_else(invalid)?;
    let mut date = date.split('-');
    let mut clock = time.split(':');
    let number = |part: Option<&str>| -> Result<u32, ConnectorFailure> {
        part.ok_or_else(invalid)?
            .parse::<u32>()
            .map_err(|_| invalid())
    };
    let year = number(date.next())?;
    let month = number(date.next())?;
    let day = number(date.next())?;
    let hour = number(clock.next())?;
    let minute = number(clock.next())?;
    let second = number(clock.next())?;
    if date.next().is_some() || clock.next().is_some() {
        return Err(invalid());
    }
    Datetime::from_ymd_hms(
        i32::try_from(year).map_err(|_| invalid())?,
        u8::try_from(month).map_err(|_| invalid())?,
        u8::try_from(day).map_err(|_| invalid())?,
        u8::try_from(hour).map_err(|_| invalid())?,
        u8::try_from(minute).map_err(|_| invalid())?,
        u8::try_from(second).map_err(|_| invalid())?,
    )
    .map(DocumentTimestamp)
    .ok_or_else(invalid)
}

/// JSON into the value language `sys.inputs` speaks.
///
/// Numbers keep their kind — an integer stays an integer — because a template
/// that formats a total should not have to know it came through JSON.
pub(crate) fn typst_value(value: &JsonValue) -> TypstValue {
    match value {
        JsonValue::Null => TypstValue::None,
        JsonValue::Bool(flag) => (*flag).into_value(),
        JsonValue::Number(number) => number
            .as_i64()
            .map(IntoValue::into_value)
            .or_else(|| number.as_f64().map(IntoValue::into_value))
            .unwrap_or(TypstValue::None),
        JsonValue::String(text) => text.clone().into_value(),
        JsonValue::Array(items) => items
            .iter()
            .map(typst_value)
            .collect::<typst::foundations::Array>()
            .into_value(),
        JsonValue::Object(object) => object_value(object),
    }
}

fn object_value(object: &JsonMap<String, JsonValue>) -> TypstValue {
    let mut dict = Dict::new();
    for (key, value) in object {
        dict.insert(key.clone().into(), typst_value(value));
    }
    dict.into_value()
}

/// The one place a compile diagnostic is written out.
///
/// It goes to `stderr` through the crate's usual diagnostic path rather than
/// into the failure, because a `ConnectorFailure` carries no free text: the
/// message would otherwise reach a journal, a retry decision, and an API
/// response, none of which should hold a template's contents.
fn tracing_free_log(what: &str, template: &str, detail: &str) {
    eprintln!("donat::local::document: {what} (template `{template}`): {detail}");
}
