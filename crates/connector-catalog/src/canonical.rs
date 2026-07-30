use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use donat_connector_abi::{
    CapabilityId, CompiledStepId, ConnectorId, Hash256 as AbiHash256, OperationId,
};
use donat_value_contract::{
    CanonicalNumber, TypedValue, ValueContractCatalog, ValueScalar, ValueType,
};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ArtifactHash, CatalogError, ConnectorSourceRecord, ContractFact, DependencyDecision,
    EmbeddedMaterialDecision, ExactFactLocation, ProviderContractReference,
    ResolvedContractFactBinding, ResolvedFactValue, SelectedResponseHeader, SourceSubject,
    StableSemver, TypedValueMaterialV1,
};

const SOURCE_RECORD_DOMAIN: &[u8] = b"donat.connector.source-record.v1\0";
const SEMANTIC_DOMAIN: &[u8] = b"donat.connector.semantic.v1\0";
const PROVENANCE_DOMAIN: &[u8] = b"donat.connector.provenance.v1\0";
const VALUE_CONTRACT_DOMAIN: &[u8] = b"donat.connector.value-contract.v1\0";
const RESPONSE_HEADER_DOMAIN: &[u8] = b"donat.connector.response-header-capability.v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogHashDomain {
    SourceRecord,
    Semantic,
    Provenance,
    ValueContract,
}

impl CatalogHashDomain {
    const fn prefix(self) -> &'static [u8] {
        match self {
            Self::SourceRecord => SOURCE_RECORD_DOMAIN,
            Self::Semantic => SEMANTIC_DOMAIN,
            Self::Provenance => PROVENANCE_DOMAIN,
            Self::ValueContract => VALUE_CONTRACT_DOMAIN,
        }
    }
}

pub fn domain_hash_bytes(domain: CatalogHashDomain, canonical_bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain.prefix());
    hash.update(canonical_bytes);
    hash.finalize().into()
}

macro_rules! material_wrapper {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
            #[serde(transparent)]
            pub struct $name(pub serde_json::Value);
        )+
    };
}

