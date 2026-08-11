//! Donat v2-compatible metadata: the typed model and the YAML directory
//! loader. This crate is the single source of truth for "what the user
//! configured"; everything downstream (schema generation, permissions,
//! sqlgen) consumes these types and never re-reads YAML.

pub mod documents;
pub mod ingest;
mod loader;
pub mod local;
pub mod media;
mod phone;
pub mod recurrence;
mod types;

pub use documents::{
    DOCUMENT_CAPABILITY, DocumentTemplate, DocumentTemplateBounds, DocumentTemplateError,
    DocumentTemplateKind, HTML_SCALAR, RESERVED_INPUT_KEYS, validate_document_templates,
};
pub use ingest::{
    INGEST_CAPABILITY, INGEST_INPUT_KEYS, INGEST_SCALARS, IngestBounds, IngestColumn,
    IngestRowErrorPolicy, IngestSchema, IngestSchemaError, IngestSchemaKind, IngestSheetSelector,
    schema_pin, validate_ingest_schemas,
};
pub use loader::{LoadError, load_metadata_dir};
pub use local::{
    LOCAL_NAMESPACE, LocalCapabilityCatalog, LocalCapabilityError, is_local,
    validate_local_capabilities,
};
pub use media::{
    CODE_CAPABILITY, CodeDelivery, CodeErrorCorrection, CodeFormat, CodePayloadSpec,
    CodePayloadType, CodeTemplate, DECODABLE_MEDIA_TYPES, IMAGE_CAPABILITY, ImageAnimation,
    ImageFit, ImageOutputFormat, ImageTarget, ImageTargetKind, MediaDeclarationError,
    MediaMetadata, SVG_MEDIA_TYPE, Symbology, parse_origin, url_origin,
    validate_media_declarations,
};
pub use phone::{PhoneRegion, PhoneRegionError, PhoneRejection, normalize_phone};
pub use recurrence::{
    MAX_DECLARABLE_OCCURRENCES, MAX_DECLARABLE_WINDOW_SECONDS, RECURRENCE_CAPABILITY,
    RECURRENCE_OPERATIONS, RecurrenceDeclarationError, RecurrenceMetadata, RecurrencePolicy,
    parse_window_seconds, validate_recurrence_declarations,
};
pub use types::*;
