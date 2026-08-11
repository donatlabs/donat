//! `email.render` — an MJML template and declared data into a responsive HTML
//! message and its plain-text alternative.
//!
//! The product is an inline typed pair rather than a stored artifact (spec 019
//! §4): a mail-sending activity in the same process consumes it directly, and
//! putting it through the attachment store would mean writing a file, signing a
//! URL, and reading it back to build one message.
//!
//! Interpolation is deliberately not a language. There is no expression
//! evaluation, no arithmetic, no function call, and no way to name anything the
//! template did not declare as an input:
//!
//! | Form | Meaning |
//! |---|---|
//! | `{{ order.number }}` | one declared value, HTML-escaped |
//! | `{{#each order.lines}} … {{/each}}` | a declared repeat over a typed list |
//! | `{{#if order.note}} … {{/if}}` | a declared optional block |
//!
//! **Escaping is the default and the exception is declared.** A value is
//! escaped unless its dotted path is one the template's declared input types
//! marked as `Html` — a decision made in metadata, resolved at load, and frozen
//! onto the template. A renderer that decided this per value would be deciding
//! it from the value's own contents, which is how injection works.
//!
//! The text alternative is derived from the rendered HTML rather than from a
//! second template, so it cannot drift from what the recipient's HTML client
//! shows: it is the same data, laid out by the same template.

use std::time::Duration;

use serde_json::{Value as JsonValue, json};

use super::{DocumentKind, contract, refuse, select_template, template_data};
use crate::local::bounds::LocalBounds;
use crate::local::capability::{LocalInvocation, LocalOperation, LocalProduct};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::ConnectorFailure;

/// The template the registration probe renders. Compiled in, not declared.
pub const PROBE_TEMPLATE: &str = "donat.probe.email";

pub const PROBE_SOURCE: &str = r#"<mjml><mj-body><mj-section><mj-column>
<mj-text>{{ subject }}</mj-text>
</mj-column></mj-section></mj-body></mjml>"#;

fn bounds() -> LocalBounds {
    LocalBounds::declare(
        Duration::from_secs(5),
        256 * 1_024,
        1_024 * 1_024,
        8 * 1_024 * 1_024,
        "characters",
        512 * 1_024,
    )
    .expect("the email bounds are static and complete")
}

pub fn operation() -> LocalOperation {
    LocalOperation::declare("email.render", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({ "template": PROBE_TEMPLATE, "subject": "donat" }),
                "the output is the declared template interpolated with the declared input; \
                 no clock, no random seed, no environment, no locale, and no include loader",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(bounds())
        // Characters are not knowable before the template is laid out, so the
        // pre-count is zero and the rendered size is charged afterwards.
        .units(|_| 0)
        .run(run)
        .build()
        .expect("email.render is deterministic")
}

fn run(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let input = invocation.input();
    let template = select_template(invocation, DocumentKind::Email)?;
    let data = JsonValue::Object(template_data(input, template)?);
    let source = template
        .file(template.entry())
        .ok_or_else(|| refuse("local_template_defect", "the template has no entry file"))?;

    invocation.reserve(source.len())?;
    // `interpolate` charges its own output as it writes it, which is what the
    // repeat form makes necessary: charging it afterwards would mean the
    // ceiling is consulted once the memory it bounds already exists.
    let interpolated = interpolate(source, &data, template.html_paths(), invocation)?;
    invocation.checkpoint()?;

    // The parser's default include loader is the no-op one: an `mj-include`
    // resolves to nothing, because a template that could pull in a file or a
    // URL at render time is neither closed nor `Pure`.
    let parsed = mrml::parse(&interpolated).map_err(|error| {
        eprintln!(
            "donat::local::document: email template `{}` did not parse: {error}",
            template.name()
        );
        refuse(
            "local_template_compile_failed",
            "the selected document template did not parse as MJML",
        )
    })?;
    let html = parsed
        .element
        .render(&mrml::prelude::render::RenderOptions {
            disable_comments: true,
            social_icon_origin: None,
            // Emptied on purpose: the default set writes `<link>` tags at a
            // font CDN into every message, which makes the mail fetch from a
            // third party when the recipient opens it.
            fonts: std::collections::HashMap::new(),
        })
        .map_err(|error| {
            eprintln!(
                "donat::local::document: email template `{}` did not render: {error}",
                template.name()
            );
            refuse(
                "local_template_export_failed",
                "the selected document template did not render to HTML",
            )
        })?;

    invocation.charge_units(html.chars().count() as u64)?;
    let text = plain_text(&html);
    Ok(LocalProduct::Value(json!({
        "html": html,
        "text": text,
        "template": template.name(),
        "template_hash": template.content_hash(),
    })))
}

// ---------------------------------------------------------------------------
// Interpolation
// ---------------------------------------------------------------------------

/// One frame of the repeat stack: the value paths resolve against, and the
/// dotted path that value sits at.
struct Scope<'a> {
    value: &'a JsonValue,
    path: String,
}

