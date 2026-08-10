//! The renderer's view of what a deployment declared about media: the code
//! templates `local.code` renders and the image targets `local.image`
//! re-encodes into.
//!
//! This is deliberately *not* `donat_metadata::MediaMetadata`. The metadata
//! crate owns YAML, the declaration grammar, and the refusals an operator reads
//! at `validate`; this crate owns rendering and decoding. Keeping the two types
//! apart is the same separation spec 019 drew for document templates, and the
//! serving binary — which depends on both — is where they meet.
//!
//! What this module adds on top of the declaration is the half that has to be
//! true *for the renderer*: an allowed origin is parsed once, at load, into the
//! exact form a payload's origin is compared against; and a media type that
//! reaches the image half is one of the four this binary compiled a decoder
//! for, with `image/svg+xml` refused by name rather than by absence.
//!
//! A [`MediaCatalog`] is immutable and travels in the [`crate::local::context`]
//! beside an operation's input. Input names a declaration; it never carries
//! one. That is the whole reason an allowed-origin list means anything: it is
//! the only part of the path the party supplying the payload does not control.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The symbologies this binary renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Symbology {
    #[default]
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

    pub const fn is_matrix(self) -> bool {
        matches!(self, Self::Qr)
    }

    /// The name the activity result reports.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Qr => "qr",
            Self::Code128 => "code128",
            Self::Code39 => "code39",
            Self::Ean13 => "ean13",
        }
    }
}

/// What a code is allowed to carry.
///
/// The default is [`Self::Ticket`] — an opaque identifier with no origins and
/// no scheme prefixes — because a default that is a URL is a default that
/// points somewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum CodePayloadType {
    Url,
    #[default]
    Ticket,
    Payment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum CodeErrorCorrection {
    #[default]
    Low,
    Medium,
    Quartile,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum CodeFormat {
    #[default]
    Png,
    Svg,
}

/// Where a rendered code goes. `Stored` is the default: a file in the
/// attachment store behind a signed URL is what every other produced artifact
/// is, and an inline string is the exception a declaration has to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum CodeDelivery {
    #[default]
    Stored,
    Inline,
}

/// Everything one code template declares, as the serving binary hands it over.
#[derive(Debug, Clone, Default)]
pub struct CodeTemplateSpec {
    pub name: String,
    pub symbology: Symbology,
    pub payload_type: CodePayloadType,
    /// For a `url` payload: the origins, in `scheme://host[:port]` form.
    pub allowed_origins: BTreeSet<String>,
    /// For a `payment` payload: the scheme prefixes.
    pub allowed_prefixes: BTreeSet<String>,
    pub max_payload_bytes: u32,
    pub version: Option<u8>,
    pub error_correction: Option<CodeErrorCorrection>,
    pub height: Option<u32>,
    pub module_size: u32,
    pub quiet_zone: u32,
    pub format: CodeFormat,
    pub delivery: CodeDelivery,
    pub max_inline_bytes: Option<u64>,
}

/// One resolved code template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeTemplate {
    name: String,
    symbology: Symbology,
    payload_type: CodePayloadType,
    allowed_origins: BTreeSet<String>,
    allowed_prefixes: BTreeSet<String>,
    max_payload_bytes: u32,
    version: Option<u8>,
    error_correction: CodeErrorCorrection,
    height: Option<u32>,
    module_size: u32,
    quiet_zone: u32,
    format: CodeFormat,
    delivery: CodeDelivery,
    max_inline_bytes: Option<u64>,
}

impl CodeTemplate {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn symbology(&self) -> Symbology {
        self.symbology
    }

    pub const fn payload_type(&self) -> CodePayloadType {
        self.payload_type
    }

    pub const fn allowed_origins(&self) -> &BTreeSet<String> {
        &self.allowed_origins
    }

    pub const fn allowed_prefixes(&self) -> &BTreeSet<String> {
        &self.allowed_prefixes
    }

    pub const fn max_payload_bytes(&self) -> u32 {
        self.max_payload_bytes
    }

