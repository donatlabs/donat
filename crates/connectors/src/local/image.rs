//! `local.image` — thumbnails and normalization (spec 022 §2).
//!
//! | Operation | Backend | Product |
//! |---|---|---|
//! | `image.thumbnail` | `image` | a stored raster fitted into the declared box |
//! | `image.normalize` | `image` | a stored raster re-encoded at or below it |
//!
//! **This is the one place in the engine that is handed bytes a stranger
//! uploaded.** Everything else a local capability reads is either a deployment
//! declaration or a value a process computed; a source image is a file whose
//! author chose every byte of it, including the ones that describe how large it
//! is. So the order below is fixed, and each step has a test of its own
//! (spec 022 §2).
//!
//! 1. **The allowlist.** The stored attachment's declared media type must be one
//!    the target declared *and* one this binary compiled a decoder for. Nothing
//!    is opened otherwise. `image/svg+xml` fails both halves: a declaration
//!    carrying one does not resolve, and no SVG decoder exists to reach.
//! 2. **The format comes from the bytes.** `guess_format` reads the magic and
//!    must agree with the stored media type. A file named `.png` whose header
//!    says JPEG is refused rather than decoded as what it turned out to be —
//!    the alternative is a path where the *uploader* picks which decoder runs.
//! 3. **Dimensions before allocation.** The decoder is constructed (which reads
//!    the header and nothing else), its dimensions are read, and the pixel count
//!    is checked against the target's `max_pixels` *before any pixel buffer
//!    exists*. This is the decompression-bomb defence: a 6 KB file declaring
//!    40000×40000 fails here, having cost six kilobytes.
//! 4. **The decoder's own limits**, derived from the same declaration, as a
//!    second line — redundant with step 3 by design, because a redundant bound
//!    is what catches the decoder that reports one size and allocates another.
//! 5. **One frame.** A multi-frame source is refused unless the target declared
//!    `first_frame`, and then the plain decoder is used, which decodes frame one
//!    and never asks for the rest.
//! 6. **Re-encode, and drop everything else.** The output is encoded from the
//!    pixels; no metadata is carried across. EXIF from a phone photo carries GPS
//!    coordinates, and a thumbnail that leaks a customer's home address is a
//!    data-protection incident rather than a cosmetic issue. Orientation is read
//!    and applied to the pixels *before* the metadata is discarded, so dropping
//!    it does not silently rotate the picture.

use std::io::Cursor;
use std::time::Duration;

use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::{JpegDecoder, JpegEncoder};
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, Limits};
use serde_json::{Value as JsonValue, json};

use crate::local::bounds::LocalBounds;
use crate::local::capability::{
    LocalArtifact, LocalCapability, LocalInvocation, LocalOperation, LocalProduct,
};
use crate::local::ingest::SourceFile;
use crate::local::media::{
    ImageAnimation, ImageFit, ImageOutputFormat, ImageTarget, ImageTargetKind, ImageTargetSpec,
};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};

/// The capability's declaration, built once by the table in
/// [`crate::local::capabilities`].
pub fn capability() -> LocalCapability {
    LocalCapability::declare("local.image", "1.0.0")
        .operation(operation(
            "image.thumbnail",
            PROBE_THUMBNAIL_TARGET,
            "the output is the declared source scaled into the declared box with the declared \
             fit and re-encoded in the declared format; the resize filter, the encoder settings, \
             and the orientation are all declared or fixed, so no clock, random seed, \
             environment, or locale reaches it",
        ))
        .operation(operation(
            "image.normalize",
            PROBE_NORMALIZE_TARGET,
            "the output is the declared source re-encoded in the declared format, scaled down \
             only when it exceeds the declared box; no clock, no random seed, no environment, \
             no locale",
        ))
        .build()
        .expect("the image capability declaration is static and complete")
}

/// The probe declarations and the probe source, compiled into this binary so
/// ADR 044's double render stays a property of the binary.
pub const PROBE_THUMBNAIL_TARGET: &str = "__probe_thumbnail";
pub const PROBE_NORMALIZE_TARGET: &str = "__probe_normalize";
pub const PROBE_SOURCE_HANDLE: &str = "__probe_image_source";