/// Replace every declared placeholder in `source`.
///
/// The scan is a single left-to-right pass over the template text. It is not a
/// parser and does not want to be: the whole grammar is three forms, and a
/// fourth would be an expression language this operation deliberately does not
/// have.
///
/// The output is charged against the working-memory ceiling *while it is
/// written*, and `charged` is what makes that exact: every call adds only what
/// the buffer has grown since the last one, so the total charged for one render
/// is the length of what it produced and nothing is counted twice. `{{#each}}`
/// is why it has to be this way — it is the one form whose output is the
/// product of a process's list and a template's body, and a body containing no
/// placeholder reaches neither a charge nor a `checkpoint` on its own.
fn interpolate(
    source: &str,
    data: &JsonValue,
    html_paths: &std::collections::BTreeSet<String>,
    invocation: &LocalInvocation<'_>,
) -> Result<String, ConnectorFailure> {
    /// Charge whatever `out` has grown by since the last charge.
    fn charge_growth(
        out: &str,
        charged: &std::cell::Cell<usize>,
        invocation: &LocalInvocation<'_>,
    ) -> Result<(), ConnectorFailure> {
        let grown = out.len().saturating_sub(charged.get());
        if grown == 0 {
            return Ok(());
        }
        charged.set(out.len());
        invocation.reserve(grown)
    }

    fn render(
        source: &str,
        scopes: &mut Vec<Scope<'_>>,
        html_paths: &std::collections::BTreeSet<String>,
        out: &mut String,
        charged: &std::cell::Cell<usize>,
        invocation: &LocalInvocation<'_>,
    ) -> Result<(), ConnectorFailure> {
        let mut rest = source;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let end = after
                .find("}}")
                .ok_or_else(|| contract("an email template has an unclosed `{{`"))?;
            let tag = after[..end].trim();
            let tail = &after[end + 2..];
            invocation.checkpoint()?;

            if let Some(path) = tag.strip_prefix("#each ") {
                let (body, remainder) = block(tail, "each")?;
                let (value, full) = resolve(scopes, path.trim())?;
                let JsonValue::Array(items) = value else {
                    return Err(refuse(
                        "local_template_input_kind",
                        "an email template repeats over a list, and the bound value is not one",
                    ));
                };
                for item in items {
                    // Per iteration, both of them: an iteration that writes a
                    // body with no placeholder in it is otherwise invisible to
                    // the ceiling and to the drain alike.
                    invocation.checkpoint()?;
                    charge_growth(out, charged, invocation)?;
                    scopes.push(Scope {
                        value: item,
                        path: full.clone(),
                    });
                    let result = render(body, scopes, html_paths, out, charged, invocation);
                    scopes.pop();
                    result?;
                }
                charge_growth(out, charged, invocation)?;
                rest = remainder;
                continue;
            }
            if let Some(path) = tag.strip_prefix("#if ") {
                let (body, remainder) = block(tail, "if")?;
                let (value, _) = resolve(scopes, path.trim())?;
                if is_present(value) {
                    render(body, scopes, html_paths, out, charged, invocation)?;
                }
                rest = remainder;
                continue;
            }
            if tag.starts_with('#') || tag.starts_with('/') {
                return Err(contract(
                    "an email template has only `{{ value }}`, `{{#each }}`, and `{{#if }}`",
                ));
            }

            let (value, full) = resolve(scopes, tag)?;
            let rendered = scalar(value)?;
            // The one branch that decides escaping, and it decides it from the
            // declared type rather than from the value.
            if html_paths.contains(&full) {
                out.push_str(&rendered);
            } else {
                escape_html(&rendered, out);
            }
            rest = tail;
        }
        out.push_str(rest);
        Ok(())
    }

    let mut out = String::with_capacity(source.len());
    let mut scopes = vec![Scope {
        value: data,
        path: String::new(),
    }];
    let charged = std::cell::Cell::new(0);
    render(
        source,
        &mut scopes,
        html_paths,
        &mut out,
        &charged,
        invocation,
    )?;
    charge_growth(&out, &charged, invocation)?;
    Ok(out)
}

/// Split a block's body from what follows its closing tag.
fn block<'a>(source: &'a str, keyword: &str) -> Result<(&'a str, &'a str), ConnectorFailure> {
    let open = format!("{{{{#{keyword} ");
    let close = format!("{{{{/{keyword}}}}}");
    let mut depth = 1_usize;
    let mut cursor = 0_usize;
    while cursor <= source.len() {
        let next_open = source[cursor..].find(&open).map(|at| cursor + at);
        let next_close = source[cursor..].find(&close).map(|at| cursor + at);
        match (next_open, next_close) {
            (Some(open_at), Some(close_at)) if open_at < close_at => {
                depth += 1;
                cursor = open_at + open.len();
            }
            (_, Some(close_at)) => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&source[..close_at], &source[close_at + close.len()..]));
                }
                cursor = close_at + close.len();
            }
            _ => break,
        }
    }
    Err(contract("an email template has an unclosed block"))
}

