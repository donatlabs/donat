//! The renderer's view of a document template: a name, a kind, a frozen file
//! set, and the bounds the declaration tightened.
//!
//! This is deliberately *not* `donat_metadata::DocumentTemplate`. The metadata
//! crate owns YAML, the type system, and the content hash; this crate owns
//! rendering. Keeping the two types apart is what stops a renderer from
//! reaching into a metadata document, and keeps `donat-connectors` free of a
//! dependency on `donat-metadata` — the same separation ADR 044 drew in the
//! other direction. The serving binary, which depends on both, converts.
//!
//! What this module adds on top of the frozen bytes is the half of spec 019 §3
//! that has to happen *at load*: a Typst source is parsed here, and a package
//! import or a path that leaves the template's own set is refused while the
//! deployment is still starting. The [`super::world`] closes the same door at
//! render time, but a template that would have hit it never reaches a process.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use typst::syntax::ast::{self, AstNode};
use typst::syntax::{SyntaxKind, SyntaxNode};

/// What a template renders. Mirrors `donat_metadata::DocumentTemplateKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DocumentKind {
    Pdf,
    Email,
    Spreadsheet,
    Calendar,
}

impl DocumentKind {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::Pdf => "pdf.render",
            Self::Email => "email.render",
            Self::Spreadsheet => "spreadsheet.render",
            Self::Calendar => "calendar.render",
        }
    }
}

/// One template, as the renderer holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentTemplate {
    name: String,
    kind: DocumentKind,
    entry: String,
    files: BTreeMap<String, String>,
    content_hash: String,
    inputs: BTreeSet<String>,
    html_paths: BTreeSet<String>,
    max_pages: Option<u64>,
    cpu_deadline_ms: Option<u64>,
    max_output_bytes: Option<u64>,
}

/// Everything one template declares, as the serving binary hands it over.
#[derive(Debug, Clone, Default)]
pub struct DocumentTemplateSpec {
    pub name: String,
    pub kind: Option<DocumentKind>,
    pub entry: String,
    pub files: BTreeMap<String, String>,
    pub content_hash: String,
    pub inputs: BTreeSet<String>,
    pub html_paths: BTreeSet<String>,
    pub max_pages: Option<u64>,
    pub cpu_deadline_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
}

/// One refusal from resolving a template set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateRejection {
    pub template: String,
    pub message: String,
}

impl TemplateRejection {
    fn new(template: &str, message: impl Into<String>) -> Self {
        Self {
            template: template.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TemplateRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "template `{}`: {}", self.template, self.message)
    }
}

