use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::path::Path;

use donat_connector_abi::{CompiledStepId, OperationId};
use donat_connector_catalog::{
    AcceptedRecordCatalog, CatalogError, CheckedFactRequirements, ContractFact, DonatPolicyId,
    FixedIdempotencyBinding, OperationEffect, OperationFactRequirement, ProviderIdempotentStep,
    ResolvedContractFactBinding, ResolvedFactOriginMaterialV1, ResolvedFactValue,
    ResolvedFactValueMaterialV1, SourceRecordId, SourceReviewRegistry, TypedValueMaterialV1,
    check_fact_requirements, load_record, resolve_fact_bindings,
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
        use_site: "operation.get.step.request.idempotency.scope".to_owned(),
        fact: ContractFact::ProviderEvidence {
            source_record_id: SourceRecordId::literal("source.demo.provider.v1"),
            fact_id: donat_connector_catalog::ProviderFactId::literal("fact.idempotency"),
        },
    }
}

fn semantic(value: &str) -> Vec<ResolvedFactValue> {
    vec![ResolvedFactValue {
        use_site: "operation.get.step.request.idempotency.scope".to_owned(),
        value: TypedValue::String(value.to_owned()),
    }]
}

fn requirements(values: &[ResolvedFactValue], effect: &OperationEffect) -> CheckedFactRequirements {
    check_fact_requirements(&[OperationFactRequirement::new(
        OperationId::literal("get"),
        effect,
        values,
    )])
    .unwrap()
}

fn provider_requirements(values: &[ResolvedFactValue]) -> CheckedFactRequirements {
    requirements(values, &OperationEffect::ReadOnly)
}

fn policy_requirements(values: &[ResolvedFactValue]) -> CheckedFactRequirements {
    requirements(
        values,
        &OperationEffect::ProviderIdempotent {
            side_effect_steps: vec![ProviderIdempotentStep {
                step: CompiledStepId::literal("request"),
                fixed_binding: FixedIdempotencyBinding::BodyField {
                    pointer: "query".to_owned(),
                },
                scope: "Idempotency-Key".to_owned(),
                minimum_retention_ms: NonZeroU64::new(1_000).unwrap(),
                clock_safety_margin_ms: NonZeroU64::new(1).unwrap(),
            }],
        },
    )
}

fn resolve_provider(
    values: &[ResolvedFactValue],
    origins: &[ResolvedContractFactBinding],
    catalog: &AcceptedRecordCatalog,
    policies: &BTreeMap<DonatPolicyId, TypedValue>,
) -> Result<
    (
        Vec<ResolvedFactValueMaterialV1>,
        Vec<ResolvedFactOriginMaterialV1>,
    ),
    CatalogError,
> {
    let requirements = provider_requirements(values);
    resolve_fact_bindings(values, origins, &requirements, catalog, policies)
}

fn resolve_policy(
    values: &[ResolvedFactValue],
    origins: &[ResolvedContractFactBinding],
    catalog: &AcceptedRecordCatalog,
    policies: &BTreeMap<DonatPolicyId, TypedValue>,
) -> Result<
    (
        Vec<ResolvedFactValueMaterialV1>,
        Vec<ResolvedFactOriginMaterialV1>,
    ),
    CatalogError,
> {
    let requirements = policy_requirements(values);
    resolve_fact_bindings(values, origins, &requirements, catalog, policies)
}

#[test]
fn provider_fact_value_must_equal_the_accepted_evidence_value() {
    let catalog = catalog();
    resolve_provider(
        &semantic("Idempotency-Key"),
        &[provider_origin()],
        &catalog,
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        resolve_provider(
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
        resolve_provider(
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
        use_site: "operation.get.step.request.idempotency.clock_safety_margin_ms".to_owned(),
        fact: ContractFact::DonatPolicy {
            policy_id,
            value: TypedValueMaterialV1::string("Idempotency-Key").unwrap(),
        },
    };
    let policies = BTreeMap::from([(policy_id, TypedValue::String("Idempotency-Key".to_owned()))]);
    let policy_semantic = vec![ResolvedFactValue {
        use_site: policy_origin.use_site.clone(),
        value: TypedValue::String("Idempotency-Key".to_owned()),
    }];
    resolve_policy(
        &policy_semantic,
        std::slice::from_ref(&policy_origin),
        &catalog,
        &policies,
    )
    .unwrap();

    assert_eq!(
        resolve_policy(
            &policy_semantic,
            &[policy_origin],
            &catalog,
            &BTreeMap::new(),
        )
        .unwrap_err()
        .code(),
        "catalog_fact_origin_unresolved"
    );
}

#[test]
fn required_provider_and_policy_domains_reject_equal_value_substitution() {
    let catalog = catalog();
    let policy_id = DonatPolicyId::literal("policy.same.value");
    let policy = BTreeMap::from([(policy_id, TypedValue::String("Idempotency-Key".to_owned()))]);
    let policy_at_provider_site = ResolvedContractFactBinding {
        use_site: "operation.get.step.request.idempotency.scope".to_owned(),
        fact: ContractFact::DonatPolicy {
            policy_id,
            value: TypedValueMaterialV1::string("Idempotency-Key").unwrap(),
        },
    };
    let provider_value = vec![ResolvedFactValue {
        use_site: policy_at_provider_site.use_site.clone(),
        value: TypedValue::String("Idempotency-Key".to_owned()),
    }];
    assert_eq!(
        resolve_provider(
            &provider_value,
            &[policy_at_provider_site],
            &catalog,
            &policy,
        )
        .unwrap_err()
        .code(),
        "catalog_fact_binding_mismatch"
    );

    let mut provider_at_policy_site = provider_origin();
    provider_at_policy_site.use_site =
        "operation.get.step.request.idempotency.clock_safety_margin_ms".to_owned();
    let policy_value = vec![ResolvedFactValue {
        use_site: provider_at_policy_site.use_site.clone(),
        value: TypedValue::String("Idempotency-Key".to_owned()),
    }];
    assert_eq!(
        resolve_policy(
            &policy_value,
            &[provider_at_policy_site],
            &catalog,
            &BTreeMap::new(),
        )
        .unwrap_err()
        .code(),
        "catalog_fact_binding_mismatch"
    );
}
