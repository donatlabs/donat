//! Document templates are deployment metadata (spec 019 §2): read at load,
//! frozen into a file set nothing else can widen, and pinned into every process
//! that renders with them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use donat_metadata::{
    DocumentTemplateKind, LoadError, Metadata, ProcessStateOperation, load_metadata_dir,
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "donat_metadata_documents_{tag}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

const INVOICE_TYP: &str = "#set text(font: \"Liberation Sans\")\n= Invoice\n";
const TOTALS_TYP: &str = "#let total(order) = order.total\n";

const DOCUMENTS_YAML: &str = "\
templates:
  - name: invoice
    kind: pdf
    source: templates/invoice.typ
    includes: [templates/partials/totals.typ]
    inputs:
      order: order_document!
    bounds: { max_pages: 40, cpu_deadline: 15s, max_output_bytes: 8MiB }
";

const FLOWS_YAML: &str = "\
- name: bill
  kind: process
  version: 1
  source: default
  start_at: render
  states:
    - id: render
      request:
        connector: local.document
        operation: pdf.render
        input:
          template: { literal: invoice }
          attachment: { literal: public.invoice.file }
          claim_role: { literal: app }
          order: { state: fetch, field: order }
        timeout: { schedule_to_start: 10s, start_to_close: 30s }
        retry:
          retry_on: [timeout]
          max_attempts: 1
          initial_interval: 1s
          max_interval: 5s
          jitter: 1s
        next: done
";

const ACTIONS_YAML: &str = "\
custom_types:
  objects:
    - name: order_document
      fields:
        - { name: number, type: 'String!' }
        - { name: note, type: Html }
";

/// A metadata directory carrying one PDF template and one process that renders
/// with it.
fn build(tag: &str) -> PathBuf {
    let dir = tempdir(tag);
    write(&dir, "version.yaml", "version: 3\n");
    write(&dir, "databases/databases.yaml", "[]\n");
    write(&dir, "actions.yaml", ACTIONS_YAML);
    write(&dir, "documents.yaml", DOCUMENTS_YAML);
    write(&dir, "flows.yaml", FLOWS_YAML);
    write(&dir, "templates/invoice.typ", INVOICE_TYP);
    write(&dir, "templates/partials/totals.typ", TOTALS_TYP);
    dir
}

fn pin(metadata: &Metadata) -> String {
    let ProcessStateOperation::Request { request } = &metadata.processes[0].states[0].operation
    else {
        panic!("the fixture's one state is a request");
    };
    request
        .template_pin
        .clone()
        .expect("a document activity carries the pin of the template it selects")
}

fn template_error(result: Result<Metadata, LoadError>) -> String {
    match result {
        Err(LoadError::Templates { message, .. }) => message,
        Err(other) => panic!("expected a template error, got {other}"),
        Ok(_) => panic!("expected a template error, but the metadata loaded"),
    }
}

/// Spec 019 §7 `pdf_template_is_pinned_by_hash`.
///
/// Changing a template file changes the process definition revision. The
/// revision is a hash of the serialized process, so what this proves is that
/// the serialized process changed — and it changed for an edit to an *included*
/// file too, which is the case a naive "hash the entry file" pin would miss.
#[test]
fn pdf_template_is_pinned_by_hash() {
    let dir = build("pin");
    let before = load_metadata_dir(&dir).expect("the fixture loads");
    let pinned = pin(&before);
    assert!(
        pinned.starts_with("invoice@"),
        "the pin names the template and its hash: {pinned}"
    );
    assert_eq!(
        pinned,
        format!("invoice@{}", before.templates[0].content_hash),
        "the pin is the template's own hash, not a second one"
    );

    // Loading the same bytes twice is the same revision: a pin that moved on
    // its own would redeploy every process on every boot.
    let again = load_metadata_dir(&dir).expect("the fixture loads again");
    assert_eq!(pin(&again), pinned);
    assert_eq!(
        serde_json::to_value(&before.processes[0]).unwrap(),
        serde_json::to_value(&again.processes[0]).unwrap()
    );

    // An edit to the entry file.
    write(&dir, "templates/invoice.typ", "= Invoice (revised)\n");
    let edited = load_metadata_dir(&dir).expect("the edited fixture loads");
    assert_ne!(pin(&edited), pinned, "an edited template is a new revision");
    assert_ne!(
        serde_json::to_value(&before.processes[0]).unwrap(),
        serde_json::to_value(&edited.processes[0]).unwrap(),
        "the serialized process — the material the definition fingerprint is \
         taken over — must differ"
    );

    // And an edit to a file the entry merely includes.
    write(&dir, "templates/invoice.typ", INVOICE_TYP);
    write(&dir, "templates/partials/totals.typ", "#let total(o) = 0\n");
    let included = load_metadata_dir(&dir).expect("the fixture loads");
    assert_ne!(
        pin(&included),
        pinned,
        "an included file is part of the template"
    );
}

