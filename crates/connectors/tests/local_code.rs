//! The code half of spec 022 §3, for `local.code`.
//!
//! Each test is named after the row of the spec's table it discharges. The
//! fixtures are authored here — a QR payload is a string and a barcode payload
//! is a string, so there is nothing to download and nothing to trust.

use donat_connectors::local::media::{
    CodeDelivery, CodeErrorCorrection, CodeFormat, CodePayloadType, CodeTemplateSpec, MediaCatalog,
    Symbology,
};
use donat_connectors::local::{LocalContext, LocalOperation, LocalProduct, StopSignal, capability};
use donat_connectors::sdk::errors::ConnectorErrorClass;
use serde_json::{Value as JsonValue, json};

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

fn payment_qr() -> CodeTemplateSpec {
    CodeTemplateSpec {
        name: "invoice_payment".to_owned(),
        symbology: Symbology::Qr,
        payload_type: CodePayloadType::Url,
        allowed_origins: ["https://pay.example.com".to_owned()].into_iter().collect(),
        max_payload_bytes: 256,
        version: Some(6),
        error_correction: Some(CodeErrorCorrection::Medium),
        module_size: 4,
        quiet_zone: 4,
        format: CodeFormat::Png,
        ..Default::default()
    }
}

fn ticket_qr() -> CodeTemplateSpec {
    CodeTemplateSpec {
        name: "ticket".to_owned(),
        symbology: Symbology::Qr,
        payload_type: CodePayloadType::Ticket,
        max_payload_bytes: 64,
        version: Some(2),
        error_correction: Some(CodeErrorCorrection::Low),
        module_size: 3,
        quiet_zone: 4,
        format: CodeFormat::Svg,
        delivery: CodeDelivery::Inline,
        max_inline_bytes: Some(16 * 1_024),
        ..Default::default()
    }
}

fn shipping_barcode() -> CodeTemplateSpec {
    CodeTemplateSpec {
        name: "shipping".to_owned(),
        symbology: Symbology::Code128,
        payload_type: CodePayloadType::Ticket,
        max_payload_bytes: 32,
        height: Some(48),
        module_size: 2,
        quiet_zone: 10,
        format: CodeFormat::Png,
        ..Default::default()
    }
}

fn context(specs: Vec<CodeTemplateSpec>) -> LocalContext {
    LocalContext::default().with_media(
        MediaCatalog::resolve(specs, Vec::new()).expect("the test declarations resolve"),
    )
}

fn operation(id: &str) -> &'static LocalOperation {
    capability("local.code")
        .expect("local.code is compiled into this binary")
        .admit_operation(id)
        .expect("the operation is declared and executable")
}

fn render(
    id: &str,
    context: &LocalContext,
    input: JsonValue,
) -> Result<LocalProduct, donat_connectors::sdk::ConnectorFailure> {
    operation(id).execute(&input, context, None, &StopSignal::new())
}

fn stored(template: &str, payload: &str) -> JsonValue {
    json!({
        "template": template,
        "payload": payload,
        "attachment": "public.invoice.code",
        "claim_role": "app",
        "file_name": "code.png",
    })
}

// ---------------------------------------------------------------------------
// Spec 022 §3
// ---------------------------------------------------------------------------

