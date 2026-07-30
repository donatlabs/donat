use std::collections::BTreeMap;

use donat_rules::{
    DecisionRow, DecisionTableDefinition, DecisionTableTestCase, DecisionTestExpectation,
    EvaluatedRuleValue, ExpressionContext, ExpressionOwner, HitPolicy, RuleDefinition, RuleError,
    RuleType, Span, compile_catalog, compile_catalog_with_declared_types,
    compile_catalog_with_declared_types_and_contexts, evaluate_bool, evaluate_value,
};
use serde_json::{Value, json};

fn map<T>(entries: impl IntoIterator<Item = (&'static str, T)>) -> BTreeMap<String, T> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
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

fn rule_context(index: usize, name: &str) -> ExpressionContext {
    ExpressionContext {
        metadata_path: format!("rules.yaml.rules[{index}].expression"),
        expression_owner: ExpressionOwner::Rule {
            name: name.to_owned(),
        },
    }
}

fn compile_rule_in_context(
    index: usize,
    definition: RuleDefinition,
    declared_types: BTreeMap<String, RuleType>,
) -> Result<donat_rules::RuleCatalog, RuleError> {
    let context = rule_context(index, &definition.name);
    compile_catalog_with_declared_types_and_contexts(
        &declared_types,
        &[definition],
        &[],
        &[context],
        &[],
    )
}

fn object(name: &str, fields: BTreeMap<String, RuleType>) -> RuleType {
    RuleType::Object {
        name: name.to_owned(),
        fields,
    }
}

fn decision_row(id: &str, when: BTreeMap<String, &str>, output: Value) -> DecisionRow {
    DecisionRow {
        id: id.to_owned(),
        description: Some(format!("decision row {id}")),
        when: when
            .into_iter()
            .map(|(input, source)| (input, source.to_owned()))
            .collect(),
        output,
    }
}

fn approval_table(hit_policy: HitPolicy) -> DecisionTableDefinition {
    DecisionTableDefinition {
        name: "invoice_approval".to_owned(),
        revision: "rules-2026-07-28".to_owned(),
        inputs: map([("amount", RuleType::Int)]),
        output: map([("route", RuleType::String)]),
        hit_policy,
        rows: vec![
            decision_row(
                "manual_review",
                map([("amount", "amount > 100")]),
                json!({"route": "manual"}),
            ),
            decision_row(
                "default",
                map([("amount", "true")]),
                json!({"route": "automatic"}),
            ),
        ],
        test_cases: vec![DecisionTableTestCase {
            name: "lower-value invoice".to_owned(),
            input: json!({"amount": 100}),
            expect: DecisionTestExpectation {
                output: json!({"route": "automatic"}),
                matched_row_id: "default".to_owned(),
            },
        }],
    }
}

fn evaluate(
    definition: RuleDefinition,
    bindings: BTreeMap<String, Value>,
) -> Result<bool, donat_rules::RuleError> {
    let catalog = compile_catalog(&[definition], &[])?;
    let compiled = catalog.rule("policy").expect("compiled rule should exist");
    catalog.evaluate_bool(compiled, &bindings)
}

#[test]
fn evaluates_exact_scalars_and_rejects_missing_bindings_or_unknown_object_fields() {
    let definition = rule(
        "policy",
        map([
            ("active", RuleType::Bool),
            (
                "customer",
                object("Customer", map([("name", RuleType::String)])),
            ),
        ]),
        RuleType::Bool,
        "active && is_null(customer.name) == false",
    );

    assert!(
        evaluate(
            definition.clone(),
            map([
                ("active", json!(true)),
                ("customer", json!({"name": "Ada"}))
            ]),
        )
        .expect("exactly typed scalar bindings should evaluate")
    );

    let missing = evaluate(
        definition.clone(),
        map([("customer", json!({"name": "Ada"}))]),
    )
    .expect_err("a declared binding must be present");
    assert!(
        matches!(missing, donat_rules::RuleError::MissingBinding { ref name } if name == "active")
    );

    let catalog = compile_catalog(
        &[rule(
            "policy",
            map([(
                "customer",
                object("Customer", map([("name", RuleType::String)])),
            )]),
            RuleType::Bool,
            "customer.email == 'ada@example.test'",
        )],
        &[],
    )
    .expect_err("an object field absent from the declared shape must fail validation");
    assert!(
        matches!(catalog, donat_rules::RuleError::UnknownField { ref field, .. } if field == "email")
    );
}

#[test]
fn evaluate_value_returns_typed_values() {
    let status = RuleType::Enum {
        name: "OrderStatus".to_owned(),
        symbols: vec!["draft".to_owned(), "submitted".to_owned()],
    };
    let customer = object("Customer", map([("nickname", RuleType::String)]));

    let cases = [
        (
            compile_catalog(
                &[rule(
                    "string_value",
                    map([("name", RuleType::String)]),
                    RuleType::String,
                    "name",
                )],
                &[],
            )
            .expect("a string value rule should compile"),
            "string_value",
            map([("name", json!("Ada"))]),
            RuleType::String,
            json!("Ada"),
        ),
        (
            compile_catalog(
                &[rule(
                    "decimal_value",
                    map([("price", RuleType::Decimal)]),
                    RuleType::Decimal,
                    "price / 2.0",
                )],
                &[],
            )
            .expect("a decimal value rule should compile"),
            "decimal_value",
            map([("price", json!(2.4))]),
            RuleType::Decimal,
            json!(1.2),
        ),
        (
            compile_catalog_with_declared_types(
                &map([("OrderStatus", status.clone())]),
                &[rule(
                    "enum_value",
                    BTreeMap::new(),
                    status.clone(),
                    "OrderStatus::draft",
                )],
                &[],
            )
            .expect("an enum value rule should compile"),
            "enum_value",
            BTreeMap::new(),
            status.clone(),
            json!("draft"),
        ),
        (
            compile_catalog(
                &[rule(
                    "missing_member",
                    map([("customer", customer.clone())]),
                    RuleType::nullable(RuleType::String),
                    "customer.nickname",
                )],
                &[],
            )
            .expect("a nullable member value rule should compile"),
            "missing_member",
            map([("customer", json!({}))]),
            RuleType::nullable(RuleType::String),
            Value::Null,
        ),
        (
            compile_catalog(
                &[rule(
                    "missing_item",
                    map([("lines", RuleType::List(Box::new(RuleType::Int)))]),
                    RuleType::nullable(RuleType::Int),
                    "lines[3]",
                )],
                &[],
            )
            .expect("a nullable list item value rule should compile"),
            "missing_item",
            map([("lines", json!([1]))]),
            RuleType::nullable(RuleType::Int),
            Value::Null,
        ),
    ];

    for (catalog, name, bindings, type_, value) in cases {
        let rule = catalog.rule(name).expect("the compiled rule should exist");
        assert_eq!(
            evaluate_value(rule, &bindings).expect("a value rule should evaluate"),
            EvaluatedRuleValue { type_, value },
            "typed value mismatch for {name}",
        );
    }
}

#[test]
fn evaluate_bool_rejects_non_bool_rule() {
    let status = RuleType::Enum {
        name: "OrderStatus".to_owned(),
        symbols: vec!["draft".to_owned()],
    };
    let customer = object("Customer", map([("nickname", RuleType::String)]));
    let cases = [
        (
            compile_catalog(
                &[rule(
                    "string_value",
                    map([("name", RuleType::String)]),
                    RuleType::String,
                    "name",
                )],
                &[],
            )
            .expect("a string value rule should compile"),
            "string_value",
            map([("name", json!("Ada"))]),
            "string",
        ),
        (
            compile_catalog_with_declared_types(
                &map([("OrderStatus", status.clone())]),
                &[rule(
                    "enum_value",
                    BTreeMap::new(),
                    status,
                    "OrderStatus::draft",
                )],
                &[],
            )
            .expect("an enum value rule should compile"),
            "enum_value",
            BTreeMap::new(),
            "enum",
        ),
        (
            compile_catalog(
                &[rule(
                    "list_value",
                    BTreeMap::new(),
                    RuleType::List(Box::new(RuleType::Int)),
                    "[1]",
                )],
                &[],
            )
            .expect("a list value rule should compile"),
            "list_value",
            BTreeMap::new(),
            "list",
        ),
        (
            compile_catalog(
                &[rule(
                    "object_value",
                    map([("customer", customer.clone())]),
                    customer,
                    "customer",
                )],
                &[],
            )
            .expect("an object value rule should compile"),
            "object_value",
            map([("customer", json!({"nickname": "Ada"}))]),
            "object",
        ),
        (
            compile_catalog(
                &[rule(
                    "null_value",
                    BTreeMap::new(),
                    RuleType::nullable(RuleType::String),
                    "null",
                )],
                &[],
            )
            .expect("a nullable null value rule should compile"),
            "null_value",
            BTreeMap::new(),
            "null",
        ),
    ];

    for (catalog, name, bindings, actual) in cases {
        let rule = catalog
            .rule(name)
            .expect("the compiled value rule should exist");
        let error = evaluate_bool(rule, &bindings)
            .expect_err("boolean evaluation must reject a non-boolean rule");

        assert!(matches!(
            error,
            RuleError::InvalidRuleResult { rule, expected, actual: error_actual }
                if rule == name && expected == "bool" && error_actual == actual
        ));
    }
}

#[test]
fn compares_extreme_scale_decimals_by_value() {
    let tiny = format!("0.{}1", "0".repeat(3999));
    let larger = format!("0.{}2", "0".repeat(1799));
    let smaller = format!("0.{}9", "0".repeat(1899));
    let cases = [
        (
            "tiny positive is greater than zero",
            format!("{tiny} * 2.0 > 0.0"),
            true,
        ),
        (
            "zero is less than a tiny positive",
            format!("0.0 < {tiny} * 2.0"),
            true,
        ),
        (
            "tiny negative is less than zero",
            format!("-{tiny} * 2.0 < 0.0"),
            true,
        ),
        (
            "zero is greater than a tiny negative",
            format!("0.0 > -{tiny} * 2.0"),
            true,
        ),
        (
            "equivalent values with different source scales compare equal",
            "1.0 <= 1.00 && 1.0 >= 1.00".to_owned(),
            true,
        ),
        (
            "equivalent values with different source scales are not ordered",
            "1.0 < 1.00".to_owned(),
            false,
        ),
        (
            "large unequal scales compare by decimal position",
            format!("{larger} > {smaller}"),
            true,
        ),
        (
            "large unequal scales preserve reverse ordering",
            format!("{larger} < {smaller}"),
            false,
        ),
        (
            "large unequal negative scales reverse magnitude ordering",
            format!("-{larger} < -{smaller}"),
            true,
        ),
    ];

    for (case, source, expected) in cases {
        assert!(
            source.len() <= donat_rules::MAX_EXPRESSION_BYTES,
            "{case} must remain inside the profile source limit",
        );
        let definition = rule("policy", BTreeMap::new(), RuleType::Bool, source.as_str());
        let actual = evaluate(definition, BTreeMap::new())
            .expect("a bounded exact decimal comparison should evaluate");

        assert_eq!(actual, expected, "decimal ordering mismatch for {case}");
    }
}

#[test]
fn semantic_rule_errors_preserve_the_originating_source_context_and_span() {
    let cases = [
        (
            0,
            rule("undeclared", BTreeMap::new(), RuleType::Bool, "missing > 0"),
            BTreeMap::new(),
            Span::new(0, 7),
            "rule `undeclared` uses undeclared binding `missing`",
        ),
        (
            1,
            rule(
                "unknown_field",
                map([(
                    "customer",
                    object("Customer", map([("name", RuleType::String)])),
                )]),
                RuleType::Bool,
                "customer.email == 'x'",
            ),
            BTreeMap::new(),
            Span::new(0, 14),
            "rule `unknown_field` cannot access undeclared field `email`",
        ),
        (
            2,
            rule(
                "nullable",
                map([("limit", RuleType::nullable(RuleType::Int))]),
                RuleType::Bool,
                "limit > 0",
            ),
            BTreeMap::new(),
            Span::new(0, 9),
            "rule `nullable` applies an operation to a nullable value",
        ),
        (
            3,
            rule(
                "branches",
                BTreeMap::new(),
                RuleType::String,
                "true ? 1 : 'no'",
            ),
            BTreeMap::new(),
            Span::new(0, 15),
            "rule `branches` has an incompatible conditional branch",
        ),
        (
            4,
            rule(
                "not_an_enum",
                BTreeMap::new(),
                RuleType::String,
                "NotAnEnum::symbol",
            ),
            map([("NotAnEnum", RuleType::String)]),
            Span::new(0, 17),
            "rule `not_an_enum` references undeclared enum type `NotAnEnum`",
        ),
    ];

    for (index, definition, declared_types, span, expected_message) in cases {
        let name = definition.name.clone();
        let error = compile_rule_in_context(index, definition, declared_types)
            .expect_err("each semantic validation error should retain a diagnostic");
        let diagnostic = error
            .diagnostic()
            .expect("semantic validation errors should expose their diagnostic");

        assert_eq!(diagnostic.context, rule_context(index, &name));
        assert_eq!(diagnostic.span, span);
        assert_eq!(diagnostic.message, expected_message);
    }
}

#[test]
fn semantic_decision_condition_errors_preserve_their_own_source_context_and_span() {
    let table = DecisionTableDefinition {
        name: "invoice_route".to_owned(),
        revision: "rules-2026-07-28".to_owned(),
        inputs: map([("amount", RuleType::Int)]),
        output: map([("route", RuleType::String)]),
        hit_policy: HitPolicy::First,
        rows: vec![
            decision_row(
                "invalid_amount",
                map([("amount", "amount + true")]),
                json!({"route": "manual"}),
            ),
            decision_row(
                "default",
                map([("amount", "true")]),
                json!({"route": "automatic"}),
            ),
        ],
        test_cases: Vec::new(),
    };
    let context = ExpressionContext {
        metadata_path: "rules.yaml.decision_tables[0].rows[0].when.amount".to_owned(),
        expression_owner: ExpressionOwner::DecisionCondition {
            table_name: "invoice_route".to_owned(),
            row_id: "invalid_amount".to_owned(),
            input_name: "amount".to_owned(),
        },
    };

    let error = compile_catalog_with_declared_types_and_contexts(
        &BTreeMap::new(),
        &[],
        &[table],
        &[],
        &[vec![
            map([("amount", context.clone())]),
            map([(
                "amount",
                ExpressionContext {
                    metadata_path: "rules.yaml.decision_tables[0].rows[1].when.amount".to_owned(),
                    expression_owner: ExpressionOwner::DecisionCondition {
                        table_name: "invoice_route".to_owned(),
                        row_id: "default".to_owned(),
                        input_name: "amount".to_owned(),
                    },
                },
            )]),
        ]],
    )
    .expect_err("a decision condition with incompatible operands must be rejected");
    let diagnostic = error
        .diagnostic()
        .expect("a decision condition semantic error should expose its diagnostic");

    assert_eq!(diagnostic.context, context);
    assert_eq!(diagnostic.span, Span::new(0, 13));
    assert_eq!(
        diagnostic.message,
        "rule `invoice_route` has incompatible types: expected matching int or decimal operands, got int and bool"
    );
}

#[test]
fn permits_only_explicit_null_checks_for_nullable_values() {
    let null_check = rule(
        "policy",
        map([("limit", RuleType::nullable(RuleType::Int))]),
        RuleType::Bool,
        "limit == null || limit != null",
    );
    assert!(
        evaluate(null_check, map([("limit", Value::Null)]))
            .expect("nullable equality with null should be valid")
    );

    let error = compile_catalog(
        &[rule(
            "policy",
            map([("limit", RuleType::nullable(RuleType::Int))]),
            RuleType::Bool,
            "limit > 100",
        )],
        &[],
    )
    .expect_err("ordering a nullable value must be rejected before evaluation");
    assert!(matches!(
        error,
        donat_rules::RuleError::NullableOperation { .. }
    ));
}

#[test]
fn deploy_validation_rejects_whole_value_equality_for_lists_and_objects() {
    let customer = object("Customer", map([("name", RuleType::String)]));
    let cases = [
        (
            "items",
            RuleType::List(Box::new(RuleType::Int)),
            "items == items",
        ),
        ("customer", customer, "customer == customer"),
    ];

    for (binding, type_, expression) in cases {
        let error = compile_catalog(
            &[rule(
                "policy",
                map([(binding, type_)]),
                RuleType::Bool,
                expression,
            )],
            &[],
        )
        .expect_err("the profile must not allow whole collection equality");

        assert!(
            matches!(error, RuleError::TypeMismatch { .. }),
            "whole collection equality must be rejected for {expression}: {error}",
        );
    }
}

#[test]
fn static_access_is_nullable_total_and_has_no_flow_sensitive_refinement() {
    let out_of_range = rule(
        "policy",
        map([("lines", RuleType::List(Box::new(RuleType::Int)))]),
        RuleType::Bool,
        "is_null(lines[3])",
    );
    assert!(
        evaluate(out_of_range, map([("lines", json!([1]))]))
            .expect("an out-of-range static list access must become null")
    );

    let missing_object_key = rule(
        "policy",
        map([(
            "customer",
            object("Customer", map([("nickname", RuleType::String)])),
        )]),
        RuleType::Bool,
        "is_null(customer.nickname)",
    );
    assert!(
        evaluate(missing_object_key, map([("customer", json!({}))]))
            .expect("an absent declared object key must decode as null")
    );

    assert_eq!(
        RuleType::nullable(RuleType::nullable(RuleType::String)),
        RuleType::nullable(RuleType::String),
        "nullable access results must not accumulate wrappers"
    );
    let nullable_item = rule(
        "policy",
        map([(
            "lines",
            RuleType::List(Box::new(RuleType::nullable(RuleType::String))),
        )]),
        RuleType::Bool,
        "is_null(lines[3])",
    );
    assert!(
        evaluate(nullable_item, map([("lines", json!(["present"]))]))
            .expect("an absent nullable list item must still be a single nullable result")
    );

    let error = compile_catalog(
        &[rule(
            "policy",
            map([(
                "customer",
                object("Customer", map([("nickname", RuleType::String)])),
            )]),
            RuleType::String,
            "is_null(customer.nickname) ? 'missing' : customer.nickname",
        )],
        &[],
    )
    .expect_err("checking an access for null must not refine it to a non-null value");
    assert!(matches!(error, RuleError::IncompatibleBranches { .. }));

    let error = compile_catalog(
        &[rule(
            "policy",
            map([(
                "customer",
                object("Customer", map([("nickname", RuleType::String)])),
            )]),
            RuleType::Bool,
            "customer.nickname == 'Ada'",
        )],
        &[],
    )
    .expect_err("non-null-only operations must reject nullable access results");
    assert!(matches!(error, RuleError::NullableOperation { .. }));
}

#[test]
fn rejects_literal_zero_divisors_at_deploy_time() {
    for expression in ["false && (1 / 0 > 0)", "false && (1.0 / 0.0 > 0)"] {
        let error = compile_catalog(
            &[rule("policy", BTreeMap::new(), RuleType::Bool, expression)],
            &[],
        )
        .expect_err("planner-visible literal zero divisors must be rejected at deploy time");
        assert!(matches!(error, RuleError::DivisionByZero { .. }));
    }
}

#[test]
fn evaluates_list_size_string_predicates_and_short_circuit_boolean_operators() {
    let definition = rule(
        "policy",
        map([
            ("lines", RuleType::List(Box::new(RuleType::Int))),
            ("name", RuleType::String),
            ("denominator", RuleType::Int),
        ]),
        RuleType::Bool,
        "size(lines) == 2 && startsWith(name, 'A') && endsWith(name, 'z') && (false && 1 / denominator > 0) == false && (true || 1 / denominator > 0)",
    );

    assert!(
        evaluate(
            definition,
            map([
                ("lines", json!([1, 2])),
                ("name", json!("Az")),
                ("denominator", json!(0)),
            ]),
        )
        .expect("short-circuited branches must not evaluate their division by zero")
    );
}

#[test]
fn rejects_implicit_numeric_and_uuid_string_conversions() {
    let numeric = compile_catalog(
        &[rule(
            "policy",
            map([("count", RuleType::Int), ("price", RuleType::Decimal)]),
            RuleType::Bool,
            "count == price",
        )],
        &[],
    )
    .expect_err("int and decimal must never convert implicitly");
    assert!(matches!(
        numeric,
        donat_rules::RuleError::TypeMismatch { .. }
    ));

    let uuid = compile_catalog(
        &[rule(
            "policy",
            map([("id", RuleType::Uuid)]),
            RuleType::Bool,
            "id == '01234567-89ab-cdef-0123-456789abcdef'",
        )],
        &[],
    )
    .expect_err("a UUID cannot compare to a string literal without a typed cast form");
    assert!(matches!(uuid, donat_rules::RuleError::TypeMismatch { .. }));
}

#[test]
fn enum_symbols_are_nominal_and_reject_unknown_symbols() {
    let order_status = RuleType::Enum {
        name: "OrderStatus".to_owned(),
        symbols: vec!["draft".to_owned(), "submitted".to_owned()],
    };
    let other_status = RuleType::Enum {
        name: "OtherStatus".to_owned(),
        symbols: vec!["draft".to_owned()],
    };
    let declared = BTreeMap::from([
        ("OrderStatus".to_owned(), order_status.clone()),
        ("OtherStatus".to_owned(), other_status),
    ]);

    let catalog = compile_catalog_with_declared_types(
        &declared,
        &[rule(
            "order_is_draft",
            BTreeMap::new(),
            RuleType::Bool,
            "OrderStatus::draft == OrderStatus::draft",
        )],
        &[],
    )
    .expect("a declared enum symbol must compile with its nominal type");
    let compiled_rule = catalog
        .rule("order_is_draft")
        .expect("the nominal enum rule remains evaluable");
    assert!(
        catalog
            .evaluate_bool(compiled_rule, &BTreeMap::new())
            .expect("a same-enum literal comparison evaluates")
    );

    let unknown = compile_catalog_with_declared_types(
        &declared,
        &[rule(
            "unknown_status",
            BTreeMap::new(),
            RuleType::Bool,
            "OrderStatus::missing == OrderStatus::draft",
        )],
        &[],
    )
    .expect_err("symbols absent from their declared enum must fail at deploy time");
    assert!(matches!(
        unknown,
        RuleError::UnknownEnumSymbol { ref enum_name, ref symbol, .. }
            if enum_name == "OrderStatus" && symbol == "missing"
    ));

    let incompatible = compile_catalog_with_declared_types(
        &declared,
        &[rule(
            "nominal_statuses",
            BTreeMap::new(),
            RuleType::Bool,
            "OrderStatus::draft == OtherStatus::draft",
        )],
        &[],
    )
    .expect_err("same symbol text from different enum declarations must not compare");
    assert!(matches!(incompatible, RuleError::TypeMismatch { .. }));
}

#[test]
fn first_decision_table_uses_the_declared_default_row_and_preserves_its_id() {
    let catalog = compile_catalog(&[], &[approval_table(HitPolicy::First)])
        .expect("a first table with a final all-true row is valid");

    let compiled = catalog
        .decision_table("invoice_approval")
        .expect("compiled decision table should remain inspectable for audit");
    assert_eq!(compiled.rows[0].id, "manual_review");
    assert_eq!(
        compiled.rows[0].description.as_deref(),
        Some("decision row manual_review")
    );

    let result = catalog
        .evaluate_decision("invoice_approval", &map([("amount", json!(100))]))
        .expect("the all-true default row should supply a result");

    assert_eq!(result.output, json!({"route": "automatic"}));
    assert_eq!(result.matched_row_id, "default");
}

#[test]
fn decision_output_names_are_typed_data() {
    let table = DecisionTableDefinition {
        name: "customer_classification".to_owned(),
        revision: "rules-2026-07-28".to_owned(),
        inputs: map([("amount", RuleType::Int)]),
        output: map([
            ("role_label", RuleType::String),
            ("permission_count", RuleType::Int),
            ("connector_reference", RuleType::String),
        ]),
        hit_policy: HitPolicy::First,
        rows: vec![decision_row(
            "default",
            map([("amount", "true")]),
            json!({
                "role_label": "priority_customer",
                "permission_count": 3,
                "connector_reference": "crm_account",
            }),
        )],
        test_cases: vec![DecisionTableTestCase {
            name: "classification is typed business data".to_owned(),
            input: json!({"amount": 100}),
            expect: DecisionTestExpectation {
                output: json!({
                    "role_label": "priority_customer",
                    "permission_count": 3,
                    "connector_reference": "crm_account",
                }),
                matched_row_id: "default".to_owned(),
            },
        }],
    };

    let catalog = compile_catalog(&[], &[table])
        .expect("decision output field names must not determine their capability");
    let result = catalog
        .evaluate_decision("customer_classification", &map([("amount", json!(100))]))
        .expect("a typed decision output should evaluate");

    assert_eq!(
        result.output,
        json!({
            "role_label": "priority_customer",
            "permission_count": 3,
            "connector_reference": "crm_account",
        })
    );
}

#[test]
fn rules_expose_no_dynamic_capability_selection_api() {
    let table = DecisionTableDefinition {
        name: "work_routing".to_owned(),
        revision: "rules-2026-07-28".to_owned(),
        inputs: map([("amount", RuleType::Int)]),
        output: map([
            ("role_label", RuleType::String),
            ("permission_count", RuleType::Int),
            ("connector_reference", RuleType::String),
        ]),
        hit_policy: HitPolicy::First,
        rows: vec![decision_row(
            "default",
            map([("amount", "true")]),
            json!({
                "role_label": "customer_service",
                "permission_count": 2,
                "connector_reference": "crm_case",
            }),
        )],
        test_cases: Vec::new(),
    };

    let catalog = compile_catalog(&[], &[table])
        .expect("Rules must expose decision values without capability selection");
    let result = catalog
        .evaluate_decision("work_routing", &map([("amount", json!(100))]))
        .expect("decision evaluation returns typed data, not a capability handle");
    let table = catalog
        .decision_table("work_routing")
        .expect("the compiled decision table should be inspectable");
    let output: &donat_rules::DecisionOutputField = table
        .output_field("permission_count")
        .expect("a declared output has a typed data schema");

    assert_eq!(
        result.output,
        json!({
            "role_label": "customer_service",
            "permission_count": 2,
            "connector_reference": "crm_case",
        })
    );
    assert_eq!(output.name, "permission_count");
    assert_eq!(output.type_, RuleType::Int);
    assert!(table.output_field("unknown").is_none());
}

#[test]
fn compiled_decision_exposes_read_only_input_types_for_typed_consumers() {
    let catalog = compile_catalog(&[], &[approval_table(HitPolicy::First)])
        .expect("decision fixture compiles");
    let table = catalog
        .decision_table("invoice_approval")
        .expect("compiled decision exists");

    assert_eq!(table.input_type("amount"), Some(&RuleType::Int));
    assert!(table.input_type("unknown").is_none());
    assert_eq!(
        table
            .input_types()
            .map(|(name, type_)| (name.as_str(), type_))
            .collect::<Vec<_>>(),
        vec![("amount", &RuleType::Int)],
        "typed consumers receive a deterministic read-only view"
    );
}

#[test]
fn unique_decision_tables_reject_zero_and_multiple_matches() {
    let mut zero_match = approval_table(HitPolicy::Unique);
    zero_match.rows.pop();
    zero_match.test_cases.clear();
    zero_match
        .inputs
        .insert("secret".to_owned(), RuleType::String);
    zero_match.rows[0]
        .when
        .insert("secret".to_owned(), "true".to_owned());
    let zero_catalog =
        compile_catalog(&[], &[zero_match]).expect("unique tables need no default row");
    let zero = zero_catalog
        .evaluate_decision(
            "invoice_approval",
            &map([("amount", json!(0)), ("secret", json!("zero-match-secret"))]),
        )
        .expect_err("zero unique matches must be a typed decision rejection");
    let donat_rules::RuleError::DecisionNoMatch { table, trace } = zero else {
        panic!("zero unique matches must preserve their typed rejection")
    };
    assert_eq!(table, "invoice_approval");
    assert_rejected_trace(
        &trace,
        donat_rules::DecisionRejection::NoMatch,
        "zero-match-secret",
        0,
        vec![donat_rules::DecisionConditionTrace {
            row_id: "manual_review".to_owned(),
            conditions: map([("amount", false), ("secret", true)]),
        }],
    );

    let mut multiple_match = approval_table(HitPolicy::Unique);
    multiple_match.rows[1].when = map([("amount", "amount >= 0".to_owned())]);
    multiple_match.test_cases.clear();
    multiple_match
        .inputs
        .insert("secret".to_owned(), RuleType::String);
    for row in &mut multiple_match.rows {
        row.when.insert("secret".to_owned(), "true".to_owned());
    }
    let multiple_catalog = compile_catalog(&[], &[multiple_match])
        .expect("overlap is syntactically valid for unique tables");
    let multiple = multiple_catalog
        .evaluate_decision(
            "invoice_approval",
            &map([
                ("amount", json!(101)),
                ("secret", json!("multiple-match-secret")),
            ]),
        )
        .expect_err("multiple unique matches must be a typed decision rejection");
    let donat_rules::RuleError::DecisionMultipleMatches {
        table,
        row_ids,
        trace,
    } = multiple
    else {
        panic!("multiple unique matches must preserve their typed rejection")
    };
    assert_eq!(table, "invoice_approval");
    assert_eq!(
        row_ids,
        vec!["manual_review".to_owned(), "default".to_owned()]
    );
    assert_rejected_trace(
        &trace,
        donat_rules::DecisionRejection::MultipleMatches,
        "multiple-match-secret",
        101,
        vec![
            donat_rules::DecisionConditionTrace {
                row_id: "manual_review".to_owned(),
                conditions: map([("amount", true), ("secret", true)]),
            },
            donat_rules::DecisionConditionTrace {
                row_id: "default".to_owned(),
                conditions: map([("amount", true), ("secret", true)]),
            },
        ],
    );
}

#[test]
fn decision_trace_is_redacted_and_contains_only_a_revision_rows_booleans_and_digest() {
    let mut table = approval_table(HitPolicy::First);
    table.inputs.insert("secret".to_owned(), RuleType::String);
    for row in &mut table.rows {
        row.when.insert("secret".to_owned(), "true".to_owned());
    }
    table.test_cases.clear();
    let catalog = compile_catalog(&[], &[table]).expect("the traced decision table should compile");

    let result = catalog
        .evaluate_decision(
            "invoice_approval",
            &map([
                ("amount", json!(100)),
                ("secret", json!("top-secret-value")),
            ]),
        )
        .expect("the decision should evaluate");
    let trace_json = serde_json::to_string(&result.trace).expect("trace should serialize");

    assert_sha256_hex(&result.trace.table_revision, "table revision");
    assert_eq!(result.trace.table_name, "invoice_approval");
    assert_eq!(result.trace.matched_row_id.as_deref(), Some("default"));
    assert_eq!(result.trace.rejection, None);
    assert_eq!(
        result.trace.condition_results,
        vec![
            donat_rules::DecisionConditionTrace {
                row_id: "manual_review".to_owned(),
                conditions: map([("amount", false), ("secret", true)]),
            },
            donat_rules::DecisionConditionTrace {
                row_id: "default".to_owned(),
                conditions: map([("amount", true), ("secret", true)]),
            },
        ]
    );
    assert_sha256_hex(&result.trace.input_digest, "canonical decoded input digest");
    assert!(!trace_json.contains("top-secret-value"));
    assert!(!format!("{:#?}", result.trace).contains("top-secret-value"));
    assert!(!trace_json.contains("\"amount\":100"));
}

#[test]
fn accepts_canonical_temporals_and_signed_decimals_but_rejects_invalid_scalar_bindings() {
    let date = rule(
        "policy",
        map([("value", RuleType::Date)]),
        RuleType::Bool,
        "value == value",
    );
    assert!(
        evaluate(date.clone(), map([("value", json!("2026-07-28"))]))
            .expect("a canonical Gregorian date should be accepted")
    );
    let invalid_date = evaluate(date, map([("value", json!("2026-02-30"))]))
        .expect_err("an impossible calendar date must fail before evaluation");
    assert!(
        matches!(invalid_date, donat_rules::RuleError::InvalidBinding { ref name, .. } if name == "value")
    );

    let timestamp = rule(
        "policy",
        map([("value", RuleType::Timestamp)]),
        RuleType::Bool,
        "value == value",
    );
    assert!(
        evaluate(
            timestamp.clone(),
            map([("value", json!("2026-07-28T14:30:15.123+03:00"))]),
        )
        .expect("a canonical offset timestamp should be accepted")
    );
    let invalid_timestamp = evaluate(timestamp, map([("value", json!("2026-07-28T25:30:15Z"))]))
        .expect_err("an impossible timestamp must fail before evaluation");
    assert!(
        matches!(invalid_timestamp, donat_rules::RuleError::InvalidBinding { ref name, .. } if name == "value")
    );

    let decimal = rule(
        "policy",
        map([("value", RuleType::Decimal)]),
        RuleType::Bool,
        "value < 0.0",
    );
    assert!(
        evaluate(decimal, map([("value", json!(-1.5))]))
            .expect("a signed base-10 decimal binding should be accepted")
    );
}

fn assert_rejected_trace(
    trace: &donat_rules::DecisionTrace,
    rejection: donat_rules::DecisionRejection,
    secret: &str,
    raw_amount: i64,
    expected_conditions: Vec<donat_rules::DecisionConditionTrace>,
) {
    let trace_json = serde_json::to_string(trace).expect("trace should serialize");

    assert_sha256_hex(&trace.table_revision, "table revision");
    assert_eq!(trace.table_name, "invoice_approval");
    assert_eq!(trace.matched_row_id, None);
    assert_eq!(trace.rejection, Some(rejection));
    assert_eq!(trace.condition_results, expected_conditions);
    assert!(!trace_json.contains(secret));
    assert!(!format!("{trace:#?}").contains(secret));
    assert!(!trace_json.contains(&format!("\"amount\":{raw_amount}")));
    assert_sha256_hex(&trace.input_digest, "canonical decoded input digest");
}

fn assert_sha256_hex(value: &str, label: &str) {
    assert_eq!(value.len(), 64, "{label} must be a SHA-256 hex digest");
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{label} must be lower-case hexadecimal"
    );
}

#[test]
fn catalog_rejects_duplicate_names_bad_default_rows_and_invalid_test_cases() {
    let duplicate_rules = compile_catalog(
        &[
            rule("same", BTreeMap::new(), RuleType::Bool, "true"),
            rule("same", BTreeMap::new(), RuleType::Bool, "true"),
        ],
        &[],
    )
    .expect_err("duplicate rule names must fail snapshot validation");
    assert!(
        matches!(duplicate_rules, donat_rules::RuleError::DuplicateName { ref name, .. } if name == "same")
    );

    let mut missing_default = approval_table(HitPolicy::First);
    missing_default.rows[1].when = map([("amount", "amount >= 0".to_owned())]);
    let default_error = compile_catalog(&[], &[missing_default])
        .expect_err("a first table requires a final literal all-true default row");
    assert!(matches!(
        default_error,
        donat_rules::RuleError::MissingDefaultRow { .. }
    ));

    let mut wrong_test = approval_table(HitPolicy::First);
    wrong_test.test_cases[0].expect.matched_row_id = "manual_review".to_owned();
    let test_case_error = compile_catalog(&[], &[wrong_test])
        .expect_err("declared decision cases must prove their exact output and row id");
    assert!(matches!(
        test_case_error,
        donat_rules::RuleError::DecisionTestCaseMismatch { .. }
    ));
}
