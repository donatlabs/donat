//! `local.document` — the four renderers of spec 019.
//!
//! An invoice, a transactional email, an export, a calendar invitation: the
//! work every client project repeats, with no provider behind any of it. All
//! four operations share one shape — a template selected by name from
//! deployment metadata, plus typed data from the process — and differ only in
//! what they render it with.
//!
//! | Operation | Backend | Product |
//! |---|---|---|
//! | `pdf.render` | `typst` in a [closed world](world) | a stored `.pdf` |
//! | `email.render` | `mrml` | an inline `html` / `text` pair |
//! | `spreadsheet.render` | `rust_xlsxwriter` | a stored `.xlsx` |
//! | `calendar.render` | `icalendar` | an `.ics`, stored or inline |
//!
//! Two properties of this module are security properties rather than features,
//! and each has a test of its own.
//!
//! *A spreadsheet cell is never a formula.* A value beginning `=`, `+`, `-`, or
//! `@` is written inert, because an export that carries one is a
//! code-execution vector in whichever spreadsheet application opens it — the
//! recipient's, not ours.
//!
//! *An email escapes by default.* Interpolated values are HTML-escaped unless
//! the template's declared input type says the value is already markup, which
//! is a decision made in metadata and frozen onto the template.
//!
//! The templates themselves are never input: input selects one by name from the
//! set the deployment declared, and the set arrives in the [`LocalContext`]
//! (`crate::local::context`) rather than in the operation's JSON.

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::local::capability::{LocalCapability, LocalInvocation};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};

pub mod calendar;
pub mod email;
pub mod fonts;
pub mod pdf;
pub mod spreadsheet;
pub mod template;
pub mod world;

pub use template::{
    DocumentKind, DocumentTemplate, DocumentTemplateSet, DocumentTemplateSpec, TemplateRejection,
};

/// The capability's declaration, built once by the table in
/// [`crate::local::capabilities`].
pub fn capability() -> LocalCapability {
    LocalCapability::declare("local.document", "1.0.0")
        .operation(pdf::operation())
        .operation(email::operation())
        .operation(spreadsheet::operation())
        .operation(calendar::operation())
        .build()
        .expect("the document capability declaration is static and complete")
}

/// An input that does not satisfy an operation's contract is a `validation`
/// failure: the same input will fail again, so a retry cannot help.
///
/// The message is `&'static str` because a [`ConnectorFailure`] never carries
/// caller or provider text — the code is what an operator routes on and what a
/// journal retains. The detail that would have gone into a formatted message
/// goes into the code instead, which is why there are several of them.
pub(crate) fn contract(message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Validation,
        "local_input_contract",
        message,
    )
}

/// A refusal with its own code, for the input mistakes an operator has to tell
/// apart: which template, which kind, which field.
pub(crate) fn refuse(code: &'static str, message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(ConnectorErrorClass::Validation, code, message)
}

/// Select the template an input names, from the frozen set of the deployment.
///
/// Three refusals, in the order that makes each one readable: no name, a name
/// the deployment did not declare, and a template of the wrong kind. There is
/// no fourth branch in which a template arrives from somewhere else.
pub(crate) fn select_template<'a>(
    invocation: &'a LocalInvocation<'a>,
    kind: DocumentKind,
) -> Result<&'a DocumentTemplate, ConnectorFailure> {
    let name = text(invocation.input(), "template").ok_or_else(|| {
        contract("a document activity selects its template by name: input requires `template`")
    })?;
    let template = invocation.context().templates().get(name).ok_or_else(|| {
        refuse(
            "local_template_unknown",
            "the selected document template is not declared by this deployment",
        )
    })?;
    if template.kind() != kind {
        return Err(refuse(
            "local_template_wrong_kind",
            "the selected document template renders through another operation",
        ));
    }
    Ok(template)
}

/// The declared inputs of a template, taken out of the operation input.
///
/// A declared input the activity did not bind is refused here as well as in
/// metadata validation: the runtime is the last place that can tell the
/// difference between "the field is empty" and "nobody passed it".
pub(crate) fn template_data(
    input: &JsonValue,
    template: &DocumentTemplate,
) -> Result<JsonMap<String, JsonValue>, ConnectorFailure> {
    let mut data = JsonMap::new();
    for name in template.inputs() {
        let value = input.get(name).ok_or_else(|| {
            refuse(
                "local_template_input_missing",
                "the activity does not bind an input the selected template declares",
            )
        })?;
        data.insert(name.clone(), value.clone());
    }
    Ok(data)
}

/// A required string field of the operation input.
pub(crate) fn text<'a>(input: &'a JsonValue, field: &str) -> Option<&'a str> {
    input.get(field).and_then(JsonValue::as_str)
}

pub(crate) fn required_text<'a>(
    input: &'a JsonValue,
    field: &'static str,
) -> Result<&'a str, ConnectorFailure> {
    text(input, field).ok_or_else(|| {
        ConnectorFailure::new(
            ConnectorErrorClass::Validation,
            "local_input_contract",
            field,
        )
    })
}

/// The templates compiled into this binary.
///
/// They exist for one reason: [`crate::local::context::LocalContext::builtin`]
/// is the context an operation's determinism is proven in at registration, and
/// ADR 044 makes that proof a property of the binary. Proving it against a
/// deployment's templates would mean an operation that registers on one
/// deployment and not on another.
pub fn builtin_templates() -> DocumentTemplateSet {
    let probe = |name: &str, kind: DocumentKind, extension: &str, source: &str| {
        let entry = format!("/probe.{extension}");
        DocumentTemplateSpec {
            name: name.to_owned(),
            kind: Some(kind),
            entry: entry.clone(),
            files: std::collections::BTreeMap::from([(entry, source.to_owned())]),
            // The probes are compiled in, so their "content hash" is a
            // constant: there are no bytes on disk for it to pin.
            content_hash: "0".repeat(64),
            inputs: std::collections::BTreeSet::from([match kind {
                DocumentKind::Spreadsheet => "rows".to_owned(),
                DocumentKind::Calendar => "events".to_owned(),
                _ => "subject".to_owned(),
            }]),
            ..Default::default()
        }
    };
    DocumentTemplateSet::resolve([
        probe(
            pdf::PROBE_TEMPLATE,
            DocumentKind::Pdf,
            "typ",
            pdf::PROBE_SOURCE,
        ),
        probe(
            email::PROBE_TEMPLATE,
            DocumentKind::Email,
            "mjml",
            email::PROBE_SOURCE,
        ),
        probe(
            spreadsheet::PROBE_TEMPLATE,
            DocumentKind::Spreadsheet,
            "json",
            spreadsheet::PROBE_SOURCE,
        ),
        probe(
            calendar::PROBE_TEMPLATE,
            DocumentKind::Calendar,
            "json",
            calendar::PROBE_SOURCE,
        ),
    ])
    .expect("the built-in probe templates are static and closed")
}
