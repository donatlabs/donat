use std::path::Path;

use donat_connector_catalog::{
    AdmissionState, CompatibilityDecision, DependencyDisposition, ExactSemver,
    NpmProvenanceDecision, NpmSignatureDecision, ReacquisitionPlan, RepositoryOwnerDecision,
    SourceSubject, canonical_yaml, load_record, load_record_bytes,
};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn source_record_requires_exact_artifacts() {
    let complete = std::fs::read_to_string(fixture("serpapi-npm-record.yaml")).unwrap();
    let incomplete = complete.replacen(
        "    license_file_sha256: c1d2e3f405162738495a6b7c8d9eafc0d1e2f30415263748596a7b8c9daebfd0\n",
        "",
        1,
    );
    let error =
        load_record_bytes(incomplete.as_bytes()).expect_err("an incomplete record fails closed");
    assert_eq!(error.code(), "source_record_incomplete");
}

#[test]
fn source_record_variants_are_closed() {
    for name in [
        "serpapi-npm-record.yaml",
        "provider-contract-record.yaml",
        "donat-owned-record.yaml",
    ] {
        load_record(fixture(name)).unwrap();
    }
}

#[test]
fn exact_npm_version_uses_exact_semver() {
    let record = load_record(fixture("serpapi-npm-record.yaml")).unwrap();
    let SourceSubject::ExactNpm(package) = record.subject else {
        panic!("fixture must be exact npm");
    };
    assert_eq!(package.version.as_str(), "0.1.10");
}

#[test]
fn exact_semver_accepts_prerelease_and_build() {
    assert_eq!(
        ExactSemver::try_new("1.2.3-alpha.1+build.5")
            .unwrap()
            .as_str(),
        "1.2.3-alpha.1+build.5"
    );
}

