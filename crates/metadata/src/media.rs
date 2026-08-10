//! Media derivatives as deployment metadata (spec 022).
//!
//! `local.code` renders a symbol and `local.image` re-encodes an uploaded
//! picture. Neither takes its shape from the request. What a code carries, what
//! an origin it may point at, how large a thumbnail is, and which media types a
//! decoder is ever offered are all declared here, in `media.yaml`, and a running
//! process chooses only *which* declaration applies and what value goes in it.
//!
//! Two of those are security decisions rather than configuration.
//!
//! *A QR payload is typed.* A code printed on an invoice is a contract: whoever
//! scans it believes the business put it there. A payload declared `url` is
//! checked against the origins this deployment declared for that template, so a
//! value computed from customer data cannot become a link to somewhere else.
//! There is no free-text payload type, because a free-text payload is a URL
//! whenever an attacker writes one.
//!
//! *An image target is an allowlist, not a hint.* A decoder is the one place in
//! this engine that is handed bytes a stranger uploaded, so the media types it
//! will ever be offered, the pixel count it will ever be asked to hold, and
//! whether a multi-frame source is admitted at all are declared before anything
//! runs. `image/svg+xml` is refused by name here: it is a document format with
//! external references and scripting, and no declaration may put one in front of
//! a raster decoder.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::documents::parse_byte_size;
use crate::types::{Metadata, Process, ProcessForEachState, ProcessStateOperation, ProcessValue};

/// The connector name every code operation is reached through.
pub const CODE_CAPABILITY: &str = "local.code";

/// The connector name every image operation is reached through.
pub const IMAGE_CAPABILITY: &str = "local.image";

/// The media types a compiled decoder exists for.
///
/// It is the link-level allowlist written down: `crates/connectors` builds
/// `image` with exactly these four formats, so a fifth cannot be decoded however
/// a declaration spells it. Keeping the list here lets `validate` refuse the
/// declaration instead of leaving the refusal to the first upload.
pub const DECODABLE_MEDIA_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// The media type that is never decoded, whatever a deployment declares.
pub const SVG_MEDIA_TYPE: &str = "image/svg+xml";

/// Keys of a `local.code` or `local.image` activity input that belong to the
/// capability rather than to the payload.
pub const RESERVED_MEDIA_INPUT_KEYS: &[&str] = &[
    "template",
    "target",
    "attachment",
    "claim_role",
    "file_name",
    "payload",
    "source",
];

/// Everything `media.yaml` declares.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MediaMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<CodeTemplate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageTarget>,
}

impl MediaMetadata {
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty() && self.images.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Codes
// ---------------------------------------------------------------------------

/// The symbologies compiled into this binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Symbology {
    Qr,
    Code128,
    Code39,
    Ean13,
}

impl Symbology {
    /// The operation of `local.code` that renders this symbology.
    pub const fn operation(self) -> &'static str {
        match self {
            Self::Qr => "qr.render",
            Self::Code128 | Self::Code39 | Self::Ean13 => "barcode.render",
        }
    }

    /// Whether the symbol is a two-dimensional matrix. Only a matrix has an
    /// error-correction level and a version; only a linear symbol has a height.
    pub const fn is_matrix(self) -> bool {
        matches!(self, Self::Qr)
    }
}

/// What a code is allowed to carry. There is no `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodePayloadType {
    /// An absolute `http`/`https` URL, checked against the declared origins.
    Url,
    /// An opaque identifier — a ticket, an order, a shipment.
    Ticket,
    /// A structured payment string, checked against the declared scheme
    /// prefixes (`BCD` for EPC069-12, `SPC` for a Swiss QR bill, `bitcoin:`).
    Payment,
}

/// QR error correction, in the ISO spelling this deployment writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeErrorCorrection {
    Low,
    Medium,
    Quartile,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeFormat {
    Png,
    Svg,
}

/// Where a rendered code goes.
///
/// `stored` is a file like any other, claimed by the process's own role. `inline`
/// returns the SVG source in the activity result, and is admitted only for SVG
/// and only under a declared ceiling — a result is a journal entry, so an
/// unbounded string in one is a retained megabyte per attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeDelivery {
    #[default]
    Stored,
    Inline,
}

