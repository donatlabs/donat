//! `local.code` — QR and linear barcodes (spec 022 §1).
//!
//! | Operation | Backend | Product |
//! |---|---|---|
//! | `qr.render` | `qrcode` | a stored `.png`, or a bounded inline `svg` |
//! | `barcode.render` | `barcoders` | a stored `.png`, or a bounded inline `svg` |
//!
//! Two properties of this module are security properties rather than features,
//! and each has a test of its own.
//!
//! *A payload is typed.* A code on an invoice is a contract: whoever scans it
//! believes the business put it there. So there is no free-text payload. A
//! payload declared `url` is parsed and its origin compared against the origins
//! the *deployment* declared for that template — the only part of the path the
//! party supplying the value does not control. A `ticket` is an opaque
//! identifier over a closed character set, and a `payment` string must begin
//! with a declared scheme prefix.
//!
//! *Capacity is answered before anything is rendered.* A matrix code declares a
//! fixed version, so an over-capacity payload fails as `validation` carrying the
//! two numbers an operator needs, and is never quietly re-rendered at a larger
//! version — a symbol that grew from version 6 to version 12 is one the printed
//! layout no longer fits.
//!
//! Both output writers are ours. `qrcode` and `barcoders` are asked one question
//! each — which modules are dark — and the PNG and SVG are laid out here, so one
//! input produces one byte string on every platform this engine is built for,
//! which is what `Pure` is admitted on (ADR 044).

use std::io::Cursor;
use std::time::Duration;

use image::{ExtendedColorType, ImageEncoder};
use qrcode::bits::Bits;
use qrcode::types::{Mode, Version};
use qrcode::{Color, EcLevel, QrCode};
use serde_json::{Value as JsonValue, json};

use crate::local::bounds::LocalBounds;
use crate::local::capability::{
    LocalArtifact, LocalCapability, LocalInvocation, LocalOperation, LocalProduct,
};
use crate::local::media::{
    CodeDelivery, CodeErrorCorrection, CodeFormat, CodePayloadType, CodeTemplate, CodeTemplateSpec,
    Symbology, payload_origin,
};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};

/// The capability's declaration, built once by the table in
/// [`crate::local::capabilities`].
pub fn capability() -> LocalCapability {
    LocalCapability::declare("local.code", "1.0.0")
        .operation(qr_operation())
        .operation(barcode_operation())
        .build()
        .expect("the code capability declaration is static and complete")
}

/// The templates the determinism probes render, compiled into this binary.
///
/// ADR 044 makes the double render a property of the binary, so the probes may
/// not depend on what a deployment declared.
pub const PROBE_QR_TEMPLATE: &str = "__probe_qr";
pub const PROBE_BARCODE_TEMPLATE: &str = "__probe_barcode";

pub(crate) fn builtin_code_templates() -> Vec<CodeTemplateSpec> {
    vec![
        CodeTemplateSpec {
            name: PROBE_QR_TEMPLATE.to_owned(),
            symbology: Symbology::Qr,
            payload_type: CodePayloadType::Ticket,
            max_payload_bytes: 32,
            version: Some(2),
            error_correction: Some(CodeErrorCorrection::Low),
            module_size: 2,
            quiet_zone: 4,
            format: CodeFormat::Png,
            ..Default::default()
        },
        CodeTemplateSpec {
            name: PROBE_BARCODE_TEMPLATE.to_owned(),
            symbology: Symbology::Code39,
            payload_type: CodePayloadType::Ticket,
            max_payload_bytes: 32,
            height: Some(24),
            module_size: 1,
            quiet_zone: 4,
            format: CodeFormat::Png,
            ..Default::default()
        },
    ]
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// The bounds both operations run inside.
///
/// The unit is modules: what a code costs to render is the size of its grid,
/// and it is knowable from the declaration and the payload before any pixel is
/// written. 65_536 modules is a 256×256 grid — well past QR version 40's
/// 177×177 — so the ceiling bounds the *raster*, which is the product.
fn code_bounds() -> LocalBounds {
    LocalBounds::declare(
        Duration::from_secs(2),
        8_192,
        2 * 1_024 * 1_024,
        16 * 1_024 * 1_024,
        "modules",
        65_536,
    )
    .expect("the code bounds are static and complete")
}

fn qr_operation() -> LocalOperation {
    LocalOperation::declare("qr.render", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "template": PROBE_QR_TEMPLATE,
                    "payload": "DONAT-PROBE-1",
                    "attachment": "public.probe.code",
                    "claim_role": "app",
                    "file_name": "probe.png"
                }),
                "the output is the declared payload at the declared version, error correction, \
                 module size, and quiet zone; the module grid comes from the encoder and the \
                 file layout is written here, so there is no clock, no random seed, no \
                 environment, and no locale in it",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(code_bounds())
        .units(units)
        .run(run_qr)
        .build()
        .expect("qr.render is deterministic")
}

