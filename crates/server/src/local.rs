//! Dispatch for local capabilities (spec 018).
//!
//! A `local.*` activity is the same durable activity a connector activity is —
//! same claim, same retry policy, same closed failure classes, same output
//! contract (`knowledgebase/declarative-saas/decisions/029-*`). What differs is
//! everything on the other side of the call: there is no origin, no credential,
//! no request, and no provider. The work happens here, in this process, on a
//! thread that is not one of the async runtime's.
//!
//! Three things this module is careful about.
//!
//! **It runs off the reactor.** A capability is CPU work that does not yield;
//! running it on a worker thread would stall every request, subscription, and
//! timer that thread was also driving. It goes to the blocking pool, and the
//! test that proves it drives a ticker on a single-worker runtime while a
//! capability blocks.
//!
//! **It drains.** A blocking task cannot be cancelled, so shutdown is
//! cooperative: the deployment's `stopping` token is mirrored into the
//! capability's [`StopSignal`], the implementation observes it at its own
//! checkpoints, and the dispatcher *waits* for the thread rather than
//! abandoning it. A drained execution fails `timeout` with
//! `local_capability_drained`, which is retryable — so the activity survives
//! the deploy instead of being lost with the replica
//! (`knowledgebase/operations/decisions/001-*`).
//!
//! **Bytes go to storage, never into the result.** A capability that produced
//! bytes hands back a [`LocalArtifact`]; this module writes it through
//! `crates/storage` into the attachment store and puts the stored file's
//! identity into the activity output, exactly as an uploaded attachment would
//! appear (`knowledgebase/declarative-saas/decisions/033-*`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use donat_connectors::local::document::{DocumentKind, DocumentTemplateSet, DocumentTemplateSpec};
use donat_connectors::local::ingest::{
    IngestColumnSpec, IngestKind, IngestSchemaSet, IngestSchemaSpec, MAX_SOURCE_BYTES,
    RowErrorPolicy, SourceFile,
};
use donat_connectors::local::media::{
    CodeDelivery, CodeErrorCorrection, CodeFormat, CodePayloadType, CodeTemplateSpec,
    ImageAnimation, ImageFit, ImageOutputFormat, ImageTargetKind, ImageTargetSpec, MediaCatalog,
    Symbology,
};
use donat_connectors::local::recurrence::{
    DstPolicy, RecurrencePolicySet, RecurrencePolicySpec, RepeatedTime, SkippedTime,
};
use donat_connectors::local::{
    LocalArtifact, LocalCapability, LocalContext, LocalProduct, StopSignal, capability,
};
use donat_metadata::{
    CodeDelivery as MetadataCodeDelivery, CodeErrorCorrection as MetadataErrorCorrection,
    CodeFormat as MetadataCodeFormat, CodePayloadType as MetadataPayloadType, DocumentTemplateKind,
    DstRepeatedTime, DstSkippedTime, ImageAnimation as MetadataImageAnimation,
    ImageFit as MetadataImageFit, ImageOutputFormat as MetadataImageFormat,
    ImageTargetKind as MetadataImageKind, IngestRowErrorPolicy, IngestSchemaKind,
    LocalCapabilityCatalog, LocalCapabilityError, Metadata, Symbology as MetadataSymbology,
    is_local, parse_window_seconds, validate_local_capabilities,
};
use donat_storage::{Backend, StorageRegistry};
use futures_util::future::BoxFuture;
use serde_json::{Value as JsonValue, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::connectors::{
    ConnectorErrorClass, ConnectorFailure, ConnectorSuccess, canonical_json_sha256,
};

/// How long a produced artifact waits to be claimed by the write that binds it.
///
/// An interactive upload gets minutes, because a person is holding the other
/// end of it. A produced artifact is claimed by a durable process, which is a
/// thing measured in hours: a process paused on a wait, a retry that backs off,
/// an approval that arrives tomorrow morning. Rows nobody claims are removed by
/// the collector, so the cost of the longer window is storage, not correctness.
const ARTIFACT_CLAIM_WINDOW_SECONDS: i64 = 24 * 60 * 60;

/// How long the presigned PUT that writes a produced artifact stays valid.
const ARTIFACT_PUT_TTL_SECONDS: u32 = 120;

/// A stored artifact, as the activity result names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredArtifact {
    pub id: Uuid,
    pub file_name: String,
    pub media_type: String,
    pub byte_size: i64,
}

/// Where produced bytes go.
///
/// A port rather than a concrete type, because the two things the production
/// implementation needs — an object store and a database — are exactly the two
/// things a test of the *dispatch* should not need. The production
/// implementation is [`StorageArtifactStore`].
pub trait ArtifactStore: Send + Sync {
    fn store<'a>(
        &'a self,
        artifact: &'a LocalArtifact,
    ) -> BoxFuture<'a, Result<StoredArtifact, ConnectorFailure>>;
}

/// Where a stored file a capability reads comes from.
///
/// A port beside [`ArtifactStore`], and for the same reason: the production
/// implementation needs an object store and a database, which are the two
/// things a test of the *dispatch* should not need. The production
/// implementation is [`StorageArtifactStore`], which already holds the object
/// store and the pool this needs.
///
/// `max_bytes` is spec 020's first bound, and it is applied here rather than
/// only in the capability: the size a file column already recorded is known
/// before a byte is fetched, so a file over the ceiling is refused without
/// being downloaded at all.
pub trait SourceStore: Send + Sync {
    fn fetch<'a>(
        &'a self,
        id: Uuid,
        max_bytes: u64,
    ) -> BoxFuture<'a, Result<SourceFile, ConnectorFailure>>;
}

/// The store a deployment that wired none gets.
///
/// It refuses rather than returning nothing, because "this deployment cannot
/// read stored files" is an invariant an operator has to see, not a read that
/// quietly finds an empty file.
pub struct UnavailableSourceStore;

impl SourceStore for UnavailableSourceStore {
    fn fetch<'a>(
        &'a self,
        _id: Uuid,
        _max_bytes: u64,
    ) -> BoxFuture<'a, Result<SourceFile, ConnectorFailure>> {
        Box::pin(async move {
            Err(ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "local_source_store_unavailable",
                "this deployment cannot read stored files for a local capability",
            ))
        })
    }
}

/// One deployment-enabled local capability instance.
struct LocalInstance {
    capability: &'static LocalCapability,
    enabled: BTreeSet<String>,
}

/// The compiled capability table, as metadata validation sees it.
pub struct CompiledCapabilities;

impl LocalCapabilityCatalog for CompiledCapabilities {
    fn operations(&self, name: &str) -> Option<Vec<String>> {
        capability(name).map(|capability| {
            capability
                .operations()
                .iter()
                .map(|operation| operation.id().to_owned())
                .collect()
        })
    }

    fn cpu_deadline_ms(&self, name: &str, operation: &str) -> Option<u64> {
        capability(name)?
            .operation(operation)
            .map(|operation| operation.bounds().cpu_deadline().as_millis() as u64)
    }
}

/// Immutable lookup table of deployment-enabled local capabilities.
pub struct LocalCapabilityRegistry {
    instances: BTreeMap<String, LocalInstance>,
    /// The deployment's resolved capability context, built once at boot.
    ///
    /// It is what carries the document templates of spec 019: read from the
    /// metadata directory, frozen into a file set, and handed to every
    /// execution beside its input. Nothing a request carries can add to it.
    context: LocalContext,
    artifacts: Arc<dyn ArtifactStore>,
    /// Where a stored file an ingest activity names is read from.
    sources: Arc<dyn SourceStore>,
    /// The `stopping` half of [`crate::shutdown::Shutdown`].
    shutdown: CancellationToken,
}

