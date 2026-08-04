use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};

use donat_connector_abi::{
    AuthenticatorId, CapabilityId, CodecId, CompiledStepId, ConnectorErrorClass, ConnectorId,
    CredentialFieldId, CredentialSpecId, NormalizerId, OperationId, OriginId, ProcessorFamilyId,
    StaticErrorCode, StaticSafeMessage, TriggerId, catalog_construction,
};
use donat_value_contract::{CanonicalNumber, TypedValue, ValueContractCatalog, ValueContractError};
use serde::{Deserialize, Serialize};

use crate::{
    AcceptedRecordCatalog, CatalogError, ContractFact, DonatPolicyId, LicenseDecision, NoticeId,
    SourceRecordId, resolve_fact_bindings, selected_response_header, value_contract_material,
    value_contract_sha256,
};

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

#[derive(Clone)]
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

/// A manifest that has passed the complete offline catalog compiler.
///
/// Construction is intentionally private so runtime code cannot mistake a
/// parsed or hand-built manifest for an executable one.
pub struct CheckedConnectorManifest<'checked> {
    manifest: &'checked ConnectorManifest,
    accepted_records: &'checked AcceptedRecordCatalog,
    reviewed_policies: &'checked BTreeMap<DonatPolicyId, TypedValue>,
    fact_requirements: CheckedFactRequirements,
}

impl CheckedConnectorManifest<'_> {
    pub const fn manifest(&self) -> &ConnectorManifest {
        self.manifest
    }

    pub(crate) const fn accepted_records(&self) -> &AcceptedRecordCatalog {
        self.accepted_records
    }

    pub(crate) const fn reviewed_policies(&self) -> &BTreeMap<DonatPolicyId, TypedValue> {
        self.reviewed_policies
    }

    pub(crate) const fn fact_requirements(&self) -> &CheckedFactRequirements {
        &self.fact_requirements
    }
}

/// Typed operation input used to derive an opaque fact-domain proof.
///
/// Callers identify the normalized operation and supply its typed effect;
/// they cannot directly choose provider-versus-policy ownership.
pub struct OperationFactRequirement<'operation> {
    operation: OperationId,
    effect: &'operation OperationEffect,
    resolved_fact_values: &'operation [ResolvedFactValue],
}

impl<'operation> OperationFactRequirement<'operation> {
    pub const fn new(
        operation: OperationId,
        effect: &'operation OperationEffect,
        resolved_fact_values: &'operation [ResolvedFactValue],
    ) -> Self {
        Self {
            operation,
            effect,
            resolved_fact_values,
        }
    }

    fn from_operation(operation: &'operation OperationSpec) -> Self {
        Self::new(
            operation.operation,
            &operation.effect,
            &operation.resolved_fact_values,
        )
    }
}

/// Opaque proof of the origin domain required by each normalized fact use
/// site. It can only be constructed by exhaustive inspection of typed
/// operation effects.
pub struct CheckedFactRequirements {
    domains: BTreeMap<String, RequiredFactDomain>,
}