fn barcode_operation() -> LocalOperation {
    LocalOperation::declare("barcode.render", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "template": PROBE_BARCODE_TEMPLATE,
                    "payload": "DONAT-PROBE-1",
                    "attachment": "public.probe.code",
                    "claim_role": "app",
                    "file_name": "probe.png"
                }),
                "the output is the declared payload's module pattern at the declared module \
                 size, height, and quiet zone; no clock, no random seed, no environment, no \
                 locale",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(code_bounds())
        .units(units)
        .run(run_barcode)
        .build()
        .expect("barcode.render is deterministic")
}

/// The unit count an input implies, before any work.
///
/// It cannot know the grid the encoder will produce, so it charges the payload's
/// own length as a floor; the exact module count is charged in
/// [`emit`] once the symbol exists.
fn units(input: &JsonValue) -> u64 {
    input
        .get("payload")
        .and_then(JsonValue::as_str)
        .map_or(1, |payload| payload.len() as u64)
}

// ---------------------------------------------------------------------------
// QR
// ---------------------------------------------------------------------------

fn run_qr(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let template = select_template(invocation, true)?;
    let payload = payload(invocation, &template)?;
    let version = Version::Normal(i16::from(template.version().ok_or_else(|| {
        refuse(
            "local_code_template_incomplete",
            "a matrix template declares the version its capacity is checked against",
        )
    })?));
    let ec_level = ec_level(template.error_correction());

    // Capacity, before rendering. `Bits` is fixed to the declared version, so
    // an over-capacity payload cannot be absorbed by a larger symbol: it is a
    // refusal carrying the capacity and the payload size (spec 022 §1).
    let capacity_bytes = byte_capacity(version, ec_level);
    let mut bits = Bits::new(version);
    if payload.len() as u64 > capacity_bytes
        || bits.push_optimal_data(payload.as_bytes()).is_err()
        || bits.push_terminator(ec_level).is_err()
    {
        return Err(capacity_exceeded(capacity_bytes, payload.len()));
    }
    invocation.checkpoint()?;

    let code = QrCode::with_bits(bits, ec_level).map_err(|_| {
        // Unreachable through the check above; a symbol that fails to render
        // after capacity was admitted is this binary's own defect.
        ConnectorFailure::new(
            ConnectorErrorClass::Invariant,
            "local_code_render_failed",
            "a code that fits its declared capacity failed to render",
        )
    })?;
    let width = code.width();
    let modules: Vec<bool> = code
        .into_colors()
        .into_iter()
        .map(|color| color == Color::Dark)
        .collect();

    emit(
        invocation,
        &template,
        Grid {
            width,
            height: width,
            modules,
        },
        json!({
            "symbology": template.symbology().as_str(),
            "version": template.version(),
            "modules": width,
        }),
    )
}