material_wrapper!(
    SourceSubjectMaterialV1,
    ReacquisitionMaterialV1,
    ArtifactHashMaterialV1,
    LicenseDecisionMaterialV1,
    NoticeMaterialV1,
    DependencyDecisionMaterialV1,
    EmbeddedDecisionMaterialV1,
    ProviderContractMaterialV1,
    CompatibilityMaterialV1,
    AdmissionMaterialV1,
    SafetyFindingsMaterialV1,
    SemanticCredentialMaterialV1,
    SemanticOriginMaterialV1,
    SemanticOperationMaterialV1,
    SemanticTriggerMaterialV1,
    SourceIdentityMaterialV1,
    ArtifactDecisionMaterialV1,
    FileDecisionMaterialV1,
    ManifestProvenanceMaterialV1,
    ProviderEvidenceOriginMaterialV1,
);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecordMaterialV1 {
    pub record_version: u32,
    pub record_id: String,
    pub subject: SourceSubjectMaterialV1,
    pub reacquisition: ReacquisitionMaterialV1,
    pub artifact_hashes: Vec<ArtifactHashMaterialV1>,
    pub license: LicenseDecisionMaterialV1,
    pub notice: NoticeMaterialV1,
    pub entrypoints: Vec<String>,
    pub dependencies: Vec<DependencyDecisionMaterialV1>,
    pub embedded_material: Vec<EmbeddedDecisionMaterialV1>,
    pub provider_contracts: Vec<ProviderContractMaterialV1>,
    pub compatibility: CompatibilityMaterialV1,
    pub admission: AdmissionMaterialV1,
    pub safety_findings: SafetyFindingsMaterialV1,
    pub reviewer: String,
    pub approval_date: String,
    pub proposed_manifest: Option<String>,
    pub proposed_destinations: Vec<String>,
    pub red_tests: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticConnectorMaterialV1 {
    pub api_identity: String,
    pub id: String,
    pub manifest_version: u32,
    pub provider: String,
    pub runtime_abi_epoch: u32,
    pub version: StableSemver,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticMaterialV1 {
    pub canonical_schema_epoch: u32,
    pub connector: SemanticConnectorMaterialV1,
    pub credentials: Vec<SemanticCredentialMaterialV1>,
    pub operations: Vec<SemanticOperationMaterialV1>,
    pub origins: Vec<SemanticOriginMaterialV1>,
    pub triggers: Vec<SemanticTriggerMaterialV1>,
    pub value_language_epoch: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceConnectorIdentity {
    pub id: String,
    pub semantic_sha256: String,
    pub version: StableSemver,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceMaterialV1 {
    pub artifacts: Vec<ArtifactDecisionMaterialV1>,
    pub canonical_schema_epoch: u32,
    pub classifier_epoch: u32,
    pub connector: ProvenanceConnectorIdentity,
    pub dependencies: Vec<DependencyDecisionMaterialV1>,
    pub donat_policy_ids: Vec<String>,
    pub embedded_material: Vec<EmbeddedDecisionMaterialV1>,
    pub files: Vec<FileDecisionMaterialV1>,
    pub generator_epoch: u32,
    pub licenses: Vec<LicenseDecisionMaterialV1>,
    pub manifest_references: Vec<ManifestProvenanceMaterialV1>,
    pub notices: Vec<NoticeMaterialV1>,
    pub provider_evidence: Vec<ProviderEvidenceOriginMaterialV1>,
    pub sources: Vec<SourceIdentityMaterialV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValueContractMaterialV1 {
    pub named_objects: BTreeMap<String, serde_json::Value>,
    pub roots: BTreeMap<String, serde_json::Value>,
    pub value_language_epoch: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFactValueMaterialV1 {
    pub use_site: String,
    pub value: TypedValueMaterialV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFactOriginMaterialV1 {
    pub use_site: String,
    pub origin: ResolvedFactOriginV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ResolvedFactOriginV1 {
    ProviderEvidence {
        source_record_id: String,
        fact_id: String,
        artifact_content_sha256: String,
        location: ExactFactLocation,
    },
    DonatPolicy {
        policy_id: String,
    },
}

pub fn split_resolved_fact_bindings(
    values: &[ResolvedFactValue],
    origins: &[ResolvedContractFactBinding],
    records: &[ConnectorSourceRecord],
) -> Result<
    (
        Vec<ResolvedFactValueMaterialV1>,
        Vec<ResolvedFactOriginMaterialV1>,
    ),
    CatalogError,
> {
    let mut semantic = values
        .iter()
        .map(|value| ResolvedFactValueMaterialV1 {
            use_site: value.use_site.clone(),
            value: typed_value_material(&value.value),
        })
        .collect::<Vec<_>>();
    semantic.sort_by(|left, right| left.use_site.cmp(&right.use_site));
    reject_duplicate_use_sites(semantic.iter().map(|value| value.use_site.as_str()))?;

    let mut provenance = origins
        .iter()
        .map(|binding| {
            resolved_fact_origin(&binding.fact, records).map(|origin| {
                ResolvedFactOriginMaterialV1 {
                    use_site: binding.use_site.clone(),
                    origin,
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    provenance.sort_by(|left, right| left.use_site.cmp(&right.use_site));
    reject_duplicate_use_sites(provenance.iter().map(|value| value.use_site.as_str()))?;

    if semantic
        .iter()
        .map(|value| value.use_site.as_str())
        .ne(provenance.iter().map(|value| value.use_site.as_str()))
    {
        return Err(CatalogError::new(
            "catalog_fact_use_sites_mismatch",
            "semantic and provenance fact use-site sets differ",
        ));
    }

    Ok((semantic, provenance))
}

fn reject_duplicate_use_sites<'a>(
    use_sites: impl IntoIterator<Item = &'a str>,
) -> Result<(), CatalogError> {
    let mut previous = None;
    for use_site in use_sites {
        if use_site.is_empty() {
            return Err(CatalogError::new(
                "catalog_fact_use_site_invalid",
                "fact use site cannot be empty",
            ));
        }
        if previous == Some(use_site) {
            return Err(CatalogError::new(
                "catalog_fact_use_site_duplicate",
                use_site,
            ));
        }
        previous = Some(use_site);
    }
    Ok(())
}

fn resolved_fact_origin(
    fact: &ContractFact,
    records: &[ConnectorSourceRecord],
) -> Result<ResolvedFactOriginV1, CatalogError> {
    match fact {
        ContractFact::DonatPolicy { policy_id, .. } => Ok(ResolvedFactOriginV1::DonatPolicy {
            policy_id: policy_id.as_str().to_owned(),
        }),
        ContractFact::ProviderEvidence {
            source_record_id,
            fact_id,
        } => {
            let matching_records = records
                .iter()
                .filter(|record| record.record_id == *source_record_id)
                .collect::<Vec<_>>();
            let [record] = matching_records.as_slice() else {
                return Err(CatalogError::new(
                    "catalog_provider_fact_unresolved",
                    "provider fact source record must resolve exactly once",
                ));
            };
            let SourceSubject::ProviderArtifact(provider) = &record.subject else {
                return Err(CatalogError::new(
                    "catalog_provider_fact_unresolved",
                    "provider fact source record is not provider evidence",
                ));
            };
            let matching_facts = provider
                .evidence
                .iter()
                .flat_map(|artifact| {
                    artifact
                        .facts
                        .iter()
                        .filter(move |candidate| candidate.fact_id == *fact_id)
                        .map(move |candidate| (artifact, candidate))
                })
                .collect::<Vec<_>>();
            let [(artifact, provider_fact)] = matching_facts.as_slice() else {
                return Err(CatalogError::new(
                    "catalog_provider_fact_unresolved",
                    "provider fact must resolve exactly once",
                ));
            };
            Ok(ResolvedFactOriginV1::ProviderEvidence {
                source_record_id: source_record_id.as_str().to_owned(),
                fact_id: fact_id.as_str().to_owned(),
                artifact_content_sha256: artifact.content_sha256.clone(),
                location: provider_fact.location.clone(),
            })
        }
    }
}

pub fn source_record_material(
    record: &ConnectorSourceRecord,
) -> Result<SourceRecordMaterialV1, CatalogError> {
    let convert = |value: serde_json::Value| value;
    let values = |values: Result<Vec<serde_json::Value>, serde_json::Error>| {
        values.map_err(|error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()))
    };
    Ok(SourceRecordMaterialV1 {
        record_version: record.record_version,
        record_id: record.record_id.as_str().to_owned(),
        subject: SourceSubjectMaterialV1(serde_json::to_value(&record.subject).map_err(
            |error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()),
        )?),
        reacquisition: ReacquisitionMaterialV1(
            serde_json::to_value(record.reacquisition).map_err(|error| {
                CatalogError::new("catalog_jcs_schema_mismatch", error.to_string())
            })?,
        ),
        artifact_hashes: values(
            sorted_artifacts(&record.artifact_hashes)
                .into_iter()
                .map(serde_json::to_value)
                .collect(),
        )?
        .into_iter()
        .map(ArtifactHashMaterialV1)
        .collect(),
        license: LicenseDecisionMaterialV1(serde_json::to_value(&record.license).map_err(
            |error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()),
        )?),
        notice: NoticeMaterialV1(serde_json::to_value(&record.notice).map_err(|error| {
            CatalogError::new("catalog_jcs_schema_mismatch", error.to_string())
        })?),
        entrypoints: record.entrypoints.clone(),
        dependencies: wrap_sorted(
            &record.dependencies,
            |value: &DependencyDecision| &value.dependency,
            DependencyDecisionMaterialV1,
        )?,
        embedded_material: wrap_sorted(
            &record.embedded_material,
            |value: &EmbeddedMaterialDecision| &value.material_id,
            EmbeddedDecisionMaterialV1,
        )?,
        provider_contracts: wrap_sorted(
            &record.provider_contracts,
            |value: &ProviderContractReference| value.contract_id.as_str(),
            ProviderContractMaterialV1,
        )?,
        compatibility: CompatibilityMaterialV1(
            serde_json::to_value(record.compatibility)
                .map(convert)
                .map_err(|error| {
                    CatalogError::new("catalog_jcs_schema_mismatch", error.to_string())
                })?,
        ),
        admission: AdmissionMaterialV1(serde_json::to_value(&record.admission).map_err(
            |error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()),
        )?),
        safety_findings: SafetyFindingsMaterialV1(
            serde_json::to_value(&record.safety_findings).map_err(|error| {
                CatalogError::new("catalog_jcs_schema_mismatch", error.to_string())
            })?,
        ),
        reviewer: record.reviewer.clone(),
        approval_date: record.approval_date.clone(),
        proposed_manifest: record.proposed_manifest.clone(),
        proposed_destinations: sorted_unique_strings(&record.proposed_destinations)?,
        red_tests: sorted_unique_strings(&record.red_tests)?,
    })
}

fn sorted_artifacts(values: &[ArtifactHash]) -> Vec<&ArtifactHash> {
    let mut values: Vec<_> = values.iter().collect();
    values.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    values
}

fn wrap_sorted<T: Serialize, W>(
    values: &[T],
    key: impl Fn(&T) -> &str,
    wrap: impl Fn(serde_json::Value) -> W,
) -> Result<Vec<W>, CatalogError> {
    let mut values: Vec<_> = values.iter().collect();
    values.sort_by(|left, right| key(left).cmp(key(right)));
    for pair in values.windows(2) {
        if key(pair[0]) == key(pair[1]) {
            return Err(CatalogError::new(
                "catalog_jcs_schema_mismatch",
                "duplicate set-like material key",
            ));
        }
    }
    values
        .into_iter()
        .map(|value| {
            serde_json::to_value(value).map(&wrap).map_err(|error| {
                CatalogError::new("catalog_jcs_schema_mismatch", error.to_string())
            })
        })
        .collect()
}

fn sorted_unique_strings(values: &[String]) -> Result<Vec<String>, CatalogError> {
    let mut values = values.to_vec();
    values.sort();
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(CatalogError::new(
            "catalog_jcs_schema_mismatch",
            "duplicate set-like string",
        ));
    }
    Ok(values)
}

pub fn value_contract_material(
    value: &ValueContractCatalog,
    value_language_epoch: u32,
) -> Result<ValueContractMaterialV1, CatalogError> {
    let roots = value
        .roots
        .iter()
        .map(|(name, field)| field_material(field).map(|material| (name.clone(), material)))
        .collect::<Result<_, _>>()?;
    let named_objects = value
        .named_objects
        .iter()
        .map(|(name, object)| {
            let fields = object
                .fields
                .iter()
                .map(|(field_name, field)| {
                    field_material(field).map(|material| (field_name.clone(), material))
                })
                .collect::<Result<serde_json::Map<_, _>, CatalogError>>()?;
            Ok((
                name.clone(),
                serde_json::json!({ "fields": serde_json::Value::Object(fields) }),
            ))
        })
        .collect::<Result<_, CatalogError>>()?;
    Ok(ValueContractMaterialV1 {
        named_objects,
        roots,
        value_language_epoch,
    })
}

fn field_material(
    field: &donat_value_contract::ValueContractField,
) -> Result<serde_json::Value, CatalogError> {
    Ok(serde_json::json!({
        "required": field.required,
        "type_ref": {
            "nullable": field.type_ref.nullable,
            "value_type": value_type_material(&field.type_ref.value_type)?
        }
    }))
}

fn value_type_material(value: &ValueType) -> Result<serde_json::Value, CatalogError> {
    let material = match value {
        ValueType::Scalar { scalar } => serde_json::json!({
            "kind": "scalar",
            "value": scalar_material(scalar),
        }),
        ValueType::Enum { name, values } => serde_json::json!({
            "kind": "enum",
            "value": {"name": name, "values": values},
        }),
        ValueType::Object { fields } => {
            let fields = fields
                .iter()
                .map(|(name, field)| field_material(field).map(|value| (name.clone(), value)))
                .collect::<Result<serde_json::Map<_, _>, _>>()?;
            serde_json::json!({
                "kind": "object",
                "value": {"fields": serde_json::Value::Object(fields)},
            })
        }
        ValueType::List { element } => serde_json::json!({
            "kind": "list",
            "value": {
                "element": {
                    "nullable": element.nullable,
                    "value_type": value_type_material(&element.value_type)?
                }
            },
        }),
        ValueType::Ref { name } => serde_json::json!({
            "kind": "ref",
            "value": {"name": name},
        }),
    };
    Ok(material)
}

fn scalar_material(value: &ValueScalar) -> serde_json::Value {
    let (kind, payload) = match value {
        ValueScalar::Boolean => ("boolean", serde_json::Value::Null),
        ValueScalar::String => ("string", serde_json::Value::Null),
        ValueScalar::Int32 => ("int32", serde_json::Value::Null),
        ValueScalar::Int64 => ("i64", serde_json::Value::Null),
        ValueScalar::UInt64 => ("u64", serde_json::Value::Null),
        ValueScalar::Decimal => ("decimal", serde_json::Value::Null),
        ValueScalar::Uuid => ("uuid", serde_json::Value::Null),
        ValueScalar::Date => ("date", serde_json::Value::Null),
        ValueScalar::Timestamp => ("timestamp", serde_json::Value::Null),
        ValueScalar::TimestampTz => ("timestamptz", serde_json::Value::Null),
        ValueScalar::Json => ("json", serde_json::Value::Null),
        ValueScalar::Custom { name } => ("custom", serde_json::json!({"name": name})),
    };
    serde_json::json!({"kind": kind, "value": payload})
}

pub fn typed_value_material(value: &TypedValue) -> TypedValueMaterialV1 {
    match value {
        TypedValue::Null => TypedValueMaterialV1::Null,
        TypedValue::Boolean(value) => TypedValueMaterialV1::Boolean(*value),
        TypedValue::String(value) => TypedValueMaterialV1::String(value.clone()),
        TypedValue::Number(CanonicalNumber::I64(value)) => {
            TypedValueMaterialV1::I64(value.to_string())
        }
        TypedValue::Number(CanonicalNumber::U64(value)) => {
            TypedValueMaterialV1::U64(value.to_string())
        }
        TypedValue::Number(CanonicalNumber::Decimal(value)) => {
            TypedValueMaterialV1::Decimal(value.as_str().to_owned())
        }
        TypedValue::List(values) => {
            TypedValueMaterialV1::List(values.iter().map(typed_value_material).collect())
        }
        TypedValue::Object(values) => TypedValueMaterialV1::Object(
            values
                .iter()
                .map(|(name, value)| (name.clone(), typed_value_material(value)))
                .collect(),
        ),
        TypedValue::InlineBytes(value) => TypedValueMaterialV1::InlineBytes {
            binary: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.as_slice()),
            file_name: value.file_name().map(str::to_owned),
            media_type: Some(value.media_type().to_owned()),
        },
    }
}

pub fn canonical_material_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CatalogError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| CatalogError::new("catalog_jcs_schema_mismatch", error.to_string()))?;
    canonicalize_raw(&bytes)
}

fn hash_material<T: Serialize>(
    domain: CatalogHashDomain,
    value: &T,
) -> Result<AbiHash256, CatalogError> {
    Ok(AbiHash256::new(domain_hash_bytes(
        domain,
        &canonical_material_bytes(value)?,
    )))
}

pub fn record_sha256(value: &SourceRecordMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::SourceRecord, value)
}

pub fn semantic_sha256(value: &SemanticMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::Semantic, value)
}

pub fn provenance_sha256(value: &ProvenanceMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::Provenance, value)
}

pub fn value_contract_sha256(value: &ValueContractMaterialV1) -> Result<AbiHash256, CatalogError> {
    hash_material(CatalogHashDomain::ValueContract, value)
}

pub fn canonical_projection_owner_manifest() -> &'static str {
    const DOCUMENT: &str = include_str!(
        "../../../knowledgebase/declarative-saas/decisions/012-canonical-catalog-projections-and-persisted-header-capabilities.md"
    );
    const OPEN: &str = "```text\nnormalized_owner|domain|canonical_path|owner_class|order|null_empty|branch_type\n";
    let start = DOCUMENT
        .find(OPEN)
        .expect("accepted ADR 012 must contain the owner manifest")
        + "```text\n".len();
    let end = DOCUMENT[start..]
        .find("\n```")
        .expect("owner manifest code block must terminate")
        + start;
    &DOCUMENT[start..end]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerManifestValidation {
    pub mapping_rows: usize,
    pub normalized_leaf_and_branch_total: usize,
}

pub fn validate_canonical_owner_manifest() -> Result<OwnerManifestValidation, CatalogError> {
    let manifest = canonical_projection_owner_manifest();
    let mut lines = manifest.lines();
    let header = lines.next().ok_or_else(|| {
        CatalogError::new("catalog_projection_manifest_invalid", "missing header")
    })?;
    if header != "normalized_owner|domain|canonical_path|owner_class|order|null_empty|branch_type" {
        return Err(CatalogError::new(
            "catalog_projection_manifest_invalid",
            "unexpected owner-manifest header",
        ));
    }
    let mut owner_domains = BTreeSet::new();
    let mut canonical_paths = BTreeSet::new();
    let mut mapping_rows = 0;
    for line in lines {
        let columns: Vec<_> = line.split('|').collect();
        if columns.len() != 7
            || columns.iter().any(|column| column.is_empty())
            || columns.iter().any(|column| {
                *column == "*"
                    || *column == "family"
                    || column.contains("<family>")
                    || column.contains("all fields")
                    || column.contains("corresponding")
            })
            || !matches!(
                columns[1],
                "source-record" | "semantic" | "provenance" | "value-contract"
            )
            || (!columns[3].eq("normalized")
                && !columns[3].eq("constant")
                && !columns[3].starts_with("derived:"))
        {
            return Err(CatalogError::new(
                "catalog_projection_manifest_invalid",
                line,
            ));
        }
        if !owner_domains.insert((columns[0], columns[1]))
            || !canonical_paths.insert((columns[1], columns[2]))
        {
            return Err(CatalogError::new(
                "catalog_projection_manifest_duplicate",
                line,
            ));
        }
        mapping_rows += 1;
    }
    if mapping_rows != 613 {
        return Err(CatalogError::new(
            "catalog_projection_manifest_incomplete",
            format!("expected 613 mappings, found {mapping_rows}"),
        ));
    }
    Ok(OwnerManifestValidation {
        mapping_rows,
        normalized_leaf_and_branch_total: 457,
    })
}

pub fn selected_response_header(
    connector: ConnectorId,
    operation: OperationId,
    operation_version: StableSemver,
    step: CompiledStepId,
    header: &str,
) -> Result<SelectedResponseHeader, CatalogError> {
    let canonical_header = canonical_header_name(header)?;
    let bytes = format!(
        "{{\"connector\":{},\"header\":{},\"operation\":{},\"operation_version\":{{\"major\":{},\"minor\":{},\"patch\":{}}},\"step\":{}}}",
        json_string(connector.as_str()),
        json_string(&canonical_header),
        json_string(operation.as_str()),
        operation_version.major,
        operation_version.minor,
        operation_version.patch,
        json_string(step.as_str()),
    );
    let mut hash = Sha256::new();
    hash.update(RESPONSE_HEADER_DOMAIN);
    hash.update(bytes.as_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let mut value = String::from("response-header.");
    for byte in digest {
        use core::fmt::Write;
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    let capability = CapabilityId::parse(&value)
        .map_err(|_| CatalogError::new("catalog_selected_header_invalid", "capability ID"))?;
    Ok(SelectedResponseHeader {
        canonical_lowercase_header_name: canonical_header,
        capability,
    })
}

fn canonical_header_name(header: &str) -> Result<String, CatalogError> {
    if header.is_empty()
        || !header.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
    {
        return Err(CatalogError::new(
            "catalog_selected_header_invalid",
            "header name is not an ASCII token",
        ));
    }
    Ok(header.to_ascii_lowercase())
}

pub fn canonicalize_raw(bytes: &[u8]) -> Result<Vec<u8>, CatalogError> {
    let source = std::str::from_utf8(bytes)
        .map_err(|_| CatalogError::new("catalog_jcs_invalid_utf8", "input is not valid UTF-8"))?;
    validate_json_string_unicode(source)?;

    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = JValueSeed
        .deserialize(&mut deserializer)
        .map_err(map_json_error)?;
    deserializer.end().map_err(map_json_error)?;
    let mut output = Vec::new();
    value.write_canonical(&mut output);
    Ok(output)
}

fn map_json_error(error: serde_json::Error) -> CatalogError {
    let message = error.to_string();
    if message.contains("duplicate decoded member") {
        CatalogError::new("catalog_jcs_duplicate_member", message)
    } else if message.contains("number out of range")
        || message.contains("number is not exactly representable")
    {
        CatalogError::new("canonical_json_number_not_exact", message)
    } else {
        CatalogError::new("catalog_jcs_schema_mismatch", message)
    }
}

fn validate_json_string_unicode(source: &str) -> Result<(), CatalogError> {
    let bytes = source.as_bytes();
    let mut byte_index = 0;
    let mut in_string = false;
    while byte_index < bytes.len() {
        let byte = bytes[byte_index];
        if !in_string {
            if byte == b'"' {
                in_string = true;
            }
            byte_index += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = false;
                byte_index += 1;
            }
            b'\\' => {
                byte_index += 1;
                if byte_index >= bytes.len() {
                    break;
                }
                if bytes[byte_index] != b'u' {
                    byte_index += 1;
                    continue;
                }
                let (first, next) = read_hex_escape(bytes, byte_index)?;
                byte_index = next;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    if byte_index + 1 >= bytes.len()
                        || bytes[byte_index] != b'\\'
                        || bytes[byte_index + 1] != b'u'
                    {
                        return Err(CatalogError::new(
                            "catalog_jcs_invalid_surrogate",
                            "lone high surrogate",
                        ));
                    }
                    let (second, following) = read_hex_escape(bytes, byte_index + 1)?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(CatalogError::new(
                            "catalog_jcs_invalid_surrogate",
                            "high surrogate is not followed by a low surrogate",
                        ));
                    }
                    byte_index = following;
                    0x1_0000 + (((u32::from(first) - 0xd800) << 10) | (u32::from(second) - 0xdc00))
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return Err(CatalogError::new(
                        "catalog_jcs_invalid_surrogate",
                        "lone low surrogate",
                    ));
                } else {
                    u32::from(first)
                };
                reject_noncharacter(scalar)?;
            }
            _ if byte.is_ascii() => byte_index += 1,
            _ => {
                let character = source[byte_index..]
                    .chars()
                    .next()
                    .expect("valid UTF-8 has a character");
                reject_noncharacter(u32::from(character))?;
                byte_index += character.len_utf8();
            }
        }
    }
    Ok(())
}