/// What a payload of the declared type must satisfy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodePayloadSpec {
    #[serde(rename = "type")]
    pub type_: CodePayloadType,
    /// For `url`: the origins (`scheme://host[:port]`) a payload may point at.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_origins: Vec<String>,
    /// For `payment`: the scheme prefixes a payload may begin with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_prefixes: Vec<String>,
    /// The longest payload this template admits, in bytes.
    pub max_length: u32,
}

/// One declared code template.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodeTemplate {
    pub name: String,
    pub symbology: Symbology,
    pub payload: CodePayloadSpec,
    /// Matrix symbologies only: the fixed version, so capacity is a declared
    /// number rather than something a renderer raises to fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_correction: Option<CodeErrorCorrection>,
    /// Linear symbologies only: the bar height in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Pixels (or SVG user units) per module.
    pub module_size: u32,
    /// The light margin, in modules.
    pub quiet_zone: u32,
    pub format: CodeFormat,
    #[serde(default, skip_serializing_if = "is_default_delivery")]
    pub delivery: CodeDelivery,
    /// The ceiling on an inline SVG, as a byte size (`8KiB`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_inline_bytes: Option<String>,
}

fn is_default_delivery(delivery: &CodeDelivery) -> bool {
    *delivery == CodeDelivery::default()
}

