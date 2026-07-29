use std::collections::BTreeMap;
use std::process::Command;

use donat_value_contract::{
    BoundedInlineBytes, CanonicalDecimal, CanonicalNumber, TypeRef, TypedValue,
    ValueContractCatalog, ValueContractField, ValueObjectContract, ValueScalar, ValueType,
    canonical_size,
};

fn parsed(source: &str) -> TypeRef {
    TypeRef::parse(source).unwrap_or_else(|error| panic!("`{source}` must parse: {error}"))
}

fn field(source: &str) -> ValueContractField {
    ValueContractField {
        required: source.ends_with('!'),
        type_ref: parsed(source),
    }
}

fn object(entries: impl IntoIterator<Item = (&'static str, TypedValue)>) -> TypedValue {
    TypedValue::Object(
        entries
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

fn catalog(
    entries: impl IntoIterator<Item = (&'static str, &'static str)>,
) -> ValueContractCatalog {
    ValueContractCatalog {
        roots: entries
            .into_iter()
            .map(|(name, type_ref)| (name.to_owned(), field(type_ref)))
            .collect(),
        named_objects: BTreeMap::new(),
    }
}

#[test]
fn value_type_language_is_closed_and_canonical() {
    let aliases = [
        ("Boolean", ValueScalar::Boolean),
        ("bool", ValueScalar::Boolean),
        ("boolean", ValueScalar::Boolean),
        ("String", ValueScalar::String),
        ("string", ValueScalar::String),
        ("ID", ValueScalar::String),
        ("Int", ValueScalar::Int32),
        ("int", ValueScalar::Int32),
        ("int32", ValueScalar::Int32),
        ("int64", ValueScalar::Int64),
        ("uint64", ValueScalar::UInt64),
        ("Float", ValueScalar::Decimal),
        ("float", ValueScalar::Decimal),
        ("decimal", ValueScalar::Decimal),
        ("uuid", ValueScalar::Uuid),
        ("date", ValueScalar::Date),
        ("timestamp", ValueScalar::Timestamp),
        ("timestamptz", ValueScalar::TimestampTz),
        ("json", ValueScalar::Json),
        ("jsonb", ValueScalar::Json),
    ];

    for (source, expected) in aliases {
        assert_eq!(
            parsed(source),
            TypeRef {
                nullable: true,
                value_type: ValueType::Scalar { scalar: expected },
            },
            "alias `{source}` must normalize to one canonical scalar"
        );
    }

    assert_eq!(
        parsed("[uuid!]!"),
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
    assert_eq!(
        parsed("CustomerInput"),
        TypeRef {
            nullable: true,
            value_type: ValueType::Ref {
                name: "CustomerInput".to_owned(),
            },
        }
    );

    for invalid in [
        "",
        " uuid",
        "uuid ",
        "[ uuid]",
        "uuid!!",
        "[uuid",
        "uuid]",
        "[]",
        "[uuid]tail",
        "9uuid",
        "uuid-name",
    ] {
        assert!(
            TypeRef::parse(invalid).is_err(),
            "`{invalid}` must not enter the closed type language"
        );
    }
}

#[test]
fn value_type_identifier_grammar_has_no_implicit_reserved_prefix() {
    assert_eq!(
        parsed("__bad"),
        TypeRef {
            nullable: true,
            value_type: ValueType::Ref {
                name: "__bad".to_owned(),
            },
        }
    );
}

#[test]
fn value_contract_no_std_boundary_is_mechanical() {
    let workspace_manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(workspace_manifest)
        .output()
        .expect("cargo metadata must run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata JSON");
    let package = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "donat-value-contract")
        .expect("value-contract package in workspace metadata");

    assert_eq!(package["features"], serde_json::json!({ "default": [] }));
    assert_eq!(package["publish"], serde_json::json!([]));
    assert!(
        package["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .all(|dependency| dependency["kind"] == "dev"),
        "normal/build dependencies are forbidden: {}",
        package["dependencies"]
    );
    assert!(
        package["targets"].as_array().unwrap().iter().all(|target| {
            target["kind"]
                .as_array()
                .unwrap()
                .iter()
                .all(|kind| kind != "custom-build" && kind != "proc-macro")
        }),
        "build scripts and proc-macro targets are forbidden: {}",
        package["targets"]
    );
}

#[test]
fn value_contract_has_one_owner() {
    let recursive = ValueContractCatalog {
        roots: BTreeMap::from([("root".to_owned(), field("Tree!"))]),
        named_objects: BTreeMap::from([(
            "Tree".to_owned(),
            ValueObjectContract {
                fields: BTreeMap::from([
                    ("label".to_owned(), field("String!")),
                    ("child".to_owned(), field("Tree")),
                ]),
            },
        )]),
    };

    recursive
        .validate(&object([(
            "root",
            object([
                ("label", TypedValue::String("top".to_owned())),
                (
                    "child",
                    object([("label", TypedValue::String("leaf".to_owned()))]),
                ),
            ]),
        )]))
        .expect("one named-object table resolves every recursive reference");

    let unknown = ValueContractCatalog {
        roots: BTreeMap::from([("root".to_owned(), field("Missing!"))]),
        named_objects: BTreeMap::new(),
    };
    assert!(unknown.validate(&object([])).is_err());
}

#[test]
fn value_contract_distinguishes_required_from_nullable() {
    let contract = ValueContractCatalog {
        roots: BTreeMap::from([
            (
                "required_nullable".to_owned(),
                ValueContractField {
                    required: true,
                    type_ref: parsed("String"),
                },
            ),
            (
                "optional_non_null".to_owned(),
                ValueContractField {
                    required: false,
                    type_ref: parsed("String!"),
                },
            ),
        ]),
        named_objects: BTreeMap::new(),
    };

    contract
        .validate(&object([("required_nullable", TypedValue::Null)]))
        .expect("required controls presence while nullable controls null");
    contract
        .validate(&object([
            (
                "required_nullable",
                TypedValue::String("present".to_owned()),
            ),
            (
                "optional_non_null",
                TypedValue::String("also present".to_owned()),
            ),
        ]))
        .expect("both present values satisfy their independent dimensions");

    assert!(contract.validate(&object([])).is_err());
    assert!(
        contract
            .validate(&object([
                (
                    "required_nullable",
                    TypedValue::String("present".to_owned())
                ),
                ("optional_non_null", TypedValue::Null),
            ]))
            .is_err()
    );
}

#[test]
fn value_contract_resolves_recursive_object_refs() {
    let contract = ValueContractCatalog {
        roots: BTreeMap::from([("node".to_owned(), field("Node!"))]),
        named_objects: BTreeMap::from([(
            "Node".to_owned(),
            ValueObjectContract {
                fields: BTreeMap::from([
                    ("id".to_owned(), field("uuid!")),
                    ("next".to_owned(), field("Node")),
                ]),
            },
        )]),
    };
    let value = object([(
        "node",
        object([
            (
                "id",
                TypedValue::String("018f47a3-7a4b-7e32-8d96-3b1546fb62f4".to_owned()),
            ),
            (
                "next",
                object([(
                    "id",
                    TypedValue::String("018f47a3-7a4b-7e32-8d96-3b1546fb62f5".to_owned()),
                )]),
            ),
        ]),
    )]);
    contract.validate(&value).expect("finite recursive value");
}

#[test]
fn value_contract_rejects_unknown_duplicate_and_invalid_refs() {
    let contract = ValueContractCatalog {
        roots: BTreeMap::from([("node".to_owned(), field("Node!"))]),
        named_objects: BTreeMap::from([(
            "Node".to_owned(),
            ValueObjectContract {
                fields: BTreeMap::from([("name".to_owned(), field("String!"))]),
            },
        )]),
    };

    assert!(
        contract
            .validate(&object([(
                "node",
                object([
                    ("name", TypedValue::String("known".to_owned())),
                    ("unknown", TypedValue::Boolean(true)),
                ]),
            )]))
            .is_err(),
        "unknown object fields must fail closed"
    );

    let missing_ref = ValueContractCatalog {
        roots: BTreeMap::from([("node".to_owned(), field("Missing!"))]),
        named_objects: BTreeMap::new(),
    };
    assert!(
        missing_ref
            .validate(&object([("node", object([]))]))
            .is_err()
    );
}

#[test]
fn value_contract_validates_every_closed_scalar_shape() {
    let contract = catalog([
        ("boolean", "boolean!"),
        ("string", "string!"),
        ("int32", "int32!"),
        ("int64", "int64!"),
        ("uint64", "uint64!"),
        ("decimal", "decimal!"),
        ("uuid", "uuid!"),
        ("date", "date!"),
        ("timestamp", "timestamp!"),
        ("timestamptz", "timestamptz!"),
        ("json", "json!"),
    ]);
    let valid = object([
        ("boolean", TypedValue::Boolean(true)),
        ("string", TypedValue::String("value".to_owned())),
        (
            "int32",
            TypedValue::Number(CanonicalNumber::I64(-2_147_483_648)),
        ),
        ("int64", TypedValue::Number(CanonicalNumber::I64(i64::MAX))),
        ("uint64", TypedValue::Number(CanonicalNumber::U64(u64::MAX))),
        (
            "decimal",
            TypedValue::Number(CanonicalNumber::Decimal(
                CanonicalDecimal::try_new("-12.5").unwrap(),
            )),
        ),
        (
            "uuid",
            TypedValue::String("018f47a3-7a4b-7e32-8d96-3b1546fb62f4".to_owned()),
        ),
        ("date", TypedValue::String("2024-02-29".to_owned())),
        (
            "timestamp",
            TypedValue::String("2024-02-29T23:59:59.123456".to_owned()),
        ),
        (
            "timestamptz",
            TypedValue::String("2024-02-29T23:59:59.123456+05:30".to_owned()),
        ),
        (
            "json",
            object([(
                "nested",
                TypedValue::List(vec![TypedValue::Null, TypedValue::Boolean(false)]),
            )]),
        ),
    ]);
    contract.validate(&valid).expect("all closed scalar shapes");

    let out_of_range = catalog([("value", "int32!")]);
    assert!(
        out_of_range
            .validate(&object([(
                "value",
                TypedValue::Number(CanonicalNumber::I64(2_147_483_648)),
            )]))
            .is_err()
    );

    let uuid = catalog([("value", "uuid!")]);
    assert!(
        uuid.validate(&object([(
            "value",
            TypedValue::String("018F47A3-7A4B-7E32-8D96-3B1546FB62F4".to_owned()),
        )]))
        .is_err(),
        "uppercase UUID text is not canonical"
    );
}

#[test]
fn canonical_decimal_spelling_is_exact() {
    let decimal_contract = catalog([("value", "decimal!")]);
    for (accepted, expected_size) in [("-12.5", 5), ("0.01", 4), ("10", 2), ("-0.1", 4)] {
        let decimal = CanonicalDecimal::try_new(accepted)
            .unwrap_or_else(|error| panic!("`{accepted}` must be canonical: {error}"));
        assert_eq!(decimal.as_str(), accepted);
        assert_eq!(
            canonical_size(&TypedValue::Number(CanonicalNumber::Decimal(
                decimal.clone()
            )))
            .expect("canonical decimal has a bounded size"),
            expected_size,
            "canonical_size must count the exact private decimal spelling"
        );
        decimal_contract
            .validate(&object([(
                "value",
                TypedValue::Number(CanonicalNumber::Decimal(decimal)),
            )]))
            .expect("constructed canonical decimal validates");
    }
    for rejected in [
        "-12.50e+2",
        "-12.50",
        "+1",
        "01",
        "-0",
        "0.0",
        "1.",
        ".1",
        "1e2",
    ] {
        assert!(
            CanonicalDecimal::try_new(rejected).is_err(),
            "`{rejected}` must not be stored as canonical decimal"
        );
    }
}

#[test]
fn value_contract_timestamp_grammar_is_exact() {
    let local = catalog([("value", "timestamp!")]);
    for valid in [
        "0000-01-01T00:00:00",
        "2024-02-29T23:59:59.1",
        "9999-12-31T23:59:59.123456",
    ] {
        local
            .validate(&object([("value", TypedValue::String(valid.to_owned()))]))
            .unwrap_or_else(|error| panic!("local `{valid}` must pass: {error}"));
    }
    for invalid in [
        "2023-02-29T00:00:00",
        "2024-02-29 00:00:00",
        "2024-02-29T00:00:60",
        "2024-02-29T00:00:00.",
        "2024-02-29T00:00:00.1234567",
        "2024-02-29T00:00:00Z",
        "2024-02-29T00:00:00+00:00",
    ] {
        assert!(
            local
                .validate(&object([("value", TypedValue::String(invalid.to_owned()))]))
                .is_err(),
            "local `{invalid}` must fail"
        );
    }

    let zoned = catalog([("value", "timestamptz!")]);
    for valid in ["2024-02-29T00:00:00Z", "2024-02-29T00:00:00.123456-03:30"] {
        zoned
            .validate(&object([("value", TypedValue::String(valid.to_owned()))]))
            .unwrap_or_else(|error| panic!("zoned `{valid}` must pass: {error}"));
    }
    for invalid in [
        "2024-02-29T00:00:00",
        "2024-02-29 00:00:00Z",
        "2024-02-29T00:00:00.1234567Z",
        "2024-02-29T00:00:00+24:00",
        "2024-02-29T00:00:00+00:60",
        "ééééééééa",
    ] {
        assert!(
            zoned
                .validate(&object([("value", TypedValue::String(invalid.to_owned()))]))
                .is_err(),
            "zoned `{invalid}` must fail"
        );
    }
}

#[test]
fn value_contract_assignability_is_nominal_except_json() {
    let target_json = catalog([("value", "json!")]);
    for source in [
        catalog([("value", "String!")]),
        catalog([("value", "[uuid!]!")]),
    ] {
        assert!(target_json.is_assignable_from(&source));
    }

    let non_null = catalog([("value", "String!")]);
    let nullable = catalog([("value", "String")]);
    assert!(!non_null.is_assignable_from(&nullable));
    assert!(!nullable.is_assignable_from(&non_null));

    let target_exact = catalog([("a", "String!")]);
    let source_with_extra = catalog([("a", "String!"), ("b", "String!")]);
    assert!(
        !target_exact.is_assignable_from(&source_with_extra),
        "contract assignment is exact, not width-subtyping"
    );

    let nested_contract = |with_extra: bool| ValueContractCatalog {
        roots: BTreeMap::from([(
            "value".to_owned(),
            ValueContractField {
                required: true,
                type_ref: TypeRef {
                    nullable: false,
                    value_type: ValueType::Object {
                        fields: if with_extra {
                            BTreeMap::from([
                                ("a".to_owned(), field("String!")),
                                ("b".to_owned(), field("String!")),
                            ])
                        } else {
                            BTreeMap::from([("a".to_owned(), field("String!"))])
                        },
                    },
                },
            },
        )]),
        named_objects: BTreeMap::new(),
    };
    assert!(
        !nested_contract(false).is_assignable_from(&nested_contract(true)),
        "nested object contracts are exact too"
    );

    let enum_contract = |name: &str| ValueContractCatalog {
        roots: BTreeMap::from([(
            "value".to_owned(),
            ValueContractField {
                required: true,
                type_ref: TypeRef {
                    nullable: false,
                    value_type: ValueType::Enum {
                        name: name.to_owned(),
                        values: vec!["OPEN".to_owned(), "CLOSED".to_owned()],
                    },
                },
            },
        )]),
        named_objects: BTreeMap::new(),
    };
    assert!(!enum_contract("Status").is_assignable_from(&enum_contract("OtherStatus")));

    let custom_contract = |name: &str| ValueContractCatalog {
        roots: BTreeMap::from([(
            "value".to_owned(),
            ValueContractField {
                required: true,
                type_ref: TypeRef {
                    nullable: false,
                    value_type: ValueType::Scalar {
                        scalar: ValueScalar::Custom {
                            name: name.to_owned(),
                        },
                    },
                },
            },
        )]),
        named_objects: BTreeMap::new(),
    };
    assert!(!custom_contract("Money").is_assignable_from(&custom_contract("Email")));
}

#[test]
fn value_contract_assignability_compares_unreachable_named_objects() {
    let contract = |objects: &[(&str, &str)]| ValueContractCatalog {
        roots: catalog([("value", "String!")]).roots,
        named_objects: objects
            .iter()
            .map(|(name, type_ref)| {
                (
                    (*name).to_owned(),
                    ValueObjectContract {
                        fields: BTreeMap::from([("field".to_owned(), field(type_ref))]),
                    },
                )
            })
            .collect(),
    };

    let baseline = contract(&[("Detached", "String!")]);
    let extra = contract(&[("Detached", "String!"), ("OtherDetached", "String!")]);
    assert!(
        !baseline.is_assignable_from(&extra),
        "an extra unreachable named object still changes the closed catalog"
    );
    assert!(
        !extra.is_assignable_from(&baseline),
        "a missing unreachable named object still changes the closed catalog"
    );

    let different_field_type = contract(&[("Detached", "Int!")]);
    assert!(
        !baseline.is_assignable_from(&different_field_type),
        "unreachable named-object fields remain part of exact assignment"
    );
}

#[test]
fn inline_bytes_have_one_inert_owner() {
    let bytes = BoundedInlineBytes::try_new(
        vec![0, 1, 2],
        "application/octet-stream",
        Some("../reports/final.csv"),
        3,
    )
    .expect("the file name is inert data, not a path capability");
    assert_eq!(bytes.as_slice(), &[0, 1, 2]);
    assert_eq!(bytes.media_type(), "application/octet-stream");
    assert_eq!(bytes.file_name(), Some("../reports/final.csv"));

    assert!(
        BoundedInlineBytes::try_new(vec![], &"a".repeat(255), None, 0).is_ok(),
        "255 ASCII media-type bytes are accepted"
    );
    assert!(
        BoundedInlineBytes::try_new(vec![], &"a".repeat(256), None, 0).is_err(),
        "256 ASCII media-type bytes are rejected"
    );
    assert!(
        BoundedInlineBytes::try_new(vec![], "text/é", None, 0).is_err(),
        "media types are ASCII-only"
    );
    assert!(
        BoundedInlineBytes::try_new(vec![], "text/plain", Some(&"é".repeat(127)), 0).is_ok(),
        "254 UTF-8 file-name bytes are accepted"
    );
    assert!(
        BoundedInlineBytes::try_new(
            vec![],
            "text/plain",
            Some(&format!("{}a", "é".repeat(127))),
            0,
        )
        .is_ok(),
        "255 UTF-8 file-name bytes are accepted"
    );
    assert!(
        BoundedInlineBytes::try_new(vec![], "text/plain", Some(&"é".repeat(128)), 0).is_err(),
        "256 UTF-8 file-name bytes are rejected"
    );
}

fn oracle_base64url(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(4 * (input.len() / 3) + [0, 2, 3][input.len() % 3]);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        output.push(ALPHABET[(first >> 2) as usize] as char);
        if chunk.len() == 1 {
            output.push(ALPHABET[((first & 0x03) << 4) as usize] as char);
            continue;
        }
        let second = chunk[1];
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() == 2 {
            output.push(ALPHABET[((second & 0x0f) << 2) as usize] as char);
            continue;
        }
        let third = chunk[2];
        output.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        output.push(ALPHABET[(third & 0x3f) as usize] as char);
    }
    output
}

fn oracle_jcs_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\u{000c}' => output.push_str("\\f"),
            '\r' => output.push_str("\\r"),
            '\u{0000}'..='\u{001f}' => {
                let code = character as u32;
                output.push_str("\\u00");
                output.push(char::from_digit((code >> 4) & 0x0f, 16).unwrap());
                output.push(char::from_digit(code & 0x0f, 16).unwrap());
            }
            other => output.push(other),
        }
    }
    output.push('"');
    output
}