/// `qr_payload_is_typed`: a payload of declared type `url` outside the
/// template's allowed origins is rejected.
///
/// The declaration is the only part of this path an attacker supplying the
/// payload does not control, which is why the check is against it and never
/// against the payload's own shape.
#[test]
fn qr_payload_is_typed() {
    let context = context(vec![payment_qr(), ticket_qr()]);

    // The payload the deployment declared for: rendered.
    assert!(
        render(
            "qr.render",
            &context,
            stored("invoice_payment", "https://pay.example.com/i/AB-1")
        )
        .is_ok()
    );

    // Every way an attacker-chosen destination is spelled, refused — including
    // the ones that read as the declared origin and resolve somewhere else.
    for payload in [
        "https://pay.example.com.evil.test/i/AB-1",
        "https://evil.test/i/AB-1",
        "http://pay.example.com/i/AB-1",
        "https://pay.example.com@evil.test/",
        "https://pay.example.com:8443/i/AB-1",
        "javascript:alert(1)",
        "https://pay.example.com\u{0000}.evil.test/",
        "not a url at all",
    ] {
        let failure = render("qr.render", &context, stored("invoice_payment", payload))
            .expect_err("a payload outside the declared origins is not rendered");
        assert_eq!(
            failure.class(),
            ConnectorErrorClass::Validation,
            "{payload}"
        );
        assert!(
            matches!(
                failure.code(),
                "local_code_payload_origin_refused" | "local_code_payload_invalid"
            ),
            "{payload} was refused as {}",
            failure.code()
        );
    }

    // A ticket is an opaque identifier, and a URL is not one of them: the
    // declared type is what decides, not what the value looks like.
    assert!(render("qr.render", &context, ticket_input("AB-1-2026")).is_ok());
    for payload in [
        "https://pay.example.com/i/AB-1",
        "AB 1",
        "AB\n1",
        "AB\u{00e9}1",
    ] {
        let failure = render("qr.render", &context, ticket_input(payload))
            .expect_err("a ticket payload is a bounded opaque identifier");
        assert_eq!(
            failure.class(),
            ConnectorErrorClass::Validation,
            "{payload}"
        );
    }

    // And a template nobody declared renders nothing at all.
    let failure = render(
        "qr.render",
        &context,
        stored("absent", "https://pay.example.com/"),
    )
    .expect_err("an undeclared template is not a template");
    assert_eq!(failure.code(), "local_code_template_unknown");
}

fn ticket_input(payload: &str) -> JsonValue {
    json!({ "template": "ticket", "payload": payload })
}

/// `qr_capacity_is_checked_before_render`: an over-capacity payload fails as
/// `validation`, and no silent version upgrade occurs.
///
/// The declared version *is* the contract: an invoice whose QR silently grew
/// from version 6 to version 12 is a symbol the printed layout no longer fits.
#[test]
fn qr_capacity_is_checked_before_render() {
    let context = context(vec![payment_qr()]);

    // A payload that fits the declared version renders at exactly that version.
    let product = render(
        "qr.render",
        &context,
        stored("invoice_payment", "https://pay.example.com/i/AB-1"),
    )
    .expect("a payload inside the declared capacity renders");
    let LocalProduct::Artifact { metadata, .. } = &product else {
        panic!("a stored code produces an artifact");
    };
    assert_eq!(metadata["version"], json!(6));
    assert_eq!(
        metadata["modules"],
        json!(41),
        "version 6 is 41 modules wide"
    );

    // One that does not fit fails, rather than being rendered at a larger
    // version. The refusal carries the two numbers an operator needs.
    let long = format!("https://pay.example.com/i/{}", "A".repeat(180));
    let failure = render("qr.render", &context, stored("invoice_payment", &long))
        .expect_err("a payload over the declared capacity is refused");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "local_code_capacity_exceeded");
    let ids = failure.correlation_ids();
    assert_eq!(
        ids.get("capacity_bytes").map(String::as_str),
        Some("106"),
        "version 6 at medium error correction holds 106 bytes: {ids:?}"
    );
    assert_eq!(ids.get("payload_bytes").map(String::as_str), Some("206"));

    // The template's own payload ceiling is reached first when it is smaller,
    // and it is refused before any encoding happens at all.
    let failure = render(
        "qr.render",
        &context,
        stored(
            "invoice_payment",
            &format!("https://pay.example.com/i/{}", "A".repeat(300)),
        ),
    )
    .expect_err("a payload over the declared length ceiling is refused");
    assert_eq!(failure.code(), "local_code_payload_too_long");
}