/// The pin covers the declaration, not only the bytes it points at.
///
/// A template is its file set *and* the declaration that decides how the set is
/// executed: which inputs it takes, what they are typed as — which is what says
/// whether a value is escaped — and the bounds it narrows. Hashing only the
/// files left `inputs: { order: order_document! }` and `inputs: { order: Html! }`
/// with one pin and one process revision, so a deployment could change the
/// escaping contract of every rendered email without changing anything a
/// reviewer of the recorded revision could see. `ingest::content_hash` already
/// hashes its whole declaration; a template's now agrees with it.
#[test]
fn a_template_pin_covers_its_declaration() {
    let dir = build("declaration");
    let before = load_metadata_dir(&dir).expect("the fixture loads");
    let pinned = pin(&before);

    // The same bytes under a declaration that stops escaping the whole input.
    write(
        &dir,
        "documents.yaml",
        &DOCUMENTS_YAML.replace("order: order_document!", "order: Html!"),
    );
    let retyped = load_metadata_dir(&dir).expect("the retyped fixture loads");
    assert_eq!(
        retyped.templates[0].files, before.templates[0].files,
        "nothing on disk moved: this is a declaration change and nothing else"
    );
    assert_eq!(
        retyped.templates[0].html_paths,
        ["order".to_owned()].into_iter().collect(),
        "and it is a change to what the renderer escapes"
    );
    assert_ne!(
        retyped.templates[0].content_hash, before.templates[0].content_hash,
        "a declared input type is part of what the hash pins"
    );
    assert_ne!(
        pin(&retyped),
        pinned,
        "so the process that renders with it is a new revision"
    );

    // A narrowed bound is a different template too.
    write(
        &dir,
        "documents.yaml",
        &DOCUMENTS_YAML.replace("max_pages: 40", "max_pages: 4"),
    );
    let bounded = load_metadata_dir(&dir).expect("the bounded fixture loads");
    assert_ne!(
        bounded.templates[0].content_hash, before.templates[0].content_hash,
        "a declared bound is part of what the hash pins"
    );
    assert_ne!(pin(&bounded), pinned);

    // And restoring the declaration restores the pin: the hash is over what is
    // declared, not over the order a deployment happened to be edited in.
    write(&dir, "documents.yaml", DOCUMENTS_YAML);
    assert_eq!(
        pin(&load_metadata_dir(&dir).expect("the fixture loads")),
        pinned
    );
}

/// The frozen set is exactly the declaration, keyed by paths rooted at the
/// template's own directory — which is what leaves the renderer no filesystem
/// to reach.
#[test]
fn a_template_is_frozen_into_a_file_set_at_load() {
    let metadata = load_metadata_dir(&build("freeze")).expect("the fixture loads");
    let template = &metadata.templates[0];
    assert_eq!(template.kind, DocumentTemplateKind::Pdf);
    assert_eq!(template.entry, "/invoice.typ");
    assert_eq!(
        template.files.keys().cloned().collect::<Vec<_>>(),
        vec!["/invoice.typ".to_owned(), "/partials/totals.typ".to_owned()]
    );
    assert_eq!(template.files["/invoice.typ"], INVOICE_TYP);
    assert_eq!(template.bounds.max_pages, Some(40));
    assert_eq!(template.bounds.cpu_deadline_ms(), Some(15_000));
    assert_eq!(
        template.bounds.max_output_bytes_value(),
        Some(8 * 1_048_576)
    );
    // The escaping contract, resolved from the declared type at load.
    assert_eq!(
        template.html_paths.iter().cloned().collect::<Vec<_>>(),
        vec!["order.note".to_owned()]
    );
}

/// An include outside the template's own directory is refused while the
/// declaration is still readable — not turned into a render-time file error.
#[test]
fn an_include_may_not_escape_the_template_directory() {
    let dir = build("escape");
    write(&dir, "secrets.typ", "#let key = \"s3cr3t\"\n");
    write(
        &dir,
        "documents.yaml",
        "\
templates:
  - name: invoice
    kind: pdf
    source: templates/invoice.typ
    includes: [secrets.typ]
",
    );
    assert!(
        template_error(load_metadata_dir(&dir)).contains("outside the template's directory"),
        "an include outside the set is refused at load"
    );

    let dir = build("traverse");
    write(
        &dir,
        "documents.yaml",
        "\
templates:
  - name: invoice
    kind: pdf
    source: templates/invoice.typ
    includes: ['../../etc/passwd']
",
    );
    assert!(template_error(load_metadata_dir(&dir)).contains("escapes the metadata directory"));

    let dir = build("absolute");
    write(
        &dir,
        "documents.yaml",
        "\
templates:
  - name: invoice
    kind: pdf
    source: /etc/passwd
",
    );
    assert!(template_error(load_metadata_dir(&dir)).contains("escapes the metadata directory"));
}