pub(crate) fn builtin_image_targets() -> Vec<ImageTargetSpec> {
    let target = |name: &str, kind: ImageTargetKind| ImageTargetSpec {
        name: name.to_owned(),
        kind,
        accept: ["image/png".to_owned()].into_iter().collect(),
        max_source_bytes: 64 * 1_024,
        max_pixels: 4_096,
        max_width: 8,
        max_height: 8,
        fit: ImageFit::Contain,
        format: ImageOutputFormat::Png,
        quality: None,
        animation: ImageAnimation::Reject,
    };
    vec![
        target(PROBE_THUMBNAIL_TARGET, ImageTargetKind::Thumbnail),
        target(PROBE_NORMALIZE_TARGET, ImageTargetKind::Normalize),
    ]
}

/// The source the probes decode: a 16×16 gradient, encoded here by the same
/// encoder the operations use.
///
/// It is generated rather than embedded because a PNG written by this binary is
/// exactly what "the bytes are a function of the input" means; an embedded blob
/// would be one more thing to keep true.
pub(crate) fn builtin_image_sources() -> Vec<SourceFile> {
    vec![SourceFile::new(
        PROBE_SOURCE_HANDLE,
        "probe.png",
        "image/png",
        probe_png(16, 16),
    )]
}

/// The probe raster: a deterministic gradient, encoded by the same encoder the
/// operations use.
///
/// Public because the serving binary's dispatch test needs a source that is a
/// real image rather than a stub — a decode path proven against bytes that are
/// not an image proves nothing about decoding.
pub fn probe_png(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = image::RgbImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, 0]);
    }
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(Cursor::new(&mut bytes))
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .expect("the probe raster encodes");
    bytes
}

// ---------------------------------------------------------------------------
// Declaration
// ---------------------------------------------------------------------------

/// Bytes per pixel of the widest buffer a decode can land in (RGBA8).
const BYTES_PER_PIXEL: u64 = 4;

/// What a decoder may allocate beyond the image buffer itself: row buffers,
/// palettes, Huffman tables. Bounded, because it is the part of the decoder's
/// appetite the pixel count does not describe.
const DECODER_OVERHEAD_BYTES: u64 = 4 * 1_024 * 1_024;

/// The bounds both operations run inside.
///
/// The input ceiling is small on purpose: an image activity's input is a target
/// name, a source handle, and where the result goes. The *source* does not
/// travel in it — it arrives in the execution context, resolved from the stored
/// attachment — so the input a journal retains stays a few hundred bytes
/// whatever the picture weighs.
fn image_bounds() -> LocalBounds {
    LocalBounds::declare(
        Duration::from_secs(20),
        8_192,
        16 * 1_024 * 1_024,
        256 * 1_024 * 1_024,
        "pixels",
        40_000_000,
    )
    .expect("the image bounds are static and complete")
}

fn operation(id: &'static str, probe_target: &str, statement: &str) -> LocalOperation {
    LocalOperation::declare(id, "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "target": probe_target,
                    "source": PROBE_SOURCE_HANDLE,
                    "attachment": "public.probe.image",
                    "claim_role": "app",
                    "file_name": "probe.png"
                }),
                statement,
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(image_bounds())
        // The pixel count is not knowable from the input — the input names a
        // file, it does not carry one — so the unit charge happens once the
        // header has been read, against the same ceiling.
        .units(|_| 1)
        .run(run)
        .build()
        .unwrap_or_else(|error| panic!("{id} is deterministic: {error}"))
}

// ---------------------------------------------------------------------------
// The decode order of spec 022 §2
// ---------------------------------------------------------------------------