/// The largest byte-mode payload one version and error-correction level holds.
///
/// It is computed rather than tabulated so that "the capacity" in a refusal is
/// the same number the encoder enforces: the version's data capacity, less the
/// four-bit mode indicator and the character-count indicator that version uses.
fn byte_capacity(version: Version, ec_level: EcLevel) -> u64 {
    let Ok(data_bits) = Bits::new(version).max_len(ec_level) else {
        return 0;
    };
    let header = version.mode_bits_count() + Mode::Byte.length_bits_count(version);
    ((data_bits.saturating_sub(header)) / 8) as u64
}

fn capacity_exceeded(capacity_bytes: u64, payload_bytes: usize) -> ConnectorFailure {
    // A `ConnectorFailure` message is `&'static str` by design, so the two
    // numbers ride in the correlation ids — the typed, operator-visible channel
    // this failure type has for exactly this.
    refuse(
        "local_code_capacity_exceeded",
        "the payload does not fit the code template's declared version and error correction",
    )
    .with_correlation_ids([
        ("capacity_bytes", capacity_bytes.to_string()),
        ("payload_bytes", payload_bytes.to_string()),
    ])
}

fn ec_level(level: CodeErrorCorrection) -> EcLevel {
    match level {
        CodeErrorCorrection::Low => EcLevel::L,
        CodeErrorCorrection::Medium => EcLevel::M,
        CodeErrorCorrection::Quartile => EcLevel::Q,
        CodeErrorCorrection::High => EcLevel::H,
    }
}

// ---------------------------------------------------------------------------
// Linear symbologies
// ---------------------------------------------------------------------------

fn run_barcode(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let template = select_template(invocation, false)?;
    let payload = payload(invocation, &template)?;

    // Each of these refuses a payload its symbology cannot express, and none of
    // them silently substitutes one that it can.
    let encoded = match template.symbology() {
        Symbology::Code128 => barcoders::sym::code128::Code128::new(format!("\u{0181}{payload}"))
            .map(|code| code.encode()),
        Symbology::Code39 => {
            barcoders::sym::code39::Code39::new(&payload).map(|code| code.encode())
        }
        Symbology::Ean13 => barcoders::sym::ean13::EAN13::new(&payload).map(|code| code.encode()),
        Symbology::Qr => {
            return Err(refuse(
                "local_code_template_wrong_kind",
                "a matrix template renders through another operation",
            ));
        }
    }
    .map_err(|_| {
        refuse(
            "local_code_payload_invalid",
            "the payload is not expressible in the template's declared symbology",
        )
    })?;
    invocation.checkpoint()?;

    let height = template.height().unwrap_or(1).max(1);
    let modules: Vec<bool> = encoded.iter().map(|module| *module == 1).collect();
    let width = modules.len();
    emit(
        invocation,
        &template,
        Grid {
            width,
            height: 1,
            modules,
        },
        json!({
            "symbology": template.symbology().as_str(),
            "modules": width,
            "bar_height": height,
        }),
    )
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Select the template an input names, from the frozen catalog of the
/// deployment. There is no branch in which a template arrives from anywhere
/// else, and no branch in which one is constructed from the request.
fn select_template(
    invocation: &LocalInvocation<'_>,
    matrix: bool,
) -> Result<std::sync::Arc<CodeTemplate>, ConnectorFailure> {
    let name = invocation
        .input()
        .get("template")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            refuse(
                "local_input_contract",
                "a code activity selects its template by name: input requires `template`",
            )
        })?;
    let template = invocation
        .context()
        .media()
        .code(name)
        .ok_or_else(|| {
            refuse(
                "local_code_template_unknown",
                "the selected code template is not declared by this deployment",
            )
        })?
        .clone();
    if template.symbology().is_matrix() != matrix {
        return Err(refuse(
            "local_code_template_wrong_kind",
            "the selected code template renders through another operation",
        ));
    }
    Ok(template)
}

