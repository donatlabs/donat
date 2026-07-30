use std::collections::BTreeSet;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};

use donat_connector_abi::{
    AuthenticatorId, CapabilityId, CodecId, CompiledStepId, ConnectorErrorClass, ConnectorId,
    CredentialFieldId, CredentialSpecId, NormalizerId, OperationId, OriginId, ProcessorFamilyId,
    StaticErrorCode, StaticSafeMessage, TriggerId, catalog_construction,
};
use donat_value_contract::{TypedValue, ValueContractCatalog};
use serde::{Deserialize, Serialize};

use crate::{CatalogError, ContractFact, NoticeId, SourceRecordId};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StableSemver {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl StableSemver {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct SelectedResponseHeader {
    pub canonical_lowercase_header_name: String,
    pub capability: CapabilityId,
}

pub type ApiIdentity = String;
pub type ProviderId = String;
pub type StaticBodyPointer = String;
pub type StaticDnsName = String;
pub type StaticHeaderName = String;
pub type StaticHeaderValue = String;
pub type StaticHttpMethod = String;
pub type StaticJsonPointer = String;
pub type StaticPathTemplate = String;
pub type StaticProviderCode = String;
pub type StaticQueryKey = String;
pub type StaticScope = String;
pub type ProviderIdempotencyScope = String;
pub type LicenseIdentity = String;

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct VersionedProcessorRef<Id> {
    pub id: Id,
    pub implementation_revision: u32,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct VersionedOperationReference {
    pub operation: OperationId,
    pub version: StableSemver,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct VersionedCredentialReference {
    pub credential: CredentialSpecId,
    pub version: StableSemver,
}

pub struct ConnectorManifest {
    pub connector: ConnectorId,
    pub connector_version: StableSemver,
    pub manifest_version: u32,
    pub runtime_abi_epoch: u32,
    pub value_language_epoch: u32,
    pub provider: ProviderId,
    pub api_identity: ApiIdentity,
    pub credentials: Vec<CredentialSpec>,
    pub origins: Vec<FixedOrigin>,
    pub operations: Vec<OperationSpec>,
    pub triggers: Vec<TriggerSpec>,
    pub provenance: Vec<ManifestProvenanceReference>,
}

pub struct CredentialSpec {
    pub credential: CredentialSpecId,
    pub version: StableSemver,
    pub fields: Vec<CredentialFieldSpec>,
    pub auth_plan: AuthPlan,
    pub allowed_origins: Vec<OriginId>,
    pub scopes: Vec<StaticScope>,
    pub auth_processor: Option<VersionedProcessorRef<AuthenticatorId>>,
    pub credential_test_operation: Option<VersionedOperationReference>,
    pub bounds: CredentialBounds,
}

pub struct CredentialFieldSpec {
    pub field: CredentialFieldId,
    pub required: bool,
    pub secret: SecretClassification,
    pub maximum_bytes: NonZeroU32,
    pub redaction: RedactionPlan,
}

pub struct CredentialBounds {
    pub maximum_field_bytes: NonZeroU32,
    pub maximum_aggregate_bytes: NonZeroU32,
    pub maximum_token_bytes: NonZeroU32,
}

#[allow(clippy::large_enum_variant)]
pub enum AuthPlan {
    FixedHeaderApiKey {
        field: CredentialFieldId,
        header: StaticHeaderName,
    },
    FixedQueryApiKey {
        field: CredentialFieldId,
        query: StaticQueryKey,
    },
    Bearer {
        token: CredentialFieldId,
    },
    HttpBasic {
        username: CredentialFieldId,
        password: CredentialFieldId,
    },
    OAuth2ClientCredentials {
        client_id: CredentialFieldId,
        client_secret: CredentialFieldId,
        token_origin: OriginId,
        token_step: CompiledStepId,
        scopes: Vec<StaticScope>,
        token_pointer: StaticJsonPointer,
    },
    PreprovisionedOAuthAccessToken {
        token: CredentialFieldId,
    },
}

pub struct FixedOrigin {
    pub origin: OriginId,
    pub scheme: HttpsOnly,
    pub host: StaticDnsName,
    pub port: NonZeroU16,
    pub network_policy: NetworkPolicy,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct HttpsOnly;

pub enum NetworkPolicy {
    PublicOnly,
    PrivateAllowed { policy: String },
}

pub struct CompiledStepSpec {
    pub step: CompiledStepId,
    pub method: StaticHttpMethod,
    pub origin: OriginId,
    pub path: StaticPathTemplate,
    pub query: Vec<CompiledQueryBinding>,
    pub headers: Vec<CompiledHeaderBinding>,
    pub credential_action: Option<CompiledCredentialAction>,
    pub request: CompiledRequestShape,
    pub success_statuses: Vec<StatusRange>,
    pub response: CompiledResponseShape,
    pub selected_response_headers: Vec<SelectedResponseHeader>,
    pub bounds: StepBounds,
}

impl CompiledStepSpec {
    pub fn minimal_for_identity(step: CompiledStepId) -> Self {
        let one = NonZeroU32::new(1).expect("one is nonzero");
        Self {
            step,
            method: "GET".to_owned(),
            origin: OriginId::literal("identity.origin"),
            path: "/".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            credential_action: None,
            request: CompiledRequestShape::None,
            success_statuses: vec![StatusRange {
                minimum: 200,
                maximum: 299,
            }],
            response: CompiledResponseShape::Json {
                mappings: Vec::new(),
            },
            selected_response_headers: Vec::new(),
            bounds: StepBounds {
                maximum_headers: one,
                maximum_header_bytes: one,
                maximum_url_bytes: one,
                maximum_request_bytes: one,
                maximum_response_bytes: one,
                maximum_json_depth: one,
                maximum_json_nodes: one,
                maximum_inline_binary_bytes: one,
                deadline_ms: NonZeroU64::new(1).expect("one is nonzero"),
            },
        }
    }
}

pub struct StepBounds {
    pub maximum_headers: NonZeroU32,
    pub maximum_header_bytes: NonZeroU32,
    pub maximum_url_bytes: NonZeroU32,
    pub maximum_request_bytes: NonZeroU32,
    pub maximum_response_bytes: NonZeroU32,
    pub maximum_json_depth: NonZeroU32,
    pub maximum_json_nodes: NonZeroU32,
    pub maximum_inline_binary_bytes: NonZeroU32,
    pub deadline_ms: NonZeroU64,
}

pub struct OperationBounds {
    pub maximum_calls: NonZeroU32,
    pub maximum_pages: NonZeroU32,
    pub maximum_items: NonZeroU32,
    pub maximum_aggregate_request_bytes: NonZeroU32,
    pub maximum_aggregate_response_bytes: NonZeroU32,
    pub maximum_output_canonical_bytes: NonZeroU32,
    pub maximum_redirects: u8,
    pub deadline_ms: NonZeroU64,
}

pub struct CompiledBinding {
    pub field: String,
    pub source: CompiledBindingSource,
    pub required: bool,
    pub default: Option<TypedValue>,
    pub mapping: Option<String>,
}

pub enum CompiledBindingSource {
    Input,
    Constant { value: TypedValue },
}

pub struct CompiledQueryBinding {
    pub name: StaticQueryKey,
    pub binding: CompiledBinding,
}

pub struct CompiledHeaderBinding {
    pub name: StaticHeaderName,
    pub binding: CompiledBinding,
}

pub struct CompiledCredentialAction {
    pub credential: CredentialSpecId,
}

pub enum CompiledRequestShape {
    None,
    Json { bindings: Vec<String> },
    FormUrlencoded { bindings: Vec<String> },
    Multipart { bindings: Vec<String> },
    RawBytes { binding: String },
}

pub struct ResponseMapping {
    pub pointer: StaticJsonPointer,
    pub target: String,
}

pub enum CompiledResponseShape {
    Json { mappings: Vec<ResponseMapping> },
    RawBytes { target: String },
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct StatusRange {
    pub minimum: u16,
    pub maximum: u16,
}

pub struct OperationSpec {
    pub connector: ConnectorId,
    pub connector_version: StableSemver,
    pub operation: OperationId,
    pub operation_version: StableSemver,
    pub runtime_abi_epoch: u32,
    pub value_language_epoch: u32,
    pub input: ValueContractCatalog,
    pub input_contract_sha256: [u8; 32],
    pub output: ValueContractCatalog,
    pub output_contract_sha256: [u8; 32],
    pub credential: Option<VersionedCredentialReference>,
    pub origins: Vec<FixedOrigin>,
    pub steps: Vec<CompiledStepSpec>,
    pub pre_request_transforms: Vec<VersionedProcessorRef<ProcessorFamilyId>>,
    pub post_response_transforms: Vec<VersionedProcessorRef<ProcessorFamilyId>>,
    pub operation_processor: Option<VersionedProcessorRef<ProcessorFamilyId>>,
    pub effect: OperationEffect,
    pub pagination: PaginationPlan,
    pub error_map: ErrorMap,
    pub capacity: CapacityDefaults,
    pub rate: RateDefaults,
    pub serialization_key_default: Option<TypedSerializationKeyDefault>,
    pub bounds: OperationBounds,
    pub resolved_fact_values: Vec<ResolvedFactValue>,
}

impl OperationSpec {
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.steps.is_empty() || self.origins.is_empty() {
            return Err(CatalogError::new(
                "catalog_operation_incomplete",
                "operation requires origins and steps",
            ));
        }
        if self.steps.len() != 1 && self.operation_processor.is_none() {
            return Err(CatalogError::new(
                "catalog_operation_processor_required",
                "multiple steps require a static operation processor",
            ));
        }
        let mut step_ids = BTreeSet::new();
        for step in &self.steps {
            if !step_ids.insert(step.step.as_str()) {
                return Err(CatalogError::new(
                    "catalog_operation_duplicate_step",
                    step.step.as_str(),
                ));
            }
            if !self
                .origins
                .iter()
                .any(|origin| origin.origin == step.origin)
            {
                return Err(CatalogError::new(
                    "catalog_operation_missing_origin",
                    step.origin.as_str(),
                ));
            }
            if step.success_statuses.is_empty()
                || step
                    .success_statuses
                    .iter()
                    .any(|status| status.minimum > status.maximum)
            {
                return Err(CatalogError::new(
                    "catalog_operation_invalid_status",
                    "invalid success status range",
                ));
            }
            validate_selected_headers(step)?;
        }
        match &self.effect {
            OperationEffect::ReadOnly => {}
            OperationEffect::ProviderIdempotent { side_effect_steps } => {
                if side_effect_steps.len() != self.steps.len() {
                    return Err(CatalogError::new(
                        "catalog_operation_effect_incomplete",
                        "every side-effecting step requires one idempotency record",
                    ));
                }
                let mut covered = BTreeSet::new();
                for side_effect in side_effect_steps {
                    if !step_ids.contains(side_effect.step.as_str())
                        || !covered.insert(side_effect.step.as_str())
                        || side_effect.clock_safety_margin_ms >= side_effect.minimum_retention_ms
                    {
                        return Err(CatalogError::new(
                            "catalog_operation_effect_incomplete",
                            "side-effect evidence is missing, duplicate, or invalid",
                        ));
                    }
                }
            }
        }
        validate_error_map(&self.error_map, &self.steps)
    }
}

fn validate_selected_headers(step: &CompiledStepSpec) -> Result<(), CatalogError> {
    if step.selected_response_headers.len() > 64 {
        return Err(CatalogError::new(
            "catalog_selected_header_limit",
            "at most 64 selected response headers are allowed",
        ));
    }
    let mut names = BTreeSet::new();
    let mut capabilities = BTreeSet::new();
    for selected in &step.selected_response_headers {
        if selected.canonical_lowercase_header_name
            != selected
                .canonical_lowercase_header_name
                .to_ascii_lowercase()
            || !names.insert(selected.canonical_lowercase_header_name.as_str())
            || !capabilities.insert(selected.capability.as_str())
        {
            return Err(CatalogError::new(
                "catalog_selected_header_ambiguous",
                "selected response header mappings must be unique",
            ));
        }
    }
    Ok(())
}

pub enum OperationEffect {
    ReadOnly,
    ProviderIdempotent {
        side_effect_steps: Vec<ProviderIdempotentStep>,
    },
}

pub struct ProviderIdempotentStep {
    pub step: CompiledStepId,
    pub fixed_binding: FixedIdempotencyBinding,
    pub scope: ProviderIdempotencyScope,
    pub minimum_retention_ms: NonZeroU64,
    pub clock_safety_margin_ms: NonZeroU64,
}

pub enum FixedIdempotencyBinding {
    Header { name: StaticHeaderName },
    BodyField { pointer: StaticBodyPointer },
}

pub struct CapacityDefaults {
    pub maximum_in_flight: NonZeroU32,
}

pub struct RateDefaults {
    pub burst: NonZeroU32,
    pub refill_interval_ms: NonZeroU64,
}

pub struct TypedSerializationKeyDefault {
    pub field: String,
    pub value: TypedValue,
}

pub struct ResolvedFactValue {
    pub use_site: String,
    pub value: TypedValue,
}

pub struct PaginationBounds {
    pub maximum_calls: NonZeroU32,
    pub maximum_pages: NonZeroU32,
    pub maximum_items: NonZeroU32,
    pub maximum_response_bytes: NonZeroU32,
    pub maximum_aggregate_response_bytes: NonZeroU32,
    pub maximum_output_canonical_bytes: NonZeroU32,
}

pub enum PaginationPlan {
    None,
    Cursor {
        request_binding: String,
        response_pointer: StaticJsonPointer,
        bounds: PaginationBounds,
    },
    OffsetLimit {
        offset_binding: String,
        limit_binding: String,
        initial_offset: u64,
        page_size: NonZeroU32,
        bounds: PaginationBounds,
    },
    PageNumber {
        page_binding: String,
        page_size_binding: String,
        initial_page: NonZeroU64,
        page_size: NonZeroU32,
        bounds: PaginationBounds,
    },
    LinkRelation {
        relation: String,
        selected_header: SelectedResponseHeader,
        bounds: PaginationBounds,
    },
    Processor {
        processor: VersionedProcessorRef<ProcessorFamilyId>,
        bounds: PaginationBounds,
    },
}

pub struct ErrorMap {
    pub rules: Vec<ErrorRule>,
    pub fallback: CompleteErrorFallback,
}

pub struct ErrorRule {
    pub matcher: ErrorMatcher,
    pub action: ErrorAction,
}

pub struct ErrorAction {
    pub class: ConnectorErrorClass,
    pub code: StaticErrorCode,
    pub safe_message: StaticSafeMessage,
    pub retry_after: RetryAfterPolicy,
    pub correlations: Vec<ErrorCorrelationBinding>,
}

impl ErrorAction {
    pub fn try_new(
        class: ConnectorErrorClass,
        code: &str,
        safe_message: &str,
        retry_after: RetryAfterPolicy,
        correlations: Vec<ErrorCorrelationBinding>,
    ) -> Result<Self, CatalogError> {
        if correlations.len() > 64 {
            return Err(CatalogError::new(
                "catalog_selected_header_limit",
                "at most 64 error correlations are allowed",
            ));
        }
        Ok(Self {
            class,
            code: catalog_construction::static_error_code(code).map_err(|_| {
                CatalogError::new("catalog_error_map_invalid", "invalid static error code")
            })?,
            safe_message: catalog_construction::static_safe_message(safe_message).map_err(
                |_| CatalogError::new("catalog_error_map_invalid", "invalid safe message"),
            )?,
            retry_after,
            correlations,
        })
    }
}

pub struct ErrorCorrelationBinding {
    pub canonical_lowercase_header_name: StaticHeaderName,
    pub capability: CapabilityId,
    pub step: CompiledStepId,
}

pub struct CompleteErrorFallback {
    pub transport: ErrorAction,
    pub timeout: ErrorAction,
    pub http_429: ErrorAction,
    pub http_5xx: ErrorAction,
    pub authentication: ErrorAction,
    pub validation: ErrorAction,
    pub permanent: ErrorAction,
    pub invariant: ErrorAction,
}

pub enum ErrorMatcher {
    Status(StatusRange),
    ProviderCode {
        pointer: StaticJsonPointer,
        codes: Vec<StaticProviderCode>,
    },
    Header {
        name: StaticHeaderName,
        values: Vec<StaticHeaderValue>,
    },
    MalformedDeclaredSuccess,
}

pub enum RetryAfterPolicy {
    Never,
    RetryAfterHeader {
        step: CompiledStepId,
        capability: CapabilityId,
        maximum_seconds: NonZeroU32,
    },
}

fn validate_error_map(
    error_map: &ErrorMap,
    steps: &[CompiledStepSpec],
) -> Result<(), CatalogError> {
    let actions = [
        &error_map.fallback.transport,
        &error_map.fallback.timeout,
        &error_map.fallback.http_429,
        &error_map.fallback.http_5xx,
        &error_map.fallback.authentication,
        &error_map.fallback.validation,
        &error_map.fallback.permanent,
        &error_map.fallback.invariant,
    ];
    for action in actions
        .into_iter()
        .chain(error_map.rules.iter().map(|rule| &rule.action))
    {
        for correlation in &action.correlations {
            let matches = steps
                .iter()
                .filter(|step| step.step == correlation.step)
                .flat_map(|step| &step.selected_response_headers)
                .filter(|selected| {
                    selected.canonical_lowercase_header_name
                        == correlation.canonical_lowercase_header_name
                        && selected.capability == correlation.capability
                })
                .count();
            if matches != 1 {
                return Err(CatalogError::new(
                    "catalog_selected_header_ambiguous",
                    "error correlation must resolve exactly once",
                ));
            }
        }
        if let RetryAfterPolicy::RetryAfterHeader {
            step, capability, ..
        } = &action.retry_after
        {
            let matches = steps
                .iter()
                .filter(|candidate| candidate.step == *step)
                .flat_map(|candidate| &candidate.selected_response_headers)
                .filter(|selected| selected.capability == *capability)
                .count();
            if matches != 1 {
                return Err(CatalogError::new(
                    "catalog_selected_header_ambiguous",
                    "Retry-After mapping must resolve exactly once",
                ));
            }
        }
    }
    Ok(())
}

pub enum SecretClassification {
    Secret,
    Sensitive,
    NonSecret,
}

pub enum RedactionPlan {
    Omit,
    Fixed { replacement: String },
    PreserveLast { characters: u8 },
}

pub struct SubscriptionOperationIds {
    pub create: OperationId,
    pub delete: OperationId,
    pub check: Option<OperationId>,
}

#[allow(clippy::large_enum_variant)]
pub enum TriggerSpec {
    Webhook {
        connector: ConnectorId,
        connector_version: StableSemver,
        trigger: TriggerId,
        trigger_version: StableSemver,
        event_version: StableSemver,
        runtime_abi_epoch: u32,
        authenticator: VersionedProcessorRef<AuthenticatorId>,
        codec: VersionedProcessorRef<CodecId>,
        normalizer: VersionedProcessorRef<NormalizerId>,
        selected_headers: Vec<StaticHeaderName>,
        raw_body_max_bytes: NonZeroU32,
        timestamp_window_ms: NonZeroU64,
        event_id: ValueContractCatalog,
        event_type: ValueContractCatalog,
        output: ValueContractCatalog,
        redaction: RedactionPlan,
        subscription_operations: Option<SubscriptionOperationIds>,
    },
    Poll {
        connector: ConnectorId,
        connector_version: StableSemver,
        trigger: TriggerId,
        trigger_version: StableSemver,
        event_version: StableSemver,
        runtime_abi_epoch: u32,
        checkpoint: ValueContractCatalog,
        processor: VersionedProcessorRef<ProcessorFamilyId>,
        event_type: ValueContractCatalog,
        per_poll_event_limit: NonZeroU32,
        bounds: OperationBounds,
    },
}

pub struct ManifestProvenanceReference {
    pub source_record_id: SourceRecordId,
    pub artifact_hashes: Vec<crate::ArtifactHash>,
    pub license_id: LicenseIdentity,
    pub notice_id: NoticeId,
    pub contract_facts: Vec<ResolvedContractFactBinding>,
}

pub struct ResolvedContractFactBinding {
    pub use_site: String,
    pub fact: ContractFact,
}

impl ConnectorManifest {
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.manifest_version == 0
            || self.runtime_abi_epoch == 0
            || self.value_language_epoch == 0
            || self.origins.is_empty()
            || self.operations.is_empty()
            || self.provenance.is_empty()
        {
            return Err(CatalogError::new(
                "catalog_manifest_incomplete",
                "manifest requires finite identities and collections",
            ));
        }
        let origin_ids: BTreeSet<_> = self.origins.iter().map(|origin| origin.origin).collect();
        if origin_ids.len() != self.origins.len() {
            return Err(CatalogError::new(
                "catalog_manifest_duplicate",
                "duplicate origin",
            ));
        }
        for credential in &self.credentials {
            if credential.fields.is_empty()
                || credential.allowed_origins.is_empty()
                || credential
                    .allowed_origins
                    .iter()
                    .any(|origin| !origin_ids.contains(origin))
            {
                return Err(CatalogError::new(
                    "catalog_credential_incomplete",
                    "credential fields/origins are incomplete",
                ));
            }
        }
        for operation in &self.operations {
            if operation.connector != self.connector
                || operation.connector_version != self.connector_version
            {
                return Err(CatalogError::new(
                    "catalog_manifest_reference_mismatch",
                    "operation connector identity differs from manifest",
                ));
            }
            operation.validate()?;
        }
        validate_fact_use_sites(self)
    }
}

fn validate_fact_use_sites(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    let semantic: BTreeSet<_> = manifest
        .operations
        .iter()
        .flat_map(|operation| &operation.resolved_fact_values)
        .map(|binding| binding.use_site.as_str())
        .collect();
    let origin: BTreeSet<_> = manifest
        .provenance
        .iter()
        .flat_map(|reference| &reference.contract_facts)
        .map(|binding| binding.use_site.as_str())
        .collect();
    let semantic_count: usize = manifest
        .operations
        .iter()
        .map(|operation| operation.resolved_fact_values.len())
        .sum();
    let origin_count: usize = manifest
        .provenance
        .iter()
        .map(|reference| reference.contract_facts.len())
        .sum();
    if semantic.len() != semantic_count || origin.len() != origin_count || semantic != origin {
        return Err(CatalogError::new(
            "catalog_fact_use_site_mismatch",
            "resolved fact value/origin use sites must be unique and equal",
        ));
    }
    Ok(())
}