/// The limits handed to the decoder, derived from the declaration alone.
///
/// A second line rather than the first: step 3 has already refused anything
/// over `max_pixels` from the header. What this catches is a decoder that
/// reports one size and allocates another, which is exactly the failure a
/// single check cannot.
pub fn decoder_limits(target: &ImageTarget) -> Limits {
    let mut limits = Limits::no_limits();
    // No single dimension can exceed the whole pixel budget while the other is
    // at least one pixel, so the budget is the honest per-dimension cap.
    let side = u32::try_from(target.max_pixels()).unwrap_or(u32::MAX);
    limits.max_image_width = Some(side);
    limits.max_image_height = Some(side);
    limits.max_alloc = Some(
        target
            .max_pixels()
            .saturating_mul(BYTES_PER_PIXEL)
            .saturating_add(DECODER_OVERHEAD_BYTES),
    );
    limits
}

/// The limits the open path actually installed on a decoder for one source.
///
/// It runs the real path and reports what step 4 put in place, so "the limits
/// are set from the declared bounds" is checked against the code that sets
/// them rather than against a second copy of the derivation.
pub fn installed_limits(
    target: &ImageTarget,
    source: &SourceFile,
) -> Result<Limits, ConnectorFailure> {
    open(target, source).map(|opened| opened.limits)
}

/// A source that has passed every check that can be made before pixels exist.
pub(crate) struct Opened<'a> {
    pub decoder: Box<dyn ImageDecoder + 'a>,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
    /// The limits installed on the decoder, kept so the caller — and the test
    /// that proves step 4 — can see what the declaration produced.
    pub limits: Limits,
}

/// Steps 1 to 5, in that order and no other.
pub(crate) fn open<'a>(
    target: &ImageTarget,
    source: &'a SourceFile,
) -> Result<Opened<'a>, ConnectorFailure> {
    // 1. The allowlist. Nothing below this line runs for a media type the
    //    deployment did not declare or this binary cannot decode.
    if !target.accepts(source.media_type()) {
        return Err(refuse(
            "local_image_media_type_refused",
            "the stored file's media type is not one this image target admits",
        ));
    }
    if source.byte_size() > target.max_source_bytes() {
        return Err(refuse(
            "local_image_source_too_large",
            "the stored file is larger than this image target admits",
        ));
    }

    // 2. The format comes from the bytes. The stored media type and the file
    //    name are what somebody *said*; the magic is what the file is.
    let format = image::guess_format(source.bytes()).map_err(|_| {
        refuse(
            "local_image_format_unreadable",
            "the stored file's bytes do not begin with a format this binary decodes",
        )
    })?;
    if media_type_of(format) != Some(source.media_type()) {
        return Err(refuse(
            "local_image_format_mismatch",
            "the stored file's bytes are a different format from its declared media type",
        ));
    }

    // 3. Dimensions from the header, checked before a pixel buffer exists.
    //    Constructing a decoder reads the header; it does not decode.
    let bytes = Cursor::new(source.bytes());
    let mut decoder: Box<dyn ImageDecoder + 'a> = match format {
        ImageFormat::Png => Box::new(PngDecoder::new(bytes).map_err(unreadable)?),
        ImageFormat::Jpeg => Box::new(JpegDecoder::new(bytes).map_err(unreadable)?),
        ImageFormat::Gif => Box::new(GifDecoder::new(bytes).map_err(unreadable)?),
        ImageFormat::WebP => Box::new(WebPDecoder::new(bytes).map_err(unreadable)?),
        // Unreachable: `media_type_of` answers `None` for every other format,
        // and step 2 refused it. The arm exists so adding a media type to the
        // allowlist without adding a decoder here does not compile into a
        // surprise.
        _ => {
            return Err(refuse(
                "local_image_format_refused",
                "no decoder for the stored file's format is compiled into this binary",
            ));
        }
    };
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(refuse(
            "local_image_dimensions_invalid",
            "the stored file declares an empty image",
        ));
    }
    if u64::from(width).saturating_mul(u64::from(height)) > target.max_pixels() {
        return Err(refuse(
            "local_image_too_many_pixels",
            "the stored file declares more pixels than this image target admits",
        ));
    }

    // 4. The decoder's own limits, from the same declaration.
    let limits = decoder_limits(target);
    decoder.set_limits(limits.clone()).map_err(|_| {
        refuse(
            "local_image_decoder_limit",
            "the stored file exceeds the decoder limits this image target declares",
        )
    })?;

    // 5. One frame, and only when the target permits it. The check is on the
    //    container, before decoding: counting frames by decoding them is
    //    exactly what the bound exists to prevent.
    let animated = is_animated(format, source.bytes());
    if animated && target.animation() == ImageAnimation::Reject {
        return Err(refuse(
            "local_image_animation_refused",
            "the stored file is animated and this image target admits one frame only by \
             declaration",
        ));
    }

    Ok(Opened {
        decoder,
        format,
        width,
        height,
        animated,
        limits,
    })
}