#[test]
fn exact_semver_rejects_range_tag_leading_v_and_leading_zero() {
    for invalid in ["^1.2.3", "latest", "v1.2.3", "01.2.3", "1.2.3-01"] {
        assert!(ExactSemver::try_new(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn serpapi_npm_record_round_trips_without_information_loss() {
    let record = load_record(fixture("serpapi-npm-record.yaml")).unwrap();
    let encoded = canonical_yaml(&record).unwrap();
    assert_eq!(load_record_bytes(&encoded).unwrap(), record);
    assert_eq!(record.compatibility, CompatibilityDecision::TierA);
    assert!(matches!(
        record.admission,
        AdmissionState::InventoryOnly { .. }
    ));
    let SourceSubject::ExactNpm(package) = &record.subject else {
        panic!("fixture must be exact npm");
    };
    assert!(matches!(
        package.signature,
        NpmSignatureDecision::Verified { .. }
    ));
    assert!(matches!(
        package.provenance,
        NpmProvenanceDecision::VerifiedAbsent { .. }
    ));
    assert!(package.tag_commit.is_some());
    assert!(package.provenance_commit.is_none());
    assert_eq!(package.maintainers.len(), 2);
    assert!(matches!(
        package.repository_owner,
        RepositoryOwnerDecision::Consistent { .. }
    ));
}

#[test]
fn plan_owned_negative_fixtures_are_complete_and_reach_the_named_validator() {
    let fixture = |name: &str| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    };
    for (name, code) in [
        ("missing-license-file-hash.yaml", "source_record_incomplete"),
        (
            "npm-repository-mismatch.yaml",
            "source_record_npm_identity_mismatch",
        ),
        (
            "npm-provenance-mismatch.yaml",
            "source_record_npm_identity_mismatch",
        ),
        (
            "open-dependency-disposition.yaml",
            "source_record_incomplete",
        ),
        (
            "policy-as-provider-fact.yaml",
            "source_record_evidence_mismatch",
        ),
    ] {
        let error = load_record(fixture(name)).unwrap_err();
        assert_eq!(error.code(), code, "{name}");
    }
}

#[test]
fn npm_integrity_and_repository_mapping_are_exact() {
    let complete = std::fs::read_to_string(fixture("serpapi-npm-record.yaml")).unwrap();
    let repository_mismatch = complete.replacen(
        "      commit: 0123456789abcdef0123456789abcdef01234567",
        "      commit: 1123456789abcdef0123456789abcdef01234567",
        1,
    );
    assert_eq!(
        load_record_bytes(repository_mismatch.as_bytes())
            .unwrap_err()
            .code(),
        "source_record_npm_identity_mismatch"
    );
    let provenance_mismatch = complete.replace(
        "    provenance_commit: null",
        "    provenance_commit: 1123456789abcdef0123456789abcdef01234567",
    );
    assert_eq!(
        load_record_bytes(provenance_mismatch.as_bytes())
            .unwrap_err()
            .code(),
        "source_record_npm_identity_mismatch"
    );
}

#[test]
fn npm_signature_provenance_tag_maintainer_and_owner_state_is_exact() {
    let record = load_record(fixture("serpapi-npm-record.yaml")).unwrap();
    let SourceSubject::ExactNpm(package) = record.subject else {
        panic!("fixture must be exact npm");
    };
    let NpmSignatureDecision::Verified {
        signatures,
        registry_metadata_sha256,
    } = package.signature
    else {
        panic!("fixture must retain verified signature evidence");
    };
    assert!(!signatures.is_empty());
    assert_eq!(registry_metadata_sha256.len(), 64);
    assert!(matches!(
        package.provenance,
        NpmProvenanceDecision::VerifiedAbsent { .. }
    ));
    assert_ne!(
        package.tag_commit.as_deref(),
        Some(package.npm_git_head.as_str())
    );
    assert_eq!(package.maintainers.len(), 2);
    assert!(matches!(
        package.repository_owner,
        RepositoryOwnerDecision::Consistent { .. }
    ));
}

#[test]
fn reacquisition_plan_matches_source_subject() {
    let npm = load_record(fixture("serpapi-npm-record.yaml")).unwrap();
    let provider = load_record(fixture("provider-contract-record.yaml")).unwrap();
    let owned = load_record(fixture("donat-owned-record.yaml")).unwrap();
    assert!(matches!(
        npm.reacquisition,
        ReacquisitionPlan::ExactNpmReview
    ));
    assert!(matches!(
        provider.reacquisition,
        ReacquisitionPlan::ProviderRepositoryReview
            | ReacquisitionPlan::ProviderVersionedArtifactReview
    ));
    assert!(matches!(
        owned.reacquisition,
        ReacquisitionPlan::DonatOwnedNoNetwork
    ));
}

#[test]
fn dependency_and_embedded_dispositions_are_closed() {
    let record = load_record(fixture("serpapi-npm-record.yaml")).unwrap();
    for dependency in record.dependencies {
        match dependency.disposition {
            DependencyDisposition::Shipped { .. }
            | DependencyDisposition::BuildOnly { .. }
            | DependencyDisposition::TypeOnlyReplaced { .. }
            | DependencyDisposition::BehaviorOnly { .. }
            | DependencyDisposition::Rejected { .. } => {}
        }
    }
    let complete = std::fs::read_to_string(fixture("serpapi-npm-record.yaml")).unwrap();
    let open = complete.replace("kind: type_only_replaced", "kind: open");
    assert_eq!(
        load_record_bytes(open.as_bytes()).unwrap_err().code(),
        "source_record_incomplete"
    );
}

#[test]
fn notice_and_destination_fields_are_required() {
    let bytes = std::fs::read(fixture("donat-owned-record.yaml")).unwrap();
    let source = std::str::from_utf8(&bytes).unwrap();
    let without_destination = source.replace(
        "proposed_destinations: [connector-catalog/sources/records/http.yaml]",
        "proposed_destinations: []",
    );
    assert!(load_record_bytes(without_destination.as_bytes()).is_err());

    let production =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("sources/records/donat-owned-http-v1.yaml");
    load_record(production).unwrap();
}