    /// The fixed matrix version. Capacity is checked against it; nothing ever
    /// raises it (spec 022 §1).
    pub const fn version(&self) -> Option<u8> {
        self.version
    }

    pub const fn error_correction(&self) -> CodeErrorCorrection {
        self.error_correction
    }

    pub const fn height(&self) -> Option<u32> {
        self.height
    }

    pub const fn module_size(&self) -> u32 {
        self.module_size
    }

    pub const fn quiet_zone(&self) -> u32 {
        self.quiet_zone
    }

    pub const fn format(&self) -> CodeFormat {
        self.format
    }

    pub const fn delivery(&self) -> CodeDelivery {
        self.delivery
    }

    pub const fn max_inline_bytes(&self) -> Option<u64> {
        self.max_inline_bytes
    }
}

/// What an image target renders into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ImageTargetKind {
    #[default]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ImageFit {
    #[default]
    Contain,
    Cover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ImageOutputFormat {
    #[default]
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
}

/// What a multi-frame source may become. The default refuses one: a deployment
/// that never considered animation gets the answer that decodes least.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ImageAnimation {
    #[default]
    Reject,
    FirstFrame,
}

/// The media types this binary compiled a decoder for.
///
/// It is the `image` crate's feature list, written where a declaration can be
/// checked against it. A format outside this set cannot be decoded however a
/// caller spells its media type, because the code that would decode it was
/// never linked in.
pub const DECODABLE_MEDIA_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// The one media type that is refused by name rather than by absence.
pub const SVG_MEDIA_TYPE: &str = "image/svg+xml";

/// Everything one image target declares, as the serving binary hands it over.
#[derive(Debug, Clone, Default)]
pub struct ImageTargetSpec {
    pub name: String,
    pub kind: ImageTargetKind,
    pub accept: BTreeSet<String>,
    pub max_source_bytes: u64,
    pub max_pixels: u64,
    pub max_width: u32,
    pub max_height: u32,
    pub fit: ImageFit,
    pub format: ImageOutputFormat,
    pub quality: Option<u8>,
    pub animation: ImageAnimation,
}

/// One resolved image target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageTarget {
    name: String,
    kind: ImageTargetKind,
    accept: BTreeSet<String>,
    max_source_bytes: u64,
    max_pixels: u64,
    max_width: u32,
    max_height: u32,
    fit: ImageFit,
    format: ImageOutputFormat,
    quality: u8,
    animation: ImageAnimation,
}

impl ImageTarget {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> ImageTargetKind {
        self.kind
    }

    /// The media types this target's decoder may ever be offered. Step 1 of
    /// spec 022 §2's fixed order is a lookup in this set and nothing else.
    pub const fn accept(&self) -> &BTreeSet<String> {
        &self.accept
    }

    pub fn accepts(&self, media_type: &str) -> bool {
        // Two independent conditions, because either one alone would be a
        // single point of failure: the deployment declared it, *and* this
        // binary has a decoder for it. `image/svg+xml` fails both.
        media_type != SVG_MEDIA_TYPE
            && DECODABLE_MEDIA_TYPES.contains(&media_type)
            && self.accept.contains(media_type)
    }

    pub const fn max_source_bytes(&self) -> u64 {
        self.max_source_bytes
    }

    pub const fn max_pixels(&self) -> u64 {
        self.max_pixels
    }

    pub const fn max_width(&self) -> u32 {
        self.max_width
    }

    pub const fn max_height(&self) -> u32 {
        self.max_height
    }

    pub const fn fit(&self) -> ImageFit {
        self.fit
    }

    pub const fn format(&self) -> ImageOutputFormat {
        self.format
    }

    pub const fn quality(&self) -> u8 {
        self.quality
    }

    pub const fn animation(&self) -> ImageAnimation {
        self.animation
    }
}

/// One refusal from resolving a media declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRejection {
    pub declaration: String,
    pub message: String,
}