impl LocalCapabilityRegistry {
    /// Resolve and validate the deployment's `local.*` instances before a
    /// listener opens. Every refusal is a metadata path and a message.
    pub fn build(
        metadata: &Metadata,
        artifacts: Arc<dyn ArtifactStore>,
        shutdown: CancellationToken,
    ) -> Result<Self, Vec<LocalCapabilityError>> {
        let mut errors = validate_local_capabilities(metadata, &CompiledCapabilities);
        // The renderer's own view of the templates. Metadata already checked
        // the declarations against the type system and the processes that bind
        // them; what is checked here is what only the renderer knows — that a
        // Typst source names no package and no file outside its own set.
        let templates =
            match DocumentTemplateSet::resolve(metadata.templates.iter().map(template_spec)) {
                Ok(templates) => templates,
                Err(rejections) => {
                    errors.extend(
                        rejections
                            .into_iter()
                            .map(|rejection| LocalCapabilityError {
                                path: format!("templates.{}", rejection.template),
                                message: rejection.message,
                            }),
                    );
                    DocumentTemplateSet::default()
                }
            };
        // The reader's own view of the ingest schemas. Metadata already checked
        // the declarations and the activities that select them; what is checked
        // here is what only the reader knows — that every declared column
        // resolves to a scalar a cell can become.
        let schemas = match IngestSchemaSet::resolve(
            metadata.ingest_schemas.iter().map(ingest_schema_spec),
        ) {
            Ok(schemas) => schemas,
            Err(rejections) => {
                errors.extend(
                    rejections
                        .into_iter()
                        .map(|rejection| LocalCapabilityError {
                            path: format!("schemas.{}", rejection.schema),
                            message: rejection.message,
                        }),
                );
                IngestSchemaSet::default()
            }
        };
        // The renderer's and the decoder's own view of the media declarations.
        // Metadata already checked the grammar and the activities that select
        // them; what is checked here is what only this side knows — that every
        // declared origin canonicalizes, and that every accepted media type has
        // a decoder linked into this binary (spec 022 §1 and §2).
        let media = match MediaCatalog::resolve(
            metadata.media.codes.iter().map(code_template_spec),
            metadata.media.images.iter().map(image_target_spec),
        ) {
            Ok(media) => media,
            Err(rejections) => {
                errors.extend(
                    rejections
                        .into_iter()
                        .map(|rejection| LocalCapabilityError {
                            path: format!("media.{}", rejection.declaration),
                            message: rejection.message,
                        }),
                );
                MediaCatalog::default()
            }
        };
        // The expander's own view of the recurrence policies. Metadata already
        // checked the grammar, the zone name, the DST pairing and the ceilings;
        // what is checked here is what only this side knows — that a declared
        // ceiling is one this binary's compiled bounds can actually hold
        // (spec 021 §3).
        let recurrence = match RecurrencePolicySet::resolve(
            metadata.recurrence.policies.iter().map(recurrence_spec),
        ) {
            Ok(recurrence) => recurrence,
            Err(rejections) => {
                errors.extend(
                    rejections
                        .into_iter()
                        .map(|rejection| LocalCapabilityError {
                            path: format!("recurrence.{}", rejection.policy),
                            message: rejection.message,
                        }),
                );
                RecurrencePolicySet::default()
            }
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut instances = BTreeMap::new();
        for instance in &metadata.connectors {
            if !is_local(&instance.module) {
                continue;
            }
            let capability = capability(&instance.module)
                .expect("validation refused every capability this binary lacks");
            instances.insert(
                instance.name.clone(),
                LocalInstance {
                    capability,
                    enabled: instance
                        .operations
                        .iter()
                        .map(|operation| operation.name.clone())
                        .collect(),
                },
            );
        }
        Ok(Self {
            instances,
            context: LocalContext::new(templates)
                .with_ingest_schemas(schemas)
                .with_media(media)
                .with_recurrence(recurrence),
            artifacts,
            sources: Arc::new(UnavailableSourceStore),
            shutdown,
        })
    }

    /// An empty registry, for the deployments and tests that enable none.
    pub fn empty(artifacts: Arc<dyn ArtifactStore>, shutdown: CancellationToken) -> Self {
        Self {
            instances: BTreeMap::new(),
            context: LocalContext::default(),
            artifacts,
            sources: Arc::new(UnavailableSourceStore),
            shutdown,
        }
    }

    /// Wire where stored files are read from.
    ///
    /// It is a separate step from [`Self::build`] because a source store is a
    /// source-local thing — an object store and one Postgres pool — while the
    /// registry is built from metadata that is not.
    #[must_use]
    pub fn with_sources(mut self, sources: Arc<dyn SourceStore>) -> Self {
        self.sources = sources;
        self
    }

    /// Whether this registry answers for a connector name at all.
    pub fn handles(&self, instance: &str) -> bool {
        self.instances.contains_key(instance)
    }

    /// Execute one enabled operation of one enabled capability.
    ///
    /// `deadline` is the activity's own: what the capability gets is the
    /// smaller of it and the operation's declared `cpu_deadline`.
    pub async fn execute(
        &self,
        instance: &str,
        operation: &str,
        input: JsonValue,
        deadline: tokio::time::Instant,
    ) -> Result<ConnectorSuccess, ConnectorFailure> {
        let Some(local) = self.instances.get(instance) else {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "connector_invariant",
                "local capability instance is not declared",
            ));
        };
        if !local.enabled.contains(operation) {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "connector_invariant",
                "local capability operation is not enabled by this deployment",
            ));
        }
        let Some(declared) = local.capability.operation(operation) else {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "connector_invariant",
                "local capability operation is not compiled into this binary",
            ));
        };

        // A draining replica does not start work it would have to abandon. The
        // class is retryable, so the activity is picked up by a replica that is
        // not going away.
        if self.shutdown.is_cancelled() {
            return Err(draining());
        }
        let fingerprint = canonical_json_sha256(&input);

        // The one thing resolved between the activity and the execution: a
        // stored file the input names. The bytes go into the execution context
        // and never into the input — the input keeps the identity of the file,
        // which is what the fingerprint above was taken over and what a journal
        // is allowed to retain (ADR 052).
        let context = match stored_source(&input) {
            None => self.context.clone(),
            Some(handle) => {
                let (handle, id) = handle?;
                // Keyed by the string the input wrote rather than by the
                // canonical form of the identifier. `Uuid::parse_str` accepts
                // an upper-case spelling, and a capability looks its file up by
                // the literal text it was given: keying by anything else would
                // download the object, spend the deadline, and then report the
                // file as one that does not exist.
                let source = self.sources.fetch(id, MAX_SOURCE_BYTES).await?;
                self.context.with_source(source.under_handle(handle))
            }
        };

        // Taken after the fetch, and not before: what the capability may spend
        // is what is left of the activity's deadline, and reading the file was
        // spent out of the same budget.
        let ceiling = deadline.saturating_duration_since(tokio::time::Instant::now());
        if ceiling.is_zero() {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Timeout,
                "local_cpu_deadline_exceeded",
                "local capability execution reached its declared cpu deadline",
            ));
        }

        let stop = StopSignal::new();
        // The mirror, not a cancellation: a blocking task cannot be dropped, so
        // the token becomes a flag the running capability checks, and the await
        // below still waits for the thread to finish.
        let watcher = tokio::spawn({
            let stop = stop.clone();
            let token = self.shutdown.clone();
            async move {
                token.cancelled().await;
                stop.stop();
            }
        });

        let product = tokio::task::spawn_blocking(move || {
            declared.execute(&input, &context, Some(ceiling), &stop)
        })
        .await;
        watcher.abort();

        let product = match product {
            Ok(product) => product?,
            // The blocking thread panicked. It is this binary's own code, so
            // the class is `invariant` — a deployment cannot fix it by
            // retrying, and no provider text can be involved.
            Err(error) => {
                tracing::error!(
                    target: "donat::local",
                    instance,
                    operation,
                    %error,
                    "local capability execution did not finish"
                );
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Invariant,
                    "local_capability_panicked",
                    "local capability execution did not finish",
                ));
            }
        };

        let output = match product {
            LocalProduct::Value(value) => value,
            LocalProduct::Artifact { artifact, metadata } => {
                let stored = self.artifacts.store(&artifact).await?;
                artifact_output(&stored, metadata)?
            }
        };
        Ok(ConnectorSuccess {
            output,
            request_fingerprint: fingerprint,
        })
    }
}

/// The stored file an input names, if it names one: the handle exactly as the
/// activity spelled it, and the identifier it parses to.
///
/// The shape is the whole rule: a `source` that is a file's own identifier is
/// resolved before the capability runs, and anything else is left exactly as the
/// activity wrote it. A capability that reads no file never has one fetched,
/// because its input carries no file to fetch.
///
/// Both halves are returned because both are used, for different things: the
/// identifier is what the store is asked for, and the handle is what the
/// capability will look the answer up by.
fn stored_source(input: &JsonValue) -> Option<Result<(&str, Uuid), ConnectorFailure>> {
    let handle = input.get("source")?.as_str()?;
    // A handle that is not an identifier is not a stored file, and is refused
    // rather than looked up: a lookup on caller-shaped text is how a path
    // becomes a query.
    if handle.len() != 36 {
        return None;
    }
    Some(Uuid::parse_str(handle).map(|id| (handle, id)).map_err(|_| {
        refused(
            "local_source_invalid",
            "a local capability's `source` names a stored file by its identifier",
        )
    }))
}

/// The activity output for a produced file: the stored identity, and the
/// capability's typed metadata beside it. The bytes are not here, and there is
/// no branch of this function that could put them here.
fn artifact_output(
    stored: &StoredArtifact,
    metadata: JsonValue,
) -> Result<JsonValue, ConnectorFailure> {
    let JsonValue::Object(mut object) = metadata else {
        return Err(ConnectorFailure::new(
            ConnectorErrorClass::Invariant,
            "local_output_contract_violation",
            "local capability artifact metadata must be an object",
        ));
    };
    object.insert(
        "file".to_owned(),
        json!({
            "id": stored.id.to_string(),
            "file_name": stored.file_name,
            "media_type": stored.media_type,
            "byte_size": stored.byte_size,
        }),
    );
    Ok(JsonValue::Object(object))
}

/// One metadata template declaration, as the renderer's own type.
///
/// The two types are deliberately separate — `donat-connectors` does not depend
/// on `donat-metadata`, the same way `donat-metadata` does not depend on
/// `donat-connectors` (ADR 044) — so the serving binary, which depends on both,
/// is where they meet.
fn template_spec(template: &donat_metadata::DocumentTemplate) -> DocumentTemplateSpec {
    DocumentTemplateSpec {
        name: template.name.clone(),
        kind: Some(match template.kind {
            DocumentTemplateKind::Pdf => DocumentKind::Pdf,
            DocumentTemplateKind::Email => DocumentKind::Email,
            DocumentTemplateKind::Spreadsheet => DocumentKind::Spreadsheet,
            DocumentTemplateKind::Calendar => DocumentKind::Calendar,
        }),
        entry: template.entry.clone(),
        files: template.files.clone(),
        content_hash: template.content_hash.clone(),
        inputs: template.inputs.keys().cloned().collect(),
        html_paths: template.html_paths.clone(),
        max_pages: template.bounds.max_pages,
        cpu_deadline_ms: template.bounds.cpu_deadline_ms(),
        max_output_bytes: template.bounds.max_output_bytes_value(),
    }
}