impl CodeTemplate {
    pub fn max_inline_bytes_value(&self) -> Option<u64> {
        self.max_inline_bytes.as_deref().and_then(parse_byte_size)
    }
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageTargetKind {
    Thumbnail,
    Normalize,
}

impl ImageTargetKind {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::Thumbnail => "image.thumbnail",
            Self::Normalize => "image.normalize",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFit {
    /// The whole image fits inside the box; the result may be smaller than it.
    #[default]
    Contain,
    /// The box is filled and the overflow is cropped, centred.
    Cover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageOutputFormat {
    Png,
    Jpeg,
}

impl ImageOutputFormat {
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

/// What a multi-frame source is allowed to become.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageAnimation {
    /// An animated source is refused. The default, because a deployment that
    /// never thought about animation gets the answer that decodes least.
    #[default]
    Reject,
    /// The first frame is decoded and the rest are not.
    FirstFrame,
}

/// The hard ceiling on a declared pixel count, whatever a deployment writes.
///
/// 40 megapixels is more than any thumbnail source needs and still fits a
/// bounded RGBA buffer; a declaration above it is a mistake rather than a
/// requirement.
pub const MAX_DECLARABLE_PIXELS: u64 = 40_000_000;

/// One declared image target.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageTarget {
    pub name: String,
    pub kind: ImageTargetKind,
    /// The media types this target's decoder may ever be offered.
    pub accept: Vec<String>,
    /// The largest source, in bytes, before it is opened.
    pub max_source_bytes: String,
    /// The largest source, in pixels, read from the header before a pixel
    /// buffer exists.
    pub max_pixels: u64,
    pub max_width: u32,
    pub max_height: u32,
    #[serde(default, skip_serializing_if = "is_default_fit")]
    pub fit: ImageFit,
    pub format: ImageOutputFormat,
    /// JPEG only: the encoder quality, 1 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
    #[serde(default, skip_serializing_if = "is_default_animation")]
    pub animation: ImageAnimation,
}

fn is_default_fit(fit: &ImageFit) -> bool {
    *fit == ImageFit::default()
}

fn is_default_animation(animation: &ImageAnimation) -> bool {
    *animation == ImageAnimation::default()
}

impl ImageTarget {
    pub fn max_source_bytes_value(&self) -> Option<u64> {
        parse_byte_size(&self.max_source_bytes)
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// One refusal, naming the metadata path that earned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDeclarationError {
    pub path: String,
    pub message: String,
}

impl MediaDeclarationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MediaDeclarationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

/// Every media rule, applied to one deployment's metadata.
pub fn validate_media_declarations(metadata: &Metadata) -> Vec<MediaDeclarationError> {
    let mut errors = Vec::new();
    let mut names = BTreeSet::new();
    for (index, code) in metadata.media.codes.iter().enumerate() {
        let path = format!("media.codes[{index}]");
        if !names.insert(code.name.as_str()) {
            errors.push(MediaDeclarationError::new(
                format!("{path}.name"),
                format!("code template `{}` is declared twice", code.name),
            ));
        }
        validate_code(code, &path, &mut errors);
    }
    let mut targets = BTreeSet::new();
    for (index, image) in metadata.media.images.iter().enumerate() {
        let path = format!("media.images[{index}]");
        if !targets.insert(image.name.as_str()) {
            errors.push(MediaDeclarationError::new(
                format!("{path}.name"),
                format!("image target `{}` is declared twice", image.name),
            ));
        }
        validate_image(image, &path, &mut errors);
    }
    for process in &metadata.processes {
        validate_process(process, metadata, &mut errors);
    }
    errors
}

fn validate_name(name: &str, path: &str, what: &str, errors: &mut Vec<MediaDeclarationError>) {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        errors.push(MediaDeclarationError::new(
            format!("{path}.name"),
            format!("a {what} name is alphanumeric with `_` or `-`"),
        ));
    }
}

fn validate_code(code: &CodeTemplate, path: &str, errors: &mut Vec<MediaDeclarationError>) {
    validate_name(&code.name, path, "code template", errors);

    // A matrix symbology has a version and an error-correction level; a linear
    // one has a height. Declaring the other set is declaring something the
    // renderer would have to ignore (ADR 034).
    if code.symbology.is_matrix() {
        if code.version.is_none() || code.error_correction.is_none() {
            errors.push(MediaDeclarationError::new(
                format!("{path}.version"),
                "a matrix code declares its fixed `version` and `error_correction`, because \
                 capacity is checked against them rather than raised to fit",
            ));
        }
        if let Some(version) = code.version
            && !(1..=40).contains(&version)
        {
            errors.push(MediaDeclarationError::new(
                format!("{path}.version"),
                format!("`{version}` is not a QR version (1 to 40)"),
            ));
        }
        if code.height.is_some() {
            errors.push(MediaDeclarationError::new(
                format!("{path}.height"),
                "a matrix code is square: its height is its module count",
            ));
        }
    } else {
        if code.version.is_some() || code.error_correction.is_some() {
            errors.push(MediaDeclarationError::new(
                format!("{path}.error_correction"),
                "a linear symbology has no version and no error-correction level",
            ));
        }
        match code.height {
            Some(height) if (1..=1_024).contains(&height) => {}
            _ => errors.push(MediaDeclarationError::new(
                format!("{path}.height"),
                "a linear symbology declares a bar `height` of 1 to 1024 pixels",
            )),
        }
    }

    if !(1..=32).contains(&code.module_size) {
        errors.push(MediaDeclarationError::new(
            format!("{path}.module_size"),
            "a module is 1 to 32 pixels across",
        ));
    }
    if code.quiet_zone > 64 {
        errors.push(MediaDeclarationError::new(
            format!("{path}.quiet_zone"),
            "a quiet zone is at most 64 modules",
        ));
    }
    if code.payload.max_length == 0 || code.payload.max_length > 4_096 {
        errors.push(MediaDeclarationError::new(
            format!("{path}.payload.max_length"),
            "a payload ceiling is 1 to 4096 bytes",
        ));
    }

    match code.payload.type_ {
        CodePayloadType::Url => {
            if code.payload.allowed_origins.is_empty() {
                errors.push(MediaDeclarationError::new(
                    format!("{path}.payload.allowed_origins"),
                    "a `url` payload declares the origins it may point at; a code carrying an \
                     attacker-chosen URL on a document a business signs is a phishing vector",
                ));
            }
            for (index, origin) in code.payload.allowed_origins.iter().enumerate() {
                if parse_origin(origin).as_deref() != Some(origin.as_str()) {
                    errors.push(MediaDeclarationError::new(
                        format!("{path}.payload.allowed_origins[{index}]"),
                        format!(
                            "`{origin}` is not an origin: an origin is `scheme://host[:port]` \
                             with no path, no credentials, and an `http` or `https` scheme"
                        ),
                    ));
                }
            }
            if !code.payload.allowed_prefixes.is_empty() {
                errors.push(MediaDeclarationError::new(
                    format!("{path}.payload.allowed_prefixes"),
                    "a `url` payload is checked against its origins, not against a prefix",
                ));
            }
        }
        CodePayloadType::Payment => {
            if code.payload.allowed_prefixes.is_empty() {
                errors.push(MediaDeclarationError::new(
                    format!("{path}.payload.allowed_prefixes"),
                    "a `payment` payload declares the scheme prefixes it may begin with \
                     (for example `BCD` or `bitcoin:`)",
                ));
            }
            if !code.payload.allowed_origins.is_empty() {
                errors.push(MediaDeclarationError::new(
                    format!("{path}.payload.allowed_origins"),
                    "only a `url` payload has origins",
                ));
            }
        }
        CodePayloadType::Ticket => {
            if !code.payload.allowed_origins.is_empty() || !code.payload.allowed_prefixes.is_empty()
            {
                errors.push(MediaDeclarationError::new(
                    format!("{path}.payload"),
                    "a `ticket` payload is an opaque identifier: it has neither origins nor \
                     scheme prefixes",
                ));
            }
        }
    }

    match code.delivery {
        CodeDelivery::Inline => {
            if code.format != CodeFormat::Svg {
                errors.push(MediaDeclarationError::new(
                    format!("{path}.delivery"),
                    "only an `svg` code is returned inline; a raster goes to the attachment store",
                ));
            }
            match code.max_inline_bytes_value() {
                Some(bytes) if bytes <= 64 * 1_024 => {}
                _ => errors.push(MediaDeclarationError::new(
                    format!("{path}.max_inline_bytes"),
                    "an inline code declares a `max_inline_bytes` of at most 64KiB, because an \
                     activity result is retained in the process journal",
                )),
            }
        }
        CodeDelivery::Stored => {
            if code.max_inline_bytes.is_some() {
                errors.push(MediaDeclarationError::new(
                    format!("{path}.max_inline_bytes"),
                    "a stored code has no inline ceiling to declare",
                ));
            }
        }
    }
}

fn validate_image(image: &ImageTarget, path: &str, errors: &mut Vec<MediaDeclarationError>) {
    validate_name(&image.name, path, "image target", errors);

    if image.accept.is_empty() {
        errors.push(MediaDeclarationError::new(
            format!("{path}.accept"),
            "an image target declares the media types its decoder may be offered",
        ));
    }
    for (index, media_type) in image.accept.iter().enumerate() {
        if media_type == SVG_MEDIA_TYPE {
            errors.push(MediaDeclarationError::new(
                format!("{path}.accept[{index}]"),
                "`image/svg+xml` is a document format with external references and scripting; \
                 it does not belong in a decoder path and is never accepted",
            ));
        } else if !DECODABLE_MEDIA_TYPES.contains(&media_type.as_str()) {
            errors.push(MediaDeclarationError::new(
                format!("{path}.accept[{index}]"),
                format!(
                    "`{media_type}` has no decoder compiled into this binary; the decodable set \
                     is {}",
                    DECODABLE_MEDIA_TYPES.join(", ")
                ),
            ));
        }
    }
    match image.max_source_bytes_value() {
        Some(_) => {}
        None => errors.push(MediaDeclarationError::new(
            format!("{path}.max_source_bytes"),
            format!(
                "`{}` is not a byte size (for example `8MiB`)",
                image.max_source_bytes
            ),
        )),
    }
    if image.max_pixels == 0 || image.max_pixels > MAX_DECLARABLE_PIXELS {
        errors.push(MediaDeclarationError::new(
            format!("{path}.max_pixels"),
            format!("a pixel ceiling is 1 to {MAX_DECLARABLE_PIXELS}"),
        ));
    }
    if image.max_width == 0 || image.max_height == 0 {
        errors.push(MediaDeclarationError::new(
            format!("{path}.max_width"),
            "a target box admits no image in either dimension at zero",
        ));
    }
    if u64::from(image.max_width) * u64::from(image.max_height) > image.max_pixels {
        errors.push(MediaDeclarationError::new(
            format!("{path}.max_pixels"),
            "the output box is larger than the pixel ceiling the source is admitted under",
        ));
    }
    match (image.format, image.quality) {
        (ImageOutputFormat::Jpeg, Some(quality)) if !(1..=100).contains(&quality) => {
            errors.push(MediaDeclarationError::new(
                format!("{path}.quality"),
                "a JPEG quality is 1 to 100",
            ));
        }
        (ImageOutputFormat::Png, Some(_)) => errors.push(MediaDeclarationError::new(
            format!("{path}.quality"),
            "PNG is lossless: a quality setting is a declaration the encoder would ignore",
        )),
        _ => {}
    }
}

fn validate_process(
    process: &Process,
    metadata: &Metadata,
    errors: &mut Vec<MediaDeclarationError>,
) {
    for (index, state) in process.states.iter().enumerate() {
        let path = format!("processes.{}.states[{index}]", process.name);
        match &state.operation {
            ProcessStateOperation::Request { request } => validate_request(
                &request.connector,
                &request.operation,
                &request.input,
                &format!("{path}.request"),
                metadata,
                errors,
            ),
            ProcessStateOperation::ForEach { for_each } => {
                if let ProcessForEachState::Request { request, .. } = for_each.as_ref() {
                    validate_request(
                        &request.connector,
                        &request.operation,
                        &request.input,
                        &format!("{path}.for_each.request"),
                        metadata,
                        errors,
                    );
                }
            }
            _ => {}
        }
    }
}

fn validate_request(
    connector: &str,
    operation: &str,
    input: &std::collections::BTreeMap<String, ProcessValue>,
    path: &str,
    metadata: &Metadata,
    errors: &mut Vec<MediaDeclarationError>,
) {
    let (field, declared_operation, kind) = match connector {
        CODE_CAPABILITY => {
            let Some(name) = literal_name(input, "template", path, "code template", errors) else {
                return;
            };
            let Some(code) = metadata.media.codes.iter().find(|code| code.name == name) else {
                errors.push(MediaDeclarationError::new(
                    format!("{path}.input.template"),
                    format!("code template `{name}` is not declared by this deployment"),
                ));
                return;
            };
            ("template", code.symbology.operation(), format!("`{name}`"))
        }
        IMAGE_CAPABILITY => {
            let Some(name) = literal_name(input, "target", path, "image target", errors) else {
                return;
            };
            let Some(image) = metadata
                .media
                .images
                .iter()
                .find(|image| image.name == name)
            else {
                errors.push(MediaDeclarationError::new(
                    format!("{path}.input.target"),
                    format!("image target `{name}` is not declared by this deployment"),
                ));
                return;
            };
            ("target", image.kind.operation(), format!("`{name}`"))
        }
        _ => return,
    };
    if declared_operation != operation {
        errors.push(MediaDeclarationError::new(
            format!("{path}.operation"),
            format!("{field} {kind} renders through `{declared_operation}`, not `{operation}`"),
        ));
    }
}

/// The declaration a request selects is a literal name, never a computed value:
/// a run that could pick its own template would be supplying one.
fn literal_name(
    input: &std::collections::BTreeMap<String, ProcessValue>,
    field: &'static str,
    path: &str,
    what: &str,
    errors: &mut Vec<MediaDeclarationError>,
) -> Option<String> {
    match input.get(field) {
        Some(ProcessValue::Literal {
            literal: JsonValue::String(name),
        }) => Some(name.clone()),
        Some(_) => {
            errors.push(MediaDeclarationError::new(
                format!("{path}.input.{field}"),
                format!(
                    "a {what} is selected by a literal name from this deployment's declarations, \
                     not by a computed value"
                ),
            ));
            None
        }
        None => {
            errors.push(MediaDeclarationError::new(
                format!("{path}.input.{field}"),
                format!("this activity selects its {what} with `{field}`"),
            ));
            None
        }
    }
}

/// Parse `scheme://host[:port]` and return it in its canonical spelling, or
/// `None` when the text is not an origin.
///
/// This is deliberately a tiny hand-written parser rather than a URL crate: the
/// only shapes it must accept are the ones a deployment writes and the ones a
/// payload carries, and the refusals — credentials, a path, a non-ASCII host —
/// are the point rather than an edge case. A parser that normalizes generously
/// is one an attacker writes an input for.
pub fn parse_origin(text: &str) -> Option<String> {
    let (scheme, rest) = text.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    if rest.is_empty() || rest.contains('/') || rest.contains('@') || rest.contains('\\') {
        return None;
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => {
            let port: u32 = port.parse().ok()?;
            if port == 0 || port > 65_535 {
                return None;
            }
            (host, Some(port))
        }
        None => (rest, None),
    };
    if host.is_empty()
        || !host.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '-')
        })
        || host.starts_with('.')
        || host.ends_with('.')
        || host.starts_with('-')
    {
        return None;
    }
    Some(match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

/// The origin of an absolute URL, in the same canonical spelling
/// [`parse_origin`] produces, or `None` when the text is not one this engine
/// will put in a code.
pub fn url_origin(text: &str) -> Option<String> {
    if text.chars().any(|character| {
        character.is_control() || character.is_whitespace() || !character.is_ascii()
    }) {
        return None;
    }
    let (scheme, rest) = text.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        // Credentials in a URL are the oldest way to make a link read as one
        // host and resolve to another.
        return None;
    }
    parse_origin(&format!("{scheme}://{authority}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn metadata(value: JsonValue) -> Metadata {
        serde_json::from_value(value).expect("test metadata deserializes")
    }

    fn messages(value: JsonValue) -> String {
        validate_media_declarations(&metadata(value))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The declaration a deployment is allowed to write, and the four shapes it
    /// is not.
    #[test]
    fn a_code_template_declares_a_typed_payload() {
        assert_eq!(
            messages(json!({
                "version": 3,
                "media": { "codes": [{
                    "name": "invoice_payment",
                    "symbology": "qr",
                    "payload": {
                        "type": "url",
                        "allowed_origins": ["https://pay.example.com"],
                        "max_length": 256
                    },
                    "version": 6,
                    "error_correction": "medium",
                    "module_size": 4,
                    "quiet_zone": 4,
                    "format": "png"
                }] }
            })),
            ""
        );

        // A URL payload with nowhere declared to point.
        let refused = messages(json!({
            "version": 3,
            "media": { "codes": [{
                "name": "open_redirect",
                "symbology": "qr",
                "payload": { "type": "url", "max_length": 256 },
                "version": 6, "error_correction": "medium",
                "module_size": 4, "quiet_zone": 4, "format": "png"
            }] }
        }));
        assert!(refused.contains("phishing vector"), "{refused}");

        // An origin that is not one: a path, a credential, a scheme that is
        // not the web's.
        for origin in [
            "https://pay.example.com/checkout",
            "https://user:pass@pay.example.com",
            "javascript://pay.example.com",
            "https://",
        ] {
            let refused = messages(json!({
                "version": 3,
                "media": { "codes": [{
                    "name": "invoice_payment", "symbology": "qr",
                    "payload": { "type": "url", "allowed_origins": [origin], "max_length": 256 },
                    "version": 6, "error_correction": "medium",
                    "module_size": 4, "quiet_zone": 4, "format": "png"
                }] }
            }));
            assert!(refused.contains("is not an origin"), "{origin}: {refused}");
        }

        // A matrix code without a fixed version, and a linear one with one.
        assert!(
            messages(json!({
                "version": 3,
                "media": { "codes": [{
                    "name": "unversioned", "symbology": "qr",
                    "payload": { "type": "ticket", "max_length": 64 },
                    "module_size": 4, "quiet_zone": 4, "format": "png"
                }] }
            }))
            .contains("rather than raised to fit")
        );
        assert!(
            messages(json!({
                "version": 3,
                "media": { "codes": [{
                    "name": "label", "symbology": "code128",
                    "payload": { "type": "ticket", "max_length": 32 },
                    "version": 4, "error_correction": "low", "height": 60,
                    "module_size": 2, "quiet_zone": 10, "format": "png"
                }] }
            }))
            .contains("no version and no error-correction level")
        );

        // Inline delivery is SVG-only and bounded.
        assert!(
            messages(json!({
                "version": 3,
                "media": { "codes": [{
                    "name": "inline_png", "symbology": "qr",
                    "payload": { "type": "ticket", "max_length": 64 },
                    "version": 4, "error_correction": "low",
                    "module_size": 4, "quiet_zone": 4, "format": "png",
                    "delivery": "inline", "max_inline_bytes": "8KiB"
                }] }
            }))
            .contains("only an `svg` code is returned inline")
        );
    }

    /// An image target is an allowlist. SVG is refused by name, and a media
    /// type with no compiled decoder is refused as unavailable rather than
    /// accepted and failed later.
    #[test]
    fn an_image_target_declares_a_closed_media_type_allowlist() {
        assert_eq!(
            messages(json!({
                "version": 3,
                "media": { "images": [{
                    "name": "avatar", "kind": "thumbnail",
                    "accept": ["image/png", "image/jpeg"],
                    "max_source_bytes": "8MiB", "max_pixels": 8000000,
                    "max_width": 320, "max_height": 320,
                    "fit": "cover", "format": "jpeg", "quality": 82
                }] }
            })),
            ""
        );
        let refused = messages(json!({
            "version": 3,
            "media": { "images": [{
                "name": "avatar", "kind": "thumbnail",
                "accept": ["image/svg+xml", "image/tiff"],
                "max_source_bytes": "eight megabytes", "max_pixels": 999999999,
                "max_width": 0, "max_height": 320,
                "format": "png", "quality": 82
            }] }
        }));
        assert!(
            refused.contains("does not belong in a decoder path"),
            "{refused}"
        );
        assert!(
            refused.contains("no decoder compiled into this binary"),
            "{refused}"
        );
        assert!(refused.contains("is not a byte size"), "{refused}");
        assert!(refused.contains("a pixel ceiling is"), "{refused}");
        assert!(
            refused.contains("admits no image in either dimension"),
            "{refused}"
        );
        assert!(refused.contains("PNG is lossless"), "{refused}");
    }

    /// A declaration and the operation that renders it are one decision, and
    /// the declaration a run selects is a literal.
    #[test]
    fn a_declaration_renders_only_through_its_own_operation() {
        let deployment = |connector: &str, operation: &str, input: JsonValue| {
            json!({
                "version": 3,
                "media": {
                    "codes": [{
                        "name": "ticket_qr", "symbology": "qr",
                        "payload": { "type": "ticket", "max_length": 64 },
                        "version": 4, "error_correction": "low",
                        "module_size": 4, "quiet_zone": 4, "format": "png"
                    }],
                    "images": [{
                        "name": "avatar", "kind": "thumbnail",
                        "accept": ["image/png"], "max_source_bytes": "8MiB",
                        "max_pixels": 8000000, "max_width": 320, "max_height": 320,
                        "format": "png"
                    }]
                },
                "processes": [{
                    "name": "issue", "kind": "process", "version": 1, "source": "default",
                    "start_at": "render",
                    "states": [{
                        "id": "render",
                        "request": {
                            "connector": connector, "operation": operation, "input": input,
                            "timeout": { "schedule_to_start": "10s", "start_to_close": "30s" },
                            "retry": { "retry_on": ["timeout"], "max_attempts": 1, "initial_interval": "1s", "max_interval": "5s", "jitter": "1s" },
                            "next": "done"
                        }
                    }]
                }]
            })
        };
        assert_eq!(
            messages(deployment(
                CODE_CAPABILITY,
                "qr.render",
                json!({ "template": { "literal": "ticket_qr" } })
            )),
            ""
        );
        assert!(
            messages(deployment(
                CODE_CAPABILITY,
                "barcode.render",
                json!({ "template": { "literal": "ticket_qr" } })
            ))
            .contains("renders through `qr.render`, not `barcode.render`")
        );
        assert!(
            messages(deployment(
                IMAGE_CAPABILITY,
                "image.normalize",
                json!({ "target": { "literal": "avatar" } })
            ))
            .contains("renders through `image.thumbnail`, not `image.normalize`")
        );
        assert!(
            messages(deployment(
                CODE_CAPABILITY,
                "qr.render",
                json!({ "template": { "state": "choose", "field": "template" } })
            ))
            .contains("not by a computed value")
        );
        assert!(
            messages(deployment(
                IMAGE_CAPABILITY,
                "image.thumbnail",
                json!({ "target": { "literal": "absent" } })
            ))
            .contains("image target `absent` is not declared by this deployment")
        );
    }

    /// The origin grammar, in both directions: what a deployment may declare
    /// and what a payload may carry.
    #[test]
    fn an_origin_is_scheme_host_and_port_and_nothing_else() {
        assert_eq!(
            parse_origin("https://pay.example.com").as_deref(),
            Some("https://pay.example.com")
        );
        assert_eq!(
            parse_origin("http://localhost:8080").as_deref(),
            Some("http://localhost:8080")
        );
        for refused in [
            "https://pay.example.com/",
            "https://pay.example.com:0",
            "https://PAY.example.com",
            "ftp://files.example.com",
            "pay.example.com",
            "https://пример.test",
        ] {
            assert_eq!(parse_origin(refused), None, "{refused} is not an origin");
        }

        assert_eq!(
            url_origin("https://pay.example.com/i/AB-1?x=2#f").as_deref(),
            Some("https://pay.example.com")
        );
        for refused in [
            "https://pay.example.com@evil.test/",
            "https://evil.test\u{200b}/",
            "https://pay.example.com\n",
            "not a url",
            "javascript:alert(1)",
        ] {
            assert_eq!(
                url_origin(refused),
                None,
                "{refused} has no admitted origin"
            );
        }
    }
}