/// `code_render_is_deterministic`: two renders of one input are byte-identical
/// in both formats.
///
/// Registration already proves this for each operation's declared probe; this
/// widens it to both output formats, both symbologies, and a payload the probe
/// does not use — and asserts the bytes are a symbol rather than merely equal.
#[test]
fn code_render_is_deterministic() {
    let context = context(vec![payment_qr(), ticket_qr(), shipping_barcode()]);

    // PNG, matrix.
    let first = render(
        "qr.render",
        &context,
        stored("invoice_payment", "https://pay.example.com/i/AB-1"),
    )
    .expect("the declared payload renders");
    let second = render(
        "qr.render",
        &context,
        stored("invoice_payment", "https://pay.example.com/i/AB-1"),
    )
    .expect("the declared payload renders again");
    assert_eq!(first, second, "one input, one PNG");
    let LocalProduct::Artifact { artifact, metadata } = &first else {
        panic!("a stored code produces an artifact");
    };
    assert_eq!(artifact.media_type(), "image/png");
    assert_eq!(&artifact.bytes()[..8], b"\x89PNG\r\n\x1a\n");
    // 41 modules plus a 4-module quiet zone on each side, at 4 pixels each.
    assert_eq!(metadata["width"], json!((41 + 8) * 4));
    assert_eq!(metadata["height"], json!((41 + 8) * 4));

    // SVG, inline.
    let first = render("qr.render", &context, ticket_input("AB-1-2026"))
        .expect("the declared payload renders");
    let second = render("qr.render", &context, ticket_input("AB-1-2026"))
        .expect("the declared payload renders again");
    assert_eq!(first, second, "one input, one SVG");
    let LocalProduct::Value(value) = &first else {
        panic!("an inline code produces a value, not bytes");
    };
    let svg = value["svg"]
        .as_str()
        .expect("an inline code carries its svg");
    assert!(
        svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
        "{svg}"
    );
    assert!(svg.ends_with("</svg>"), "{svg}");
    assert!(
        !svg.contains("<script") && !svg.contains("href"),
        "a rendered code is geometry and nothing else: {svg}"
    );

    // PNG, linear.
    let first = render(
        "barcode.render",
        &context,
        json!({
            "template": "shipping",
            "payload": "SHIP-000123",
            "attachment": "public.shipment.label",
            "claim_role": "app",
            "file_name": "label.png",
        }),
    )
    .expect("the declared payload renders");
    let second = render(
        "barcode.render",
        &context,
        json!({
            "template": "shipping",
            "payload": "SHIP-000123",
            "attachment": "public.shipment.label",
            "claim_role": "app",
            "file_name": "label.png",
        }),
    )
    .expect("the declared payload renders again");
    assert_eq!(first, second, "one input, one barcode");
    let LocalProduct::Artifact { artifact, metadata } = &first else {
        panic!("a stored code produces an artifact");
    };
    assert_eq!(artifact.media_type(), "image/png");
    assert_eq!(metadata["bar_height"], json!(48));
    // The bars, plus a 10-module quiet zone above and below at 2 pixels each.
    assert_eq!(metadata["height"], json!(48 + 2 * 10 * 2));
    assert_eq!(metadata["symbology"], json!("code128"));
}

/// A symbology renders through its own operation and no other, and a code that
/// would come back inline is bounded by the ceiling its declaration set.
#[test]
fn a_code_renders_through_its_own_operation_and_inside_its_declared_delivery() {
    let mut narrow = ticket_qr();
    narrow.max_inline_bytes = Some(64);
    let context = context(vec![payment_qr(), narrow, shipping_barcode()]);

    let failure = render(
        "barcode.render",
        &context,
        stored("invoice_payment", "https://pay.example.com/i/AB-1"),
    )
    .expect_err("a matrix template does not render through the linear operation");
    assert_eq!(failure.code(), "local_code_template_wrong_kind");

    let failure = render("qr.render", &context, ticket_input("AB-1-2026"))
        .expect_err("an inline code over its declared ceiling is refused");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "local_code_inline_too_large");
}