/// One metadata ingest schema, as the reader's own type.
///
/// The two types are separate for the reason `template_spec` gives: neither
/// crate depends on the other, so the serving binary is where they meet.
fn ingest_schema_spec(schema: &donat_metadata::IngestSchema) -> IngestSchemaSpec {
    IngestSchemaSpec {
        name: schema.name.clone(),
        kind: match schema.kind {
            IngestSchemaKind::Spreadsheet => IngestKind::Spreadsheet,
            IngestSchemaKind::Csv => IngestKind::Csv,
        },
        columns: schema
            .columns
            .iter()
            .map(|column| IngestColumnSpec {
                header: column.header.clone(),
                field: column.field.clone(),
                declared: column.type_.clone(),
                trim: column.trim,
                min: column.min.clone(),
                max: column.max.clone(),
            })
            .collect(),
        sheet_by_name: schema.sheet.by_name.clone(),
        sheet_by_index: schema.sheet.by_index.map(|index| index as usize),
        header_row: schema.header_row,
        delimiter: schema
            .delimiter
            .as_ref()
            .and_then(|delimiter| delimiter.chars().next()),
        on_row_error: match schema.on_row_error {
            IngestRowErrorPolicy::Collect => RowErrorPolicy::Collect,
            IngestRowErrorPolicy::Fail => RowErrorPolicy::Fail,
        },
        max_rows: schema.bounds.max_rows,
        max_columns: schema.bounds.max_columns,
        max_cell_bytes: schema.bounds.max_cell_bytes,
        max_source_bytes: schema.bounds.max_source_bytes,
        max_archive_entries: schema.bounds.max_archive_entries,
        max_uncompressed_bytes: schema.bounds.max_uncompressed_bytes,
        max_compression_ratio: schema.bounds.max_compression_ratio,
        max_working_bytes: schema.bounds.max_working_bytes,
        max_rejections: schema.bounds.max_rejections,
    }
}

/// One metadata code template, as the renderer's own type.
///
/// Separate types for the reason `template_spec` gives. The conversion is total
/// and does nothing else: every refusal belongs either to `donat-metadata`,
/// which reads the declaration, or to `MediaCatalog::resolve`, which is the only
/// thing that knows what a renderer can honour.
fn code_template_spec(code: &donat_metadata::CodeTemplate) -> CodeTemplateSpec {
    CodeTemplateSpec {
        name: code.name.clone(),
        symbology: match code.symbology {
            MetadataSymbology::Qr => Symbology::Qr,
            MetadataSymbology::Code128 => Symbology::Code128,
            MetadataSymbology::Code39 => Symbology::Code39,
            MetadataSymbology::Ean13 => Symbology::Ean13,
        },
        payload_type: match code.payload.type_ {
            MetadataPayloadType::Url => CodePayloadType::Url,
            MetadataPayloadType::Ticket => CodePayloadType::Ticket,
            MetadataPayloadType::Payment => CodePayloadType::Payment,
        },
        allowed_origins: code.payload.allowed_origins.iter().cloned().collect(),
        allowed_prefixes: code.payload.allowed_prefixes.iter().cloned().collect(),
        max_payload_bytes: code.payload.max_length,
        version: code.version,
        error_correction: code.error_correction.map(|level| match level {
            MetadataErrorCorrection::Low => CodeErrorCorrection::Low,
            MetadataErrorCorrection::Medium => CodeErrorCorrection::Medium,
            MetadataErrorCorrection::Quartile => CodeErrorCorrection::Quartile,
            MetadataErrorCorrection::High => CodeErrorCorrection::High,
        }),
        height: code.height,
        module_size: code.module_size,
        quiet_zone: code.quiet_zone,
        format: match code.format {
            MetadataCodeFormat::Png => CodeFormat::Png,
            MetadataCodeFormat::Svg => CodeFormat::Svg,
        },
        delivery: match code.delivery {
            MetadataCodeDelivery::Stored => CodeDelivery::Stored,
            MetadataCodeDelivery::Inline => CodeDelivery::Inline,
        },
        max_inline_bytes: code.max_inline_bytes_value(),
    }
}

/// One metadata image target, as the decoder's own type.
fn image_target_spec(image: &donat_metadata::ImageTarget) -> ImageTargetSpec {
    ImageTargetSpec {
        name: image.name.clone(),
        kind: match image.kind {
            MetadataImageKind::Thumbnail => ImageTargetKind::Thumbnail,
            MetadataImageKind::Normalize => ImageTargetKind::Normalize,
        },
        accept: image.accept.iter().cloned().collect(),
        max_source_bytes: image.max_source_bytes_value().unwrap_or(0),
        max_pixels: image.max_pixels,
        max_width: image.max_width,
        max_height: image.max_height,
        fit: match image.fit {
            MetadataImageFit::Contain => ImageFit::Contain,
            MetadataImageFit::Cover => ImageFit::Cover,
        },
        format: match image.format {
            MetadataImageFormat::Png => ImageOutputFormat::Png,
            MetadataImageFormat::Jpeg => ImageOutputFormat::Jpeg,
        },
        quality: image.quality,
        animation: match image.animation {
            MetadataImageAnimation::Reject => ImageAnimation::Reject,
            MetadataImageAnimation::FirstFrame => ImageAnimation::FirstFrame,
        },
    }
}

/// One declared recurrence policy, in the expander's vocabulary.
///
/// The two DST enums are mapped rather than shared, because the metadata crate
/// and the connector crate do not depend on each other — but they are the same
/// two questions with the same two answers, which is the whole point of ADR 039
/// having chosen the spellings once.
///
/// A `max_window` that does not parse arrives here as zero, which the expander
/// refuses; metadata validation refuses it first, on its own path, so this is a
/// fallback rather than the message an operator reads.
fn recurrence_spec(policy: &donat_metadata::RecurrencePolicy) -> RecurrencePolicySpec {
    RecurrencePolicySpec {
        name: policy.name.clone(),
        timezone: policy.timezone.clone(),
        dst: policy.dst.map(|dst| DstPolicy {
            skipped_time: match dst.skipped_time {
                DstSkippedTime::FireAfterGap => SkippedTime::FireAfterGap,
                DstSkippedTime::Skip => SkippedTime::Skip,
            },
            repeated_time: match dst.repeated_time {
                DstRepeatedTime::FireAtFirst => RepeatedTime::FireAtFirst,
                DstRepeatedTime::FireAtSecond => RepeatedTime::FireAtSecond,
            },
        }),
        max_occurrences: policy.max_occurrences,
        max_window_seconds: parse_window_seconds(&policy.max_window).unwrap_or(0),
    }
}

fn draining() -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Timeout,
        "local_capability_drained",
        "local capability execution stopped because the deployment is draining",
    )
}

fn refused(code: &'static str, message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(ConnectorErrorClass::Validation, code, message)
}

// ---------------------------------------------------------------------------
// The production store
// ---------------------------------------------------------------------------

/// Everything one produced artifact needs, decided before a byte is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPlan {
    pub id: Uuid,
    pub attachment: String,
    pub backend: String,
    pub object_key: String,
    pub file_name: String,
    pub media_type: String,
    pub claim_role: String,
    /// The identity the claiming session will carry, when its role has one.
    /// `None` is a session with no identity, not "any session": the claim
    /// compares with `IS NOT DISTINCT FROM`.
    pub claim_session_key: Option<String>,
    pub byte_size: i64,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub expires_at_epoch: i64,
}

