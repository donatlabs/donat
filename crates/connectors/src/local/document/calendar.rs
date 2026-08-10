//! `calendar.render` — declared events into an `.ics` calendar.
//!
//! **A UID comes from declared input and is never generated** (spec 019 §6). A
//! calendar client keys an event on its UID: re-rendering with the same UID
//! updates the event a recipient already has, while a fresh UID adds a second
//! one beside it. A generated UID would therefore turn every re-send — a retry,
//! a corrected time, a second confirmation mail — into a duplicate in somebody
//! else's calendar, which is why an event without one is refused rather than
//! given one.
//!
//! `DTSTAMP` is declared input for the same reason the PDF's timestamp is:
//! `icalendar` fills it from the wall clock when it is absent, and that alone
//! would make two renders of one input differ.
//!
//! The product is a stored artifact when the input names an attachment column,
//! and an inline string when it does not — a calendar is small, and a process
//! that attaches one to an outgoing mail wants the text rather than a file it
//! would have to read back.
//!
//! **No declared value may write a second line.** An `.ics` is a line-oriented
//! format delimited by CRLF, so a value carrying one is a property injection —
//! an organizer of `mailto:a@b.c\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:victim@x.y`
//! adds an accepted attendee to somebody else's invitation. `icalendar` escapes
//! only the properties it classifies as `TEXT`, and `ORGANIZER`/`ATTENDEE` are
//! `CAL-ADDRESS`, `RRULE` is `RECUR`, and a `TZID` is a parameter — all written
//! through verbatim — while even its `TEXT` escape does not cover a bare `\r`.
//! So [`plain`] gates *every* value this operation writes, template-supplied
//! and process-supplied alike, rather than the three the library happens to
//! leave raw today. It refuses rather than escapes: none of these fields has a
//! legitimate use for a control character, and an escape would have to be
//! correct per value type where a refusal is correct for all of them.

use std::time::Duration;

use icalendar::{Calendar, Component, Event, Property};
use serde_json::{Value as JsonValue, json};

use super::{
    DocumentKind, DocumentTemplate, contract, refuse, required_text, select_template, text,
};
use crate::local::bounds::LocalBounds;
use crate::local::capability::{LocalArtifact, LocalInvocation, LocalOperation, LocalProduct};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::ConnectorFailure;

/// The template the registration probe renders. Compiled in, not declared.
pub const PROBE_TEMPLATE: &str = "donat.probe.calendar";

pub const PROBE_SOURCE: &str = r#"{"product_id":"-//donat//probe//EN","method":"PUBLISH"}"#;

const ICS: &str = "text/calendar";

/// The one input key the event list is bound to.
const EVENTS: &str = "events";

fn bounds() -> LocalBounds {
    LocalBounds::declare(
        Duration::from_secs(5),
        1_024 * 1_024,
        4 * 1_024 * 1_024,
        8 * 1_024 * 1_024,
        "events",
        1_000,
    )
    .expect("the calendar bounds are static and complete")
}

pub fn operation() -> LocalOperation {
    LocalOperation::declare("calendar.render", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "template": PROBE_TEMPLATE,
                    "document_timestamp": "2026-01-01T00:00:00Z",
                    "events": [{
                        "uid": "probe@donat.invalid",
                        "summary": "donat",
                        "start": "2026-01-01T09:00:00Z",
                        "end": "2026-01-01T10:00:00Z"
                    }]
                }),
                "the output is the declared events written into the declared calendar, with \
                 every UID and the DTSTAMP taken from declared input; no clock, no random \
                 seed, no environment, no locale",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(bounds())
        .units(|input| {
            input
                .get(EVENTS)
                .and_then(JsonValue::as_array)
                .map_or(0, |events| events.len() as u64)
        })
        .run(run)
        .build()
        .expect("calendar.render is deterministic")
}

/// The calendar-level declaration: what the template says about the whole file.
struct Layout {
    product_id: String,
    method: Option<String>,
    name: Option<String>,
}

