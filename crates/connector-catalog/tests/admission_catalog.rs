use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use donat_connector_abi::OperationId;
use donat_connector_catalog::{
    AcceptedRecordCatalog, EvidenceAcceptedRecord, PortApprovedRecord, SourceRecordId,
    SourceReviewRegistry, load_record, load_record_bytes,
};

fn load(name: &str) -> donat_connector_catalog::ConnectorSourceRecord {
    load_record(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn reviews() -> SourceReviewRegistry {
    let mut reviews = SourceReviewRegistry::default();
    reviews.approve_reviewed_use("review.demo").unwrap();
    reviews
}

fn operation_closures() -> BTreeMap<SourceRecordId, BTreeSet<OperationId>> {
    BTreeMap::from([(
        SourceRecordId::literal("source.donat.http.v1"),
        BTreeSet::from([OperationId::literal("get")]),
    )])
}

fn accepted_catalog() -> AcceptedRecordCatalog {
    AcceptedRecordCatalog::build(
        vec![
            load("serpapi-npm-record.yaml"),
            load("provider-contract-record.yaml"),
            load("donat-owned-record.yaml"),
        ],
        &operation_closures(),
        &reviews(),
    )
    .unwrap()
}

fn accepts_port_capability(_record: PortApprovedRecord<'_>) {}
fn accepts_evidence_capability(_record: EvidenceAcceptedRecord<'_>) {}

#[test]
fn accepted_record_capabilities_are_distinct_and_closed() {
    let catalog = accepted_catalog();
    accepts_port_capability(
        catalog
            .port_approved(SourceRecordId::literal("source.donat.http.v1"))
            .unwrap(),
    );
    accepts_evidence_capability(
        catalog
            .evidence_accepted(SourceRecordId::literal("source.demo.provider.v1"))
            .unwrap(),
    );
    assert_eq!(
        catalog
            .port_approved(SourceRecordId::literal("source.serpapi.npm.0_1_10"))
            .unwrap_err()
            .code(),
        "catalog_source_not_executable"
    );
    assert_eq!(
        catalog
            .port_approved(SourceRecordId::literal("source.demo.provider.v1"))
            .unwrap_err()
            .code(),
        "catalog_source_not_executable"
    );
}

#[test]
fn approved_operation_closure_must_match_in_both_directions() {
    let record = load("donat-owned-record.yaml");
    for closure in [
        BTreeSet::new(),
        BTreeSet::from([
            OperationId::literal("get"),
            OperationId::literal("unexpected"),
        ]),
    ] {
        let error = AcceptedRecordCatalog::build(
            vec![record.clone()],
            &BTreeMap::from([(record.record_id, closure)]),
            &SourceReviewRegistry::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "source_record_admission_mismatch");
    }
}

#[test]
fn reviewed_legal_decisions_resolve_through_an_explicit_registry() {
    let provider = load("provider-contract-record.yaml");
    assert_eq!(
        AcceptedRecordCatalog::build(
            vec![provider.clone()],
            &BTreeMap::new(),
            &SourceReviewRegistry::default(),
        )
        .unwrap_err()
        .code(),
        "source_record_legal_mismatch"
    );
    AcceptedRecordCatalog::build(vec![provider], &BTreeMap::new(), &reviews()).unwrap();
}

#[test]
fn unrelated_or_empty_evidence_admission_never_builds_a_capability() {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap();
    let unrelated = source.replace("contracts: [contract.demo]", "contracts: [contract.other]");
    assert_eq!(
        load_record_bytes(unrelated.as_bytes()).unwrap_err().code(),
        "source_record_admission_mismatch"
    );
}