/// The typed payload check of spec 022 §1.
///
/// The order matters: the length ceiling first, because it is the one bound
/// that must hold before any parsing happens at all; then the declared type,
/// which is the only thing here the party supplying the value does not choose.
fn payload(
    invocation: &LocalInvocation<'_>,
    template: &CodeTemplate,
) -> Result<String, ConnectorFailure> {
    let payload = invocation
        .input()
        .get("payload")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            refuse(
                "local_input_contract",
                "a code activity carries its payload as a string `payload`",
            )
        })?;
    if payload.is_empty() {
        return Err(refuse(
            "local_code_payload_invalid",
            "a code carries a payload",
        ));
    }
    if payload.len() > template.max_payload_bytes() as usize {
        return Err(refuse(
            "local_code_payload_too_long",
            "the payload is longer than the code template's declared ceiling",
        ));
    }

    match template.payload_type() {
        CodePayloadType::Url => {
            let origin = payload_origin(payload).ok_or_else(|| {
                refuse(
                    "local_code_payload_invalid",
                    "a `url` payload is an absolute http or https URL with no credentials and \
                     an ASCII host",
                )
            })?;
            if !template.allowed_origins().contains(&origin) {
                return Err(refuse(
                    "local_code_payload_origin_refused",
                    "a `url` payload must point at an origin the code template declares",
                ));
            }
        }
        CodePayloadType::Ticket => {
            // An opaque identifier over a closed character set. Anything that
            // could make a scanner read the value as a destination — a colon, a
            // slash, whitespace, a non-ASCII glyph — is not one.
            if !payload.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'=')
            }) {
                return Err(refuse(
                    "local_code_payload_invalid",
                    "a `ticket` payload is an opaque identifier of ASCII letters, digits, and \
                     `-`, `_`, `.`, `=`",
                ));
            }
        }
        CodePayloadType::Payment => {
            if !template
                .allowed_prefixes()
                .iter()
                .any(|prefix| payload.starts_with(prefix.as_str()))
            {
                return Err(refuse(
                    "local_code_payload_prefix_refused",
                    "a `payment` payload must begin with a scheme prefix the code template \
                     declares",
                ));
            }
            if payload
                .bytes()
                .any(|byte| (byte.is_ascii_control() && byte != b'\n') || !byte.is_ascii())
            {
                return Err(refuse(
                    "local_code_payload_invalid",
                    "a `payment` payload is printable ASCII with newlines",
                ));
            }
        }
    }
    Ok(payload.to_owned())
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// One symbol's module grid: `width * height` booleans, dark where true.
struct Grid {
    width: usize,
    height: usize,
    modules: Vec<bool>,
}