/// Resolve one produced artifact against the deployment's declared attachments.
///
/// Everything the attachment column declares is applied here, before the bytes
/// go anywhere: which backend holds them, which media types the column admits,
/// and how large a file it accepts. A capability cannot widen any of them by
/// producing something else.
pub fn plan_artifact(
    storage: &StorageRegistry,
    artifact: &LocalArtifact,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<ArtifactPlan, ConnectorFailure> {
    let Some(spec) = storage.attachment(artifact.attachment()) else {
        return Err(refused(
            "local_artifact_attachment_unknown",
            "local capability produced a file for a column this deployment does not declare",
        ));
    };
    if !spec.allows_media_type(artifact.media_type()) {
        return Err(refused(
            "local_artifact_media_type_refused",
            "local capability produced a media type its file column does not admit",
        ));
    }
    let byte_size = artifact.byte_size() as u64;
    if byte_size > spec.max_bytes {
        return Err(refused(
            "local_artifact_too_large",
            "local capability produced a file larger than its column admits",
        ));
    }
    let Some(Backend::S3(s3)) = storage.backend_for(spec) else {
        return Err(refused(
            "local_artifact_backend_unknown",
            "local capability produced a file for a column whose backend is not resolved",
        ));
    };
    let object_key = spec.object_key(id);
    let (url, headers) = s3.presign_produced_put(
        &object_key,
        artifact.media_type(),
        byte_size,
        now,
        ARTIFACT_PUT_TTL_SECONDS,
    );
    Ok(ArtifactPlan {
        id,
        attachment: spec.key.clone(),
        backend: spec.backend.clone(),
        object_key,
        file_name: artifact.file_name().to_owned(),
        media_type: artifact.media_type().to_owned(),
        claim_role: artifact.claim_role().to_owned(),
        claim_session_key: artifact.claim_session_key().map(str::to_owned),
        byte_size: byte_size as i64,
        url,
        headers,
        expires_at_epoch: now.timestamp() + ARTIFACT_CLAIM_WINDOW_SECONDS,
    })
}

/// The pending row a produced artifact leaves behind, for a later write to
/// claim.
///
/// `session_key` is bound rather than written as a literal `NULL`, and that is
/// the whole reason this statement has a name: the claim in `crates/sqlgen`
/// matches `session_key IS NOT DISTINCT FROM` the identity variable of the
/// session doing the write, so a row hard-coded to `NULL` is claimable only by
/// a session that has no identity — and a file produced for a role whose
/// sessions carry one could never be bound into its column at all.
const RECORD_ARTIFACT: &str = "INSERT INTO donat.file_uploads (id, attachment, backend, object_key, file_name, \
     media_type, declared_bytes, byte_size, state, session_role, session_key, expires_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $7, 'pending', $8, $9, \
     to_timestamp($10::double precision))";

/// The production [`ArtifactStore`]: the object store for the bytes, and
/// `donat.file_uploads` for the pending row a later write claims.
pub struct StorageArtifactStore {
    storage: Arc<StorageRegistry>,
    http: reqwest::Client,
    pool: deadpool_postgres::Pool,
}

impl StorageArtifactStore {
    pub fn new(
        storage: Arc<StorageRegistry>,
        http: reqwest::Client,
        pool: deadpool_postgres::Pool,
    ) -> Self {
        Self {
            storage,
            http,
            pool,
        }
    }

    async fn put(&self, artifact: &LocalArtifact) -> Result<StoredArtifact, ConnectorFailure> {
        let plan = plan_artifact(&self.storage, artifact, Uuid::new_v4(), Utc::now())?;

        let mut request = self
            .http
            .put(&plan.url)
            .timeout(Duration::from_secs(30))
            .body(artifact.bytes().to_vec());
        for (name, value) in &plan.headers {
            request = request.header(name, value);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                tracing::warn!(
                    target: "donat::local",
                    status = %response.status(),
                    attachment = %plan.attachment,
                    "storage refused a produced artifact"
                );
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Transport,
                    "local_artifact_storage_refused",
                    "the attachment store refused a produced file",
                ));
            }
            Err(_) => {
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Transport,
                    "local_artifact_storage_unavailable",
                    "the attachment store did not answer",
                ));
            }
        }

        // The row is written after the bytes, and never before: a pending row
        // pointing at an object that does not exist is a claim that fails at
        // commit, while an object with no row is an orphan the collector
        // removes.
        let client = self.pool.get().await.map_err(|_| {
            ConnectorFailure::new(
                ConnectorErrorClass::Transport,
                "local_artifact_source_unavailable",
                "the attachment source did not answer",
            )
        })?;
        client
            .execute(
                RECORD_ARTIFACT,
                &[
                    &plan.id,
                    &plan.attachment,
                    &plan.backend,
                    &plan.object_key,
                    &plan.file_name,
                    &plan.media_type,
                    &plan.byte_size,
                    &plan.claim_role,
                    &plan.claim_session_key,
                    &(plan.expires_at_epoch as f64),
                ],
            )
            .await
            .map_err(|error| {
                tracing::error!(target: "donat::local", %error, "cannot record a produced artifact");
                ConnectorFailure::new(
                    ConnectorErrorClass::Transport,
                    "local_artifact_not_recorded",
                    "the produced file could not be recorded",
                )
            })?;

        Ok(StoredArtifact {
            id: plan.id,
            file_name: plan.file_name,
            media_type: plan.media_type,
            byte_size: plan.byte_size,
        })
    }
}

impl ArtifactStore for StorageArtifactStore {
    fn store<'a>(
        &'a self,
        artifact: &'a LocalArtifact,
    ) -> BoxFuture<'a, Result<StoredArtifact, ConnectorFailure>> {
        Box::pin(self.put(artifact))
    }
}

/// How long the presigned GET that reads a stored source stays valid.
const SOURCE_GET_TTL_SECONDS: u32 = 120;

/// The production [`SourceStore`], on the same type as the artifact store:
/// `donat.file_uploads` for what the file is, and the object store for its
/// bytes.
///
/// Spec 020's first bound lives here, in the order that makes it worth having:
/// the recorded size is read from the row and compared *before* the object is
/// fetched, so a file over the ceiling costs one query rather than a download.
impl SourceStore for StorageArtifactStore {
    fn fetch<'a>(
        &'a self,
        id: Uuid,
        max_bytes: u64,
    ) -> BoxFuture<'a, Result<SourceFile, ConnectorFailure>> {
        Box::pin(self.read(id, max_bytes))
    }
}

impl StorageArtifactStore {
    async fn read(&self, id: Uuid, max_bytes: u64) -> Result<SourceFile, ConnectorFailure> {
        let client = self.pool.get().await.map_err(|_| {
            ConnectorFailure::new(
                ConnectorErrorClass::Transport,
                "local_source_unavailable",
                "the attachment source did not answer",
            )
        })?;
        // Only a claimed row: a pending upload is bytes nobody has bound to a
        // column yet, and reading one would let a process read a file its own
        // deployment never accepted.
        let row = client
            .query_opt(
                "SELECT attachment, backend, object_key, file_name, media_type, byte_size \
                 FROM donat.file_uploads WHERE id = $1 AND state = 'claimed'",
                &[&id],
            )
            .await
            .map_err(|error| {
                tracing::error!(target: "donat::local", %error, "cannot resolve a stored source");
                ConnectorFailure::new(
                    ConnectorErrorClass::Transport,
                    "local_source_unavailable",
                    "the attachment source did not answer",
                )
            })?
            .ok_or_else(|| {
                refused(
                    "local_source_unknown",
                    "the stored file this activity names is not a claimed attachment",
                )
            })?;

        let attachment: String = row.get(0);
        let object_key: String = row.get(2);
        let file_name: String = row.get(3);
        let media_type: String = row.get(4);
        let byte_size: Option<i64> = row.get(5);
        // BOUND 1, before anything is fetched.
        let byte_size = byte_size.unwrap_or_default().max(0) as u64;
        if byte_size > max_bytes {
            return Err(refused(
                "ingest_source_too_large",
                "the stored file is larger than a local capability will open",
            ));
        }
        let Some(spec) = self.storage.attachment(&attachment) else {
            return Err(refused(
                "local_source_attachment_unknown",
                "the stored file belongs to a column this deployment does not declare",
            ));
        };
        let Some(Backend::S3(s3)) = self.storage.backend_for(spec) else {
            return Err(refused(
                "local_source_backend_unknown",
                "the stored file's backend is not resolved",
            ));
        };

        let url = s3.presign("GET", &object_key, Utc::now(), SOURCE_GET_TTL_SECONDS);
        let response = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| {
                ConnectorFailure::new(
                    ConnectorErrorClass::Transport,
                    "local_source_storage_unavailable",
                    "the attachment store did not answer",
                )
            })?;
        if !response.status().is_success() {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Transport,
                "local_source_storage_refused",
                "the attachment store refused a stored file",
            ));
        }
        let bytes = response.bytes().await.map_err(|_| {
            ConnectorFailure::new(
                ConnectorErrorClass::Transport,
                "local_source_storage_unavailable",
                "the attachment store did not answer",
            )
        })?;
        // The row said one size and the store returned another: the ceiling is
        // applied to what actually arrived as well as to what was recorded.
        if bytes.len() as u64 > max_bytes {
            return Err(refused(
                "ingest_source_too_large",
                "the stored file is larger than a local capability will open",
            ));
        }
        Ok(SourceFile::new(
            id.to_string(),
            file_name,
            media_type,
            bytes.to_vec(),
        ))
    }
}

/// The activity boundary a source serves: local capabilities first, every
/// other instance through the connector registry.
///
/// Routing lives here rather than inside `ConnectorRegistry` because the two
/// registries answer for disjoint instance names — a `local.*` instance never
/// reaches the connector table, and a provider instance is never in this one.
/// A name neither registry claims keeps the connector registry's own refusal,
/// so an unknown instance still fails exactly as it did before.
pub struct RoutedActivityExecutor {
    local: Arc<LocalCapabilityRegistry>,
    connectors: Arc<crate::connectors::ConnectorRegistry>,
}

impl RoutedActivityExecutor {
    pub fn new(
        local: Arc<LocalCapabilityRegistry>,
        connectors: Arc<crate::connectors::ConnectorRegistry>,
    ) -> Self {
        Self { local, connectors }
    }
}

