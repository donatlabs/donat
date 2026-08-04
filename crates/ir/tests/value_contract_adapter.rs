use std::collections::BTreeMap;

use donat_ir::{
    BoundedInlineBytes, ProcessStartPolicy, TypeRef, TypedValue, ValueContractCatalog,
    ValueContractError, ValueScalar, ValueType, canonical_size, compile_value_contract_catalog,
};
use donat_metadata::Metadata;
use serde_json::json;

type InlineBytesConstructor =
    fn(Vec<u8>, &str, Option<&str>, usize) -> Result<BoundedInlineBytes, ValueContractError>;

fn metadata(custom_types: serde_json::Value) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "custom_types": custom_types,
        "sources": []
    }))
    .expect("metadata fixture")
}

#[test]
fn ir_reexports_the_only_value_contract_types() {
    let constructor: InlineBytesConstructor = BoundedInlineBytes::try_new;
    let size: fn(&TypedValue) -> Result<usize, ValueContractError> = canonical_size;

    let bytes = constructor(
        vec![1, 2, 3],
        "application/octet-stream",
        Some("payload.bin"),
        3,
    )
    .unwrap();
    let independent_oracle = "{\"$binary\":\"AQID\",\"file_name\":\"payload.bin\",\"media_type\":\"application/octet-stream\"}";
    assert_eq!(independent_oracle.len(), 84);
    assert_eq!(size(&TypedValue::InlineBytes(bytes)), Ok(84));

    fn direct_owner(value: donat_value_contract::ValueContractCatalog) -> ValueContractCatalog {
        value
    }
    let direct = direct_owner(ValueContractCatalog {
        roots: BTreeMap::new(),
        named_objects: BTreeMap::new(),
    });
    assert!(direct.roots.is_empty());
    assert_eq!(ProcessStartPolicy::Enabled, ProcessStartPolicy::Enabled);
    assert_ne!(
        ProcessStartPolicy::Enabled,
        ProcessStartPolicy::RejectRetired
    );
}

#[test]
fn adapter_normalizes_custom_types_and_recursive_refs() {
    let metadata = metadata(json!({
        "input_objects": [{
            "name": "Tree",
            "fields": [
                { "name": "label", "type": "String!" },
                { "name": "next", "type": "Tree" }
            ]
        }],
        "enums": [{
            "name": "Status",
            "values": [{ "value": "OPEN" }, { "value": "CLOSED" }]
        }],
        "scalars": [{ "name": "Money" }]
    }));
    let fields = BTreeMap::from([
        ("tree".to_owned(), "Tree!".to_owned()),
        ("status".to_owned(), "Status!".to_owned()),
        ("amount".to_owned(), "Money".to_owned()),
        ("ids".to_owned(), "[uuid!]!".to_owned()),
    ]);

    let contract = compile_value_contract_catalog(&metadata, &fields).expect("valid adapter input");
    assert_eq!(
        contract.roots["ids"].type_ref,
        TypeRef {
            nullable: false,
            value_type: ValueType::List {
                element: Box::new(TypeRef {
                    nullable: false,
                    value_type: ValueType::Scalar {
                        scalar: ValueScalar::Uuid,
                    },
                }),
            },
        }
    );
    assert!(matches!(
        contract.roots["tree"].type_ref.value_type,
        ValueType::Ref { ref name } if name == "Tree"
    ));
    assert!(matches!(
        contract.roots["status"].type_ref.value_type,
        ValueType::Enum { ref name, ref values }
            if name == "Status" && values == &["OPEN", "CLOSED"]
    ));
    assert!(matches!(
        contract.roots["amount"].type_ref.value_type,
        ValueType::Scalar {
            scalar: ValueScalar::Custom { ref name },
        } if name == "Money"
    ));
    assert!(matches!(
        contract.named_objects["Tree"].fields["next"]
            .type_ref
            .value_type,
        ValueType::Ref { ref name } if name == "Tree"
    ));
}

#[test]
fn adapter_rejects_unknown_duplicate_and_invalid_refs() {
    let unknown = metadata(json!({}));
    assert!(
        compile_value_contract_catalog(
            &unknown,
            &BTreeMap::from([("value".to_owned(), "Missing!".to_owned())]),
        )
        .is_err()
    );

    let duplicate = metadata(json!({
        "input_objects": [
            { "name": "Repeated", "fields": [] },
            { "name": "Repeated", "fields": [] }
        ]
    }));
    assert!(
        compile_value_contract_catalog(
            &duplicate,
            &BTreeMap::from([("value".to_owned(), "Repeated!".to_owned())]),
        )
        .is_err()
    );

    let duplicate_field = metadata(json!({
        "input_objects": [{
            "name": "RepeatedField",
            "fields": [
                { "name": "value", "type": "String!" },
                { "name": "value", "type": "String!" }
            ]
        }]
    }));
    assert!(
        compile_value_contract_catalog(
            &duplicate_field,
            &BTreeMap::from([("value".to_owned(), "RepeatedField!".to_owned())]),
        )
        .is_err()
    );

    let malformed = metadata(json!({}));
    assert!(
        compile_value_contract_catalog(
            &malformed,
            &BTreeMap::from([("value".to_owned(), "[ uuid!]!".to_owned())]),
        )
        .is_err()
    );
}

#[test]
fn adapter_accepts_the_declared_double_underscore_identifier_grammar() {
    let metadata = metadata(json!({
        "input_objects": [{
            "name": "__Tree",
            "fields": [{ "name": "__label", "type": "String!" }]
        }]
    }));
    let contract = compile_value_contract_catalog(
        &metadata,
        &BTreeMap::from([("__root".to_owned(), "__Tree!".to_owned())]),
    )
    .expect("the shared identifier grammar reserves no double-underscore prefix");
    assert!(contract.roots.contains_key("__root"));
    assert!(
        contract.named_objects["__Tree"]
            .fields
            .contains_key("__label")
    );
}