fn layout(template: &DocumentTemplate) -> Result<Layout, ConnectorFailure> {
    let defect = || {
        refuse(
            "local_template_defect",
            "the selected calendar template's layout is not a declared calendar",
        )
    };
    let source = template.file(template.entry()).ok_or_else(defect)?;
    let parsed: JsonValue = serde_json::from_str(source).map_err(|_| defect())?;
    // A template is deployment metadata rather than a stranger's input, and it
    // is gated here anyway: these three are written into the same three raw
    // slots, and "who supplied it" is not what makes a CRLF safe.
    let optional = |field: &str| -> Result<Option<String>, ConnectorFailure> {
        match parsed.get(field).and_then(JsonValue::as_str) {
            Some(value) => Ok(Some(plain(value)?.to_owned())),
            None => Ok(None),
        }
    };
    Ok(Layout {
        product_id: plain(
            parsed
                .get("product_id")
                .and_then(JsonValue::as_str)
                .ok_or_else(defect)?,
        )?
        .to_owned(),
        method: optional("method")?,
        name: optional("name")?,
    })
}

/// Every value this operation writes goes through here first.
///
/// A control character is the only thing being refused, and it is refused
/// everywhere: `\r` and `\n` end a content line, and the rest have no meaning
/// in a calendar a client will read. Structural characters (`:`, `;`, `,`) are
/// left alone — they are legitimate inside a `CAL-ADDRESS` and inside a
/// `RECUR`, and the library escapes them where the value type says it must.
fn plain(value: &str) -> Result<&str, ConnectorFailure> {
    if value.chars().any(char::is_control) {
        return Err(refuse(
            "local_calendar_control_character",
            "a calendar value carries a control character; an `.ics` is delimited by line \
             breaks, so a value that contains one writes a property nobody declared",
        ));
    }
    Ok(value)
}

fn run(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let input = invocation.input();
    let template = select_template(invocation, DocumentKind::Calendar)?;
    let layout = layout(template)?;
    let stamp = ical_instant(required_text(input, "document_timestamp")?)?;

    let JsonValue::Array(events) = input.get(EVENTS).unwrap_or(&JsonValue::Null) else {
        return Err(refuse(
            "local_template_input_missing",
            "a calendar activity binds a list of events",
        ));
    };
    if events.is_empty() {
        return Err(refuse(
            "local_calendar_empty",
            "a calendar with no events is not a calendar",
        ));
    }
    invocation.reserve(events.len() * 512)?;

    let mut calendar = Calendar::new();
    calendar.append_property(Property::new("PRODID", layout.product_id.clone()));
    calendar.append_property(Property::new("VERSION", "2.0"));
    if let Some(method) = &layout.method {
        calendar.append_property(Property::new("METHOD", method.clone()));
    }
    if let Some(name) = &layout.name {
        calendar.name(name);
    }

    for declared in events {
        invocation.checkpoint()?;
        calendar.push(event(declared, &stamp)?);
    }

    let ics = calendar.done().to_string();
    if let Some(declared) = template.max_output_bytes()
        && ics.len() as u64 > declared
    {
        return Err(refuse(
            "local_output_too_large",
            "local capability output exceeds the operation's declared output ceiling",
        ));
    }

    let metadata = json!({
        "events": events.len(),
        "template": template.name(),
        "template_hash": template.content_hash(),
    });
    // Stored when the process named a column to store it in, inline when it did
    // not: a calendar attached to an outgoing message is wanted as text, and a
    // round trip through the attachment store to get it back would be work
    // nobody asked for.
    match text(input, "attachment") {
        Some(attachment) => Ok(LocalProduct::Artifact {
            artifact: LocalArtifact::new(
                attachment,
                required_text(input, "claim_role")?,
                required_text(input, "file_name")?,
                ICS,
                ics.into_bytes(),
            )?
            .claimed_by_session(text(input, "claim_session_key"))?,
            metadata,
        }),
        None => {
            let JsonValue::Object(mut object) = metadata else {
                unreachable!("the metadata above is an object")
            };
            object.insert("ics".to_owned(), JsonValue::String(ics));
            Ok(LocalProduct::Value(JsonValue::Object(object)))
        }
    }
}