/// The media type a format's magic implies, or `None` when this binary decodes
/// no such thing.
///
/// It is the inverse of the allowlist and deliberately total: a format the
/// `image` crate can name but this engine does not admit has no media type
/// here, so step 2 refuses it.
pub(crate) fn media_type_of(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

/// Whether a container holds more than one frame, read from its structure.
///
/// Each of these walks the container's own index — a GIF's block chain, a PNG's
/// chunk list, a WebP's RIFF chunks — and stops at the first evidence. None of
/// them decompresses anything, which is the point: "how many frames" must be
/// answerable before "decode a frame" is.
pub(crate) fn is_animated(format: ImageFormat, bytes: &[u8]) -> bool {
    match format {
        ImageFormat::Gif => gif_has_second_frame(bytes),
        ImageFormat::Png => png_has_chunk(bytes, b"acTL"),
        ImageFormat::WebP => webp_is_animated(bytes),
        _ => false,
    }
}

fn gif_has_second_frame(bytes: &[u8]) -> bool {
    // Header (6) + logical screen descriptor (7), then the global colour table
    // if the packed field says there is one.
    let Some(packed) = bytes.get(10) else {
        return false;
    };
    let mut at = 13;
    if packed & 0x80 != 0 {
        at += 3 * (1_usize << ((packed & 0x07) + 1));
    }
    let mut frames = 0;
    // The chain is bounded by the file, and every step advances, so the walk
    // terminates on any input including a truncated or hostile one.
    while at < bytes.len() {
        match bytes[at] {
            // Extension: a label, then length-prefixed sub-blocks.
            0x21 => {
                at += 2;
                at = skip_sub_blocks(bytes, at);
            }
            // Image descriptor: nine bytes, an optional local colour table,
            // the LZW minimum code size, then sub-blocks.
            0x2C => {
                frames += 1;
                if frames > 1 {
                    return true;
                }
                let Some(&local) = bytes.get(at + 9) else {
                    return false;
                };
                at += 10;
                if local & 0x80 != 0 {
                    at += 3 * (1_usize << ((local & 0x07) + 1));
                }
                at += 1;
                at = skip_sub_blocks(bytes, at);
            }
            // Trailer, or anything the chain does not define.
            _ => return false,
        }
    }
    false
}

fn skip_sub_blocks(bytes: &[u8], mut at: usize) -> usize {
    while let Some(&length) = bytes.get(at) {
        at += 1 + length as usize;
        if length == 0 {
            break;
        }
    }
    at
}

fn png_has_chunk(bytes: &[u8], name: &[u8; 4]) -> bool {
    let mut at = 8;
    while at + 8 <= bytes.len() {
        let length =
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        let kind = &bytes[at + 4..at + 8];
        if kind == name {
            return true;
        }
        if kind == b"IDAT" {
            // Every animation control chunk precedes the first IDAT, so there
            // is nothing after it worth walking.
            return false;
        }
        at = match at.checked_add(12).and_then(|at| at.checked_add(length)) {
            Some(next) if next > at => next,
            _ => return false,
        };
    }
    false
}

fn webp_is_animated(bytes: &[u8]) -> bool {
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let kind = &bytes[at + 4 - 4..at + 4];
        let length =
            u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
                as usize;
        if kind == b"ANIM" || kind == b"ANMF" {
            return true;
        }
        if kind == b"VP8X" {
            // The animation flag lives in the first byte of the extended
            // header, ahead of any ANIM chunk.
            if bytes.get(at + 8).is_some_and(|flags| flags & 0x02 != 0) {
                return true;
            }
        }
        at = match at
            .checked_add(8)
            .and_then(|at| at.checked_add(length + length % 2))
        {
            Some(next) if next > at => next,
            _ => return false,
        };
    }
    false
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

fn run(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let input = invocation.input();
    let target_name = text(input, "target").ok_or_else(|| {
        refuse(
            "local_input_contract",
            "an image activity selects its target by name: input requires `target`",
        )
    })?;
    let target = invocation
        .context()
        .media()
        .image(target_name)
        .ok_or_else(|| {
            refuse(
                "local_image_target_unknown",
                "the selected image target is not declared by this deployment",
            )
        })?
        .clone();
    let handle = text(input, "source").ok_or_else(|| {
        refuse(
            "local_input_contract",
            "an image activity names its stored source with `source`",
        )
    })?;
    let source = invocation.context().source(handle).ok_or_else(|| {
        refuse(
            "local_image_source_unresolved",
            "the stored file this activity names was not resolved for this execution",
        )
    })?;

    let opened = open(&target, source)?;
    // The pixel count is charged now, against the operation's own unit ceiling,
    // because it is the first moment it is known and the last moment before it
    // costs anything.
    invocation.charge_units(u64::from(opened.width) * u64::from(opened.height))?;
    invocation.reserve(usize::try_from(opened.decoder.total_bytes()).unwrap_or(usize::MAX))?;
    invocation.checkpoint()?;

    // 6. Re-encode. The orientation is read from the metadata and applied to
    //    the pixels; nothing else about the metadata survives this function.
    let Opened {
        mut decoder,
        format,
        width,
        height,
        animated,
        ..
    } = opened;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut decoded = DynamicImage::from_decoder(decoder).map_err(|_| {
        refuse(
            "local_image_decode_failed",
            "the stored file's pixels could not be decoded inside the declared bounds",
        )
    })?;
    decoded.apply_orientation(orientation);
    invocation.checkpoint()?;

    let fitted = fit(&target, decoded.width(), decoded.height());
    invocation
        .reserve((u64::from(fitted.width) * u64::from(fitted.height) * BYTES_PER_PIXEL) as usize)?;
    if (fitted.width, fitted.height) != (decoded.width(), decoded.height()) {
        decoded = decoded.resize_exact(
            fitted.width,
            fitted.height,
            image::imageops::FilterType::Lanczos3,
        );
    }
    if let Some(crop) = fitted.crop {
        decoded = decoded.crop_imm(crop.x, crop.y, crop.width, crop.height);
    }
    invocation.checkpoint()?;

    let bytes = encode(&target, &decoded)?;
    let attachment = text(input, "attachment").ok_or_else(|| {
        refuse(
            "local_input_contract",
            "a produced image names the `attachment` its file belongs to",
        )
    })?;
    let claim_role = text(input, "claim_role").ok_or_else(|| {
        refuse(
            "local_input_contract",
            "a produced image names the `claim_role` that will bind its file",
        )
    })?;
    let media_type = target.format().media_type();
    let file_name = text(input, "file_name").unwrap_or(match target.format() {
        ImageOutputFormat::Png => "image.png",
        ImageOutputFormat::Jpeg => "image.jpg",
    });

    let metadata = json!({
        "width": decoded.width(),
        "height": decoded.height(),
        "media_type": media_type,
        "byte_size": bytes.len(),
        "source_width": width,
        "source_height": height,
        "source_media_type": media_type_of(format),
        "source_animated": animated,
        "frames_decoded": 1,
        "orientation_applied": orientation != Orientation::NoTransforms,
        "metadata_stripped": true,
    });
    Ok(LocalProduct::Artifact {
        artifact: LocalArtifact::new(attachment, claim_role, file_name, media_type, bytes)?
            .claimed_by_session(text(input, "claim_session_key"))?,
        metadata,
    })
}

/// The declared box, applied with integer arithmetic so one source produces one
/// output size on every platform.
///
/// Neither policy ever enlarges an image: a thumbnail of something smaller than
/// the box is the thing itself. `Cover` fills the box and crops the overflow
/// from the centre; `Contain` fits the whole picture inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fitted {
    /// What the source is resized to before anything is cut away.
    pub width: u32,
    pub height: u32,
    /// The centred window kept afterwards, when the policy fills the box.
    pub crop: Option<Crop>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

fn fit(target: &ImageTarget, width: u32, height: u32) -> Fitted {
    let (box_width, box_height) = (target.max_width(), target.max_height());
    let (width_64, height_64) = (u64::from(width), u64::from(height));
    match target.fit() {
        ImageFit::Contain => {
            if width <= box_width && height <= box_height {
                return Fitted {
                    width,
                    height,
                    crop: None,
                };
            }
            let scaled = if width_64 * u64::from(box_height) >= height_64 * u64::from(box_width) {
                (
                    box_width,
                    ((height_64 * u64::from(box_width)) / width_64).max(1) as u32,
                )
            } else {
                (
                    ((width_64 * u64::from(box_height)) / height_64).max(1) as u32,
                    box_height,
                )
            };
            Fitted {
                width: scaled.0,
                height: scaled.1,
                crop: None,
            }
        }
        ImageFit::Cover => {
            // Fill: scale so *both* sides reach the box, then crop the excess.
            // Never above 1, so a small source is cropped rather than blown up.
            let (scaled_width, scaled_height) = if width <= box_width || height <= box_height {
                (width, height)
            } else if width_64 * u64::from(box_height) >= height_64 * u64::from(box_width) {
                (
                    ((width_64 * u64::from(box_height)).div_ceil(height_64)).max(1) as u32,
                    box_height,
                )
            } else {
                (
                    box_width,
                    ((height_64 * u64::from(box_width)).div_ceil(width_64)).max(1) as u32,
                )
            };
            let crop_width = scaled_width.min(box_width);
            let crop_height = scaled_height.min(box_height);
            Fitted {
                width: scaled_width,
                height: scaled_height,
                crop: ((crop_width, crop_height) != (scaled_width, scaled_height)).then(|| Crop {
                    x: (scaled_width - crop_width) / 2,
                    y: (scaled_height - crop_height) / 2,
                    width: crop_width,
                    height: crop_height,
                }),
            }
        }
    }
}

/// Encode the pixels, and nothing beside them.
///
/// Neither encoder is given metadata to write, and neither reads any from the
/// image: an `ImageEncoder::write_image` call takes a buffer, a size, and a
/// colour type. That is the whole of what leaves this function.
fn encode(target: &ImageTarget, decoded: &DynamicImage) -> Result<Vec<u8>, ConnectorFailure> {
    let mut bytes = Vec::new();
    let failed = || {
        ConnectorFailure::new(
            ConnectorErrorClass::Invariant,
            "local_image_encode_failed",
            "the produced image could not be encoded",
        )
    };
    match target.format() {
        ImageOutputFormat::Png => {
            let rgba = decoded.to_rgba8();
            image::codecs::png::PngEncoder::new_with_quality(
                Cursor::new(&mut bytes),
                image::codecs::png::CompressionType::Default,
                image::codecs::png::FilterType::Adaptive,
            )
            .write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|_| failed())?;
        }
        ImageOutputFormat::Jpeg => {
            // JPEG has no alpha, so the conversion is explicit rather than left
            // to an encoder's own idea of what to do with one.
            let rgb = decoded.to_rgb8();
            JpegEncoder::new_with_quality(Cursor::new(&mut bytes), target.quality())
                .write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|_| failed())?;
        }
    }
    Ok(bytes)
}

