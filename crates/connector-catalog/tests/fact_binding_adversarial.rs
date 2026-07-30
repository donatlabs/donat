use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use donat_connector_catalog::{
    AcceptedRecordCatalog, ContractFact, DonatPolicyId, ResolvedContractFactBinding,
    ResolvedFactValue, SourceRecordId, SourceReviewRegistry, TypedValueMaterialV1, load_record,
    resolve_fact_bindings,
};
use donat_value_contract::TypedValue;

fn catalog() -> AcceptedRecordCatalog {
    let record = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap();
    let mut reviews = SourceReviewRegistry::default();
    reviews.approve_reviewed_use("review.demo").unwrap();
    AcceptedRecordCatalog::build(
        vec![record],
        &BTreeMap::<SourceRecordId, BTreeSet<_>>::new(),
        &reviews,
    )
    .unwrap()
}

fn provider_origin() -> ResolvedContractFactBinding {
    ResolvedContractFactBinding {
        use_site: "effect.request.binding".to_owned(),
        fact: ContractFact::ProviderEvidence {
            source_record_id: SourceRecordId::literal("source.demo.provider.v1"),
            fact_id: donat_connector_catalog::ProviderFactId::literal("fact.idempotency"),
        },
    }
}

fn semantic(value: &str) -> Vec<ResolvedFactValue> {
    vec![ResolvedFactValue {
        use_site: "effect.request.binding".to_owned(),
        value: TypedValue::String(value.to_owned()),
    }]
}

#[test]
fn provider_fact_value_must_equal_the_accepted_evidence_value() {
    let catalog = catalog();
    resolve_fact_bindings(
        &semantic("Idempotency-Key"),
        &[provider_origin()],
        &catalog,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        resolve_fact_bindings(
            &semantic("X-Different"),
            &[provider_origin()],
            &catalog,
            &BTreeMap::new(),
        )
        .unwrap_err()
        .code(),
        "catalog_fact_binding_mismatch"
    );
}

#[test]
fn unknown_provider_fact_origin_is_unresolved() {
    let catalog = catalog();
    let mut origin = provider_origin();
    origin.fact = ContractFact::ProviderEvidence {
        source_record_id: SourceRecordId::literal("source.unknown"),
        fact_id: donat_connector_catalog::ProviderFactId::literal("fact.idempotency"),
    };
    assert_eq!(
        resolve_fact_bindings(
            &semantic("Idempotency-Key"),
            &[origin],
            &catalog,
            &BTreeMap::new(),
        )
        .unwrap_err()
        .code(),
        "catalog_fact_origin_unresolved"
    );
}

#[test]
fn reviewed_policy_registry_value_is_exact_and_non_substitutable() {
    let catalog = catalog();
    let policy_id = DonatPolicyId::literal("policy.idempotency.header");
    let policy_origin = ResolvedContractFactBinding {
        use_site: "effect.request.binding".to_owned(),
        fact: ContractFact::DonatPolicy {
            policy_id,
            value: TypedValueMaterialV1::string("Idempotency-Key").unwrap(),
        },
    };
    let policies = BTreeMap::from([(policy_id, TypedValue::String("Idempotency-Key".to_owned()))]);
    resolve_fact_bindings(
        &semantic("Idempotency-Key"),
        std::slice::from_ref(&policy_origin),
        &catalog,
        &policies,
    )
    .unwrap();

    assert_eq!(
        resolve_fact_bindings(
            &semantic("Idempotency-Key"),
            &[policy_origin],
            &catalog,
            &BTreeMap::new(),
        )
        .unwrap_err()
        .code(),
        "catalog_fact_origin_unresolved"
    );
}