impl MediaRejection {
    fn new(declaration: &str, message: impl Into<String>) -> Self {
        Self {
            declaration: declaration.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MediaRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "media declaration `{}`: {}",
            self.declaration, self.message
        )
    }
}

/// The deployment's resolved media declarations, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaCatalog {
    codes: BTreeMap<String, Arc<CodeTemplate>>,
    images: BTreeMap<String, Arc<ImageTarget>>,
}

impl MediaCatalog {
    /// Resolve a whole catalog. Every rejection is collected, because a
    /// deployment with three broken declarations should learn about three.
    pub fn resolve(
        codes: impl IntoIterator<Item = CodeTemplateSpec>,
        images: impl IntoIterator<Item = ImageTargetSpec>,
    ) -> Result<Self, Vec<MediaRejection>> {
        let mut catalog = Self::default();
        let mut rejections = Vec::new();
        for spec in codes {
            let name = spec.name.clone();
            match resolve_code(spec) {
                Ok(code) => {
                    if catalog.codes.insert(name.clone(), Arc::new(code)).is_some() {
                        rejections.push(MediaRejection::new(&name, "is declared twice"));
                    }
                }
                Err(rejection) => rejections.push(rejection),
            }
        }
        for spec in images {
            let name = spec.name.clone();
            match resolve_image(spec) {
                Ok(image) => {
                    if catalog
                        .images
                        .insert(name.clone(), Arc::new(image))
                        .is_some()
                    {
                        rejections.push(MediaRejection::new(&name, "is declared twice"));
                    }
                }
                Err(rejection) => rejections.push(rejection),
            }
        }
        if rejections.is_empty() {
            Ok(catalog)
        } else {
            Err(rejections)
        }
    }

    pub fn code(&self, name: &str) -> Option<&Arc<CodeTemplate>> {
        self.codes.get(name)
    }

    pub fn image(&self, name: &str) -> Option<&Arc<ImageTarget>> {
        self.images.get(name)
    }

    pub fn code_names(&self) -> impl Iterator<Item = &str> {
        self.codes.keys().map(String::as_str)
    }

    pub fn image_names(&self) -> impl Iterator<Item = &str> {
        self.images.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty() && self.images.is_empty()
    }
}

fn resolve_code(spec: CodeTemplateSpec) -> Result<CodeTemplate, MediaRejection> {
    let name = spec.name.clone();
    let reject = |message: &str| MediaRejection::new(&name, message);
    if spec.name.is_empty() {
        return Err(reject("a code template has a name"));
    }
    if spec.max_payload_bytes == 0 {
        return Err(reject(
            "a code template admits a payload of at least one byte",
        ));
    }
    if spec.module_size == 0 {
        return Err(reject("a module is at least one pixel across"));
    }
    if spec.symbology.is_matrix() {
        match spec.version {
            Some(version) if (1..=40).contains(&version) => {}
            _ => {
                return Err(reject(
                    "a matrix code declares the fixed version its capacity is checked against",
                ));
            }
        }
        if spec.error_correction.is_none() {
            return Err(reject("a matrix code declares its error-correction level"));
        }
    } else if spec.height.unwrap_or(0) == 0 {
        return Err(reject("a linear symbology declares its bar height"));
    }

    // The origins are parsed once, here, into the exact spelling a payload's
    // own origin is compared against. Comparing two strings that were each
    // normalized at a different time is how an allowlist develops a hole.
    let mut allowed_origins = BTreeSet::new();
    for origin in &spec.allowed_origins {
        let canonical = canonical_origin(origin)
            .ok_or_else(|| MediaRejection::new(&name, format!("`{origin}` is not an origin")))?;
        allowed_origins.insert(canonical);
    }
    if spec.payload_type == CodePayloadType::Url && allowed_origins.is_empty() {
        return Err(reject(
            "a `url` payload declares the origins it may point at",
        ));
    }
    if spec.payload_type == CodePayloadType::Payment && spec.allowed_prefixes.is_empty() {
        return Err(reject(
            "a `payment` payload declares the scheme prefixes it may begin with",
        ));
    }
    if spec.delivery == CodeDelivery::Inline
        && (spec.format != CodeFormat::Svg || spec.max_inline_bytes.unwrap_or(0) == 0)
    {
        return Err(reject(
            "an inline code is an `svg` under a declared `max_inline_bytes`",
        ));
    }

    Ok(CodeTemplate {
        name: spec.name,
        symbology: spec.symbology,
        payload_type: spec.payload_type,
        allowed_origins,
        allowed_prefixes: spec.allowed_prefixes,
        max_payload_bytes: spec.max_payload_bytes,
        version: spec.version,
        error_correction: spec.error_correction.unwrap_or_default(),
        height: spec.height,
        module_size: spec.module_size,
        quiet_zone: spec.quiet_zone,
        format: spec.format,
        delivery: spec.delivery,
        max_inline_bytes: spec.max_inline_bytes,
    })
}