fn text<'a>(input: &'a JsonValue, field: &str) -> Option<&'a str> {
    input.get(field).and_then(JsonValue::as_str)
}

fn unreadable(_error: image::ImageError) -> ConnectorFailure {
    refuse(
        "local_image_header_unreadable",
        "the stored file's header could not be read",
    )
}

/// A source that does not satisfy the target's declaration is a `validation`
/// failure: the same bytes will fail again.
fn refuse(code: &'static str, message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(ConnectorErrorClass::Validation, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::media::MediaCatalog;

    fn target(mutate: impl FnOnce(&mut ImageTargetSpec)) -> std::sync::Arc<ImageTarget> {
        let mut spec = ImageTargetSpec {
            name: "avatar".to_owned(),
            kind: ImageTargetKind::Thumbnail,
            accept: ["image/png".to_owned(), "image/jpeg".to_owned()]
                .into_iter()
                .collect(),
            max_source_bytes: 8 * 1_024 * 1_024,
            max_pixels: 8_000_000,
            max_width: 320,
            max_height: 240,
            fit: ImageFit::Contain,
            format: ImageOutputFormat::Png,
            quality: None,
            animation: ImageAnimation::Reject,
        };
        mutate(&mut spec);
        MediaCatalog::resolve([], [spec])
            .expect("the target resolves")
            .image("avatar")
            .expect("the target is declared")
            .clone()
    }

    /// The box is applied with integer arithmetic, and neither policy enlarges
    /// anything: a thumbnail of a postage stamp is the postage stamp.
    #[test]
    fn a_declared_box_scales_down_and_never_up() {
        let box_only = |width, height| Fitted {
            width,
            height,
            crop: None,
        };
        let contain = target(|_| {});
        assert_eq!(fit(&contain, 640, 480), box_only(320, 240));
        assert_eq!(fit(&contain, 1000, 200), box_only(320, 64));
        assert_eq!(fit(&contain, 200, 1000), box_only(48, 240));
        assert_eq!(fit(&contain, 100, 80), box_only(100, 80), "no upscale");

        let cover = target(|spec| spec.fit = ImageFit::Cover);
        // 640×480 fills a 320×240 box exactly; nothing is cropped.
        assert_eq!(fit(&cover, 640, 480), box_only(320, 240));
        // 1000×200 is too short to cover, so it is left alone and cropped.
        assert_eq!(
            fit(&cover, 1000, 200),
            Fitted {
                width: 1000,
                height: 200,
                crop: Some(Crop {
                    x: 340,
                    y: 0,
                    width: 320,
                    height: 200
                }),
            }
        );
        // 800×800 covers by width and is cropped vertically, from the centre.
        assert_eq!(
            fit(&cover, 800, 800),
            Fitted {
                width: 320,
                height: 320,
                crop: Some(Crop {
                    x: 0,
                    y: 40,
                    width: 320,
                    height: 240
                }),
            }
        );
    }

    /// The frame probe reads a container's own index and answers before any
    /// decoding — including for a truncated or hostile file, where it
    /// terminates rather than looping.
    #[test]
    fn the_frame_probe_reads_the_container_and_terminates() {
        assert!(!is_animated(ImageFormat::Jpeg, &[0xff, 0xd8, 0xff]));
        assert!(!is_animated(ImageFormat::Gif, b"GIF89a"));
        assert!(!is_animated(ImageFormat::Gif, &[]));
        // A GIF whose sub-block chain never terminates still ends the walk.
        let mut truncated = b"GIF89a".to_vec();
        truncated.extend_from_slice(&[1, 0, 1, 0, 0x00, 0, 0]);
        truncated.extend_from_slice(&[0x21, 0xf9, 4, 0, 0, 0, 0]);
        assert!(!is_animated(ImageFormat::Gif, &truncated));
        // A WebP whose VP8X flags declare animation, with no ANIM chunk yet.
        let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
        webp.extend_from_slice(b"VP8X");
        webp.extend_from_slice(&10_u32.to_le_bytes());
        webp.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(is_animated(ImageFormat::WebP, &webp));
    }
}