impl DocumentTemplate {
    /// Resolve one declaration, running every check that belongs at load.
    pub fn resolve(spec: DocumentTemplateSpec) -> Result<Self, TemplateRejection> {
        let name = spec.name.clone();
        let reject = |message: &str| TemplateRejection::new(&name, message);
        let kind = spec
            .kind
            .ok_or_else(|| reject("a template declares which of the four kinds it renders"))?;
        if spec.name.is_empty() {
            return Err(reject("a template has a name"));
        }
        if !spec.files.contains_key(&spec.entry) {
            return Err(reject(
                "a template's entry file is part of its own file set",
            ));
        }
        if spec.content_hash.len() != 64 {
            return Err(reject(
                "a template carries the hash of the bytes it was loaded from",
            ));
        }

        // Spec 019 §3, the load-time half. A Typst template names the files it
        // reads and the packages it imports in its source, so both are decided
        // here, once, rather than at every render.
        if kind == DocumentKind::Pdf {
            for (path, text) in &spec.files {
                check_typst_closure(&spec.files, path, text)
                    .map_err(|message| TemplateRejection::new(&name, message))?;
            }
        }

        Ok(Self {
            name: spec.name,
            kind,
            entry: spec.entry,
            files: spec.files,
            content_hash: spec.content_hash,
            inputs: spec.inputs,
            html_paths: spec.html_paths,
            max_pages: spec.max_pages,
            cpu_deadline_ms: spec.cpu_deadline_ms,
            max_output_bytes: spec.max_output_bytes,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> DocumentKind {
        self.kind
    }

    pub fn entry(&self) -> &str {
        &self.entry
    }

    pub const fn files(&self) -> &BTreeMap<String, String> {
        &self.files
    }

    /// The text of one file in the frozen set, or `None` — there is no other
    /// answer, and no path that leads anywhere else.
    pub fn file(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(String::as_str)
    }

    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    pub const fn inputs(&self) -> &BTreeSet<String> {
        &self.inputs
    }

    /// Dotted input paths whose value is already HTML (spec 019 §4).
    pub const fn html_paths(&self) -> &BTreeSet<String> {
        &self.html_paths
    }

    pub const fn max_pages(&self) -> Option<u64> {
        self.max_pages
    }

    pub const fn cpu_deadline_ms(&self) -> Option<u64> {
        self.cpu_deadline_ms
    }

    pub const fn max_output_bytes(&self) -> Option<u64> {
        self.max_output_bytes
    }
}

/// The deployment's resolved templates, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentTemplateSet {
    by_name: BTreeMap<String, Arc<DocumentTemplate>>,
}

impl DocumentTemplateSet {
    /// Resolve a whole set. Every rejection is collected, because a deployment
    /// with three broken templates should learn about three of them.
    pub fn resolve(
        specs: impl IntoIterator<Item = DocumentTemplateSpec>,
    ) -> Result<Self, Vec<TemplateRejection>> {
        let mut by_name = BTreeMap::new();
        let mut rejections = Vec::new();
        for spec in specs {
            let name = spec.name.clone();
            match DocumentTemplate::resolve(spec) {
                Ok(template) => {
                    if by_name.insert(name.clone(), Arc::new(template)).is_some() {
                        rejections.push(TemplateRejection::new(&name, "is declared twice"));
                    }
                }
                Err(rejection) => rejections.push(rejection),
            }
        }
        if rejections.is_empty() {
            Ok(Self { by_name })
        } else {
            Err(rejections)
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<DocumentTemplate>> {
        self.by_name.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

// ---------------------------------------------------------------------------
// The load-time closure check
// ---------------------------------------------------------------------------

/// The functions whose first string argument is a path into the project.
const PATH_ARGUMENTS: &[&str] = &[
    "read",
    "image",
    "csv",
    "json",
    "yaml",
    "xml",
    "toml",
    "cbor",
    "bibliography",
    "raw",
];

/// Refuse a Typst source that imports a package or names a file the frozen set
/// does not contain.
///
/// This is a *lexical* check over the parsed source, which is exactly the right
/// strength for the two things spec 019 §3 asks to fail at load: a package
/// import and a path literal are both written in the template. A path a
/// template computes at render time is not covered here and does not need to
/// be — [`super::world`] has no way to answer it either.
fn check_typst_closure(
    files: &BTreeMap<String, String>,
    path: &str,
    text: &str,
) -> Result<(), String> {
    let root = typst::syntax::parse(text);
    let directory = path.rsplit_once('/').map_or("", |(head, _)| head);
    let mut error = None;
    walk(&root, &mut |node| {
        if error.is_some() {
            return;
        }
        let referenced = match node.kind() {
            SyntaxKind::ModuleImport => node
                .cast::<ast::ModuleImport>()
                .and_then(|import| string_of(import.source().to_untyped())),
            SyntaxKind::ModuleInclude => node
                .cast::<ast::ModuleInclude>()
                .and_then(|include| string_of(include.source().to_untyped())),
            SyntaxKind::FuncCall => node
                .cast::<ast::FuncCall>()
                .filter(|call| {
                    matches!(call.callee(), ast::Expr::Ident(ident)
                        if PATH_ARGUMENTS.contains(&ident.as_str()))
                })
                .and_then(|call| {
                    call.args().items().find_map(|argument| match argument {
                        ast::Arg::Pos(ast::Expr::Str(value)) => Some(value.get().to_string()),
                        _ => None,
                    })
                }),
            _ => None,
        };
        let Some(referenced) = referenced else {
            return;
        };
        // A package import is refused by name, so the message says what was
        // wrong rather than "no such file": there is no registry, no network,
        // and no cache directory to point an operator at.
        if referenced.starts_with('@') {
            error = Some(format!(
                "`{referenced}` is a package import. A template renders from the files it \
                 declares; there is no package registry, no network, and no cache directory"
            ));
            return;
        }
        let Some(resolved) = resolve_reference(directory, &referenced) else {
            error = Some(format!("`{referenced}` leaves the template's own file set"));
            return;
        };
        if !files.contains_key(&resolved) {
            error = Some(format!(
                "`{referenced}` resolves to `{resolved}`, which the template does not declare"
            ));
        }
    });
    match error {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

fn walk(node: &SyntaxNode, visit: &mut impl FnMut(&SyntaxNode)) {
    visit(node);
    for child in node.children() {
        walk(child, visit);
    }
}

fn string_of(node: &SyntaxNode) -> Option<String> {
    node.cast::<ast::Str>().map(|value| value.get().to_string())
}

/// Resolve a reference the way Typst does — relative to the referring file's
/// directory, `/` from the project root — and return `None` when it escapes.
pub(crate) fn resolve_reference(directory: &str, reference: &str) -> Option<String> {
    let (base, rest) = match reference.strip_prefix('/') {
        Some(rest) => (Vec::new(), rest),
        None => (
            directory
                .split('/')
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect(),
            reference,
        ),
    };
    let mut segments = base;
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            part => segments.push(part.to_owned()),
        }
    }
    if segments.is_empty() {
        return None;
    }
    Some(format!("/{}", segments.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(entry: &str, files: &[(&str, &str)]) -> DocumentTemplateSpec {
        DocumentTemplateSpec {
            name: "probe".to_owned(),
            kind: Some(DocumentKind::Pdf),
            entry: entry.to_owned(),
            files: files
                .iter()
                .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
                .collect(),
            content_hash: "0".repeat(64),
            ..Default::default()
        }
    }

    /// A reference is resolved the way Typst resolves it, and one that walks
    /// out of the root has no answer at all.
    #[test]
    fn a_reference_resolves_inside_the_root_or_nowhere() {
        assert_eq!(
            resolve_reference("/partials", "totals.typ").as_deref(),
            Some("/partials/totals.typ")
        );
        assert_eq!(
            resolve_reference("/partials", "../invoice.typ").as_deref(),
            Some("/invoice.typ")
        );
        assert_eq!(
            resolve_reference("", "/logo.svg").as_deref(),
            Some("/logo.svg")
        );
        assert_eq!(resolve_reference("", "../../etc/passwd"), None);
        assert_eq!(resolve_reference("/partials", "../../../etc/passwd"), None);
    }

    /// An include inside the set resolves; every other shape is refused before
    /// the template becomes a template.
    #[test]
    fn a_pdf_template_is_closed_when_it_resolves() {
        assert!(
            DocumentTemplate::resolve(spec(
                "/invoice.typ",
                &[
                    ("/invoice.typ", "#include \"partials/totals.typ\"\n"),
                    ("/partials/totals.typ", "#let total = 1\n"),
                ]
            ))
            .is_ok()
        );
        for (source, expected) in [
            ("#import \"@preview/cetz:0.3.0\": *\n", "package import"),
            (
                "#import \"../../secrets.typ\": key\n",
                "leaves the template",
            ),
            ("#include \"missing.typ\"\n", "does not declare"),
            ("#read(\"/etc/passwd\")\n", "does not declare"),
        ] {
            let rejection =
                DocumentTemplate::resolve(spec("/invoice.typ", &[("/invoice.typ", source)]))
                    .expect_err("a template that reaches outside its set is not a template");
            assert!(
                rejection.message.contains(expected),
                "`{source}` must be refused for {expected}: {rejection}"
            );
        }
    }
}