/// Render the grid in the declared format and deliver it the declared way.
fn emit(
    invocation: &LocalInvocation<'_>,
    template: &CodeTemplate,
    grid: Grid,
    mut metadata: JsonValue,
) -> Result<LocalProduct, ConnectorFailure> {
    let scale = template.module_size() as usize;
    let quiet = template.quiet_zone() as usize;
    let bar_height = template.height().unwrap_or(0) as usize;

    // The raster's own size, charged as units and as working memory before it
    // is allocated. A code is small, but "small" is the declaration's claim,
    // not the payload's.
    let columns = grid.width + 2 * quiet;
    let rows = if grid.height == 1 {
        // A linear symbol's bar height is pixels, not modules, so the quiet
        // zone above and below is expressed in module widths as usual.
        bar_height.max(1) + 2 * quiet * scale
    } else {
        (grid.height + 2 * quiet) * scale
    };
    let pixel_width = columns * scale;
    invocation.charge_units((columns * (rows.div_ceil(scale.max(1)))) as u64)?;
    invocation.reserve(pixel_width.saturating_mul(rows))?;
    invocation.checkpoint()?;

    let dark_at = |x: usize, y: usize| -> bool {
        let Some(column) = (x / scale).checked_sub(quiet) else {
            return false;
        };
        if column >= grid.width {
            return false;
        }
        if grid.height == 1 {
            let top = quiet * scale;
            return y >= top && y < top + bar_height.max(1) && grid.modules[column];
        }
        let Some(row) = (y / scale).checked_sub(quiet) else {
            return false;
        };
        if row >= grid.height {
            return false;
        }
        grid.modules[row * grid.width + column]
    };

    let bytes = match template.format() {
        CodeFormat::Png => png(invocation, pixel_width, rows, dark_at)?,
        CodeFormat::Svg => svg(
            invocation,
            &grid,
            scale,
            quiet,
            bar_height,
            pixel_width,
            rows,
        )?
        .into_bytes(),
    };

    if let JsonValue::Object(object) = &mut metadata {
        object.insert("width".to_owned(), json!(pixel_width));
        object.insert("height".to_owned(), json!(rows));
        object.insert("byte_size".to_owned(), json!(bytes.len()));
    }

    match template.delivery() {
        CodeDelivery::Inline => {
            let ceiling = template.max_inline_bytes().unwrap_or(0);
            if bytes.len() as u64 > ceiling {
                return Err(refuse(
                    "local_code_inline_too_large",
                    "the rendered code is larger than the template's declared inline ceiling",
                ));
            }
            let svg = String::from_utf8(bytes).map_err(|_| {
                ConnectorFailure::new(
                    ConnectorErrorClass::Invariant,
                    "local_code_render_failed",
                    "an inline code must be text",
                )
            })?;
            if let JsonValue::Object(object) = &mut metadata {
                object.insert("svg".to_owned(), JsonValue::String(svg));
            }
            Ok(LocalProduct::Value(metadata))
        }
        CodeDelivery::Stored => {
            let input = invocation.input();
            let attachment = input
                .get("attachment")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    refuse(
                        "local_input_contract",
                        "a stored code names the `attachment` its file belongs to",
                    )
                })?;
            let claim_role = input
                .get("claim_role")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    refuse(
                        "local_input_contract",
                        "a stored code names the `claim_role` that will bind its file",
                    )
                })?;
            let media_type = match template.format() {
                CodeFormat::Png => "image/png",
                CodeFormat::Svg => "image/svg+xml",
            };
            let file_name = input
                .get("file_name")
                .and_then(JsonValue::as_str)
                .unwrap_or(match template.format() {
                    CodeFormat::Png => "code.png",
                    CodeFormat::Svg => "code.svg",
                });
            if let JsonValue::Object(object) = &mut metadata {
                object.insert("media_type".to_owned(), json!(media_type));
            }
            Ok(LocalProduct::Artifact {
                artifact: LocalArtifact::new(attachment, claim_role, file_name, media_type, bytes)?
                    .claimed_by_session(
                        input.get("claim_session_key").and_then(JsonValue::as_str),
                    )?,
                metadata,
            })
        }
    }
}

/// A one-bit-per-pixel greyscale PNG, written by us.
fn png(
    invocation: &LocalInvocation<'_>,
    width: usize,
    height: usize,
    dark_at: impl Fn(usize, usize) -> bool,
) -> Result<Vec<u8>, ConnectorFailure> {
    let mut pixels = vec![255_u8; width * height];
    for y in 0..height {
        invocation.checkpoint()?;
        for x in 0..width {
            if dark_at(x, y) {
                pixels[y * width + x] = 0;
            }
        }
    }
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(Cursor::new(&mut bytes))
        .write_image(&pixels, width as u32, height as u32, ExtendedColorType::L8)
        .map_err(|_| {
            ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "local_code_render_failed",
                "a rendered code could not be encoded",
            )
        })?;
    Ok(bytes)
}