impl crate::processes::ProcessActivityExecutor for RoutedActivityExecutor {
    fn execute<'a>(
        &'a self,
        instance: &'a str,
        operation: &'a str,
        input: JsonValue,
        idempotency_key: &'a str,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            if self.local.handles(instance) {
                // A local capability is `Pure`: it has no provider to key, so
                // the activity's idempotency key has nothing to bind to.
                return self
                    .local
                    .execute(instance, operation, input, deadline)
                    .await;
            }
            self.connectors
                .execute(instance, operation, input, idempotency_key, deadline)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    use donat_connectors::local::{
        LocalBounds, LocalCapability, LocalInvocation, LocalOperation, capabilities,
    };
    use donat_connectors::sdk::effect::{DeterminismEvidence, Effect};

    use super::*;

    /// Spec 018 §8 `local_execution_is_drainable`.
    ///
    /// The deployment stops while a capability is running: the execution
    /// observes it, ends without output, and the blocking thread is *finished*
    /// when the dispatcher returns — the drain waits for the work rather than
    /// leaving it running against a runtime that is going away. The activity
    /// fails retryably, so it is picked up rather than lost.
    #[test]
    fn local_execution_is_drainable() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a test runtime starts");
        runtime.block_on(async {
            SPIN_FINISHED.store(false, Ordering::SeqCst);
            let shutdown = CancellationToken::new();
            let registry = registry(shutdown.clone());

            let stopping = shutdown.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                stopping.cancel();
            });

            let failure = registry
                .execute(
                    "local.probe",
                    "spin.render",
                    json!({ "spin_ms": 5_000 }),
                    tokio::time::Instant::now() + Duration::from_secs(30),
                )
                .await
                .expect_err("a drained execution produces nothing");
            assert_eq!(failure.class(), ConnectorErrorClass::Timeout);
            assert_eq!(failure.code(), "local_capability_drained");
            assert!(
                SPIN_FINISHED.load(Ordering::SeqCst),
                "the drain must wait for the blocking thread, not abandon it"
            );

            // And a replica that is already draining does not start more work.
            let refused = registry
                .execute(
                    "local.probe",
                    "spin.render",
                    json!({ "spin_ms": 0 }),
                    tokio::time::Instant::now() + Duration::from_secs(30),
                )
                .await
                .expect_err("a draining replica claims no further work");
            assert_eq!(refused.code(), "local_capability_drained");
        });
    }

    /// Spec 018 §8 `local_capability_runs_off_the_async_runtime`.
    ///
    /// One worker thread, a ticker on a 5ms timer, and a capability that spins
    /// for 300ms without yielding. If the capability ran on the runtime's
    /// worker, the ticker would not tick — which is exactly what a stalled
    /// reactor looks like to every request and subscription sharing it.
    #[test]
    fn local_capability_runs_off_the_async_runtime() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a single-worker test runtime starts");
        runtime.block_on(async {
            let registry = Arc::new(registry(CancellationToken::new()));
            let ticks = Arc::new(AtomicU64::new(0));
            let ticker = tokio::spawn({
                let ticks = ticks.clone();
                async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        ticks.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });

            // The execution is a spawned task, so it runs *on* the runtime's
            // one worker thread — which is the thread the ticker needs. Driving
            // it from `block_on` instead would prove nothing: that runs on the
            // calling thread, which no other task was using.
            let success = tokio::spawn({
                let registry = registry.clone();
                async move {
                    registry
                        .execute(
                            "local.probe",
                            "spin.render",
                            json!({ "spin_ms": 300 }),
                            tokio::time::Instant::now() + Duration::from_secs(30),
                        )
                        .await
                }
            })
            .await
            .expect("the execution task finishes")
            .expect("a bounded spin completes");
            ticker.abort();

            assert_eq!(success.output, json!({ "spun": true }));
            assert!(
                ticks.load(Ordering::SeqCst) > 10,
                "the reactor kept only {} ticks while a capability ran: it was blocked",
                ticks.load(Ordering::SeqCst)
            );
        });
    }

    /// Spec 018 §8 `local_artifacts_go_to_storage`.
    ///
    /// Two halves. The bytes are resolved against the declaring column and
    /// written to the attachment store's own key through `crates/storage`; and
    /// the activity result carries the stored file's identity with no bytes in
    /// it anywhere.
    #[test]
    fn local_artifacts_go_to_storage() {
        // 1. The handoff. Everything the column declares is applied before a
        //    byte moves: its backend, its media types, its size ceiling.
        let storage = test_storage();
        let artifact = LocalArtifact::new(
            "public.pet.photo",
            "app",
            "receipt.txt",
            "text/plain",
            b"stored bytes".to_vec(),
        )
        .expect("a complete artifact declaration is valid");
        let id = Uuid::from_u128(11);
        let plan = plan_artifact(&storage, &artifact, id, Utc::now())
            .expect("a declared attachment resolves");
        assert_eq!(plan.object_key, format!("public.pet.photo/{id}"));
        assert_eq!(plan.byte_size, 12);
        assert_eq!(plan.claim_role, "app");
        assert!(plan.url.contains("X-Amz-Signature="));
        assert!(plan.url.contains(&plan.object_key));
        assert!(
            plan.headers
                .contains(&("Content-Length".to_string(), "12".to_string())),
            "the size is signed into the URL the bytes are written with"
        );

        // A produced artifact is a pending upload, and a pending upload is
        // claimed by `session_role` *and* `session_key`: the claim compares
        // `session_key IS NOT DISTINCT FROM` the identity variable of the
        // session doing the write. A row written with a `NULL` key can
        // therefore only ever be claimed by a session that has no identity —
        // so a file produced for a `Caller` role that declares one could never
        // be bound into its column at all. The key travels the same way the
        // role does: from the activity's own input.
        let identified = LocalArtifact::new(
            "public.pet.photo",
            "app",
            "receipt.txt",
            "text/plain",
            b"stored bytes".to_vec(),
        )
        .expect("a complete artifact declaration is valid")
        .claimed_by_session(Some("u-1"))
        .expect("a session identity is a plain value");
        let plan = plan_artifact(&storage, &identified, id, Utc::now())
            .expect("a declared attachment resolves");
        assert_eq!(plan.claim_session_key.as_deref(), Some("u-1"));
        // And the statement that writes the row binds it, rather than the
        // literal `NULL` that made the row unclaimable in the first place.
        assert!(
            !RECORD_ARTIFACT.contains("NULL"),
            "the pending row's session key is bound, not hard-coded: {RECORD_ARTIFACT}"
        );
        assert!(RECORD_ARTIFACT.contains("$9"));

        let elsewhere = LocalArtifact::new(
            "public.other.file",
            "app",
            "receipt.txt",
            "text/plain",
            b"x".to_vec(),
        )
        .expect("a complete artifact declaration is valid");
        assert_eq!(
            plan_artifact(&storage, &elsewhere, id, Utc::now())
                .unwrap_err()
                .code(),
            "local_artifact_attachment_unknown"
        );
        let oversized = LocalArtifact::new(
            "public.pet.photo",
            "app",
            "receipt.txt",
            "text/plain",
            vec![b'x'; 2_048],
        )
        .expect("a complete artifact declaration is valid");
        assert_eq!(
            plan_artifact(&storage, &oversized, id, Utc::now())
                .unwrap_err()
                .code(),
            "local_artifact_too_large"
        );

        // 2. The result. A file reference and typed metadata; the bytes are
        //    nowhere in it.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a test runtime starts");
        runtime.block_on(async {
            let registry = registry(CancellationToken::new());
            let success = registry
                .execute(
                    "local.probe",
                    "file.render",
                    json!({ "text": "stored bytes" }),
                    tokio::time::Instant::now() + Duration::from_secs(30),
                )
                .await
                .expect("a produced file is stored");
            assert_eq!(
                success.output,
                json!({
                    "bytes": 12,
                    "file": {
                        "id": Uuid::from_u128(7).to_string(),
                        "file_name": "probe.txt",
                        "media_type": "text/plain",
                        "byte_size": 12
                    }
                })
            );
            let rendered = success.output.to_string();
            assert!(
                !rendered.contains("stored bytes"),
                "an activity result carries a file reference, never the bytes: {rendered}"
            );
        });
    }

    /// An operation the deployment did not enable, and an instance it never
    /// declared, are both refused before anything runs.
    #[test]
    fn only_an_enabled_capability_operation_is_dispatched() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a test runtime starts");
        runtime.block_on(async {
            let registry = registry(CancellationToken::new());
            for (instance, operation) in [
                ("local.probe", "absent.render"),
                ("local.absent", "spin.render"),
            ] {
                let failure = registry
                    .execute(
                        instance,
                        operation,
                        json!({}),
                        tokio::time::Instant::now() + Duration::from_secs(5),
                    )
                    .await
                    .expect_err("only a declared, enabled operation is dispatched");
                assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
            }
        });
    }

    /// Spec 020: the one thing dispatch resolves between an activity and an
    /// ingest execution.
    ///
    /// The activity names a stored file by its identifier; the dispatcher
    /// fetches it and hands the bytes to the execution as context. Two things
    /// are proven here — the read happens at all, and the bytes are nowhere in
    /// the input the fingerprint was taken over or in the result the journal
    /// keeps.
    #[test]
    fn an_ingest_activity_reads_the_stored_file_it_names() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a test runtime starts");
        runtime.block_on(async {
            let capability = donat_connectors::local::capability("local.ingest")
                .expect("local.ingest is compiled into this binary");
            let metadata: Metadata =
                serde_yaml::from_str(TEST_INGEST_METADATA).expect("test ingest metadata");
            let schemas =
                IngestSchemaSet::resolve(metadata.ingest_schemas.iter().map(ingest_schema_spec))
                    .expect("the declared test schema resolves");
            let registry = LocalCapabilityRegistry {
                instances: BTreeMap::from([(
                    "local.ingest".to_owned(),
                    LocalInstance {
                        capability,
                        enabled: BTreeSet::from(["csv.read".to_owned()]),
                    },
                )]),
                context: LocalContext::default().with_ingest_schemas(schemas),
                artifacts: Arc::new(FixedStore),
                sources: Arc::new(FixedSource),
                shutdown: CancellationToken::new(),
            };

            let id = Uuid::from_u128(31).to_string();
            let success = registry
                .execute(
                    "local.ingest",
                    "csv.read",
                    json!({ "schema": "prices", "source": id }),
                    tokio::time::Instant::now() + Duration::from_secs(30),
                )
                .await
                .expect("a declared schema reads the stored file");
            assert_eq!(success.output["row_count"], json!(1));
            assert_eq!(
                success.output["rows"],
                json!([{ "sku": "A-1", "price": "12.50" }])
            );
            assert_eq!(success.output["source"], json!(id));
            assert_eq!(success.output["rejected_count"], json!(1));

            // A deployment that wired no source store refuses rather than
            // reading an empty file.
            let unwired = LocalCapabilityRegistry {
                instances: BTreeMap::new(),
                context: LocalContext::default(),
                artifacts: Arc::new(FixedStore),
                sources: Arc::new(UnavailableSourceStore),
                shutdown: CancellationToken::new(),
            };
            assert!(!unwired.handles("local.ingest"));
            assert_eq!(
                unwired
                    .sources
                    .fetch(Uuid::from_u128(31), 1_024)
                    .await
                    .unwrap_err()
                    .code(),
                "local_source_store_unavailable"
            );

            // And a `source` that is not a stored file's identifier is refused
            // rather than looked up.
            assert!(stored_source(&json!({ "schema": "prices" })).is_none());
            assert!(stored_source(&json!({ "source": "prices" })).is_none());
            assert_eq!(
                stored_source(&json!({ "source": "0000000000000000000000000000000000ZZ" }))
                    .expect("an identifier-shaped handle is resolved")
                    .unwrap_err()
                    .code(),
                "local_source_invalid"
            );

            // The capability looks its file up by the string the input wrote,
            // and `Uuid::parse_str` accepts an upper-case one. A file keyed by
            // the canonical spelling instead would be downloaded in full, would
            // spend the activity's deadline, and would then be reported as a
            // file that does not exist.
            let shouted = id.to_uppercase();
            assert_ne!(shouted, id);
            let success = registry
                .execute(
                    "local.ingest",
                    "csv.read",
                    json!({ "schema": "prices", "source": shouted }),
                    tokio::time::Instant::now() + Duration::from_secs(30),
                )
                .await
                .expect("a file named in upper case is the same file");
            assert_eq!(success.output["row_count"], json!(1));
            assert_eq!(
                success.output["source"],
                json!(shouted),
                "the answer names the file the activity named"
            );
        });
    }

    const TEST_INGEST_METADATA: &str = r#"
