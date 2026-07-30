use donat_connector_abi::{CompiledStepId, ConnectorId, OperationId};
use donat_connector_catalog::{
    CatalogHashDomain, ProvenanceMaterialV1, SemanticMaterialV1, SourceRecordMaterialV1,
    ValueContractMaterialV1, canonical_material_bytes, canonical_projection_owner_manifest,
    canonicalize_raw, domain_hash_bytes, provenance_sha256, record_sha256,
    selected_response_header, semantic_sha256, typed_value_material,
    validate_canonical_owner_manifest, value_contract_sha256,
};
use donat_value_contract::{BoundedInlineBytes, CanonicalNumber, TypedValue};

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn canonical_projection_domains_and_calculation_order_are_exact() {
    let vectors = [
        (
            CatalogHashDomain::SourceRecord,
            b"{}".as_slice(),
            "210c9ca679adf8e51a22e107484e4dd5e27a1d894901541bf5b5abd5a71fcbd4",
        ),
        (
            CatalogHashDomain::Semantic,
            b"{}".as_slice(),
            "799ea52772e70c9b45d9af5fcd185ae47f0fdcccd5957214d5425a1941c36f19",
        ),
        (
            CatalogHashDomain::Provenance,
            b"{}".as_slice(),
            "a0b89c2c2f1c7e90d8427e2c4251234ce596f2af9105f23745237aa08b1e06f4",
        ),
        (
            CatalogHashDomain::ValueContract,
            b"{}".as_slice(),
            "6f72f51c0e8b4f09a064c507a1d879921d4753cc4378fb6fefecb27e25e3dd2f",
        ),
        (
            CatalogHashDomain::SourceRecord,
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "d6c4fc943d8ed980d248ffa25f2d8d16be65953603705d5afc29e5e8a045269f",
        ),
        (
            CatalogHashDomain::Semantic,
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "2f7116c006c1fdfccdd12b1fa954cd94feffee889ceac39a0f76df616da7be34",
        ),
        (
            CatalogHashDomain::Provenance,
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "4e31e445b6c8d06e6b93fd5cc66731b850a84853dd4ee28d6a76663138217a23",
        ),
        (
            CatalogHashDomain::ValueContract,
            br#"{"a":1,"b":[true,null,"x"]}"#,
            "e74426ca8fb7b23e99f1f14f4a6d281575489c33312e27df9e9005f37158d4ab",
        ),
    ];

    for (domain, bytes, expected) in vectors {
        assert_eq!(hex(domain_hash_bytes(domain, bytes)), expected);
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
    assert_eq!(report.mapping_rows, 613);
    assert_eq!(report.normalized_leaf_and_branch_total, 457);
}

#[test]
fn canonical_owner_manifest_has_no_wildcards_or_duplicate_paths() {
    let manifest = canonical_projection_owner_manifest();
    assert!(!manifest.contains("|*|"));
    assert!(!manifest.contains("|family|"));
    assert!(!manifest.contains("<family>"));
    assert_eq!(manifest.lines().count(), 614);
    validate_canonical_owner_manifest().unwrap();
}

#[test]
fn canonical_owner_manifest_matches_normalized_leaf_and_branch_set() {
    let report = validate_canonical_owner_manifest().unwrap();
    assert_eq!(
        (report.mapping_rows, report.normalized_leaf_and_branch_total),
        (613, 457)
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
    let record: SourceRecordMaterialV1 =
        serde_json::from_str(full_vector("source-record")).unwrap();
    let value_contract: ValueContractMaterialV1 =
        serde_json::from_str(full_vector("value-contract")).unwrap();
    let semantic: SemanticMaterialV1 = serde_json::from_str(full_vector("semantic")).unwrap();
    let provenance: ProvenanceMaterialV1 = serde_json::from_str(full_vector("provenance")).unwrap();

    assert_eq!(
        hex(*record_sha256(&record).unwrap().as_bytes()),
        "420f0a4efd63b5d02479658c7686ec3da5ee688a0bc6aaf45bebfb98809fe991"
    );
    assert_eq!(
        hex(*value_contract_sha256(&value_contract).unwrap().as_bytes()),
        "79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a"
    );
    assert_eq!(
        hex(*semantic_sha256(&semantic).unwrap().as_bytes()),
        "f6bc86c9d5004885bb3156ab320fa76ad3ff7e9686320c54735dcfbd8c27e934"
    );
    assert_eq!(
        hex(*provenance_sha256(&provenance).unwrap().as_bytes()),
        "326236f741dfa72628b63ae308599b94e83b1c2aa1aa00bd80025ff5381a7531"
    );
}

#[test]
fn semantic_projection_uses_no_provenance_bearing_runtime_descriptor() {
    let semantic: SemanticMaterialV1 = serde_json::from_str(full_vector("semantic")).unwrap();
    let bytes = canonical_material_bytes(&semantic).unwrap();
    let source = std::str::from_utf8(&bytes).unwrap();
    for provenance_member in [
        "source_record_id",
        "record_sha256",
        "artifact_content_sha256",
        "notice_id",
        "provider_evidence",
    ] {
        assert!(!source.contains(provenance_member), "{provenance_member}");
    }
}

#[test]
fn final_provenance_commits_semantic_hash() {
    let mut provenance: ProvenanceMaterialV1 =
        serde_json::from_str(full_vector("provenance")).unwrap();
    let before = provenance_sha256(&provenance).unwrap();
    provenance.connector.semantic_sha256 =
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_owned();
    let after = provenance_sha256(&provenance).unwrap();
    assert_ne!(before.as_bytes(), after.as_bytes());
}

#[test]
fn canonical_projection_one_field_mutations_are_separate() {
    let mut record: SourceRecordMaterialV1 =
        serde_json::from_str(full_vector("source-record")).unwrap();
    let semantic: SemanticMaterialV1 = serde_json::from_str(full_vector("semantic")).unwrap();
    let provenance: ProvenanceMaterialV1 = serde_json::from_str(full_vector("provenance")).unwrap();
    let value_contract: ValueContractMaterialV1 =
        serde_json::from_str(full_vector("value-contract")).unwrap();

    let semantic_before = semantic_sha256(&semantic).unwrap();
    let provenance_before = provenance_sha256(&provenance).unwrap();
    let value_before = value_contract_sha256(&value_contract).unwrap();
    let record_before = record_sha256(&record).unwrap();
    record.reviewer.push_str(".changed");
    assert_ne!(
        record_before.as_bytes(),
        record_sha256(&record).unwrap().as_bytes()
    );
    assert_eq!(
        semantic_before.as_bytes(),
        semantic_sha256(&semantic).unwrap().as_bytes()
    );
    assert_eq!(
        provenance_before.as_bytes(),
        provenance_sha256(&provenance).unwrap().as_bytes()
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
