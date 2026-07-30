use std::path::Path;

use donat_connector_catalog::{
    ContractFact, DonatPolicyId, ResolvedContractFactBinding, ResolvedFactValue, SourceSubject,
    canonical_material_bytes, load_record, split_resolved_fact_bindings,
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
            value: donat_connector_catalog::TypedValueMaterialV1::String(
                "Idempotency-Key".to_owned(),
            ),
        },
    }];
    let (semantic, provenance) = split_resolved_fact_bindings(&values, &origins, &[]).unwrap();
    assert_eq!(semantic[0].use_site, provenance[0].use_site);

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
    assert!(split_resolved_fact_bindings(&duplicate, &origins, &[]).is_err());
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
            value: donat_connector_catalog::TypedValueMaterialV1::String(
                "Idempotency-Key".to_owned(),
            ),
        },
    };
    let (left, _) =
        split_resolved_fact_bindings(&values, &[origin("policy.idempotency.header")], &[]).unwrap();
    let (right, _) = split_resolved_fact_bindings(&values, &[origin("policy.other")], &[]).unwrap();
    assert_eq!(
        canonical_material_bytes(&left).unwrap(),
        canonical_material_bytes(&right).unwrap()
    );
}

#[test]
fn value_only_mutation_preserves_direct_origin_material() {
    let origin = ResolvedContractFactBinding {
        use_site: "effect.request.binding".to_owned(),
        fact: ContractFact::DonatPolicy {
            policy_id: DonatPolicyId::literal("policy.idempotency.header"),
            value: donat_connector_catalog::TypedValueMaterialV1::String(
                "Idempotency-Key".to_owned(),
            ),
        },
    };
    let values = |value: &str| {
        vec![ResolvedFactValue {
            use_site: "effect.request.binding".to_owned(),
            value: TypedValue::String(value.to_owned()),
        }]
    };
    let (_, left) =
        split_resolved_fact_bindings(&values("first"), std::slice::from_ref(&origin), &[]).unwrap();
    let (_, right) =
        split_resolved_fact_bindings(&values("second"), std::slice::from_ref(&origin), &[])
            .unwrap();
    assert_eq!(
        canonical_material_bytes(&left).unwrap(),
        canonical_material_bytes(&right).unwrap()
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
            value: donat_connector_catalog::TypedValueMaterialV1::String(
                "Idempotency-Key".to_owned(),
            ),
        },
    }];
    let (semantic, provenance) = split_resolved_fact_bindings(&values, &origins, &[]).unwrap();
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