version: 3
ingest_schemas:
  - name: prices
    kind: csv
    columns:
      - { header: "SKU", field: sku, type: "String!", trim: true }
      - { header: "Price", field: price, type: "Decimal!", trim: true }
"#;

    /// A store that answers with one CSV: this suite is about the dispatch, and
    /// the object store and the row are covered by the production path.
    struct FixedSource;

    impl SourceStore for FixedSource {
        fn fetch<'a>(
            &'a self,
            id: Uuid,
            _max_bytes: u64,
        ) -> BoxFuture<'a, Result<SourceFile, ConnectorFailure>> {
            Box::pin(async move {
                Ok(SourceFile::new(
                    id.to_string(),
                    "prices.csv",
                    "text/csv",
                    b"SKU,Price\nA-1,12.50\nA-2,not a decimal\n".to_vec(),
                ))
            })
        }
    }

    /// The metadata half, answered from the compiled table: this binary's own
    /// capabilities are what a deployment may enable.
    #[test]
    fn the_compiled_catalog_answers_from_the_table() {
        let catalog = CompiledCapabilities;
        for compiled in capabilities() {
            let operations = catalog
                .operations(compiled.name())
                .expect("a compiled capability is known to the catalog");
            for operation in compiled.operations() {
                assert!(operations.contains(&operation.id().to_owned()));
                assert_eq!(
                    catalog.cpu_deadline_ms(compiled.name(), operation.id()),
                    Some(operation.bounds().cpu_deadline().as_millis() as u64)
                );
            }
        }
        assert!(catalog.operations("local.absent").is_none());
        assert!(
            catalog
                .cpu_deadline_ms("local.echo", "absent.render")
                .is_none()
        );
    }

    /// Spec 019 §7 `document_artifacts_are_stored_and_signed`.
    ///
    /// Every file a document capability produces lands in the attachment store
    /// and is reachable only through a signed URL. The two halves are the
    /// handoff — the bytes are resolved against the declaring column and
    /// written to a presigned key — and the result, which carries the stored
    /// file's identity and no bytes at all.
    #[test]
    fn document_artifacts_are_stored_and_signed() {
        let storage = document_storage();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a test runtime starts");

        for (operation, input, media_type, file_name) in [
            (
                "pdf.render",
                json!({
                    "template": "invoice",
                    "document_id": "invoice:A-1",
                    "document_timestamp": "2026-03-04T09:30:00Z",
                    "attachment": "public.document.file",
                    "claim_role": "app",
                    "file_name": "invoice.pdf"
                }),
                "application/pdf",
                "invoice.pdf",
            ),
            (
                "spreadsheet.render",
                json!({
                    "template": "orders",
                    "rows": [{ "number": "A-1" }],
                    "document_timestamp": "2026-03-04T09:30:00Z",
                    "attachment": "public.document.file",
                    "claim_role": "app",
                    "file_name": "orders.xlsx"
                }),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "orders.xlsx",
            ),
            (
                "calendar.render",
                json!({
                    "template": "deliveries",
                    "document_timestamp": "2026-03-04T09:30:00Z",
                    "events": [{
                        "uid": "order:A-1@example.test",
                        "start": "2026-03-06T09:00:00Z",
                        "end": "2026-03-06T10:00:00Z"
                    }],
                    "attachment": "public.document.file",
                    "claim_role": "app",
                    "file_name": "deliveries.ics"
                }),
                "text/calendar",
                "deliveries.ics",
            ),
        ] {
            // 1. The handoff. Everything the column declares is applied before
            //    a byte moves, and the URL the bytes are written with is
            //    signed — there is no unsigned way into the store.
            let capability = donat_connectors::local::capability("local.document")
                .expect("local.document is compiled into this binary");
            let declared = capability
                .admit_operation(operation)
                .expect("the operation is declared and executable");
            let product = declared
                .execute(&input, &document_context(), None, &StopSignal::new())
                .unwrap_or_else(|failure| panic!("{operation} renders: {failure:?}"));
            let LocalProduct::Artifact { artifact, metadata } = product else {
                panic!("{operation} produces bytes");
            };
            assert_eq!(artifact.media_type(), media_type);

            let id = Uuid::from_u128(21);
            let plan = plan_artifact(&storage, &artifact, id, Utc::now())
                .unwrap_or_else(|failure| panic!("{operation} resolves its column: {failure:?}"));
            assert_eq!(plan.object_key, format!("public.document.file/{id}"));
            assert_eq!(plan.claim_role, "app");
            assert_eq!(plan.byte_size as usize, artifact.byte_size());
            assert!(plan.url.contains("X-Amz-Signature="), "{}", plan.url);
            assert!(plan.url.contains("X-Amz-Expires="), "{}", plan.url);
            assert!(plan.url.contains(&plan.object_key));
            assert!(
                plan.expires_at_epoch > Utc::now().timestamp(),
                "the claim window is open when the row is written"
            );

            // 2. The result. A file reference and typed metadata; the bytes are
            //    nowhere in it, for any of the three producing operations.
            let registry = LocalCapabilityRegistry {
                instances: BTreeMap::from([(
                    "local.document".to_owned(),
                    LocalInstance {
                        capability,
                        enabled: BTreeSet::from([operation.to_owned()]),
                    },
                )]),
                context: document_context(),
                artifacts: Arc::new(FixedStore),
                sources: Arc::new(UnavailableSourceStore),
                shutdown: CancellationToken::new(),
            };
            let success = runtime.block_on(async {
                registry
                    .execute(
                        "local.document",
                        operation,
                        input.clone(),
                        tokio::time::Instant::now() + Duration::from_secs(30),
                    )
                    .await
                    .unwrap_or_else(|failure| panic!("{operation} is dispatched: {failure:?}"))
            });
            assert_eq!(
                success.output["file"],
                json!({
                    "id": Uuid::from_u128(7).to_string(),
                    "file_name": file_name,
                    "media_type": media_type,
                    "byte_size": artifact.byte_size(),
                })
            );
            assert_eq!(success.output["template_hash"], metadata["template_hash"]);
            let rendered = success.output.to_string();
            assert!(
                !rendered.contains("%PDF-") && !rendered.contains("BEGIN:VCALENDAR"),
                "an activity result carries a file reference, never the bytes: {rendered}"
            );
        }
    }

    /// Spec 022 §3 `media_artifacts_are_stored_and_signed`.
    ///
    /// Every file `local.code` and `local.image` produce lands in the
    /// attachment store behind a signed URL, exactly as a rendered document
    /// does — and the activity result carries the stored file's identity and
    /// the typed metadata, never a raster.
    ///
    /// It also proves the wiring the two capabilities need: the declarations
    /// reach the execution through the context the registry builds from
    /// `media.yaml`, and the source image reaches it through the same stored
    /// file the dispatcher resolves for an ingest read.
    #[test]
    fn media_artifacts_are_stored_and_signed() {
        let storage = media_storage();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a test runtime starts");
        let source_id = Uuid::from_u128(41).to_string();

        for (module, operation, input, file_name) in [
            (
                "local.code",
                "qr.render",
                json!({
                    "template": "invoice_payment",
                    "payload": "https://pay.example.test/i/A-1",
                    "attachment": "public.pet.photo",
                    "claim_role": "app",
                    "file_name": "invoice-qr.png"
                }),
                "invoice-qr.png",
            ),
            (
                "local.image",
                "image.thumbnail",
                json!({
                    "target": "avatar",
                    "source": source_id,
                    "attachment": "public.pet.photo",
                    "claim_role": "app",
                    "file_name": "thumb.png"
                }),
                "thumb.png",
            ),
        ] {
            let capability = donat_connectors::local::capability(module)
                .unwrap_or_else(|| panic!("{module} is compiled into this binary"));
            let registry = LocalCapabilityRegistry {
                instances: BTreeMap::from([(
                    module.to_owned(),
                    LocalInstance {
                        capability,
                        enabled: BTreeSet::from([operation.to_owned()]),
                    },
                )]),
                context: media_context(),
                artifacts: Arc::new(FixedStore),
                sources: Arc::new(PngSource),
                shutdown: CancellationToken::new(),
            };

            // 1. The result: a file reference and typed metadata, dispatched
            //    through the same boundary a Process calls.
            let success = runtime.block_on(async {
                registry
                    .execute(
                        module,
                        operation,
                        input.clone(),
                        tokio::time::Instant::now() + Duration::from_secs(30),
                    )
                    .await
                    .unwrap_or_else(|failure| panic!("{operation} is dispatched: {failure:?}"))
            });
            assert_eq!(success.output["file"]["file_name"], json!(file_name));
            assert_eq!(success.output["file"]["media_type"], json!("image/png"));
            assert_eq!(
                success.output["file"]["id"],
                json!(Uuid::from_u128(7).to_string())
            );
            assert!(
                success.output["width"]
                    .as_u64()
                    .is_some_and(|width| width > 0),
                "{operation} reports the produced raster's size"
            );
            let rendered = success.output.to_string();
            assert!(
                !rendered.contains("\\u0089PNG") && !rendered.contains("PNG\\r\\n"),
                "an activity result carries a file reference, never the bytes: {rendered}"
            );

            // 2. The handoff: the column's declaration is applied before a byte
            //    moves, and the only way in is a signed URL.
            let declared = capability
                .admit_operation(operation)
                .expect("the operation is declared and executable");
            let context = runtime.block_on(async {
                media_context().with_source(
                    PngSource
                        .fetch(Uuid::from_u128(41), MAX_SOURCE_BYTES)
                        .await
                        .expect("the stored source reads"),
                )
            });
            let LocalProduct::Artifact { artifact, .. } = declared
                .execute(&input, &context, None, &StopSignal::new())
                .unwrap_or_else(|failure| panic!("{operation} renders: {failure:?}"))
            else {
                panic!("{operation} produces bytes");
            };
            assert_eq!(artifact.media_type(), "image/png");

            let id = Uuid::from_u128(23);
            let plan = plan_artifact(&storage, &artifact, id, Utc::now())
                .unwrap_or_else(|failure| panic!("{operation} resolves its column: {failure:?}"));
            assert_eq!(plan.object_key, format!("public.pet.photo/{id}"));
            assert_eq!(plan.claim_role, "app");
            assert_eq!(plan.byte_size as usize, artifact.byte_size());
            assert!(plan.url.contains("X-Amz-Signature="), "{}", plan.url);
            assert!(plan.url.contains("X-Amz-Expires="), "{}", plan.url);
            assert!(
                plan.expires_at_epoch > Utc::now().timestamp(),
                "the claim window is open when the row is written"
            );
        }
    }

    /// The media declarations a deployment writes, as the registry resolves
    /// them — through the same two conversions `build` uses.
    fn media_context() -> LocalContext {
        let metadata: Metadata =
            serde_yaml::from_str(TEST_MEDIA_METADATA).expect("test media metadata");
        assert!(
            donat_metadata::validate_media_declarations(&metadata).is_empty(),
            "the test declarations are ones a deployment could write"
        );
        LocalContext::default().with_media(
            MediaCatalog::resolve(
                metadata.media.codes.iter().map(code_template_spec),
                metadata.media.images.iter().map(image_target_spec),
            )
            .expect("the test media declarations resolve"),
        )
    }

    const TEST_MEDIA_METADATA: &str = r#"
