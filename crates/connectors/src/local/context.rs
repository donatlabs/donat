//! What a local capability gets from the deployment, resolved at boot and
//! frozen.
//!
//! ADR 044 made a local operation a static declaration whose executor is a
//! plain `fn` pointer: nothing about a capability is decided at request time.
//! Spec 019 then adds the one thing that *is* a deployment decision — the
//! document templates — and it cannot travel as input, because "a template
//! cannot be supplied, selected by path, or modified through any request"
//! (spec 019 §2). Input names a template; it never carries one.
//!
//! So it travels beside the input instead. A [`LocalContext`] is built once
//! from metadata, before a listener opens, and handed to every execution. It is
//! immutable: an executor can read a template out of it and can add nothing to
//! it, which is what keeps "the file set is frozen" true at the type level
//! rather than by convention.
//!
//! [`LocalContext::builtin`] is the context registration runs in. An operation
//! proves its determinism against templates compiled into this binary, so the
//! double render of ADR 044 stays a property of the binary and never depends on
//! what a deployment happened to declare.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use crate::local::document::{DocumentTemplateSet, builtin_templates};
use crate::local::ingest::{IngestSchemaSet, SourceFile, builtin_schemas, builtin_sources};
use crate::local::media::MediaCatalog;

/// The deployment's resolved capability context.
#[derive(Debug, Clone, Default)]
pub struct LocalContext {
    templates: Arc<DocumentTemplateSet>,
    media: Arc<MediaCatalog>,
    ingest_schemas: Arc<IngestSchemaSet>,
    recurrence_policies: Arc<crate::local::recurrence::RecurrencePolicySet>,
    /// Stored files resolved for *this* execution, by the handle its input
    /// names.
    ///
    /// Unlike every other field, this one is per-execution rather than
    /// per-deployment: spec 020's input is a file a user uploaded, and its
    /// bytes travel beside the input for the reason ADR 050 gave for a
    /// template's bytes — inside the input they would be measured by the input
    /// ceiling, retained by the journal, and hashed by the determinism probe.
    sources: Arc<BTreeMap<String, Arc<SourceFile>>>,
}

impl LocalContext {
    /// Build a context over one resolved template set.
    pub fn new(templates: DocumentTemplateSet) -> Self {
        Self {
            templates: Arc::new(templates),
            ..Default::default()
        }
    }

    /// Add the deployment's resolved media declarations (spec 022): the code
    /// templates `local.code` renders and the image targets `local.image`
    /// re-encodes into.
    ///
    /// It is a separate argument from the input for the same reason a document
    /// template is: an allowed-origin list means something only while it is the
    /// part of the path the party supplying the payload does not control.
    #[must_use]
    pub fn with_media(mut self, media: MediaCatalog) -> Self {
        self.media = Arc::new(media);
        self
    }

    /// Add the deployment's resolved ingest schemas (spec 020 §2).
    ///
    /// A schema is a declaration for the same reason a template is: there is no
    /// inference, so the columns a file is read with have to come from
    /// somewhere the uploader does not control.
    #[must_use]
    pub fn with_ingest_schemas(mut self, schemas: IngestSchemaSet) -> Self {
        self.ingest_schemas = Arc::new(schemas);
        self
    }

    /// Add the deployment's resolved recurrence policies (spec 021 §3): the
    /// zone a rule's wall-clock times are read in, what the rule does at the
    /// two local times a DST transition breaks, and the window and occurrence
    /// ceilings every expansion is bounded by.
    ///
    /// It is a separate argument from the input for the same reason a document
    /// template is. A DST policy is a promise the deployment made about when
    /// things happen; a run that could supply its own would be making that
    /// promise itself, one expansion at a time.
    #[must_use]
    pub fn with_recurrence(
        mut self,
        policies: crate::local::recurrence::RecurrencePolicySet,
    ) -> Self {
        self.recurrence_policies = Arc::new(policies);
        self
    }

    /// The same context, with one stored file bound to the handle its input
    /// names.
    ///
    /// It clones cheaply — everything else is behind an `Arc` — so the
    /// dispatcher builds one of these per execution without rebuilding what the
    /// deployment declared.
    #[must_use]
    pub fn with_source(&self, source: SourceFile) -> Self {
        let mut sources = (*self.sources).clone();
        sources.insert(source.handle().to_owned(), Arc::new(source));
        Self {
            sources: Arc::new(sources),
            ..self.clone()
        }
    }

    /// The context registration and the determinism probes run in: the
    /// templates compiled into this binary, and nothing a deployment declared.
    pub fn builtin() -> &'static Self {
        static BUILTIN: LazyLock<LocalContext> = LazyLock::new(|| {
            let context = LocalContext::new(builtin_templates())
                .with_media(
                    MediaCatalog::resolve(
                        crate::local::code::builtin_code_templates(),
                        crate::local::image::builtin_image_targets(),
                    )
                    .expect("the built-in probe declarations are static and closed"),
                )
                .with_ingest_schemas(builtin_schemas())
                .with_recurrence(
                    crate::local::recurrence::RecurrencePolicySet::resolve(
                        crate::local::recurrence::builtin_policies(),
                    )
                    .expect("the built-in probe policies are static and closed"),
                );
            builtin_sources()
                .into_iter()
                .chain(crate::local::image::builtin_image_sources())
                .fold(context, |context, source| context.with_source(source))
        });
        &BUILTIN
    }

    /// The frozen template set.
    pub fn templates(&self) -> &DocumentTemplateSet {
        &self.templates
    }

    /// The frozen media declarations.
    pub fn media(&self) -> &MediaCatalog {
        &self.media
    }

    /// The frozen ingest schemas.
    pub fn ingest_schemas(&self) -> &IngestSchemaSet {
        &self.ingest_schemas
    }

    /// The frozen recurrence policies.
    pub fn recurrence_policies(&self) -> &crate::local::recurrence::RecurrencePolicySet {
        &self.recurrence_policies
    }

    /// The stored file one handle names, for this execution only.
    pub fn source(&self, handle: &str) -> Option<&SourceFile> {
        self.sources.get(handle).map(AsRef::as_ref)
    }
}