/// One declared event.
///
/// Everything here is a declared field. There is no default summary, no
/// invented duration, and above all no generated UID.
fn event(declared: &JsonValue, stamp: &str) -> Result<Event, ConnectorFailure> {
    let uid = declared
        .get("uid")
        .and_then(JsonValue::as_str)
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| {
            refuse(
                "local_calendar_uid_missing",
                "a calendar event's UID comes from declared input; a generated one would add a \
                 second event to the recipient's calendar instead of updating the first",
            )
        })?;
    let uid = plain(uid)?;

    let mut event = Event::new();
    event.uid(uid);
    // `icalendar` fills DTSTAMP from the wall clock when it is absent, so it is
    // always written here.
    event.add_property("DTSTAMP", stamp);

    // The zone is written as a `TZID` parameter, which the library quotes but
    // never escapes, so it is gated exactly like a value.
    let timezone = match declared.get("timezone").and_then(JsonValue::as_str) {
        Some(zone) => Some(plain(zone)?),
        None => None,
    };
    event.append_property(instant_property("DTSTART", declared, "start", timezone)?);
    event.append_property(instant_property("DTEND", declared, "end", timezone)?);

    for (field, key) in [
        ("summary", "SUMMARY"),
        ("description", "DESCRIPTION"),
        ("location", "LOCATION"),
    ] {
        if let Some(value) = declared.get(field) {
            if value.is_null() {
                continue;
            }
            let value = value.as_str().ok_or_else(kind_mismatch)?;
            event.add_property(key, plain(value)?);
        }
    }
    if let Some(organizer) = declared.get("organizer").and_then(JsonValue::as_str) {
        event.add_property("ORGANIZER", plain(organizer)?);
    }
    if let Some(attendees) = declared.get("attendees") {
        let JsonValue::Array(attendees) = attendees else {
            return Err(kind_mismatch());
        };
        for attendee in attendees {
            event.append_multi_property(Property::new(
                "ATTENDEE",
                plain(attendee.as_str().ok_or_else(kind_mismatch)?)?,
            ));
        }
    }
    // The recurrence rule is produced by spec 021 and arrives already spelled
    // as an RFC 5545 RRULE; this operation writes it and does not invent one.
    if let Some(rule) = declared.get("recurrence").and_then(JsonValue::as_str) {
        event.add_property("RRULE", plain(rule)?);
    }
    // A sequence is how a client tells an update from the event it already has.
    if let Some(sequence) = declared.get("sequence") {
        let sequence = sequence.as_u64().ok_or_else(kind_mismatch)?;
        event.add_property("SEQUENCE", sequence.to_string());
    }
    Ok(event.done())
}

/// `DTSTART`/`DTEND`, with the TZID parameter when the event declared a zone.
fn instant_property(
    key: &'static str,
    declared: &JsonValue,
    field: &'static str,
    timezone: Option<&str>,
) -> Result<Property, ConnectorFailure> {
    let value = declared
        .get(field)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            refuse(
                "local_calendar_instant_missing",
                "a calendar event declares both its start and its end",
            )
        })?;
    match timezone {
        // A local time in a named zone: the `Z` suffix would say UTC, which is
        // a different instant, so it is refused rather than reinterpreted.
        Some(zone) => {
            if value.ends_with('Z') {
                return Err(contract(
                    "an event with a `timezone` declares local times, without a `Z` suffix",
                ));
            }
            let mut property = Property::new(key, ical_local(value)?);
            property.add_parameter("TZID", zone);
            Ok(property)
        }
        None => Ok(Property::new(key, ical_instant(value)?)),
    }
}