fn oracle_binary_json(bytes: &[u8], media_type: &str, file_name: Option<&str>) -> String {
    let encoded = oracle_base64url(bytes);
    match file_name {
        Some(file_name) => format!(
            "{{\"$binary\":{},\"file_name\":{},\"media_type\":{}}}",
            oracle_jcs_string(&encoded),
            oracle_jcs_string(file_name),
            oracle_jcs_string(media_type),
        ),
        None => format!(
            "{{\"$binary\":{},\"media_type\":{}}}",
            oracle_jcs_string(&encoded),
            oracle_jcs_string(media_type),
        ),
    }
}

#[test]
fn inline_binary_canonical_size_vectors_are_exact() {
    let decoded = vec![0; 131_072];
    let binary =
        BoundedInlineBytes::try_new(decoded.clone(), "application/octet-stream", None, 131_072)
            .expect("the exact decoded ceiling is accepted");
    let canonical = oracle_binary_json(&decoded, "application/octet-stream", None);

    assert_eq!(canonical.len(), 174_817);
    assert!(canonical.starts_with("{\"$binary\":\""));
    assert!(canonical.ends_with("\",\"media_type\":\"application/octet-stream\"}"));
    let encoded = canonical
        .strip_prefix("{\"$binary\":\"")
        .and_then(|value| value.strip_suffix("\",\"media_type\":\"application/octet-stream\"}"))
        .expect("oracle uses the normative member order");
    assert_eq!(encoded.len(), 174_763);
    assert!(encoded.bytes().all(|byte| byte == b'A'));
    assert!(!encoded.contains('='));
    assert_eq!(
        canonical_size(&TypedValue::InlineBytes(binary)),
        Ok(174_817)
    );

    let root_binary =
        BoundedInlineBytes::try_new(vec![0; 131_072], "application/octet-stream", None, 131_072)
            .unwrap();
    let accepted = object([
        ("binary", TypedValue::InlineBytes(root_binary.clone())),
        ("padding", TypedValue::String("a".repeat(87_303))),
    ]);
    let accepted_oracle = format!(
        "{{\"binary\":{},\"padding\":{}}}",
        canonical,
        oracle_jcs_string(&"a".repeat(87_303)),
    );
    assert_eq!(accepted_oracle.len(), 262_144);
    assert_eq!(canonical_size(&accepted), Ok(262_144));

    let rejected = object([
        ("binary", TypedValue::InlineBytes(root_binary)),
        ("padding", TypedValue::String("a".repeat(87_304))),
    ]);
    let rejected_oracle = format!(
        "{{\"binary\":{},\"padding\":{}}}",
        canonical,
        oracle_jcs_string(&"a".repeat(87_304)),
    );
    assert_eq!(rejected_oracle.len(), 262_145);
    assert!(canonical_size(&rejected).is_err());

    let escaped =
        BoundedInlineBytes::try_new(vec![0xff], "text/\"plain\\", Some("line\n\u{0001}.txt"), 1)
            .unwrap();
    let escaped_oracle = oracle_binary_json(&[0xff], "text/\"plain\\", Some("line\n\u{0001}.txt"));
    assert_eq!(
        canonical_size(&TypedValue::InlineBytes(escaped)),
        Ok(escaped_oracle.len())
    );
    assert_eq!(
        escaped_oracle,
        "{\"$binary\":\"_w\",\"file_name\":\"line\\n\\u0001.txt\",\"media_type\":\"text/\\\"plain\\\\\"}"
    );
}