/// A spreadsheet or calendar layout is YAML on disk and JSON by the time the
/// renderer holds it: the renderer parses its own layout and this workspace
/// keeps exactly one YAML reader.
#[test]
fn a_layout_template_is_normalized_to_json_at_load() {
    let dir = tempdir("layout");
    write(&dir, "version.yaml", "version: 3\n");
    write(&dir, "databases/databases.yaml", "[]\n");
    write(
        &dir,
        "documents.yaml",
        "\
templates:
  - name: orders
    kind: spreadsheet
    source: templates/orders.yaml
",
    );
    write(
        &dir,
        "templates/orders.yaml",
        "\
sheet: Orders
columns:
  - { header: Number, field: number, type: text }
  - { header: Total, field: total, type: decimal }
",
    );
    let metadata = load_metadata_dir(&dir).expect("the layout fixture loads");
    let text = &metadata.templates[0].files["/orders.yaml"];
    let parsed: serde_json::Value = serde_json::from_str(text).expect("the layout is JSON");
    assert_eq!(parsed["sheet"], "Orders");
    assert_eq!(parsed["columns"][1]["type"], "decimal");
}

/// The pin is derived, never declared: an operator who writes one gets an
/// unknown field, because a pin an operator can set is not a pin.
#[test]
fn a_template_pin_cannot_be_written_by_hand() {
    let dir = build("handwritten");
    write(
        &dir,
        "flows.yaml",
        &FLOWS_YAML.replace(
            "        next: done\n",
            "        next: done\n        template_pin: invoice@0000\n",
        ),
    );
    // The loader refuses it by name. `deny_unknown_fields` cannot do this job:
    // the field must deserialize, because the engine reads its own persisted
    // definitions back through these types, so what tells a deployment's YAML
    // apart from the engine's own output is the door the YAML came through.
    let error = load_metadata_dir(&dir).expect_err("a hand-written pin is not metadata");
    assert!(
        error.to_string().contains(
            "state `render` declares `template_pin`, which is derived from the template it selects and cannot be written"
        ),
        "a written pin must stop the load, and say why: {error}"
    );

    // And the derived pin still round-trips: it is serialized, because that is
    // how it reaches the definition fingerprint.
    let loaded = load_metadata_dir(&build("roundtrip")).expect("the fixture loads");
    let rendered = serde_json::to_string(&loaded.processes[0]).expect("a process serializes");
    assert!(
        rendered.contains("\"template_pin\":\"invoice@"),
        "{rendered}"
    );
}

/// A serialized process is not write-only material: `reconcile` persists the
/// compiled definition as JSON and the next boot decodes it back into a
/// [`donat_metadata::Process`] to recompile any revision that still has
/// in-flight instances. A pin that serializes but cannot deserialize turns that
/// boot into a refusal, so the pin round-trips — and the loader, not
/// `deny_unknown_fields`, is what refuses a hand-written one.
#[test]
fn a_derived_pin_survives_the_round_trip_a_persisted_definition_makes() {
    let metadata = load_metadata_dir(&build("persisted_roundtrip")).expect("the fixture loads");
    let expected = pin(&metadata);
    let persisted =
        serde_json::to_value(&metadata.processes[0]).expect("a compiled definition serializes");

    let read_back: donat_metadata::Process =
        serde_json::from_value(persisted.clone()).expect("a persisted definition reads back");

    let ProcessStateOperation::Request { request } = &read_back.states[0].operation else {
        panic!("the persisted definition's one state is a request");
    };
    assert_eq!(
        request.template_pin.as_deref(),
        Some(expected.as_str()),
        "a pin dropped on the way back is a different definition, and a different revision"
    );
    assert_eq!(
        serde_json::to_value(&read_back).expect("the decoded definition serializes"),
        persisted,
        "the persisted definition and the one the engine boots from must be the same bytes"
    );
}

/// A process bound to a template that does not exist, or bound wrongly, does
/// not load at all: `validate` is where a template declaration meets the data
/// that fills it.
#[test]
fn a_process_that_does_not_typecheck_against_its_template_does_not_load() {
    let dir = build("typecheck");
    write(
        &dir,
        "flows.yaml",
        &FLOWS_YAML.replace(
            "          order: { state: fetch, field: order }\n",
            "          customer: { state: fetch, field: customer }\n",
        ),
    );
    let message = template_error(load_metadata_dir(&dir));
    assert!(message.contains("declares input `order`, which this activity does not bind"));
    assert!(message.contains("declares no input `customer`"));
}