version: 3
media:
  codes:
    - name: invoice_payment
      symbology: qr
      payload: { type: url, allowed_origins: ["https://pay.example.test"], max_length: 256 }
      version: 6
      error_correction: medium
      module_size: 2
      quiet_zone: 4
      format: png
  images:
    - name: avatar
      kind: thumbnail
      accept: ["image/png"]
      max_source_bytes: 1MiB
      max_pixels: 1000000
      max_width: 16
      max_height: 16
      format: png
"#;

    /// A stored source that is a real PNG, so the decode path in the dispatch
    /// test is the decode path and not a stub.
    struct PngSource;

    impl SourceStore for PngSource {
        fn fetch<'a>(
            &'a self,
            id: Uuid,
            _max_bytes: u64,
        ) -> BoxFuture<'a, Result<SourceFile, ConnectorFailure>> {
            Box::pin(async move {
                Ok(SourceFile::new(
                    id.to_string(),
                    "photo.png",
                    "image/png",
                    donat_connectors::local::image::probe_png(64, 48),
                ))
            })
        }
    }

    /// The storage declaration, with a file column that admits a PNG.
    fn media_storage() -> StorageRegistry {
        let metadata: Metadata = serde_yaml::from_str(
            &TEST_STORAGE_METADATA
                .replace(
                    r#"media_types: ["text/plain"]"#,
                    r#"media_types: ["image/png"]"#,
                )
                .replace("max_bytes: 1024", "max_bytes: 1048576"),
        )
        .expect("test storage metadata");
        StorageRegistry::build_with(&metadata, &|name| match name {
            "DONAT_LOCAL_TEST_STORAGE_KEY" => Some("test-key".to_owned()),
            "DONAT_LOCAL_TEST_STORAGE_SECRET" => Some("s3cr3t".to_owned()),
            _ => None,
        })
        .expect("a resolved test storage registry")
    }

    /// The template set a deployment declares, as the registry builds it.
    fn document_context() -> LocalContext {
        let metadata: Metadata =
            serde_yaml::from_str(TEST_DOCUMENT_METADATA).expect("test document metadata");
        let mut templates: Vec<donat_metadata::DocumentTemplate> = metadata.templates;
        // The loader fills these from the metadata directory; this test has no
        // directory, so the frozen set is written out here instead.
        for template in &mut templates {
            let (entry, source) = match template.kind {
                DocumentTemplateKind::Pdf => (
                    "/invoice.typ",
                    "#set page(width: 200pt, height: 120pt)\n= Invoice\n",
                ),
                DocumentTemplateKind::Spreadsheet => (
                    "/orders.json",
                    r#"{"sheet":"Orders","columns":[{"header":"Number","field":"number","type":"text"}]}"#,
                ),
                DocumentTemplateKind::Calendar => (
                    "/deliveries.json",
                    r#"{"product_id":"-//donat//deliveries//EN"}"#,
                ),
                DocumentTemplateKind::Email => ("/mail.mjml", "<mjml><mj-body/></mjml>"),
            };
            template.entry = entry.to_owned();
            template.files = BTreeMap::from([(entry.to_owned(), source.to_owned())]);
            template.content_hash = donat_metadata::documents::content_hash(template);
        }
        LocalContext::new(
            DocumentTemplateSet::resolve(templates.iter().map(template_spec))
                .expect("the test templates resolve"),
        )
    }

    const TEST_DOCUMENT_METADATA: &str = r#"
version: 3
templates:
  - { name: invoice,    kind: pdf,         source: templates/invoice.typ }
  - { name: orders,     kind: spreadsheet, source: templates/orders.yaml, inputs: { rows: '[order_row!]!' } }
  - { name: deliveries, kind: calendar,    source: templates/deliveries.yaml, inputs: { events: '[event!]!' } }
