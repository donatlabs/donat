//! The only fonts the PDF renderer ever sees.
//!
//! Spec 019 §3 makes this a determinism property rather than a packaging
//! convenience: a renderer that discovers fonts from the operating system
//! produces one invoice on a developer's laptop and a different one on the base
//! image, and `Pure` is admitted on two renders being byte-identical. So the
//! set is compiled in with `include_bytes!`, and there is no code path in this
//! crate that reads a font from anywhere else — not a directory, not an
//! environment variable, not a fontconfig call.
//!
//! Two families, both under the SIL Open Font License 1.1, both recorded in the
//! root `THIRD_PARTY_NOTICES.md` with their version and full license text: a
//! font is not covered by its renderer's license.
//!
//! * **Liberation Sans** — the text family.
//! * **Liberation Mono** — the monospace family.
//!
//! A glyph neither family has renders as the font's own notdef and is counted
//! into the capability's typed warnings by [`super::pdf`]; it is never silently
//! dropped.

use std::sync::LazyLock;

use typst::foundations::Bytes;
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;

/// The family a template gets when it sets no font of its own.
pub const DEFAULT_TEXT_FAMILY: &str = "Liberation Sans";

/// The family a template's `raw` blocks fall back to.
pub const DEFAULT_MONO_FAMILY: &str = "Liberation Mono";

/// The embedded font files, in book order.
///
/// The order is part of the output: a font book index is what the compiler
/// hands back to [`crate::local::document::world::ClosedWorld::font`], so
/// reordering this array reorders glyph selection for a template that names no
/// family. It is therefore written out rather than globbed.
const EMBEDDED: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf"),
    include_bytes!("../../../assets/fonts/LiberationSans-Bold.ttf"),
    include_bytes!("../../../assets/fonts/LiberationSans-Italic.ttf"),
    include_bytes!("../../../assets/fonts/LiberationSans-BoldItalic.ttf"),
    include_bytes!("../../../assets/fonts/LiberationMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/LiberationMono-Bold.ttf"),
    include_bytes!("../../../assets/fonts/LiberationMono-Italic.ttf"),
    include_bytes!("../../../assets/fonts/LiberationMono-BoldItalic.ttf"),
];

/// The compiled font set: the loaded faces and the book that indexes them.
pub struct EmbeddedFonts {
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
}

impl EmbeddedFonts {
    pub const fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    pub fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    /// Whether every character of `text` has a glyph in some embedded face.
    ///
    /// The answer feeds the capability's typed warnings: a missing glyph is
    /// reported rather than discovered by whoever opens the PDF.
    pub fn covers(&self, text: &str) -> bool {
        text.chars()
            .filter(|character| !character.is_whitespace() && !character.is_control())
            .all(|character| {
                self.fonts
                    .iter()
                    .any(|font| font.info().coverage.contains(character as u32))
            })
    }
}

/// The one font set, built once.
pub fn embedded() -> &'static EmbeddedFonts {
    static FONTS: LazyLock<EmbeddedFonts> = LazyLock::new(|| {
        let mut book = FontBook::new();
        let mut fonts = Vec::with_capacity(EMBEDDED.len());
        for bytes in EMBEDDED {
            let buffer = Bytes::new(*bytes);
            // `Font::iter` rather than `Font::new(.., 0)`: a collection file
            // carries several faces, and taking only the first would silently
            // drop the rest.
            for font in Font::iter(buffer) {
                book.push(font.info().clone());
                fonts.push(font);
            }
        }
        EmbeddedFonts {
            book: LazyHash::new(book),
            fonts,
        }
    });
    &FONTS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The set is what the module says it is, and both families are in it. A
    /// font that failed to parse would silently shrink the book, so the count
    /// is asserted rather than assumed.
    #[test]
    fn both_embedded_families_load() {
        let fonts = embedded();
        assert_eq!(fonts.len(), EMBEDDED.len());
        for family in [DEFAULT_TEXT_FAMILY, DEFAULT_MONO_FAMILY] {
            assert!(
                fonts
                    .book()
                    .families()
                    .any(|(name, _)| name.eq_ignore_ascii_case(family)),
                "{family} must be in the embedded book"
            );
        }
    }

    /// Coverage is a real question with a real answer, which is what makes the
    /// missing-glyph warning worth reporting.
    #[test]
    fn coverage_answers_for_the_glyphs_the_families_have() {
        let fonts = embedded();
        assert!(fonts.covers("Invoice A-1 — €12.50"));
        assert!(!fonts.covers("発"), "a CJK glyph is not in either family");
    }
}
