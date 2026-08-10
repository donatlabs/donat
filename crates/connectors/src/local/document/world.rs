//! The closed [`typst::World`] (spec 019 §3).
//!
//! Typst's template language can import packages and read files, so the world a
//! template is compiled in *is* the sandbox. Every one of the trait's seven
//! methods is implemented here, and each one is closed by construction rather
//! than by a check that could be forgotten:
//!
//! | Method | What it can reach |
//! |---|---|
//! | `library` | the standard library, with `sys.inputs` set from declared input |
//! | `book`, `font` | only [`super::fonts`], which is `include_bytes!` |
//! | `main`, `source`, `file` | only the template's frozen file set |
//! | `today` | the timestamp the input declared, never a clock |
//!
//! There is no filesystem handle, no package storage, no HTTP client, and no
//! environment lookup in this type — not disabled, absent. `source` and `file`
//! answer from a `BTreeMap` that was populated before the process opened a
//! listener; a path outside it has no answer other than [`FileError::NotFound`],
//! and a `FileId` carrying a package root is refused with
//! [`FileError::Package`] before its path is even looked at.
//!
//! Two of these deserve their reasoning written down.
//!
//! *`today` returns what the input said.* Typst's `datetime.today()` and the
//! PDF creation date both flow from here. Reading the real clock would make two
//! renders of one invoice differ in their bytes, which is exactly what `Pure`
//! forbids — so the clock is an input, and a template that wants today's date
//! gets the day the process decided on.
//!
//! *File ids are interned per template.* `FileId` is a globally interned,
//! 16-bit handle in `typst-syntax`, so a world that minted a fresh id per
//! render would exhaust the interner in a long-running deployment. Paths here
//! are `/<template>/<path in the template>`, which is bounded by the
//! deployment's own declarations and stable across renders.

use std::collections::HashMap;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Dict, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::FontBook;
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};

use super::fonts::{self, DEFAULT_TEXT_FAMILY};
use super::template::DocumentTemplate;

/// One compilation's world: one template, one input dictionary, one date.
pub struct ClosedWorld {
    library: LazyHash<Library>,
    main: FileId,
    sources: HashMap<FileId, Source>,
    today: Option<Datetime>,
}

impl ClosedWorld {
    /// Build the world for one render.
    ///
    /// Every file of the template is interned and parsed here, up front: after
    /// this constructor returns there is nothing left to resolve, which is the
    /// operational meaning of "resolved at boot and frozen".
    pub fn new(template: &DocumentTemplate, inputs: Dict, today: Option<Datetime>) -> Self {
        let mut library = Library::builder().with_inputs(inputs).build();
        // A template that names no family gets the embedded text family rather
        // than the compiler's default, which is a font this binary does not
        // carry. Without this a plain template renders through a fallback and
        // an "unknown font family" warning on every page.
        library
            .styles
            .set(typst::text::TextElem::font, default_font_list());

        let mut sources = HashMap::new();
        for (path, text) in template.files() {
            let id = file_id(template.name(), path);
            sources.insert(id, Source::new(id, text.clone()));
        }
        let main = file_id(template.name(), template.entry());
        Self {
            library: LazyHash::new(library),
            main,
            sources,
            today,
        }
    }

    /// The interned id of one file of one template.
    ///
    /// Templates get their own directory inside the virtual root so two
    /// templates that both call their entry `main.typ` stay distinct, and so a
    /// path that resolves out of the template's directory lands somewhere the
    /// set does not contain.
    fn identify(&self, id: FileId) -> FileResult<&Source> {
        if let VirtualRoot::Package(package) = id.root() {
            // The template never named a package — the load-time check refused
            // that — so reaching here means the compiler synthesised one. There
            // is nothing to fetch it from either way.
            return Err(FileError::Package(typst::diag::PackageError::Other(Some(
                format!("`{package}` is not available: this renderer has no package registry")
                    .into(),
            ))));
        }
        self.sources.get(&id).ok_or_else(|| {
            FileError::NotFound(std::path::PathBuf::from(id.vpath().get_with_slash()))
        })
    }
}

fn file_id(template: &str, path: &str) -> FileId {
    let vpath = VirtualPath::new(format!("/{template}{path}"))
        .expect("a template path is a normalized virtual path");
    RootedPath::new(VirtualRoot::Project, vpath).intern()
}

fn default_font_list() -> typst::text::FontList {
    typst::text::FontList(vec![typst::text::FontFamily::new(DEFAULT_TEXT_FAMILY)])
}

impl World for ClosedWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        fonts::embedded().book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.identify(id).cloned()
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        // Bytes and sources are the same map: a template may `read()` a file it
        // declared, and nothing else exists to be read.
        self.identify(id)
            .map(|source| Bytes::from_string(source.text().to_owned()))
    }

    fn font(&self, index: usize) -> Option<typst::text::Font> {
        fonts::embedded().font(index)
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        // The offset is ignored on purpose: the declared timestamp is already
        // the instant the process chose, and shifting it by a zone the template
        // asked for would make one input render two ways.
        self.today
    }
}