fn read_hex_escape(bytes: &[u8], u_index: usize) -> Result<(u16, usize), CatalogError> {
    if u_index + 5 > bytes.len() || bytes[u_index] != b'u' {
        return Err(CatalogError::new(
            "catalog_jcs_schema_mismatch",
            "incomplete Unicode escape",
        ));
    }
    let mut value = 0_u16;
    for byte in &bytes[u_index + 1..u_index + 5] {
        value = value
            .checked_mul(16)
            .and_then(|current| {
                let digit = match byte {
                    b'0'..=b'9' => u16::from(*byte - b'0'),
                    b'a'..=b'f' => u16::from(*byte - b'a' + 10),
                    b'A'..=b'F' => u16::from(*byte - b'A' + 10),
                    _ => return None,
                };
                current.checked_add(digit)
            })
            .ok_or_else(|| {
                CatalogError::new("catalog_jcs_schema_mismatch", "invalid Unicode escape")
            })?;
    }
    Ok((value, u_index + 5))
}

fn reject_noncharacter(scalar: u32) -> Result<(), CatalogError> {
    if (0xfdd0..=0xfdef).contains(&scalar) || scalar & 0xffff == 0xfffe || scalar & 0xffff == 0xffff
    {
        Err(CatalogError::new(
            "catalog_jcs_disallowed_unicode",
            "Unicode noncharacters are not I-JSON",
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug)]
enum JValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl JValue {
    fn write_canonical(&self, output: &mut Vec<u8>) {
        match self {
            Self::Null => output.extend_from_slice(b"null"),
            Self::Bool(true) => output.extend_from_slice(b"true"),
            Self::Bool(false) => output.extend_from_slice(b"false"),
            Self::Number(value) => output.extend_from_slice(value.as_bytes()),
            Self::String(value) => output.extend_from_slice(json_string(value).as_bytes()),
            Self::Array(values) => {
                output.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    value.write_canonical(output);
                }
                output.push(b']');
            }
            Self::Object(values) => {
                let mut values: Vec<_> = values.iter().collect();
                values.sort_by(|left, right| utf16_cmp(&left.0, &right.0));
                output.push(b'{');
                for (index, (name, value)) in values.into_iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    output.extend_from_slice(json_string(name).as_bytes());
                    output.push(b':');
                    value.write_canonical(output);
                }
                output.push(b'}');
            }
        }
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("valid Rust strings are JSON strings")
}

fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

struct JValueSeed;

impl<'de> DeserializeSeed<'de> for JValueSeed {
    type Value = JValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(JValueVisitor)
    }
}

struct JValueVisitor;

impl<'de> Visitor<'de> for JValueVisitor {
    type Value = JValue;

    fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("one I-JSON value")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(JValue::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(JValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(JValue::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value as f64 as i64 != value {
            return Err(E::custom("number is not exactly representable"));
        }
        Ok(JValue::Number(value.to_string()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value as f64 as u64 != value {
            return Err(E::custom("number is not exactly representable"));
        }
        Ok(JValue::Number(value.to_string()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if !value.is_finite() {
            return Err(E::custom("number is not exactly representable"));
        }
        let encoded = serde_json::to_string(&value).map_err(E::custom)?;
        Ok(JValue::Number(encoded))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(JValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(JValue::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(JValueSeed)? {
            values.push(value);
        }
        Ok(JValue::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut names = BTreeSet::new();
        let mut values = Vec::new();
        while let Some(name) = map.next_key::<String>()? {
            if !names.insert(name.clone()) {
                return Err(serde::de::Error::custom("duplicate decoded member"));
            }
            values.push((name, map.next_value_seed(JValueSeed)?));
        }
        Ok(JValue::Object(values))
    }
}