fn resolve_image(spec: ImageTargetSpec) -> Result<ImageTarget, MediaRejection> {
    let name = spec.name.clone();
    let reject = |message: &str| MediaRejection::new(&name, message);
    if spec.name.is_empty() {
        return Err(reject("an image target has a name"));
    }
    if spec.accept.is_empty() {
        return Err(reject(
            "an image target declares the media types its decoder may be offered",
        ));
    }
    for media_type in &spec.accept {
        if media_type == SVG_MEDIA_TYPE {
            return Err(reject(
                "`image/svg+xml` is never decoded: it is a document format with external \
                 references and scripting, and it does not belong in a decoder path",
            ));
        }
        if !DECODABLE_MEDIA_TYPES.contains(&media_type.as_str()) {
            return Err(MediaRejection::new(
                &name,
                format!("`{media_type}` has no decoder compiled into this binary"),
            ));
        }
    }
    if spec.max_source_bytes == 0 || spec.max_pixels == 0 {
        return Err(reject(
            "an image target declares a positive byte and pixel ceiling for its source",
        ));
    }
    if spec.max_width == 0 || spec.max_height == 0 {
        return Err(reject("a target box admits no image at zero"));
    }
    if u64::from(spec.max_width) * u64::from(spec.max_height) > spec.max_pixels {
        return Err(reject(
            "the output box is larger than the pixel ceiling the source is admitted under",
        ));
    }
    let quality = match (spec.format, spec.quality) {
        (ImageOutputFormat::Jpeg, Some(quality)) if (1..=100).contains(&quality) => quality,
        (ImageOutputFormat::Jpeg, None) => 82,
        (ImageOutputFormat::Jpeg, Some(_)) => return Err(reject("a JPEG quality is 1 to 100")),
        // PNG is lossless, so there is no quality to carry; the declaration
        // refuses one, and the resolved target holds a value nothing reads.
        (ImageOutputFormat::Png, None) => 100,
        (ImageOutputFormat::Png, Some(_)) => {
            return Err(reject("PNG is lossless: it has no quality setting"));
        }
    };

    Ok(ImageTarget {
        name: spec.name,
        kind: spec.kind,
        accept: spec.accept,
        max_source_bytes: spec.max_source_bytes,
        max_pixels: spec.max_pixels,
        max_width: spec.max_width,
        max_height: spec.max_height,
        fit: spec.fit,
        format: spec.format,
        quality,
        animation: spec.animation,
    })
}

