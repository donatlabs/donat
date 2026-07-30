use std::path::Path;

use donat_connector_abi::{CompiledStepId, ConnectorId, OperationId};
use donat_connector_catalog::{
    SourcePath, TypedValueMaterialV1, ValueContractMaterialV1, canonical_material_bytes,
    canonical_projection_owner_manifest, canonicalize_raw, decode_source_record_material,
    decode_value_contract_material, load_record, record_sha256, selected_response_header,
    source_record_material, typed_value_material, validate_canonical_owner_manifest,
    value_contract_sha256,
};
use donat_value_contract::{BoundedInlineBytes, CanonicalNumber, TypedValue};
use sha2::{Digest, Sha256};

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn canonical_projection_domains_and_calculation_order_are_exact() {
    let vectors = [
        (
            b"donat.connector.source-record.v1\0".as_slice(),
            b"{}".as_slice(),
            "210c9ca679adf8e51a22e107484e4dd5e27a1d894901541bf5b5abd5a71fcbd4",
        ),
        (
            b"donat.connector.semantic.v1\0".as_slice(),
            b"{}".as_slice(),
            "799ea52772e70c9b45d9af5fcd185ae47f0fdcccd5957214d5425a1941c36f19",
        ),
        (
            b"donat.connector.provenance.v1\0".as_slice(),
            b"{}".as_slice(),
            "a0b89c2c2f1c7e90d8427e2c4251234ce596f2af9105f23745237aa08b1e06f4",
        ),
        (
            b"donat.connector.value-contract.v1\0".as_slice(),
            b"{}".as_slice(),
            "6f72f51c0e8b4f09a064c507a1d879921d4753cc4378fb6fefecb27e25e3dd2f",
        ),
        (
            b"donat.connector.source-record.v1\0".as_slice(),
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "d6c4fc943d8ed980d248ffa25f2d8d16be65953603705d5afc29e5e8a045269f",
        ),
        (
            b"donat.connector.semantic.v1\0".as_slice(),
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "2f7116c006c1fdfccdd12b1fa954cd94feffee889ceac39a0f76df616da7be34",
        ),
        (
            b"donat.connector.provenance.v1\0".as_slice(),
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "4e31e445b6c8d06e6b93fd5cc66731b850a84853dd4ee28d6a76663138217a23",
        ),
        (
            b"donat.connector.value-contract.v1\0".as_slice(),
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "e74426ca8fb7b23e99f1f14f4a6d281575489c33312e27df9e9005f37158d4ab",
        ),
    ];

    for (domain, bytes, expected) in vectors {
        let mut hash = Sha256::new();
        hash.update(domain);
        hash.update(bytes);
        assert_eq!(hex(hash.finalize().into()), expected);
    }
}

#[test]
fn selected_header_capability_vector_is_exact() {
    let selected = selected_response_header(
        ConnectorId::literal("donat.http"),
        OperationId::literal("get"),
        donat_connector_catalog::StableSemver::new(1, 0, 0),
        CompiledStepId::literal("request"),
        "X-Request-ID",
    )
    .unwrap();
    assert_eq!(selected.canonical_lowercase_header_name, "x-request-id");
    assert_eq!(
        selected.capability.as_str(),
        "response-header.fc5e32fca2bee508e1689d6423697c171e5db342f1eaf082987183756c5ac3d3"
    );
    assert_eq!(selected.capability.as_str().len(), 80);
}

#[test]
fn jcs_member_names_use_utf16_order() {
    assert_eq!(
        canonicalize_raw("{\"�\":1,\"𐀀\":2}".as_bytes()).unwrap(),
        "{\"𐀀\":2,\"�\":1}".as_bytes()
    );
}

#[test]
fn canonical_projection_field_matrix_is_total() {
    let report = validate_canonical_owner_manifest().unwrap();
    assert!(report.mapping_rows > 0);
    assert!(report.normalized_leaf_and_branch_total > 0);
}

#[test]
fn canonical_owner_manifest_has_no_wildcards_or_duplicate_paths() {
    let manifest = canonical_projection_owner_manifest();
    assert!(!manifest.contains("|*|"));
    assert!(!manifest.contains("|family|"));
    assert!(!manifest.contains("<family>"));
    let report = validate_canonical_owner_manifest().unwrap();
    assert_eq!(manifest.lines().count(), report.mapping_rows + 1);
}

#[test]
fn canonical_owner_manifest_matches_normalized_leaf_and_branch_set() {
    let report = validate_canonical_owner_manifest().unwrap();
    let manifest = canonical_projection_owner_manifest();
    let rows: Vec<_> = manifest.lines().skip(1).collect();
    let normalized_owners: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|row| {
            let columns: Vec<_> = row.split('|').collect();
            (columns[0], columns[1])
        })
        .collect();
    assert_eq!(report.mapping_rows, rows.len());
    assert_eq!(
        report.normalized_leaf_and_branch_total,
        normalized_owners.len()
    );
}