/// `2026-03-04T09:00:00Z` into the iCalendar UTC form `20260304T090000Z`.
fn ical_instant(source: &str) -> Result<String, ConnectorFailure> {
    let local = source.strip_suffix('Z').ok_or_else(|| {
        contract("a calendar instant is a UTC time spelled `YYYY-MM-DDTHH:MM:SSZ`")
    })?;
    Ok(format!("{}Z", ical_local(local)?))
}

/// `2026-03-04T09:00:00` into `20260304T090000`.
fn ical_local(source: &str) -> Result<String, ConnectorFailure> {
    let invalid = || contract("a calendar time is spelled `YYYY-MM-DDTHH:MM:SS`");
    let (date, time) = source.split_once('T').ok_or_else(invalid)?;
    let digits = |source: &str, separator: char, parts: usize| {
        let fields: Vec<&str> = source.split(separator).collect();
        if fields.len() != parts
            || fields
                .iter()
                .any(|field| field.len() != 2 && field.len() != 4)
        {
            return None;
        }
        if !fields
            .iter()
            .all(|field| field.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return None;
        }
        Some(fields.concat())
    };
    let date = digits(date, '-', 3).ok_or_else(invalid)?;
    let time = digits(time, ':', 3).ok_or_else(invalid)?;
    if date.len() != 8 || time.len() != 6 {
        return Err(invalid());
    }
    Ok(format!("{date}T{time}"))
}

fn kind_mismatch() -> ConnectorFailure {
    refuse(
        "local_template_input_kind",
        "a calendar event field's value does not match the kind the format requires",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two time spellings, and what each refuses.
    #[test]
    fn a_calendar_time_is_written_in_the_one_form_clients_read() {
        assert_eq!(
            ical_instant("2026-03-04T09:00:00Z").as_deref(),
            Ok("20260304T090000Z")
        );
        assert_eq!(
            ical_local("2026-03-04T09:00:00").as_deref(),
            Ok("20260304T090000")
        );
        assert!(
            ical_instant("2026-03-04T09:00:00").is_err(),
            "a UTC instant carries its `Z`"
        );
        assert!(
            ical_local("2026-3-4T9:0:0").is_err(),
            "the fields are fixed width"
        );
        assert!(ical_local("2026-03-04 09:00:00").is_err());
    }

    /// The one gate every written value passes, at the unit it is decided in.
    #[test]
    fn a_control_character_never_reaches_a_property() {
        assert_eq!(
            plain("mailto:ops@example.test").as_deref(),
            Ok("mailto:ops@example.test")
        );
        // The structural characters stay: they are legal inside a CAL-ADDRESS
        // and a RECUR, and the library escapes them where the type says so.
        assert!(plain("FREQ=WEEKLY;COUNT=4").is_ok());
        assert!(plain("Delivery, boxes; bay 3").is_ok());
        for injected in [
            "mailto:a@b.c\r\nATTENDEE:mailto:victim@x.y",
            "a\nb",
            "a\rb",
            "a\u{0}b",
            "a\u{7f}b",
            "a\u{85}b",
        ] {
            let failure = plain(injected).expect_err("a control character is refused");
            assert_eq!(failure.code(), "local_calendar_control_character");
        }
    }

    /// A zone and a `Z` suffix contradict each other, and the contradiction is
    /// refused rather than resolved by guessing which one the caller meant.
    #[test]
    fn a_zoned_event_declares_local_times() {
        let declared = json!({ "start": "2026-03-04T09:00:00Z", "end": "2026-03-04T10:00:00Z" });
        assert!(instant_property("DTSTART", &declared, "start", Some("Europe/Berlin")).is_err());
        let declared = json!({ "start": "2026-03-04T09:00:00", "end": "2026-03-04T10:00:00" });
        let property = instant_property("DTSTART", &declared, "start", Some("Europe/Berlin"))
            .expect("a local time in a named zone is written with its TZID");
        assert_eq!(property.value(), "20260304T090000");
    }
}
