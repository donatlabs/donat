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
            &BTreeMap::from([(record.record_id(), closure)]),
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

#[test]
fn inventory_only_findings_are_the_exact_safety_finding_closure() {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serpapi-npm-record.yaml"),
    )
    .unwrap();
    let bytes = source.replace(
        "findings:\n    - finding_id: finding.awaiting.port\n      kind: port.pending\n      location: null\n      message: Port implementation has not been approved.",
        "findings: []",
    );
    assert_eq!(
        load_record_bytes(bytes.as_bytes()).unwrap_err().code(),
        "source_record_admission_mismatch"
    );
}

#[test]
fn rejected_source_states_cannot_mint_checked_capabilities() {
    let npm = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serpapi-npm-record.yaml"),
    )
    .unwrap()
    .replacen(
        r#"signature:
      kind: verified
      value:
        signatures:
          - key_id: npm.key.1
            signature_sha256: 415263748596a7b8c9daebfc0d1e2f405162738495a6b7c8d9eafb0c1d2e3f50
        registry_metadata_sha256: 8192a3b4c5d6e7f8091a2b3c4d5e6f8091a2b3c4d5e6f708192a3b4c5d6e7f90
"#,
        "signature:\n      kind: rejected\n      value:\n        finding: finding.npm.signature.rejected\n",
        1,
    )
    .replacen(
        "kind: inventory_only\n  value:\n    findings:\n      - finding.awaiting.port",
        "kind: approved_for_port\n  value:\n    operations: [get]",
        1,
    );
    assert_eq!(
        load_record_bytes(npm.as_bytes()).unwrap_err().code(),
        "source_record_admission_mismatch"
    );

    let provider = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-contract-record.yaml"),
    )
    .unwrap()
    .replacen("kind: tier_a", "kind: rejected", 1);
    assert_eq!(
        load_record_bytes(provider.as_bytes()).unwrap_err().code(),
        "source_record_admission_mismatch"
    );
}
