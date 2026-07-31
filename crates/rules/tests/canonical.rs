use std::collections::BTreeMap;

use donat_rules::{
    CanonicalRoot, CanonicalValue, DecisionRow, DecisionTableDefinition, HitPolicy, RuleDefinition,
    RuleType, canonical_bytes, compile_catalog, compile_catalog_with_declared_types,
};
use serde_json::{Map, Value, json};

const MAGIC_AND_VERSION: &str = "444f4e41542d52554c45532d43414e4f4e4943414c000001";

fn map<T>(entries: impl IntoIterator<Item = (&'static str, T)>) -> BTreeMap<String, T> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn int64_type() -> RuleType {
    serde_json::from_value(serde_json::json!("Int64"))
        .expect("the closed bigint rule type must deserialize")
}

fn rule(
    name: &str,
    bindings: BTreeMap<String, RuleType>,
    result: RuleType,
    expression: &str,
) -> RuleDefinition {
    RuleDefinition {
        name: name.to_owned(),
        bindings,
        result,
        expression: expression.to_owned(),
    }
}

fn decision_row(id: &str, when: BTreeMap<String, &str>, output: Value) -> DecisionRow {
    DecisionRow {
        id: id.to_owned(),
        description: Some(format!("operator description for {id}")),
        when: when
            .into_iter()
            .map(|(input, expression)| (input.to_owned(), expression.to_owned()))
            .collect(),
        output,
    }
}

fn approval_table(
    inputs: BTreeMap<String, RuleType>,
    output: BTreeMap<String, RuleType>,
    rows: Vec<DecisionRow>,
) -> DecisionTableDefinition {
    DecisionTableDefinition {
        name: "approval".to_owned(),
        // This legacy source field must not affect the derived revision.
        revision: "caller-controlled-name-is-not-a-revision".to_owned(),
        inputs,
        output,
        hit_policy: HitPolicy::First,
        rows,
        test_cases: Vec::new(),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn canonical_rule_bytes(definition: RuleDefinition) -> (Vec<u8>, donat_rules::CompiledRule) {
    let name = definition.name.clone();
    let catalog = compile_catalog(&[definition], &[]).expect("canonical fixture compiles");
    let compiled = catalog
        .rule(&name)
        .expect("fixture rule is retained")
        .clone();
    let bytes = canonical_bytes(
        CanonicalRoot::TypedRuleAst,
        &CanonicalValue::TypedRule(compiled.clone()),
    );
    (bytes, compiled)
}

#[test]
fn canonical_literal_true_rule_vector() {
    let (bytes, compiled) =
        canonical_rule_bytes(rule("literal", BTreeMap::new(), RuleType::Bool, "true"));

    assert_eq!(
        hex(&bytes),
        "444f4e41542d52554c45532d43414e4f4e4943414c00000101000000076c69746572616c000000001020010110"
    );
    assert_eq!(
        compiled.artifact.canonical_ast_sha256,
        "acfb7be4c3c31baa5f72433eabedc47d721ad2f72d7d70aa73a7f368b0445d34"
    );
}

#[test]
fn canonical_nullable_index_rule_vector() {
    let (bytes, compiled) = canonical_rule_bytes(rule(
        "nullable_index",
        map([("items", RuleType::List(Box::new(RuleType::String)))]),
        RuleType::Bool,
        "is_null(items[0])",
    ));

    assert_eq!(
        hex(&bytes),
        "444f4e41542d52554c45532d43414e4f4e4943414c000001010000000e6e756c6c61626c655f696e64657800000001000000056974656d731811102601000000012521000000056974656d731811000000001a1110"
    );
    assert_eq!(
        compiled.artifact.canonical_ast_sha256,
        "113eae1a24d9dcc45ec7e3f26b07cab1ac0b5e09f44725c980ccf54a2571fe75"
    );
}

#[test]
fn canonical_size_function_rule_vector() {
    let (bytes, compiled) = canonical_rule_bytes(rule(
        "size_items",
        map([("items", RuleType::List(Box::new(RuleType::String)))]),
        RuleType::Int,
        "size(items)",
    ));

    assert_eq!(
        hex(&bytes),
        "444f4e41542d52554c45532d43414e4f4e4943414c000001010000000a73697a655f6974656d7300000001000000056974656d7318111226000000000121000000056974656d73181112"
    );
    assert_eq!(
        compiled.artifact.canonical_ast_sha256,
        "c9419c2368337338f014257d0ff3d286240a1373e073c46e3d6d096aca9f01cd"
    );
}

#[test]
fn canonical_decision_declaration_vector() {
    let table = approval_table(
        map([("amount", RuleType::Int)]),
        map([("route", RuleType::String)]),
        vec![decision_row(
            "default",
            map([("amount", "true")]),
            json!({"route": "manual"}),
        )],
    );
    let catalog = compile_catalog(&[], &[table]).expect("canonical decision compiles");
    let compiled = catalog
        .decision_table("approval")
        .expect("decision definition is retained")
        .clone();
    let bytes = canonical_bytes(
        CanonicalRoot::DecisionDefinition,
        &CanonicalValue::DecisionDefinition(compiled.clone()),
    );

    assert_eq!(
        hex(&bytes),
        "444f4e41542d52554c45532d43414e4f4e4943414c0000010200000008617070726f76616c000000000000000100000006616d6f756e74120000000100000005726f7574651100000000010000000764656661756c740000000100000006616d6f756e74200101100000000100000005726f7574651132000000066d616e75616c00000000"
    );
    assert_eq!(
        compiled.revision.0,
        "4b43abdf0d8c689127993ba14614011cb5399e895a43f990c96166e8afa4ee32"
    );
}

#[test]
fn canonical_decoded_input_vector_and_map_order_are_typed() {
    let types = map([("amount", RuleType::Int), ("enabled", RuleType::Bool)]);
    let forward = map([("amount", json!(42)), ("enabled", json!(true))]);
    let reverse = map([("enabled", json!(true)), ("amount", json!(42))]);
    let forward = canonical_bytes(
        CanonicalRoot::DecodedTypedInput,
        &CanonicalValue::DecodedTypedInput {
            types: types.clone(),
            bindings: forward,
        },
    );
    let reverse = canonical_bytes(
        CanonicalRoot::DecodedTypedInput,
        &CanonicalValue::DecodedTypedInput {
            types,
            bindings: reverse,
        },
    );

    assert_eq!(
        hex(&forward),
        format!(
            "{MAGIC_AND_VERSION}030000000200000006616d6f756e74123300000002343200000007656e61626c6564103101"
        )
    );
    assert_eq!(forward, reverse, "decoded binding maps sort by UTF-8 bytes");

    let decimal_one = canonical_bytes(
        CanonicalRoot::DecodedTypedInput,
        &CanonicalValue::DecodedTypedInput {
            types: map([("amount", RuleType::Decimal)]),
            bindings: map([(
                "amount",
                serde_json::from_str("1.00").expect("valid JSON decimal"),
            )]),
        },
    );
    let decimal_two = canonical_bytes(
        CanonicalRoot::DecodedTypedInput,
        &CanonicalValue::DecodedTypedInput {
            types: map([("amount", RuleType::Decimal)]),
            bindings: map([(
                "amount",
                serde_json::from_str("1.0").expect("valid JSON decimal"),
            )]),
        },
    );
    assert_eq!(
        decimal_one, decimal_two,
        "fractional trailing zeroes must not alter the decimal canonical form"
    );
}

#[test]
fn canonical_maps_ignore_insertion_order_and_descriptions_and_expression_spans() {
    let object_forward = RuleType::Object {
        name: "Customer".to_owned(),
        fields: map([("name", RuleType::String), ("tier", RuleType::Int)]),
    };
    let object_reverse = RuleType::Object {
        name: "Customer".to_owned(),
        fields: map([("tier", RuleType::Int), ("name", RuleType::String)]),
    };
    let rule_forward = compile_catalog(
        &[rule(
            "same_ast",
            map([("customer", object_forward)]),
            RuleType::Bool,
            "true",
        )],
        &[],
    )
    .expect("forward map compiles");
    let rule_reverse = compile_catalog(
        &[rule(
            "same_ast",
            map([("customer", object_reverse)]),
            RuleType::Bool,
            "  true  ",
        )],
        &[],
    )
    .expect("reverse map compiles");
    assert_eq!(
        rule_forward
            .rule("same_ast")
            .expect("rule")
            .artifact
            .canonical_ast_sha256,
        rule_reverse
            .rule("same_ast")
            .expect("rule")
            .artifact
            .canonical_ast_sha256,
        "binding/object map order and parser spans cannot alter canonical bytes"
    );

    let mut output_forward = Map::new();
    output_forward.insert("route".to_owned(), json!("manual"));
    output_forward.insert("tier".to_owned(), json!(1));
    let mut output_reverse = Map::new();
    output_reverse.insert("tier".to_owned(), json!(1));
    output_reverse.insert("route".to_owned(), json!("manual"));
    let forward = approval_table(
        map([("amount", RuleType::Int), ("limit", RuleType::Int)]),
        map([("route", RuleType::String), ("tier", RuleType::Int)]),
        vec![decision_row(
            "default",
            map([("amount", "true"), ("limit", "true")]),
            Value::Object(output_forward),
        )],
    );
    let mut reverse = forward.clone();
    reverse.rows[0].description = Some("a different non-executable description".to_owned());
    reverse.inputs = map([("limit", RuleType::Int), ("amount", RuleType::Int)]);
    reverse.output = map([("tier", RuleType::Int), ("route", RuleType::String)]);
    reverse.rows[0].when = map([("limit", "true".to_owned()), ("amount", "true".to_owned())]);
    reverse.rows[0].output = Value::Object(output_reverse);
    let forward = compile_catalog(&[], &[forward]).expect("forward decision compiles");
    let reverse = compile_catalog(&[], &[reverse]).expect("reverse decision compiles");
    assert_eq!(
        forward
            .decision_table("approval")
            .expect("decision")
            .revision,
        reverse
            .decision_table("approval")
            .expect("decision")
            .revision,
        "inputs, outputs, row conditions, and row output maps must sort while descriptions do not encode"
    );
}

#[test]
fn canonical_semantic_changes_change_the_affected_definition_or_input_digest() {
    let mut base = approval_table(
        map([("amount", RuleType::Int)]),
        map([("route", RuleType::String)]),
        vec![
            decision_row(
                "manual",
                map([("amount", "amount > 100")]),
                json!({"route": "manual"}),
            ),
            decision_row(
                "default",
                map([("amount", "true")]),
                json!({"route": "automatic"}),
            ),
        ],
    );
    base.hit_policy = HitPolicy::Unique;
    let base_catalog = compile_catalog(&[], &[base.clone()]).expect("base decision compiles");
    let base_revision = base_catalog
        .decision_table("approval")
        .expect("decision")
        .revision
        .clone();

    let mut condition_changed = base.clone();
    condition_changed.rows[0].when = map([("amount", "amount > 200".to_owned())]);
    let mut output_changed = base.clone();
    output_changed.rows[0].output = json!({"route": "review"});
    let mut row_order_changed = base.clone();
    row_order_changed.rows.swap(0, 1);

    for changed in [condition_changed, output_changed, row_order_changed] {
        let catalog = compile_catalog(&[], &[changed]).expect("changed decision remains valid");
        assert_ne!(
            base_revision,
            catalog
                .decision_table("approval")
                .expect("changed decision")
                .revision,
            "a semantic decision change requires a new immutable revision"
        );
    }

    let mut trace_changed = base.clone();
    trace_changed.rows[0].output = json!({"route": "review"});
    let trace_changed = compile_catalog(&[], &[trace_changed]).expect("changed decision compiles");
    let input = map([("amount", json!(1))]);
    let base_trace = base_catalog
        .evaluate_decision("approval", &input)
        .expect("base decision evaluates")
        .trace;
    let changed_trace = trace_changed
        .evaluate_decision("approval", &input)
        .expect("same-name changed decision evaluates")
        .trace;
    assert_eq!(base_trace.input_digest, changed_trace.input_digest);
    assert_ne!(base_trace, changed_trace);

    let state_v1 = RuleType::Enum {
        name: "State".to_owned(),
        symbols: vec!["draft".to_owned()],
    };
    let state_v2 = RuleType::Enum {
        name: "State".to_owned(),
        symbols: vec!["draft".to_owned(), "published".to_owned()],
    };
    let typed_table = |state: RuleType| {
        approval_table(
            map([("state", state)]),
            map([("route", RuleType::String)]),
            vec![decision_row(
                "default",
                map([("state", "true")]),
                json!({"route": "manual"}),
            )],
        )
    };
    let declaration_v1 = compile_catalog_with_declared_types(
        &map([("State", state_v1.clone())]),
        &[],
        &[typed_table(state_v1)],
    )
    .expect("first declared type compiles");
    let declaration_v2 = compile_catalog_with_declared_types(
        &map([("State", state_v2.clone())]),
        &[],
        &[typed_table(state_v2)],
    )
    .expect("changed declared type compiles");
    assert_ne!(
        declaration_v1
            .decision_table("approval")
            .expect("decision")
            .revision,
        declaration_v2
            .decision_table("approval")
            .expect("decision")
            .revision,
        "a resolved type declaration change requires a new immutable revision"
    );

    let enum_a = map([(
        "State",
        RuleType::Enum {
            name: "State".to_owned(),
            symbols: vec!["draft".to_owned()],
        },
    )]);
    let enum_b = map([(
        "OtherState",
        RuleType::Enum {
            name: "OtherState".to_owned(),
            symbols: vec!["draft".to_owned()],
        },
    )]);
    let rule_a = compile_catalog_with_declared_types(
        &enum_a,
        &[rule(
            "enum_identity",
            BTreeMap::new(),
            RuleType::Bool,
            "State::draft == State::draft",
        )],
        &[],
    )
    .expect("first nominal enum compiles");
    let rule_b = compile_catalog_with_declared_types(
        &enum_b,
        &[rule(
            "enum_identity",
            BTreeMap::new(),
            RuleType::Bool,
            "OtherState::draft == OtherState::draft",
        )],
        &[],
    )
    .expect("second nominal enum compiles");
    assert_ne!(
        rule_a
            .rule("enum_identity")
            .expect("rule")
            .artifact
            .canonical_ast_sha256,
        rule_b
            .rule("enum_identity")
            .expect("rule")
            .artifact
            .canonical_ast_sha256,
        "overlapping symbols remain nominally distinct by enum declaration name"
    );

    let one = canonical_bytes(
        CanonicalRoot::DecodedTypedInput,
        &CanonicalValue::DecodedTypedInput {
            types: map([("amount", RuleType::Int)]),
            bindings: map([("amount", json!(1))]),
        },
    );
    let two = canonical_bytes(
        CanonicalRoot::DecodedTypedInput,
        &CanonicalValue::DecodedTypedInput {
            types: map([("amount", RuleType::Int)]),
            bindings: map([("amount", json!(2))]),
        },
    );
    assert_ne!(
        one, two,
        "a typed input value changes its canonical digest bytes"
    );
}

#[test]
fn canonical_rule_types_distinguish_int_from_bigint() {
    let (int_bytes, _) = canonical_rule_bytes(rule(
        "identity",
        map([("value", RuleType::Int)]),
        RuleType::Int,
        "value",
    ));
    let (bigint_bytes, _) = canonical_rule_bytes(rule(
        "identity",
        map([("value", int64_type())]),
        int64_type(),
        "value",
    ));

    assert_ne!(
        int_bytes, bigint_bytes,
        "width is part of the immutable Rule fingerprint"
    );
}

#[test]
fn compiled_profiles_and_traces_retain_hashes_without_raw_input() {
    let source = "true";
    let rule_catalog = compile_catalog(
        &[rule("profiled", BTreeMap::new(), RuleType::Bool, source)],
        &[],
    )
    .expect("profiled rule compiles");
    let profiled = rule_catalog.rule("profiled").expect("compiled rule");
    assert_eq!(profiled.artifact.profile_version, 1);
    assert_eq!(profiled.artifact.original_source, source);
    assert_eq!(
        profiled.artifact.source_sha256,
        "b5bea41b6c623f7c09f1bf24dcae58ebab3c0cdd90ad966bc43a45b44867e12b"
    );
    assert!(
        profiled
            .artifact
            .canonical_ast_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
    );

    let table = approval_table(
        map([("amount", RuleType::Int), ("secret", RuleType::String)]),
        map([("route", RuleType::String)]),
        vec![decision_row(
            "default",
            map([("amount", "true"), ("secret", "true")]),
            json!({"route": "manual"}),
        )],
    );
    let catalog = compile_catalog(&[], &[table]).expect("decision compiles");
    let result = catalog
        .evaluate_decision(
            "approval",
            &map([("amount", json!(42)), ("secret", json!("never-in-a-trace"))]),
        )
        .expect("decision evaluates");
    assert_eq!(result.trace.table_revision.len(), 64);
    assert_eq!(result.trace.input_digest.len(), 64);
    assert!(result.trace.input_digest.bytes().all(|byte| {
        byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
    }));
    assert!(!format!("{:#?}", result.trace).contains("never-in-a-trace"));
}

#[test]
fn canonical_trace_keeps_total_object_access_nulls_grammar_valid() {
    let customer = RuleType::Object {
        name: "Customer".to_owned(),
        fields: map([("tier", RuleType::String)]),
    };
    let table = approval_table(
        map([("customer", customer)]),
        map([("route", RuleType::String)]),
        vec![decision_row(
            "default",
            map([("customer", "true")]),
            json!({"route": "manual"}),
        )],
    );
    let catalog = compile_catalog(&[], &[table]).expect("decision compiles");
    let result = catalog
        .evaluate_decision("approval", &map([("customer", json!({}))]))
        .expect("missing object members remain total when tracing");

    assert_eq!(result.trace.input_digest.len(), 64);
}