/// Resolve a dotted path against the repeat stack, innermost first, and return
/// the value with the full path it was found at.
///
/// The full path is what the escaping decision is keyed on, so a field of a
/// repeated element is judged by the type its declaration gave it and not by
/// where the loop happened to put it.
fn resolve<'a>(
    scopes: &[Scope<'a>],
    path: &str,
) -> Result<(&'a JsonValue, String), ConnectorFailure> {
    if path.is_empty() || path.split('.').any(str::is_empty) {
        return Err(contract("an email template names a declared input path"));
    }
    for scope in scopes.iter().rev() {
        let mut value = scope.value;
        let mut found = true;
        for segment in path.split('.') {
            match value.get(segment) {
                Some(next) => value = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            let full = if scope.path.is_empty() {
                path.to_owned()
            } else {
                format!("{}.{path}", scope.path)
            };
            return Ok((value, full));
        }
    }
    Err(refuse(
        "local_template_input_missing",
        "an email template names a value the activity did not bind",
    ))
}

/// What `{{#if }}` treats as present: a value that would render as nothing is
/// absent, so an empty string does not produce an empty greeting block.
fn is_present(value: &JsonValue) -> bool {
    match value {
        JsonValue::Null => false,
        JsonValue::Bool(flag) => *flag,
        JsonValue::String(text) => !text.is_empty(),
        JsonValue::Array(items) => !items.is_empty(),
        JsonValue::Object(object) => !object.is_empty(),
        JsonValue::Number(_) => true,
    }
}

/// The one conversion from a bound value to template text.
fn scalar(value: &JsonValue) -> Result<String, ConnectorFailure> {
    Ok(match value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Number(number) => number.to_string(),
        JsonValue::Bool(flag) => flag.to_string(),
        JsonValue::Null => String::new(),
        // An object or a list has no rendering, and guessing one would put a
        // debug format in front of a customer.
        _ => {
            return Err(refuse(
                "local_template_input_kind",
                "an email template interpolates a scalar, and the bound value is not one",
            ));
        }
    })
}

/// HTML escaping, applied to everything the declaration did not exempt.
fn escape_html(source: &str, out: &mut String) {
    for character in source.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
}

// ---------------------------------------------------------------------------
// The text alternative
// ---------------------------------------------------------------------------

/// Derive the plain-text part from the rendered HTML.
///
/// Deriving rather than declaring is the point: a second template would be a
/// second thing to keep in step with the first, and the part recipients see
/// when their client refuses HTML would be the one nobody checked.
fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 4);
    let mut rest = html;
    let mut skipping: Option<&'static str> = None;
    while let Some(start) = rest.find('<') {
        if skipping.is_none() {
            push_text(&rest[..start], &mut out);
        }
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else { break };
        let tag = &after[..end];
        let name = tag
            .trim_start_matches('/')
            .split(|character: char| character.is_whitespace() || character == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        match skipping {
            // `<style>` and `<head>` hold CSS and metadata, which is not text.
            Some(open) if tag.starts_with('/') && name == open => skipping = None,
            Some(_) => {}
            None if matches!(name.as_str(), "style" | "head" | "script" | "title")
                && !tag.starts_with('/') =>
            {
                skipping = Some(match name.as_str() {
                    "style" => "style",
                    "head" => "head",
                    "script" => "script",
                    _ => "title",
                });
            }
            None if matches!(
                name.as_str(),
                "br" | "p" | "div" | "tr" | "table" | "td" | "h1" | "h2" | "h3" | "li"
            ) =>
            {
                push_break(&mut out);
            }
            None => {}
        }
        rest = &after[end + 1..];
    }
    if skipping.is_none() {
        push_text(rest, &mut out);
    }
    out.trim().to_owned()
}

fn push_text(source: &str, out: &mut String) {
    let decoded = source
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    for word in decoded.split_whitespace() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push(' ');
        }
        out.push_str(word);
    }
}

fn push_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text derivation is its own small thing, so it is checked on its own:
    /// styles and head metadata are not text, and block elements are breaks.
    #[test]
    fn the_text_alternative_drops_markup_and_keeps_the_words() {
        let html = "<html><head><title>x</title><style>.a{color:red}</style></head>\
                    <body><p>Hello &amp; welcome</p><p>Order&nbsp;A-1</p></body></html>";
        assert_eq!(plain_text(html), "Hello & welcome\nOrder A-1");
    }

    /// Escaping is the default; only a declared path is exempt.
    #[test]
    fn escaping_is_decided_by_the_declared_path() {
        let mut out = String::new();
        escape_html("<script>alert('x')</script>", &mut out);
        assert_eq!(out, "&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;");
    }

    /// A block's body ends at its own closing tag, including when it holds
    /// another block of the same kind.
    #[test]
    fn a_block_body_ends_at_its_own_close() {
        let (body, rest) =
            block("a{{#each x}}b{{/each}}c{{/each}}d", "each").expect("a balanced block splits");
        assert_eq!(body, "a{{#each x}}b{{/each}}c");
        assert_eq!(rest, "d");
        assert!(block("a{{/if}}", "each").is_err());
    }
}