/// Canonicalize `scheme://host[:port]`, or return `None`.
///
/// A hand-written parser rather than a URL crate, and strict on purpose: the
/// shapes it refuses — a credential, a path, an upper-case or non-ASCII host —
/// are the ones an attacker writes. A parser that normalizes generously is one
/// that eventually agrees two different destinations are the same origin.
pub fn canonical_origin(text: &str) -> Option<String> {
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

/// The origin of an absolute URL in the same canonical spelling, or `None` when
/// the text is not a URL this engine will put in a code.
pub fn payload_origin(text: &str) -> Option<String> {
    if text.chars().any(|character| {
        character.is_control() || character.is_whitespace() || !character.is_ascii()
    }) {
        return None;
    }
    let (scheme, rest) = text.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        // Credentials are the oldest way to write a link that reads as one host
        // and resolves to another.
        return None;
    }
    canonical_origin(&format!("{scheme}://{authority}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(name: &str) -> CodeTemplateSpec {
        CodeTemplateSpec {
            name: name.to_owned(),
            symbology: Symbology::Qr,
            payload_type: CodePayloadType::Ticket,
            max_payload_bytes: 64,
            version: Some(4),
            error_correction: Some(CodeErrorCorrection::Low),
            module_size: 4,
            quiet_zone: 4,
            ..Default::default()
        }
    }

    fn image(name: &str) -> ImageTargetSpec {
        ImageTargetSpec {
            name: name.to_owned(),
            kind: ImageTargetKind::Thumbnail,
            accept: ["image/png".to_owned()].into_iter().collect(),
            max_source_bytes: 8 * 1_024 * 1_024,
            max_pixels: 8_000_000,
            max_width: 320,
            max_height: 320,
            ..Default::default()
        }
    }

    /// The catalog refuses what a renderer could not honour, and collects every
    /// refusal rather than stopping at the first.
    #[test]
    fn a_catalog_resolves_or_names_every_refusal() {
        assert!(MediaCatalog::resolve([code("ticket")], [image("avatar")]).is_ok());

        let mut unversioned = code("unversioned");
        unversioned.version = None;
        let mut svg_source = image("svg_source");
        svg_source.accept = [SVG_MEDIA_TYPE.to_owned()].into_iter().collect();
        let rejections = MediaCatalog::resolve([unversioned], [svg_source])
            .expect_err("neither declaration is renderable");
        assert_eq!(rejections.len(), 2, "{rejections:?}");
        assert!(rejections[0].message.contains("fixed version"));
        assert!(
            rejections[1]
                .message
                .contains("does not belong in a decoder path")
        );

        // A duplicate name is a refusal too: two declarations of one name are
        // two answers to one question.
        assert!(MediaCatalog::resolve([code("ticket"), code("ticket")], []).is_err());
    }

    /// `accepts` is two independent conditions, and the SVG answer does not
    /// depend on either the declaration or the feature list alone.
    #[test]
    fn an_image_target_accepts_only_a_declared_and_compiled_media_type() {
        let mut spec = image("avatar");
        spec.accept = ["image/png".to_owned(), "image/jpeg".to_owned()]
            .into_iter()
            .collect();
        let catalog = MediaCatalog::resolve([], [spec]).expect("the declaration resolves");
        let target = catalog.image("avatar").expect("the target is declared");
        assert!(target.accepts("image/png"));
        assert!(target.accepts("image/jpeg"));
        assert!(!target.accepts("image/gif"), "compiled, but not declared");
        assert!(
            !target.accepts("image/tiff"),
            "declared nowhere, compiled nowhere"
        );
        assert!(!target.accepts(SVG_MEDIA_TYPE));
        assert!(
            !target.accepts("IMAGE/PNG"),
            "a media type is compared exactly"
        );
    }

    /// The origin grammar, which is the whole of the QR phishing defence.
    #[test]
    fn an_origin_is_canonical_or_nothing() {
        assert_eq!(
            canonical_origin("https://pay.example.com").as_deref(),
            Some("https://pay.example.com")
        );
        assert_eq!(
            payload_origin("https://pay.example.com/i/AB-1?x=2#f").as_deref(),
            Some("https://pay.example.com")
        );
        for refused in [
            "https://pay.example.com@evil.test/",
            "https://pay.example.com\u{0000}.evil.test/",
            "HTTPS://PAY.EXAMPLE.COM/",
            "javascript:alert(1)",
            "//pay.example.com/",
            "https://пример.test/",
        ] {
            assert_eq!(payload_origin(refused), None, "{refused}");
        }
    }
}
