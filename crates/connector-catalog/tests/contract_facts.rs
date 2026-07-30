use std::collections::BTreeMap;
use std::path::Path;

use donat_connector_catalog::{
    AcceptedRecordCatalog, ContractFact, DonatPolicyId, ResolvedContractFactBinding,
    ResolvedFactValue, SourceReviewRegistry, SourceSubject, canonical_material_bytes, load_record,
    resolve_fact_bindings,
};
use donat_value_contract::TypedValue;

fn load(name: &str) -> donat_connector_catalog::ConnectorSourceRecord {
    load_record(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn resolve_policy(
    values: &[ResolvedFactValue],
    origins: &[ResolvedContractFactBinding],
    policies: BTreeMap<DonatPolicyId, TypedValue>,
) -> Result<
    (
        Vec<donat_connector_catalog::ResolvedFactValueMaterialV1>,
        Vec<donat_connector_catalog::ResolvedFactOriginMaterialV1>,
    ),
    donat_connector_catalog::CatalogError,
> {
    let catalog = AcceptedRecordCatalog::build(
        Vec::new(),
        &BTreeMap::new(),
        &SourceReviewRegistry::default(),
    )
    .unwrap();
    resolve_fact_bindings(values, origins, &catalog, &policies)
}

#[test]
fn contract_fact_origins_are_closed_and_non_substitutable() {
    let record = load("provider-contract-record.yaml");
    let fact = &record.provider_contracts[0].facts[0];
    assert!(matches!(fact, ContractFact::ProviderEvidence { .. }));
}

#[test]
fn provider_contract_reference_requires_matching_record_and_facts() {
    let record = load("provider-contract-record.yaml");
    let SourceSubject::ProviderArtifact(provider) = record.subject else {
        panic!("fixture must be provider evidence");
    };
    assert_eq!(provider.evidence[0].facts.len(), 1);
}

#[test]
fn donat_policy_cannot_satisfy_required_provider_evidence() {
    let record = load("provider-contract-record.yaml");
    assert!(!matches!(
        record.provider_contracts[0].facts[0],
        ContractFact::DonatPolicy { .. }
    ));
}

#[test]
fn provider_evidence_acceptance_is_closed_and_non_executable() {
    let record = load("provider-contract-record.yaml");
    assert!(matches!(
        record.admission,
        donat_connector_catalog::AdmissionState::EvidenceAccepted { .. }
    ));
}

#[test]
fn resolved_fact_use_sites_are_unique_and_equal_across_domains() {
    let values = vec![ResolvedFactValue {
        use_site: "effect.request.binding".to_owned(),
        value: TypedValue::String("Idempotency-Key".to_owned()),
    }];
    let origins = vec![ResolvedContractFactBinding {
        use_site: "effect.request.binding".to_owned(),
        fact: ContractFact::DonatPolicy {
            policy_id: DonatPolicyId::literal("policy.idempotency.header"),
            value: donat_connector_catalog::TypedValueMaterialV1::string("Idempotency-Key")
                .unwrap(),
        },
    }];
    let policies = [(
        DonatPolicyId::literal("policy.idempotency.header"),
        TypedValue::String("Idempotency-Key".to_owned()),
    )]
    .into_iter()
    .collect();
    let (semantic, provenance) = resolve_policy(&values, &origins, policies).unwrap();
    assert_eq!(semantic[0].use_site(), provenance[0].use_site());

    let duplicate = vec![
        ResolvedFactValue {
            use_site: "effect.request.binding".to_owned(),
            value: TypedValue::String("first".to_owned()),
        },
        ResolvedFactValue {
            use_site: "effect.request.binding".to_owned(),
            value: TypedValue::String("second".to_owned()),
        },
    ];
    let policies = [(
        DonatPolicyId::literal("policy.idempotency.header"),
        TypedValue::String("Idempotency-Key".to_owned()),
    )]
    .into_iter()
    .collect();
    assert!(resolve_policy(&duplicate, &origins, policies).is_err());
}

#[test]
fn origin_only_mutation_preserves_semantic_hash() {
    let values = vec![ResolvedFactValue {
        use_site: "effect.request.binding".to_owned(),
        value: TypedValue::String("Idempotency-Key".to_owned()),
    }];
    let origin = |policy| ResolvedContractFactBinding {
        use_site: "effect.request.binding".to_owned(),
        fact: ContractFact::DonatPolicy {
            policy_id: DonatPolicyId::parse(policy).unwrap(),
            value: donat_connector_catalog::TypedValueMaterialV1::string("Idempotency-Key")
                .unwrap(),
        },
    };
    let policies = [
        (
            DonatPolicyId::literal("policy.idempotency.header"),
            TypedValue::String("Idempotency-Key".to_owned()),
        ),
        (
            DonatPolicyId::literal("policy.other"),
            TypedValue::String("Idempotency-Key".to_owned()),
        ),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let (left, _) = resolve_policy(
        &values,
        &[origin("policy.idempotency.header")],
        policies.clone(),
    )
    .unwrap();
    let (right, _) = resolve_policy(&values, &[origin("policy.other")], policies).unwrap();
    assert_eq!(
        canonical_material_bytes(&left).unwrap(),
        canonical_material_bytes(&right).unwrap()
    );
}

#[test]
fn value_only_mutation_is_rejected_before_origin_material_exists() {
    let origin = ResolvedContractFactBinding {
        use_site: "effect.request.binding".to_owned(),
        fact: ContractFact::DonatPolicy {
            policy_id: DonatPolicyId::literal("policy.idempotency.header"),
            value: donat_connector_catalog::TypedValueMaterialV1::string("Idempotency-Key")
                .unwrap(),
        },
    };
    let values = |value: &str| {
        vec![ResolvedFactValue {
            use_site: "effect.request.binding".to_owned(),
            value: TypedValue::String(value.to_owned()),
        }]
    };
    let policies = [(
        DonatPolicyId::literal("policy.idempotency.header"),
        TypedValue::String("Idempotency-Key".to_owned()),
    )]
    .into_iter()
    .collect();
    assert_eq!(
        resolve_policy(&values("first"), std::slice::from_ref(&origin), policies)
            .unwrap_err()
            .code(),
        "catalog_fact_binding_mismatch"
    );
}

#[test]
fn contract_fact_semantic_and_provenance_hashes_are_separate() {
    let values = vec![ResolvedFactValue {
        use_site: "effect.request.binding".to_owned(),
        value: TypedValue::String("Idempotency-Key".to_owned()),
    }];
    let origins = vec![ResolvedContractFactBinding {
        use_site: "effect.request.binding".to_owned(),
        fact: ContractFact::DonatPolicy {
            policy_id: DonatPolicyId::literal("policy.idempotency.header"),
            value: donat_connector_catalog::TypedValueMaterialV1::string("Idempotency-Key")
                .unwrap(),
        },
    }];
    let policies = [(
        DonatPolicyId::literal("policy.idempotency.header"),
        TypedValue::String("Idempotency-Key".to_owned()),
    )]
    .into_iter()
    .collect();
    let (semantic, provenance) = resolve_policy(&values, &origins, policies).unwrap();
    let semantic_bytes = canonical_material_bytes(&semantic).unwrap();
    let provenance_bytes = canonical_material_bytes(&provenance).unwrap();
    assert!(
        std::str::from_utf8(&semantic_bytes)
            .unwrap()
            .contains("Idempotency-Key")
    );
    assert!(
        !std::str::from_utf8(&semantic_bytes)
            .unwrap()
            .contains("policy.idempotency.header")
    );
    assert!(
        std::str::from_utf8(&provenance_bytes)
            .unwrap()
            .contains("policy.idempotency.header")
    );
    assert!(
        !std::str::from_utf8(&provenance_bytes)
            .unwrap()
            .contains("Idempotency-Key")
    );
}