#[test]
fn typed_value_projection_tags_do_not_collide() {
    let values = [
        TypedValue::Number(CanonicalNumber::I64(1)),
        TypedValue::Number(CanonicalNumber::U64(1)),
        TypedValue::String("1".to_owned()),
    ];
    let bytes: Vec<_> = values
        .iter()
        .map(|value| canonical_material_bytes(&typed_value_material(value)).unwrap())
        .collect();
    assert_eq!(bytes[0], br#"{"kind":"i64","value":"1"}"#);
    assert_eq!(bytes[1], br#"{"kind":"u64","value":"1"}"#);
    assert_eq!(bytes[2], br#"{"kind":"string","value":"1"}"#);
    assert_ne!(bytes[0], bytes[1]);
    assert_ne!(bytes[1], bytes[2]);
}

#[test]
fn typed_value_material_constructor_rejects_noncanonical_i64() {
    assert_eq!(
        TypedValueMaterialV1::i64("not-an-integer")
            .unwrap_err()
            .code(),
        "catalog_jcs_schema_mismatch"
    );
}

#[test]
fn typed_value_projection_preserves_u64_decimal_and_base64() {
    let maximum = typed_value_material(&TypedValue::Number(CanonicalNumber::U64(u64::MAX)));
    assert_eq!(
        canonical_material_bytes(&maximum).unwrap(),
        br#"{"kind":"u64","value":"18446744073709551615"}"#
    );
    let bytes =
        BoundedInlineBytes::try_new(vec![0xff, 0x00], "application/octet-stream", None, 2).unwrap();
    let material = typed_value_material(&TypedValue::InlineBytes(bytes));
    assert_eq!(
        canonical_material_bytes(&material).unwrap(),
        br#"{"kind":"inline_bytes","value":{"$binary":"_wA","file_name":null,"media_type":"application/octet-stream"}}"#
    );
}

#[test]
fn typed_value_projection_string_and_object_do_not_collide() {
    let string = TypedValue::String(r#"{"kind":"null","value":null}"#.to_owned());
    let object = TypedValue::Object(
        [("kind".to_owned(), TypedValue::String("null".to_owned()))]
            .into_iter()
            .collect(),
    );
    assert_ne!(
        canonical_material_bytes(&typed_value_material(&string)).unwrap(),
        canonical_material_bytes(&typed_value_material(&object)).unwrap()
    );
}

#[test]
fn source_projection_preserves_declared_entrypoint_order() {
    let mut record = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/donat-owned-record.yaml"),
    )
    .unwrap();
    record.entrypoints = vec![
        SourcePath::parse("z/entrypoint.rs").unwrap(),
        SourcePath::parse("a/entrypoint.rs").unwrap(),
    ];
    let material = source_record_material(&record).unwrap();
    let document: serde_json::Value =
        serde_json::from_slice(&canonical_material_bytes(&material).unwrap()).unwrap();
    assert_eq!(
        document["entrypoints"],
        serde_json::json!(["z/entrypoint.rs", "a/entrypoint.rs"])
    );
}

#[test]
fn npm_integrity_projection_is_a_closed_algorithm_and_digest_object() {
    let record = load_record(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serpapi-npm-record.yaml"),
    )
    .unwrap();
    let material = source_record_material(&record).unwrap();
    let document: serde_json::Value =
        serde_json::from_slice(&canonical_material_bytes(&material).unwrap()).unwrap();
    assert_eq!(
        document["subject"]["value"]["integrity"],
        serde_json::json!({
            "algorithm": {"kind": "sha512", "value": null},
            "digest": "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCkqKywtLi8wMTIzNDU2Nzg5Ojs8PT4_QA"
        })
    );
}

fn full_vector(label: &str) -> &'static str {
    let document = include_str!(
        "../../../knowledgebase/declarative-saas/decisions/012-canonical-catalog-projections-and-persisted-header-capabilities.md"
    );
    let marker = format!("\n{label}:\n{{");
    let start = document.rfind(&marker).unwrap() + marker.len() - 1;
    let end = document[start..].find('\n').unwrap() + start;
    &document[start..end]
}

#[test]
fn canonical_projection_full_material_vectors_are_exact() {
    let record = decode_source_record_material(full_vector("source-record").as_bytes()).unwrap();
    let value_contract: ValueContractMaterialV1 =
        decode_value_contract_material(full_vector("value-contract").as_bytes()).unwrap();

    assert_eq!(
        hex(*record_sha256(&record).unwrap().as_bytes()),
        "420f0a4efd63b5d02479658c7686ec3da5ee688a0bc6aaf45bebfb98809fe991"
    );
    assert_eq!(
        hex(*value_contract_sha256(&value_contract).unwrap().as_bytes()),
        "79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a"
    );
}

#[test]
fn canonical_projection_one_field_mutations_are_separate() {
    let record = decode_source_record_material(full_vector("source-record").as_bytes()).unwrap();
    let changed_source = full_vector("source-record").replace(
        "\"reviewer\":\"reviewer.demo\"",
        "\"reviewer\":\"reviewer.changed\"",
    );
    let changed = decode_source_record_material(changed_source.as_bytes()).unwrap();
    let value_contract: ValueContractMaterialV1 =
        decode_value_contract_material(full_vector("value-contract").as_bytes()).unwrap();

    let value_before = value_contract_sha256(&value_contract).unwrap();
    let record_before = record_sha256(&record).unwrap();
    assert_ne!(
        record_before.as_bytes(),
        record_sha256(&changed).unwrap().as_bytes()
    );
    assert_eq!(
        value_before.as_bytes(),
        value_contract_sha256(&value_contract).unwrap().as_bytes()
    );
}

#[test]
fn selected_header_capability_scope_and_abi_fit_are_exact() {
    let derive = |operation, step, header| {
        selected_response_header(
            ConnectorId::literal("donat.http"),
            OperationId::parse(operation).unwrap(),
            donat_connector_catalog::StableSemver::new(1, 0, 0),
            CompiledStepId::parse(step).unwrap(),
            header,
        )
        .unwrap()
        .capability
    };
    let baseline = derive("get", "request", "x-request-id");
    assert!(baseline != derive("post", "request", "x-request-id"));
    assert!(baseline != derive("get", "response", "x-request-id"));
    assert!(baseline != derive("get", "request", "retry-after"));
    assert!(donat_connector_abi::CapabilityId::parse(baseline.as_str()).is_ok());
}