"#;

    /// The same storage declaration, with the file column a document lands in.
    fn document_storage() -> StorageRegistry {
        let metadata: Metadata = serde_yaml::from_str(
            &TEST_STORAGE_METADATA
                .replace("name: pet", "name: document")
                .replace("column: photo", "column: file")
                .replace("max_bytes: 1024", "max_bytes: 1048576")
                .replace(
                    r#"media_types: ["text/plain"]"#,
                    r#"media_types: ["application/pdf", "text/calendar", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"]"#,
                ),
        )
        .expect("test storage metadata");
        StorageRegistry::build_with(&metadata, &|name| match name {
            "DONAT_LOCAL_TEST_STORAGE_KEY" => Some("test-key".to_owned()),
            "DONAT_LOCAL_TEST_STORAGE_SECRET" => Some("s3cr3t".to_owned()),
            _ => None,
        })
        .expect("a resolved test storage registry")
    }

    // -- the test capability and its store ---------------------------------

    static SPIN_FINISHED: AtomicBool = AtomicBool::new(false);

    /// A store that records nothing anywhere: this suite is about the dispatch,
    /// and the object store and the row are covered by `plan_artifact` above
    /// and by the conformance suite.
    struct FixedStore;

    impl ArtifactStore for FixedStore {
        fn store<'a>(
            &'a self,
            artifact: &'a LocalArtifact,
        ) -> BoxFuture<'a, Result<StoredArtifact, ConnectorFailure>> {
            Box::pin(async move {
                Ok(StoredArtifact {
                    id: Uuid::from_u128(7),
                    file_name: artifact.file_name().to_owned(),
                    media_type: artifact.media_type().to_owned(),
                    byte_size: artifact.byte_size() as i64,
                })
            })
        }
    }

    /// The wiring proof for ADR 034: a `local.*` instance a deployment
    /// declares must actually be reached by the activity boundary the Process
    /// worker calls, and every other name must keep the connector registry's
    /// own answer.
    #[test]
    fn a_local_instance_is_dispatched_by_the_activity_boundary() {
        use crate::processes::ProcessActivityExecutor;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a test runtime starts");
        runtime.block_on(async {
            let routed = RoutedActivityExecutor::new(
                Arc::new(registry(CancellationToken::new())),
                Arc::new(crate::connectors::ConnectorRegistry::empty()),
            );
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

            let success = routed
                .execute(
                    "local.probe",
                    "spin.render",
                    json!({ "spin_ms": 0 }),
                    "activity-key",
                    deadline,
                )
                .await
                .expect("a declared local capability answers the activity boundary");
            assert_eq!(success.output, json!({ "spun": true }));

            // Nothing about the local table may soften the connector
            // registry's refusal of a name it does not carry.
            let failure = routed
                .execute(
                    "provider.absent",
                    "any",
                    json!({}),
                    "activity-key",
                    deadline,
                )
                .await
                .expect_err("an unknown instance is still refused");
            assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
        });
    }

    /// Spec 021 §3: the seam between `recurrence.yaml` and the expander.
    ///
    /// A policy declared in metadata reaches the execution through the context
    /// the registry builds — its zone, its two DST answers, and its ceilings —
    /// and a declaration this binary cannot honour stops the boot on its own
    /// path instead of surfacing as a wrong answer at the first expansion.
    #[test]
    fn a_recurrence_activity_expands_under_its_declared_policy() {
        let declaration = |max_occurrences: u64, max_window: &str| {
            json!({
                "version": 3,
                "recurrence": { "policies": [{
                    "name": "booking",
                    "timezone": "Europe/Berlin",
                    "dst": {
                        "skipped_time": "fire_after_gap",
                        "repeated_time": "fire_at_first"
                    },
                    "max_occurrences": max_occurrences,
                    "max_window": max_window
                }]},
                "connectors": [{
                    "name": "local.recurrence",
                    "module": "local.recurrence",
                    "operations": [{ "name": "rule.expand" }]
                }]
            })
        };
        let build = |value: JsonValue| {
            LocalCapabilityRegistry::build(
                &serde_json::from_value::<Metadata>(value).expect("test metadata deserializes"),
                Arc::new(FixedStore),
                CancellationToken::new(),
            )
        };

        let registry = build(declaration(500, "52w")).expect("the declaration is honourable");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a test runtime starts");
        let success = runtime.block_on(async {
            registry
                .execute(
                    "local.recurrence",
                    "rule.expand",
                    json!({
                        "policy": "booking",
                        "rule": "FREQ=DAILY;COUNT=3",
                        "start": "2026-03-28T09:00:00",
                        "window": {
                            "from": "2026-03-28T00:00:00Z",
                            "to": "2026-03-31T00:00:00Z"
                        }
                    }),
                    tokio::time::Instant::now() + Duration::from_secs(30),
                )
                .await
                .expect("a declared policy expands")
        });
        assert_eq!(
            success.output["occurrences"],
            json!([
                { "at": "2026-03-28T08:00:00Z", "local": "2026-03-28T09:00:00" },
                { "at": "2026-03-29T07:00:00Z", "local": "2026-03-29T09:00:00" },
                { "at": "2026-03-30T07:00:00Z", "local": "2026-03-30T09:00:00" }
            ]),
            "the declared zone moved the instant across the transition, not the wall clock"
        );
        assert_eq!(success.output["timezone"], json!("Europe/Berlin"));

        // A ceiling this binary's compiled bounds cannot hold is refused where
        // every other structural refusal is: at boot, on its own path.
        let Err(errors) = build(declaration(50_000, "52w")) else {
            panic!("a ceiling over the compiled one is not honourable");
        };
        assert_eq!(errors[0].path, "recurrence.booking");
        assert!(errors[0].message.contains("one occurrence"), "{errors:?}");
    }

    fn registry(shutdown: CancellationToken) -> LocalCapabilityRegistry {
        let metadata: Metadata = serde_json::from_value(json!({
            "version": 3,
            "connectors": [{
                "name": "local.probe",
                "module": "local.probe",
                "operations": [{ "name": "spin.render" }, { "name": "file.render" }]
            }]
        }))
        .expect("test metadata deserializes");

        // The compiled table has no probe capability in it — that is the point
        // of a table — so the registry is assembled here from the same
        // declaration the production path uses.
        let mut instances = BTreeMap::new();
        instances.insert(
            "local.probe".to_owned(),
            LocalInstance {
                capability: probe_capability(),
                enabled: metadata.connectors[0]
                    .operations
                    .iter()
                    .map(|operation| operation.name.clone())
                    .collect(),
            },
        );
        LocalCapabilityRegistry {
            instances,
            context: LocalContext::default(),
            artifacts: Arc::new(FixedStore),
            sources: Arc::new(UnavailableSourceStore),
            shutdown,
        }
    }

    fn probe_capability() -> &'static LocalCapability {
        static CAPABILITY: std::sync::LazyLock<LocalCapability> = std::sync::LazyLock::new(|| {
            LocalCapability::declare("local.probe", "1.0.0")
                .operation(spin_render())
                .operation(file_render())
                .build()
                .expect("the probe capability is static and complete")
        });
        &CAPABILITY
    }

    /// Work that does not yield: the shape every real capability has, and the
    /// reason execution belongs on the blocking pool.
    fn spin_render() -> LocalOperation {
        fn run(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
            SPIN_FINISHED.store(false, Ordering::SeqCst);
            let spin = invocation
                .input()
                .get("spin_ms")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let until = std::time::Instant::now() + Duration::from_millis(spin);
            let outcome = loop {
                if std::time::Instant::now() >= until {
                    break Ok(LocalProduct::Value(json!({ "spun": true })));
                }
                if let Err(failure) = invocation.checkpoint() {
                    break Err(failure);
                }
                std::thread::yield_now();
            };
            SPIN_FINISHED.store(true, Ordering::SeqCst);
            outcome
        }

        LocalOperation::declare("spin.render", "1.0.0")
            .effect(Effect::pure(
                DeterminismEvidence::double_render(
                    json!({ "spin_ms": 0 }),
                    "the output is constant; the spin only occupies the thread",
                )
                .expect("a probe and a statement are evidence"),
            ))
            .bounds(
                LocalBounds::declare(Duration::from_secs(20), 128, 128, 128, "spins", 1)
                    .expect("a complete bound declaration is valid"),
            )
            .units(|_| 1)
            .run(run)
            .build()
            .expect("spin.render is deterministic")
    }

    fn file_render() -> LocalOperation {
        fn run(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
            let text = invocation
                .input()
                .get("text")
                .and_then(JsonValue::as_str)
                .unwrap_or("x");
            let bytes = text.as_bytes().to_vec();
            let size = bytes.len();
            Ok(LocalProduct::Artifact {
                artifact: LocalArtifact::new(
                    "public.pet.photo",
                    "app",
                    "probe.txt",
                    "text/plain",
                    bytes,
                )?,
                metadata: json!({ "bytes": size }),
            })
        }

        LocalOperation::declare("file.render", "1.0.0")
            .effect(Effect::pure(
                DeterminismEvidence::double_render(
                    json!({ "text": "x" }),
                    "the output is the declared text",
                )
                .expect("a probe and a statement are evidence"),
            ))
            .bounds(
                LocalBounds::declare(Duration::from_secs(2), 1_024, 1_024, 1_024, "files", 1)
                    .expect("a complete bound declaration is valid"),
            )
            .units(|_| 1)
            .run(run)
            .build()
            .expect("file.render is deterministic")
    }

    const TEST_STORAGE_METADATA: &str = r#"
version: 3
sources:
  - name: default
    kind: postgres
    configuration:
      connection_info:
        database_url: postgresql://localhost/x
    tables:
      - table: {schema: public, name: pet}
        attachments:
          - column: photo
            backend: local
            max_bytes: 1024
            media_types: ["text/plain"]
storage:
  backends:
    - name: local
      kind: s3
      bucket: donat-test
      region: eu-central-1
      endpoint: http://127.0.0.1:19000
      path_style: true
      access_key_id: { value_from_env: DONAT_LOCAL_TEST_STORAGE_KEY }
      secret_access_key: { value_from_env: DONAT_LOCAL_TEST_STORAGE_SECRET }
  signing:
    secret: { value_from_env: DONAT_LOCAL_TEST_STORAGE_SECRET }
"#;

    fn test_storage() -> StorageRegistry {
        let metadata: Metadata =
            serde_yaml::from_str(TEST_STORAGE_METADATA).expect("test storage metadata");
        StorageRegistry::build_with(&metadata, &|name| match name {
            "DONAT_LOCAL_TEST_STORAGE_KEY" => Some("test-key".to_owned()),
            "DONAT_LOCAL_TEST_STORAGE_SECRET" => Some("s3cr3t".to_owned()),
            _ => None,
        })
        .expect("a resolved test storage registry")
    }
}