impl CheckedFactRequirements {
    pub(crate) fn required_domain(&self, use_site: &str) -> Option<RequiredFactDomain> {
        self.domains.get(use_site).copied()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum RequiredFactDomain {
    ProviderEvidence,
    DonatPolicy,
}

impl RequiredFactDomain {
    pub(crate) const fn accepts(self, fact: &ContractFact) -> bool {
        matches!(
            (self, fact),
            (
                Self::ProviderEvidence,
                ContractFact::ProviderEvidence {
                    source_record_id: _,
                    fact_id: _,
                }
            ) | (
                Self::DonatPolicy,
                ContractFact::DonatPolicy {
                    policy_id: _,
                    value: _,
                },
            )
        )
    }
}

/// Derive exact fact-domain requirements from normalized typed operations.
///
/// Generic fact use sites are provider-evidence owned. A typed effect may
/// override that default only for behavior it structurally owns.
pub fn check_fact_requirements(
    operations: &[OperationFactRequirement<'_>],
) -> Result<CheckedFactRequirements, CatalogError> {
    let mut domains = BTreeMap::new();
    for operation in operations {
        for binding in operation.resolved_fact_values {
            if binding.use_site.is_empty()
                || domains
                    .insert(
                        binding.use_site.clone(),
                        RequiredFactDomain::ProviderEvidence,
                    )
                    .is_some()
            {
                return Err(CatalogError::new(
                    "catalog_fact_binding_mismatch",
                    "semantic fact use sites must be nonempty and unique",
                ));
            }
        }
        match operation.effect {
            OperationEffect::ReadOnly => {}
            OperationEffect::ProviderIdempotent { side_effect_steps } => {
                for side_effect in side_effect_steps {
                    let step = side_effect.step.as_str();
                    let scope = format!(
                        "operation.{}.step.{step}.idempotency.scope",
                        operation.operation.as_str()
                    );
                    let retention = format!(
                        "operation.{}.step.{step}.idempotency.minimum_retention_ms",
                        operation.operation.as_str()
                    );
                    let margin = format!(
                        "operation.{}.step.{step}.idempotency.clock_safety_margin_ms",
                        operation.operation.as_str()
                    );
                    for use_site in [scope, retention] {
                        if let Some(domain) = domains.get_mut(&use_site) {
                            *domain = RequiredFactDomain::ProviderEvidence;
                        }
                    }
                    if let Some(domain) = domains.get_mut(&margin) {
                        *domain = RequiredFactDomain::DonatPolicy;
                    }
                }
            }
        }
    }
    Ok(CheckedFactRequirements { domains })
}

pub fn compile_connector_manifest<'checked>(
    manifest: &'checked ConnectorManifest,
    accepted_records: &'checked AcceptedRecordCatalog,
    reviewed_policies: &'checked BTreeMap<DonatPolicyId, TypedValue>,
) -> Result<CheckedConnectorManifest<'checked>, CatalogError> {
    validate_manifest_structure(manifest)?;
    validate_manifest_primitives(manifest)?;
    validate_manifest_step_identity(manifest)?;
    validate_manifest_credentials(manifest)?;
    validate_manifest_contracts(manifest)?;
    validate_manifest_selected_headers(manifest)?;
    validate_manifest_error_maps(manifest)?;
    validate_manifest_effects(manifest)?;
    validate_fact_use_sites(manifest)?;
    let fact_requirements = check_fact_requirements(
        &manifest
            .operations
            .iter()
            .map(OperationFactRequirement::from_operation)
            .collect::<Vec<_>>(),
    )?;
    validate_manifest_identity(manifest)?;
    validate_manifest_provenance(
        manifest,
        accepted_records,
        reviewed_policies,
        &fact_requirements,
    )?;
    Ok(CheckedConnectorManifest {
        manifest,
        accepted_records,
        reviewed_policies,
        fact_requirements,
    })
}

fn validate_manifest_step_identity(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    for operation in &manifest.operations {
        let mut steps = BTreeSet::new();
        for step in &operation.steps {
            if !steps.insert(step.step) {
                return Err(CatalogError::new(
                    "catalog_operation_duplicate_step",
                    step.step.as_str(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_manifest_structure(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    if manifest.manifest_version == 0
        || manifest.runtime_abi_epoch == 0
        || manifest.value_language_epoch == 0
        || manifest.origins.is_empty()
        || manifest.operations.is_empty()
        || manifest.provenance.is_empty()
    {
        return Err(CatalogError::new(
            "catalog_manifest_incomplete",
            "manifest requires nonzero epochs and complete collections",
        ));
    }
    if manifest
        .credentials
        .iter()
        .any(|credential| credential.fields.is_empty() || credential.allowed_origins.is_empty())
        || manifest.operations.iter().any(|operation| {
            operation.origins.is_empty()
                || operation.steps.is_empty()
                || operation
                    .steps
                    .iter()
                    .any(|step| step.success_statuses.is_empty())
        })
    {
        return Err(CatalogError::new(
            "catalog_manifest_incomplete",
            "credential and operation structural collections are nonempty",
        ));
    }
    Ok(())
}

fn validate_manifest_primitives(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    if !valid_catalog_id(&manifest.provider) || !valid_catalog_id(&manifest.api_identity) {
        return invalid_manifest_primitive("provider/API identity");
    }
    for origin in &manifest.origins {
        if !valid_dns_name(&origin.host) {
            return invalid_manifest_primitive("fixed-origin DNS host");
        }
        if let NetworkPolicy::PrivateAllowed { policy } = &origin.network_policy
            && !valid_catalog_id(policy)
        {
            return invalid_manifest_primitive("private-network policy");
        }
    }
    for credential in &manifest.credentials {
        if credential
            .auth_processor
            .as_ref()
            .is_some_and(|processor| processor.implementation_revision == 0)
        {
            return invalid_manifest_primitive("credential processor revision");
        }
        if credential
            .scopes
            .iter()
            .any(|scope| !valid_catalog_token(scope))
        {
            return invalid_manifest_primitive("credential scope");
        }
        match &credential.auth_plan {
            AuthPlan::FixedHeaderApiKey { header, .. } => {
                validate_manifest_header(header)?;
            }
            AuthPlan::FixedQueryApiKey { query, .. } => {
                validate_manifest_query_key(query)?;
            }
            AuthPlan::OAuth2ClientCredentials {
                scopes,
                token_pointer,
                ..
            } => {
                if scopes.iter().any(|scope| !valid_catalog_token(scope))
                    || !valid_json_pointer(token_pointer)
                {
                    return invalid_manifest_primitive("OAuth scope or token pointer");
                }
            }
            AuthPlan::Bearer { .. }
            | AuthPlan::HttpBasic { .. }
            | AuthPlan::PreprovisionedOAuthAccessToken { .. } => {}
        }
    }
    for operation in &manifest.operations {
        if operation
            .pre_request_transforms
            .iter()
            .chain(&operation.post_response_transforms)
            .any(|processor| processor.implementation_revision == 0)
            || operation
                .operation_processor
                .as_ref()
                .is_some_and(|processor| processor.implementation_revision == 0)
        {
            return invalid_manifest_primitive("operation processor revision");
        }
        for origin in &operation.origins {
            if !valid_dns_name(&origin.host) {
                return invalid_manifest_primitive("operation DNS host");
            }
        }
        for step in &operation.steps {
            if !matches!(
                step.method.as_str(),
                "DELETE" | "GET" | "HEAD" | "OPTIONS" | "PATCH" | "POST" | "PUT"
            ) || !valid_path_template(&step.path)
                || step
                    .success_statuses
                    .iter()
                    .any(|status| status.minimum > status.maximum)
            {
                return invalid_manifest_primitive(
                    "HTTP method, path template, or success status range",
                );
            }
            for query in &step.query {
                validate_manifest_query_key(&query.name)?;
                validate_compiled_binding_primitive(&query.binding)?;
            }
            for header in &step.headers {
                validate_manifest_header(&header.name)?;
                validate_compiled_binding_primitive(&header.binding)?;
            }
            match &step.request {
                CompiledRequestShape::RawBytes { binding } => {
                    if !valid_catalog_id(binding) {
                        return invalid_manifest_primitive("raw request binding");
                    }
                }
                CompiledRequestShape::Json { bindings }
                | CompiledRequestShape::FormUrlencoded { bindings }
                | CompiledRequestShape::Multipart { bindings } => {
                    if bindings.iter().any(|binding| !valid_catalog_id(binding)) {
                        return invalid_manifest_primitive("request binding");
                    }
                }
                CompiledRequestShape::None => {}
            }
            match &step.response {
                CompiledResponseShape::Json { mappings } => {
                    if mappings.iter().any(|mapping| {
                        !valid_json_pointer(&mapping.pointer) || !valid_catalog_id(&mapping.target)
                    }) {
                        return invalid_manifest_primitive("response mapping");
                    }
                }
                CompiledResponseShape::RawBytes { target } => {
                    if !valid_catalog_id(target) {
                        return invalid_manifest_primitive("raw response target");
                    }
                }
            }
            for selected in &step.selected_response_headers {
                validate_manifest_header(&selected.canonical_lowercase_header_name)?;
            }
        }
        validate_pagination_primitives(&operation.pagination)?;
        for rule in &operation.error_map.rules {
            match &rule.matcher {
                ErrorMatcher::ProviderCode { pointer, codes } => {
                    if !valid_json_pointer(pointer)
                        || codes.is_empty()
                        || codes.iter().any(|code| !valid_catalog_token(code))
                    {
                        return invalid_manifest_primitive("provider error code");
                    }
                }
                ErrorMatcher::Header { name, values } => {
                    validate_manifest_header(name)?;
                    if values.is_empty()
                        || values.iter().any(|value| {
                            value.is_empty()
                                || !value
                                    .bytes()
                                    .all(|byte| byte.is_ascii_graphic() || byte == b' ')
                        })
                    {
                        return invalid_manifest_primitive("error header value");
                    }
                }
                ErrorMatcher::Status(status) => {
                    if status.minimum > status.maximum {
                        return invalid_manifest_primitive("status range");
                    }
                }
                ErrorMatcher::MalformedDeclaredSuccess => {}
            }
        }
    }
    for trigger in &manifest.triggers {
        match trigger {
            TriggerSpec::Webhook {
                authenticator,
                codec,
                normalizer,
                selected_headers,
                ..
            } => {
                if [
                    authenticator.implementation_revision,
                    codec.implementation_revision,
                    normalizer.implementation_revision,
                ]
                .contains(&0)
                {
                    return invalid_manifest_primitive("webhook processor revision");
                }
                for header in selected_headers {
                    validate_manifest_header(header)?;
                }
            }
            TriggerSpec::Poll { processor, .. } => {
                if processor.implementation_revision == 0 {
                    return invalid_manifest_primitive("poll processor revision");
                }
            }
        }
    }
    for reference in &manifest.provenance {
        if !valid_license_identity(&reference.license_id)
            || reference
                .contract_facts
                .iter()
                .any(|binding| !valid_catalog_id(&binding.use_site))
        {
            return invalid_manifest_primitive("provenance license or use-site identity");
        }
    }
    Ok(())
}

fn validate_compiled_binding_primitive(binding: &CompiledBinding) -> Result<(), CatalogError> {
    if !valid_catalog_id(&binding.field)
        || binding
            .mapping
            .as_ref()
            .is_some_and(|mapping| !valid_catalog_token(mapping))
    {
        return invalid_manifest_primitive("compiled binding");
    }
    Ok(())
}

fn validate_pagination_primitives(pagination: &PaginationPlan) -> Result<(), CatalogError> {
    match pagination {
        PaginationPlan::None => Ok(()),
        PaginationPlan::Processor { processor, .. } => {
            if processor.implementation_revision == 0 {
                return invalid_manifest_primitive("pagination processor revision");
            }
            Ok(())
        }
        PaginationPlan::Cursor {
            request_binding,
            response_pointer,
            ..
        } => {
            if !valid_catalog_id(request_binding) || !valid_json_pointer(response_pointer) {
                return invalid_manifest_primitive("cursor pagination");
            }
            Ok(())
        }
        PaginationPlan::OffsetLimit {
            offset_binding,
            limit_binding,
            ..
        }
        | PaginationPlan::PageNumber {
            page_binding: offset_binding,
            page_size_binding: limit_binding,
            ..
        } => {
            if !valid_catalog_id(offset_binding) || !valid_catalog_id(limit_binding) {
                return invalid_manifest_primitive("pagination binding");
            }
            Ok(())
        }
        PaginationPlan::LinkRelation { relation, .. } => {
            if !valid_catalog_token(relation) {
                return invalid_manifest_primitive("link relation");
            }
            Ok(())
        }
    }
}

fn validate_manifest_credentials(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    let origins = manifest
        .origins
        .iter()
        .map(|origin| (origin.origin, origin))
        .collect::<BTreeMap<_, _>>();
    if origins.len() != manifest.origins.len() {
        return credential_incomplete("duplicate manifest origin");
    }
    let operation_ids = manifest
        .operations
        .iter()
        .map(|operation| (operation.operation, operation.operation_version))
        .collect::<BTreeSet<_>>();
    let mut credentials = BTreeMap::new();
    for credential in &manifest.credentials {
        if credentials
            .insert((credential.credential, credential.version), credential)
            .is_some()
        {
            return credential_incomplete("duplicate credential identity");
        }
        let fields = credential
            .fields
            .iter()
            .map(|field| field.field)
            .collect::<BTreeSet<_>>();
        if fields.len() != credential.fields.len()
            || credential
                .allowed_origins
                .iter()
                .any(|origin| !origins.contains_key(origin))
            || credential
                .credential_test_operation
                .as_ref()
                .is_some_and(|reference| {
                    !operation_ids.contains(&(reference.operation, reference.version))
                })
        {
            return credential_incomplete("credential field/origin/operation closure");
        }
        let require_field = |field: CredentialFieldId| {
            if fields.contains(&field) {
                Ok(())
            } else {
                credential_incomplete("auth-plan field does not resolve")
            }
        };
        match &credential.auth_plan {
            AuthPlan::FixedHeaderApiKey { field, .. }
            | AuthPlan::FixedQueryApiKey { field, .. } => require_field(*field)?,
            AuthPlan::Bearer { token } | AuthPlan::PreprovisionedOAuthAccessToken { token } => {
                require_field(*token)?
            }
            AuthPlan::HttpBasic { username, password } => {
                require_field(*username)?;
                require_field(*password)?;
            }
            AuthPlan::OAuth2ClientCredentials {
                client_id,
                client_secret,
                token_origin,
                token_step,
                ..
            } => {
                require_field(*client_id)?;
                require_field(*client_secret)?;
                if !credential.allowed_origins.contains(token_origin)
                    || manifest
                        .operations
                        .iter()
                        .flat_map(|operation| &operation.steps)
                        .filter(|step| step.step == *token_step && step.origin == *token_origin)
                        .count()
                        != 1
                {
                    return credential_incomplete("OAuth token origin/step does not resolve");
                }
            }
        }
    }

    for operation in &manifest.operations {
        let credential = match operation.credential {
            Some(reference) => Some(
                *credentials
                    .get(&(reference.credential, reference.version))
                    .ok_or_else(|| {
                        CatalogError::new(
                            "catalog_credential_incomplete",
                            "operation credential identity does not resolve",
                        )
                    })?,
            ),
            None => None,
        };
        let operation_origins = operation
            .origins
            .iter()
            .map(|origin| origin.origin)
            .collect::<BTreeSet<_>>();
        if operation_origins.len() != operation.origins.len()
            || operation.origins.iter().any(|origin| {
                origins
                    .get(&origin.origin)
                    .is_none_or(|manifest_origin| !same_origin(origin, manifest_origin))
            })
        {
            return credential_incomplete("operation origin differs from manifest origin");
        }
        for step in &operation.steps {
            if !operation_origins.contains(&step.origin) {
                return credential_incomplete("step origin is absent from operation");
            }
            match (&step.credential_action, credential) {
                (None, None) => {}
                (Some(action), Some(spec))
                    if action.credential == spec.credential
                        && spec.allowed_origins.contains(&step.origin) => {}
                _ => return credential_incomplete("step credential action/origin mismatch"),
            }
            validate_request_binding_closure(operation, step)?;
        }
    }
    Ok(())
}

fn same_origin(left: &FixedOrigin, right: &FixedOrigin) -> bool {
    left.origin == right.origin
        && left.host == right.host
        && left.port == right.port
        && match (&left.network_policy, &right.network_policy) {
            (NetworkPolicy::PublicOnly, NetworkPolicy::PublicOnly) => true,
            (
                NetworkPolicy::PrivateAllowed { policy: left },
                NetworkPolicy::PrivateAllowed { policy: right },
            ) => left == right,
            _ => false,
        }
}

fn validate_request_binding_closure(
    operation: &OperationSpec,
    step: &CompiledStepSpec,
) -> Result<(), CatalogError> {
    let declared = operation
        .input
        .roots
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut compiled = BTreeSet::new();
    for binding in step
        .query
        .iter()
        .map(|binding| &binding.binding)
        .chain(step.headers.iter().map(|binding| &binding.binding))
    {
        if !declared.contains(binding.field.as_str()) || !compiled.insert(binding.field.as_str()) {
            return credential_incomplete(
                "compiled query/header binding is unresolved or duplicate",
            );
        }
    }
    let request_bindings: &[String] = match &step.request {
        CompiledRequestShape::None => &[],
        CompiledRequestShape::Json { bindings }
        | CompiledRequestShape::FormUrlencoded { bindings }
        | CompiledRequestShape::Multipart { bindings } => bindings,
        CompiledRequestShape::RawBytes { binding } => std::slice::from_ref(binding),
    };
    for binding in request_bindings {
        if !declared.contains(binding.as_str()) || !compiled.insert(binding.as_str()) {
            return credential_incomplete("compiled request binding is unresolved or duplicate");
        }
    }
    Ok(())
}

fn validate_manifest_contracts(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    for operation in &manifest.operations {
        for contract in [&operation.input, &operation.output] {
            match contract.validate(&TypedValue::Object(BTreeMap::new())) {
                Ok(()) | Err(ValueContractError::InvalidValue(_)) => {}
                Err(error) => {
                    return contract_hash_mismatch(format!(
                        "invalid value-contract definition: {error}"
                    ));
                }
            }
        }
        if operation.input_contract_sha256 == [0; 32] || operation.output_contract_sha256 == [0; 32]
        {
            return contract_hash_mismatch("zero value-contract hash");
        }
        let input = value_contract_material(&operation.input, operation.value_language_epoch)?;
        let output = value_contract_material(&operation.output, operation.value_language_epoch)?;
        if value_contract_sha256(&input)?.as_bytes() != &operation.input_contract_sha256
            || value_contract_sha256(&output)?.as_bytes() != &operation.output_contract_sha256
        {
            return contract_hash_mismatch("stored value-contract hash is not recomputed hash");
        }
    }
    Ok(())
}

fn validate_manifest_selected_headers(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    for operation in &manifest.operations {
        for step in &operation.steps {
            if step.selected_response_headers.len() > 64 {
                return selected_header_invalid("selected-header limit");
            }
            let mut names = BTreeSet::new();
            let mut capabilities = BTreeSet::new();
            for selected in &step.selected_response_headers {
                let recomputed = selected_response_header(
                    operation.connector,
                    operation.operation,
                    operation.operation_version,
                    step.step,
                    &selected.canonical_lowercase_header_name,
                )?;
                if *selected != recomputed
                    || !names.insert(selected.canonical_lowercase_header_name.as_str())
                    || !capabilities.insert(selected.capability)
                {
                    return selected_header_invalid(
                        "selected header is forged, stale, or duplicate",
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_manifest_error_maps(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    for operation in &manifest.operations {
        let actions = [
            &operation.error_map.fallback.transport,
            &operation.error_map.fallback.timeout,
            &operation.error_map.fallback.http_429,
            &operation.error_map.fallback.http_5xx,
            &operation.error_map.fallback.authentication,
            &operation.error_map.fallback.validation,
            &operation.error_map.fallback.permanent,
            &operation.error_map.fallback.invariant,
        ];
        for action in actions
            .into_iter()
            .chain(operation.error_map.rules.iter().map(|rule| &rule.action))
        {
            for correlation in &action.correlations {
                let matches = operation
                    .steps
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
                    return error_map_invalid("error correlation is not exact and step-local");
                }
            }
            if let RetryAfterPolicy::RetryAfterHeader {
                step, capability, ..
            } = action.retry_after
            {
                let matches = operation
                    .steps
                    .iter()
                    .filter(|candidate| candidate.step == step)
                    .flat_map(|candidate| &candidate.selected_response_headers)
                    .filter(|selected| selected.capability == capability)
                    .count();
                if matches != 1 {
                    return error_map_invalid("Retry-After capability is foreign or unresolved");
                }
            }
        }
    }
    Ok(())
}

fn validate_manifest_effects(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    for operation in &manifest.operations {
        match &operation.effect {
            OperationEffect::ReadOnly => {}
            OperationEffect::ProviderIdempotent { side_effect_steps } => {
                if side_effect_steps.len() != operation.steps.len() {
                    return effect_incomplete("every side-effecting step needs exact evidence");
                }
                let mut steps = BTreeSet::new();
                for side_effect in side_effect_steps {
                    let Some(step) = operation
                        .steps
                        .iter()
                        .find(|candidate| candidate.step == side_effect.step)
                    else {
                        return effect_incomplete("idempotency evidence names an unknown step");
                    };
                    if !steps.insert(side_effect.step)
                        || side_effect.clock_safety_margin_ms >= side_effect.minimum_retention_ms
                        || !idempotency_binding_exists(step, &side_effect.fixed_binding)
                    {
                        return effect_incomplete(
                            "idempotency binding, retention, or margin is incomplete",
                        );
                    }
                    let scope_site = format!(
                        "operation.{}.step.{}.idempotency.scope",
                        operation.operation.as_str(),
                        step.step.as_str()
                    );
                    let retention_site = format!(
                        "operation.{}.step.{}.idempotency.minimum_retention_ms",
                        operation.operation.as_str(),
                        step.step.as_str()
                    );
                    let margin_site = format!(
                        "operation.{}.step.{}.idempotency.clock_safety_margin_ms",
                        operation.operation.as_str(),
                        step.step.as_str()
                    );
                    validate_optional_effect_fact(
                        operation,
                        &scope_site,
                        |value| matches!(value, TypedValue::String(value) if value == &side_effect.scope),
                    )?;
                    validate_optional_effect_fact(operation, &retention_site, |value| {
                        matches!(
                            value,
                            TypedValue::Number(CanonicalNumber::U64(value))
                                if *value == side_effect.minimum_retention_ms.get()
                        )
                    })?;
                    validate_optional_effect_fact(operation, &margin_site, |value| {
                        matches!(
                            value,
                            TypedValue::Number(CanonicalNumber::U64(value))
                                if *value == side_effect.clock_safety_margin_ms.get()
                        )
                    })?;
                }
            }
        }
    }
    Ok(())
}

fn validate_optional_effect_fact(
    operation: &OperationSpec,
    use_site: &str,
    expected: impl FnOnce(&TypedValue) -> bool,
) -> Result<(), CatalogError> {
    let mut matches = operation
        .resolved_fact_values
        .iter()
        .filter(|binding| binding.use_site == use_site);
    let Some(value) = matches.next().map(|binding| &binding.value) else {
        return Ok(());
    };
    if matches.next().is_some() {
        // The checked requirement map owns use-site uniqueness and its
        // catalog_fact_binding_mismatch error.
        return Ok(());
    }
    if !expected(value) {
        return effect_incomplete(
            "idempotency behavior differs from its exact admitted fact bindings",
        );
    }
    Ok(())
}

fn idempotency_binding_exists(step: &CompiledStepSpec, binding: &FixedIdempotencyBinding) -> bool {
    match binding {
        FixedIdempotencyBinding::Header { name } => {
            step.headers.iter().any(|header| header.name == *name)
        }
        FixedIdempotencyBinding::BodyField { pointer } => match &step.request {
            CompiledRequestShape::Json { bindings }
            | CompiledRequestShape::FormUrlencoded { bindings }
            | CompiledRequestShape::Multipart { bindings } => bindings.contains(pointer),
            CompiledRequestShape::RawBytes { binding } => binding == pointer,
            CompiledRequestShape::None => false,
        },
    }
}

fn validate_manifest_identity(manifest: &ConnectorManifest) -> Result<(), CatalogError> {
    let mut operation_ids = BTreeSet::new();
    for operation in &manifest.operations {
        if operation.connector != manifest.connector
            || operation.connector_version != manifest.connector_version
            || operation.runtime_abi_epoch != manifest.runtime_abi_epoch
            || operation.value_language_epoch != manifest.value_language_epoch
            || !operation_ids.insert((operation.operation, operation.operation_version))
        {
            return manifest_identity_mismatch("operation identity differs from manifest");
        }
        if operation.steps.len() != 1 && operation.operation_processor.is_none() {
            return manifest_identity_mismatch("multi-step operation lacks a processor identity");
        }
    }
    let mut trigger_ids = BTreeSet::new();
    for trigger in &manifest.triggers {
        let (connector, version, trigger_id, trigger_version, runtime_abi_epoch) = match trigger {
            TriggerSpec::Webhook {
                connector,
                connector_version,
                trigger,
                trigger_version,
                runtime_abi_epoch,
                ..
            }
            | TriggerSpec::Poll {
                connector,
                connector_version,
                trigger,
                trigger_version,
                runtime_abi_epoch,
                ..
            } => (
                connector,
                connector_version,
                trigger,
                trigger_version,
                runtime_abi_epoch,
            ),
        };
        if *connector != manifest.connector
            || *version != manifest.connector_version
            || *runtime_abi_epoch != manifest.runtime_abi_epoch
            || !trigger_ids.insert((*trigger_id, *trigger_version))
        {
            return manifest_identity_mismatch("trigger identity differs from manifest");
        }
        if let TriggerSpec::Webhook {
            subscription_operations: Some(subscription),
            ..
        } = trigger
            && [
                Some(subscription.create),
                Some(subscription.delete),
                subscription.check,
            ]
            .into_iter()
            .flatten()
            .any(|operation| {
                manifest
                    .operations
                    .iter()
                    .filter(|candidate| candidate.operation == operation)
                    .count()
                    != 1
            })
        {
            return manifest_identity_mismatch(
                "webhook subscription operation does not resolve exactly once",
            );
        }
    }
    Ok(())
}

fn validate_manifest_provenance(
    manifest: &ConnectorManifest,
    accepted_records: &AcceptedRecordCatalog,
    reviewed_policies: &BTreeMap<DonatPolicyId, TypedValue>,
    fact_requirements: &CheckedFactRequirements,
) -> Result<(), CatalogError> {
    let reference_ids = manifest
        .provenance
        .iter()
        .map(|reference| reference.source_record_id)
        .collect::<BTreeSet<_>>();
    if reference_ids.len() != manifest.provenance.len() {
        return manifest_reference_mismatch("duplicate provenance source record");
    }
    for reference in &manifest.provenance {
        let record = accepted_records
            .capability_record(reference.source_record_id)
            .ok_or_else(|| {
                CatalogError::new(
                    "catalog_manifest_reference_mismatch",
                    "provenance source record has no exact checked capability",
                )
            })?;
        let port_capability = accepted_records
            .port_approved(reference.source_record_id)
            .ok();
        let evidence_capability = accepted_records
            .evidence_accepted(reference.source_record_id)
            .ok();
        let reference_artifacts = artifact_keys(&reference.artifact_hashes);
        if reference_artifacts.len() != reference.artifact_hashes.len()
            || reference_artifacts != artifact_keys(&record.artifact_hashes)
            || reference.notice_id != record.notice.id
            || !license_identity_matches(&reference.license_id, &record.license)
        {
            return manifest_reference_mismatch(
                "provenance artifacts, license, or notice differ from source record",
            );
        }
        for binding in &reference.contract_facts {
            match &binding.fact {
                ContractFact::ProviderEvidence {
                    source_record_id, ..
                } => {
                    if !reference_ids.contains(source_record_id) {
                        return Err(CatalogError::new(
                            "catalog_fact_origin_unresolved",
                            "provider fact source is absent from manifest provenance",
                        ));
                    }
                    if *source_record_id != reference.source_record_id
                        || evidence_capability.is_none()
                    {
                        return Err(CatalogError::new(
                            "catalog_fact_binding_mismatch",
                            "provider fact is foreign to its containing provenance capability",
                        ));
                    }
                }
                ContractFact::DonatPolicy { .. } if port_capability.is_none() => {
                    return Err(CatalogError::new(
                        "catalog_fact_binding_mismatch",
                        "policy fact is not owned by an executable provenance capability",
                    ));
                }
                ContractFact::DonatPolicy { .. } => {}
            }
        }
    }

    for operation in &manifest.operations {
        let authorizers = manifest
            .provenance
            .iter()
            .filter(|reference| {
                accepted_records
                    .port_approved(reference.source_record_id)
                    .is_ok_and(|approved| approved.authorizes(operation.operation))
            })
            .count();
        if authorizers != 1 {
            return Err(CatalogError::new(
                "catalog_source_not_executable",
                "operation must resolve to exactly one approved source record",
            ));
        }
    }

    let semantic = manifest
        .operations
        .iter()
        .flat_map(|operation| operation.resolved_fact_values.iter())
        .cloned()
        .collect::<Vec<_>>();
    let origins = manifest
        .provenance
        .iter()
        .flat_map(|reference| reference.contract_facts.iter())
        .map(|binding| ResolvedContractFactBinding {
            use_site: binding.use_site.clone(),
            fact: binding.fact.clone(),
        })
        .collect::<Vec<_>>();
    resolve_fact_bindings(
        &semantic,
        &origins,
        fact_requirements,
        accepted_records,
        reviewed_policies,
    )?;
    Ok(())
}

fn artifact_keys(values: &[crate::ArtifactHash]) -> BTreeSet<String> {
    values
        .iter()
        .map(|artifact| {
            format!(
                "{}\0{:?}\0{}\0{}",
                artifact.artifact_id.as_str(),
                artifact.algorithm,
                artifact.digest,
                artifact.path.as_ref().map_or("", AsRef::as_ref)
            )
        })
        .collect()
}

fn license_identity_matches(identity: &str, license: &LicenseDecision) -> bool {
    match license {
        LicenseDecision::Permissive { spdx_id, .. } => identity == spdx_id,
        LicenseDecision::WrittenGrant { decision_id, .. } => identity == decision_id.as_ref(),
        LicenseDecision::Rejected { .. } => false,
    }
}

fn valid_catalog_id(value: &str) -> bool {
    (1..=96).contains(&value.len())
        && value.is_ascii()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn valid_catalog_token(value: &str) -> bool {
    !value.is_empty()
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~:/".contains(&byte))
}

fn valid_dns_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.is_ascii()
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

fn valid_json_pointer(value: &str) -> bool {
    value.is_empty()
        || (value.starts_with('/')
            && !value
                .as_bytes()
                .windows(2)
                .any(|pair| pair[0] == b'~' && !matches!(pair[1], b'0' | b'1'))
            && !value.ends_with('~'))
}

fn valid_path_template(value: &str) -> bool {
    value.starts_with('/')
        && !value.contains(['?', '#', '\\'])
        && value
            .split('/')
            .skip(1)
            .all(|segment| segment != "." && segment != "..")
}

fn valid_license_identity(value: &str) -> bool {
    !value.is_empty()
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b' '))
}

fn validate_manifest_header(value: &str) -> Result<(), CatalogError> {
    if value != value.to_ascii_lowercase() || !valid_catalog_token(value) || value.contains(':') {
        return invalid_manifest_primitive("header name");
    }
    Ok(())
}

fn validate_manifest_query_key(value: &str) -> Result<(), CatalogError> {
    if !valid_catalog_token(value) || value.contains(['&', '=', '?', '#', ':', '/']) {
        return invalid_manifest_primitive("query key");
    }
    Ok(())
}

fn invalid_manifest_primitive<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new(
        "catalog_manifest_invalid_primitive",
        detail,
    ))
}

fn credential_incomplete<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new("catalog_credential_incomplete", detail))
}

fn contract_hash_mismatch<T>(detail: impl Into<String>) -> Result<T, CatalogError> {
    Err(CatalogError::new("catalog_contract_hash_mismatch", detail))
}

fn selected_header_invalid<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new("catalog_selected_header_invalid", detail))
}

fn error_map_invalid<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new("catalog_error_map_invalid", detail))
}

fn effect_incomplete<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new(
        "catalog_operation_effect_incomplete",
        detail,
    ))
}

fn manifest_identity_mismatch<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new(
        "catalog_manifest_identity_mismatch",
        detail,
    ))
}

fn manifest_reference_mismatch<T>(detail: &'static str) -> Result<T, CatalogError> {
    Err(CatalogError::new(
        "catalog_manifest_reference_mismatch",
        detail,
    ))
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
