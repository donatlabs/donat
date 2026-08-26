//! Donat v2-compatible metadata: the typed model and the YAML directory
//! loader. This crate is the single source of truth for "what the user
//! configured"; everything downstream (schema generation, permissions,
//! sqlgen) consumes these types and never re-reads YAML.

pub mod bounds;
pub mod documents;
pub mod iam;
pub mod ingest;
pub mod invoke;
pub mod limits;
mod loader;
pub mod local;
pub mod media;
mod phone;
pub mod quotas;
pub mod recurrence;
pub mod tenancy;
mod types;

pub use bounds::{
    BoundsError, PermissionsMetadata, UnboundedPolicy, UnboundedReason, binds_caller,
    validate_permission_bounds,
};
pub use documents::{
    DOCUMENT_CAPABILITY, DocumentTemplate, DocumentTemplateBounds, DocumentTemplateError,
    DocumentTemplateKind, HTML_SCALAR, RESERVED_INPUT_KEYS, validate_document_templates,
};
pub use iam::{
    ActionMapping, ActionTemplates, CommandActionMapping, CommandActionOverride, GrantRelation,
    GrantWriteTarget, IamDeclarationError, IamMetadata, IamOperation, ResourceOverride,
    validate_iam_declaration,
};
pub use ingest::{
    INGEST_CAPABILITY, INGEST_INPUT_KEYS, INGEST_SCALARS, IngestBounds, IngestColumn,
    IngestRowErrorPolicy, IngestSchema, IngestSchemaError, IngestSchemaKind, IngestSheetSelector,
    schema_pin, validate_ingest_schemas,
};
pub use invoke::{
    Bind, Foreach, InvokeSession, InvokeTarget, ThenCommand, Unnest, cron_target, event_target,
    validate_invoke_targets,
};
pub use limits::{Ceiling, LimitsMetadata};
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
pub use quotas::{
    Entitlement, QuotaConsumer, QuotaCounters, QuotaDeclarationError, QuotaLimitLookup,
    QuotaLimits, QuotaMetadata, validate_quota_declaration,
};
pub use recurrence::{
    MAX_DECLARABLE_OCCURRENCES, MAX_DECLARABLE_WINDOW_SECONDS, RECURRENCE_CAPABILITY,
    RECURRENCE_OPERATIONS, RecurrenceDeclarationError, RecurrenceMetadata, RecurrencePolicy,
    parse_window_seconds, validate_recurrence_declarations,
};
pub use tenancy::{
    ColumnBinding, CrossTenantRead, SharedAccess, SubjectBinding, TableScope, TenancyBinding,
    TenancyDeclarationError, TenancyMetadata, TenancyTrust, TenantExemption, TenantKeyOverride,
    TenantRegistry, TenantStatusGate, UnscopedStepPolicy, validate_tenancy_declaration,
    validate_untenanted_commands,
};
pub use types::*;