/// One `<path>` of module rectangles.
///
/// Geometry and nothing else: no `<script>`, no `<image>`, no `href`, no
/// external reference and no text. An SVG this engine produces is a shape a
/// viewer draws, which is the only reason it is safe to hand one back inline.
fn svg(
    invocation: &LocalInvocation<'_>,
    grid: &Grid,
    scale: usize,
    quiet: usize,
    bar_height: usize,
    width: usize,
    height: usize,
) -> Result<String, ConnectorFailure> {
    use std::fmt::Write;

    let mut path = String::new();
    for row in 0..grid.height {
        invocation.checkpoint()?;
        for column in 0..grid.width {
            if !grid.modules[row * grid.width + column] {
                continue;
            }
            let x = (quiet + column) * scale;
            let (y, module_height) = if grid.height == 1 {
                (quiet * scale, bar_height.max(1))
            } else {
                ((quiet + row) * scale, scale)
            };
            let _ = write!(path, "M{x} {y}h{scale}v{module_height}h-{scale}z");
        }
    }
    let mut out = String::with_capacity(path.len() + 256);
    let _ = write!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" shape-rendering=\"crispEdges\">\
         <rect width=\"{width}\" height=\"{height}\" fill=\"#ffffff\"/>\
         <path fill=\"#000000\" d=\"{path}\"/></svg>"
    );
    Ok(out)
}

/// An input that does not satisfy the operation's contract, or a declaration it
/// does not honour, is a `validation` failure: the same input will fail again.
fn refuse(code: &'static str, message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(ConnectorErrorClass::Validation, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::capability::StopSignal;
    use crate::local::context::LocalContext;
    use crate::local::media::MediaCatalog;

    fn context(specs: Vec<CodeTemplateSpec>) -> LocalContext {
        LocalContext::default()
            .with_media(MediaCatalog::resolve(specs, Vec::new()).expect("the specs resolve"))
    }

    /// The capacity a refusal reports is the capacity the encoder enforces —
    /// not a table this module keeps in parallel with the crate's.
    #[test]
    fn the_reported_capacity_is_the_encoders_own() {
        for (version, level, expected) in [
            (1_u8, EcLevel::L, 17_u64),
            (6, EcLevel::M, 106),
            (10, EcLevel::Q, 151),
            (40, EcLevel::H, 1_273),
        ] {
            let version = Version::Normal(i16::from(version));
            assert_eq!(byte_capacity(version, level), expected);

            // And it is exact: the last admitted byte encodes, the next does not.
            let mut bits = Bits::new(version);
            assert!(
                bits.push_optimal_data(&vec![b'\xff'; expected as usize])
                    .is_ok()
            );
            assert!(bits.push_terminator(level).is_ok());
            let mut bits = Bits::new(version);
            let one_over = bits
                .push_optimal_data(&vec![b'\xff'; expected as usize + 1])
                .and_then(|()| bits.push_terminator(level));
            assert!(one_over.is_err(), "version {version:?} at {level:?}");
        }
    }

    /// A payment payload is checked against declared scheme prefixes, which is
    /// the payment shape of the same rule the URL type states.
    #[test]
    fn a_payment_payload_begins_with_a_declared_scheme() {
        let context = context(vec![CodeTemplateSpec {
            name: "sepa".to_owned(),
            symbology: Symbology::Qr,
            payload_type: CodePayloadType::Payment,
            allowed_prefixes: ["BCD\n".to_owned()].into_iter().collect(),
            max_payload_bytes: 331,
            version: Some(13),
            error_correction: Some(CodeErrorCorrection::Medium),
            module_size: 3,
            quiet_zone: 4,
            format: CodeFormat::Png,
            ..Default::default()
        }]);
        let capability = capability();
        let operation = capability
            .admit_operation("qr.render")
            .expect("qr.render is declared");
        let render = |payload: &str| {
            operation.execute(
                &json!({
                    "template": "sepa",
                    "payload": payload,
                    "attachment": "public.invoice.code",
                    "claim_role": "app",
                }),
                &context,
                None,
                &StopSignal::new(),
            )
        };
        assert!(render("BCD\n002\n1\nSCT\n\nAcme\nDE02120300000000202051\n").is_ok());
        assert_eq!(
            render("bitcoin:bc1qexample")
                .expect_err("an undeclared scheme is refused")
                .code(),
            "local_code_payload_prefix_refused"
        );
    }
}