#[test]
fn inline_binary_count_and_decoded_bounds_are_exact() {
    assert!(
        BoundedInlineBytes::try_new(vec![0; 131_073], "application/octet-stream", None, 131_072,)
            .is_err(),
        "131,073 decoded bytes are rejected before sizing"
    );
    assert!(
        BoundedInlineBytes::try_new(vec![], "application/octet-stream", None, 131_073).is_err(),
        "a constructor cannot declare a bound above the engine ceiling"
    );

    let sixteen = TypedValue::List(
        (0..16)
            .map(|_| {
                TypedValue::InlineBytes(
                    BoundedInlineBytes::try_new(
                        vec![0; 8_192],
                        "application/octet-stream",
                        None,
                        8_192,
                    )
                    .unwrap(),
                )
            })
            .collect(),
    );
    canonical_size(&sixteen).expect("16 values and 131,072 aggregate bytes are accepted");

    let seventeen = TypedValue::List(
        (0..17)
            .map(|_| {
                TypedValue::InlineBytes(
                    BoundedInlineBytes::try_new(vec![], "application/octet-stream", None, 0)
                        .unwrap(),
                )
            })
            .collect(),
    );
    assert!(canonical_size(&seventeen).is_err());

    let aggregate_over = TypedValue::List(vec![
        TypedValue::InlineBytes(
            BoundedInlineBytes::try_new(vec![0; 65_536], "application/octet-stream", None, 65_536)
                .unwrap(),
        ),
        TypedValue::InlineBytes(
            BoundedInlineBytes::try_new(vec![0; 65_537], "application/octet-stream", None, 65_537)
                .unwrap(),
        ),
    ]);
    assert!(canonical_size(&aggregate_over).is_err());
}

#[test]
fn inline_binary_external_adapters_remain_disabled() {
    let bytes = TypedValue::InlineBytes(
        BoundedInlineBytes::try_new(vec![1], "application/octet-stream", None, 1).unwrap(),
    );
    let json_contract = catalog([("value", "json!")]);
    assert!(
        json_contract.validate(&object([("value", bytes)])).is_err(),
        "inert bytes are not a JSON-shaped value"
    );
}
