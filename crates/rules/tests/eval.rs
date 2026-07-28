use std::collections::BTreeMap;

use donat_rules::{
    DecisionRow, DecisionTableDefinition, DecisionTableTestCase, DecisionTestExpectation,
    HitPolicy, RuleDefinition, RuleError, RuleType, compile_catalog,
    compile_catalog_with_declared_types,
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
        "active && customer.name == 'Ada'",
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
fn evaluates_list_size_string_predicates_and_short_circuit_boolean_operators() {
    let definition = rule(
        "policy",
        map([
            ("lines", RuleType::List(Box::new(RuleType::Int))),
            ("name", RuleType::String),
        ]),
        RuleType::Bool,
        "size(lines) == 2 && startsWith(name, 'A') && endsWith(name, 'z') && (false && 1 / 0 > 0) == false && (true || 1 / 0 > 0)",
    );

    assert!(
        evaluate(
            definition,
            map([("lines", json!([1, 2])), ("name", json!("Az"))]),
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

    assert_eq!(result.trace.table_revision, "rules-2026-07-28");
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
    assert!(result.trace.input_digest.len() >= 16);
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

    assert_eq!(trace.table_revision, "rules-2026-07-28");
    assert_eq!(trace.table_name, "invoice_approval");
    assert_eq!(trace.matched_row_id, None);
    assert_eq!(trace.rejection, Some(rejection));
    assert_eq!(trace.condition_results, expected_conditions);
    assert!(!trace_json.contains(secret));
    assert!(!format!("{trace:#?}").contains(secret));
    assert!(!trace_json.contains(&format!("\"amount\":{raw_amount}")));
}

#[test]
fn catalog_rejects_duplicate_names_bad_default_rows_result_permissions_and_invalid_test_cases() {
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

    let mut forbidden_output = approval_table(HitPolicy::First);
    forbidden_output
        .output
        .insert("run_as_role".to_owned(), RuleType::String);
    forbidden_output.rows[0].output = json!({"route": "manual", "run_as_role": "bypass"});
    forbidden_output.rows[1].output = json!({"route": "automatic", "run_as_role": "bypass"});
    let forbidden_error = compile_catalog(&[], &[forbidden_output])
        .expect_err("a decision result may not select a runtime role or permission");
    assert!(
        matches!(forbidden_error, donat_rules::RuleError::ForbiddenDecisionOutput { ref field } if field == "run_as_role")
    );

    let mut wrong_test = approval_table(HitPolicy::First);
    wrong_test.test_cases[0].expect.matched_row_id = "manual_review".to_owned();
    let test_case_error = compile_catalog(&[], &[wrong_test])
        .expect_err("declared decision cases must prove their exact output and row id");
    assert!(matches!(
        test_case_error,
        donat_rules::RuleError::DecisionTestCaseMismatch { .. }
    ));
}
