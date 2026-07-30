use std::collections::BTreeMap;

use donat_connector_catalog::{
    canonical_material_bytes, decode_value_contract_material, value_contract_material,
    value_contract_sha256,
};
use donat_value_contract::{
    TypeRef, ValueContractCatalog, ValueContractField, ValueScalar, ValueType,
};

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn contract(scalar: ValueScalar, nullable: bool) -> ValueContractCatalog {
    ValueContractCatalog {
        roots: BTreeMap::from([(
            "query".to_owned(),
            ValueContractField {
                required: true,
                type_ref: TypeRef {
                    nullable,
                    value_type: ValueType::Scalar { scalar },
                },
            },
        )]),
        named_objects: BTreeMap::new(),
    }
}

#[test]
fn value_scalar_projection_matches_every_spec_005_owner_branch() {
    let vectors = [
        (
            ValueScalar::Boolean,
            r#"{"kind":"boolean","value":null}"#,
            "d0b19f2e9f814ddc5457fd85728dfe4ef649042a5134f12d3ac42fb4009ecc58",
        ),
        (
            ValueScalar::String,
            r#"{"kind":"string","value":null}"#,
            "79654c21d469a22dc151e57c973b41c2539a7b7e197b1652ff80d6b3dcc3c18a",
        ),
        (
            ValueScalar::Int32,
            r#"{"kind":"int32","value":null}"#,
            "d91c7215c24937b62dc176287b48ca5c5f923d034777706323f0b61157a6a2f2",
        ),
        (
            ValueScalar::Int64,
            r#"{"kind":"int64","value":null}"#,
            "d1f1966e3e49124f6cce79167814e323315c0e810143684321e7bc7ade23a972",
        ),
        (
            ValueScalar::UInt64,
            r#"{"kind":"uint64","value":null}"#,
            "a64ccafb81f9b513c634d8f0e206e1aac705d5fcbd06fa8c5adacc34247f7ddb",
        ),
        (
            ValueScalar::Decimal,
            r#"{"kind":"decimal","value":null}"#,
            "8f1a181165ec3d629693f106d9566d02f76eefe9317746044ee0693b7aa08f6b",
        ),
        (
            ValueScalar::Uuid,
            r#"{"kind":"uuid","value":null}"#,
            "66c4b3a082f73831d439eb7c624409e9741621b4f7af14892896229f0bee524a",
        ),
        (
            ValueScalar::Date,
            r#"{"kind":"date","value":null}"#,
            "93ed861bcc9b7f6213abcbdb87514515856c4d84eab444f13b3d678e3f3716a7",
        ),
        (
            ValueScalar::Timestamp,
            r#"{"kind":"timestamp","value":null}"#,
            "f1c17e281b279e50480d60a9ee6568df17f7eef1da6e18fd60131a2300df971a",
        ),
        (
            ValueScalar::TimestampTz,
            r#"{"kind":"timestamptz","value":null}"#,
            "d79bbe1e56bc00033fcae029d1c9b5826bb805e0657bf0acac285420bf42b169",
        ),
        (
            ValueScalar::Json,
            r#"{"kind":"json","value":null}"#,
            "0b3c1359fac4024dc5dc65e6bace2144f075ba8cc55cfb62e8003ae244b0a879",
        ),
        (
            ValueScalar::Custom {
                name: "custom.demo".to_owned(),
            },
            r#"{"kind":"custom","value":{"name":"custom.demo"}}"#,
            "5f7c7c1db65b1e54751e4189a4ae314952912d31c0cab1b0d0f7b7ca6792e6ad",
        ),
    ];
    let prefix = r#"{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":"#;
    let suffix = r#"}}}},"value_language_epoch":1}"#;
    for (scalar, scalar_bytes, expected_hash) in vectors {
        let material = value_contract_material(&contract(scalar, false), 1).unwrap();
        let expected = format!("{prefix}{scalar_bytes}{suffix}");
        assert_eq!(
            canonical_material_bytes(&material).unwrap(),
            expected.as_bytes()
        );
        assert_eq!(
            hex(value_contract_sha256(&material).unwrap().as_bytes()),
            expected_hash
        );
    }
}

#[test]
fn value_scalar_decoder_rejects_unowned_null_and_inline_bytes_tags() {
    let valid = br#"{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":false,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1}"#;
    for tag in ["null", "inline_bytes", "i64", "u64"] {
        let invalid = std::str::from_utf8(valid)
            .unwrap()
            .replace(r#""kind":"string""#, &format!(r#""kind":"{tag}""#));
        assert_eq!(
            decode_value_contract_material(invalid.as_bytes())
                .unwrap_err()
                .code(),
            "catalog_projection_input_mismatch"
        );
    }
}

#[test]
fn nullable_semantics_change_only_type_ref_nullability() {
    let material = value_contract_material(&contract(ValueScalar::String, true), 1).unwrap();
    assert_eq!(
        canonical_material_bytes(&material).unwrap(),
        br#"{"named_objects":{},"roots":{"query":{"required":true,"type_ref":{"nullable":true,"value_type":{"kind":"scalar","value":{"kind":"string","value":null}}}}},"value_language_epoch":1}"#
    );
    assert_eq!(
        hex(value_contract_sha256(&material).unwrap().as_bytes()),
        "9630316fc75152223f33663a03f6be51d4953603a7fa9ccabf8560ca9585bd84"
    );
}
