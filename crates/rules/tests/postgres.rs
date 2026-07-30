use std::collections::BTreeMap;

use donat_rules::{
    DecisionRow, DecisionTableDefinition, HitPolicy, PostgresDecisionHitPolicy, RuleDefinition,
    RuleError, RuleType, SqlBinding, SqlBindings, SqlExpression, compile_catalog,
    compile_catalog_with_declared_types, evaluate_bool, evaluate_value, lower_postgres,
    lower_postgres_decision, lower_postgres_value,
};
use postgres::{Client, NoTls, error::SqlState};
use serde_json::{Value, json};

fn map<T>(pairs: impl IntoIterator<Item = (&'static str, T)>) -> BTreeMap<String, T> {
    pairs
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}

fn object(name: &str, fields: BTreeMap<String, RuleType>) -> RuleType {
    RuleType::Object {
        name: name.to_owned(),
        fields,
    }
}

fn compiled_rule(
    bindings: BTreeMap<String, RuleType>,
    expression: &str,
) -> donat_rules::CompiledRule {
    compiled_value_rule(RuleType::Bool, bindings, expression)
}

fn compiled_value_rule(
    result: RuleType,
    bindings: BTreeMap<String, RuleType>,
    expression: &str,
) -> donat_rules::CompiledRule {
    let catalog = compile_catalog(
        &[RuleDefinition {
            name: "checkout_policy".to_owned(),
            bindings,
            result,
            expression: expression.to_owned(),
        }],
        &[],
    )
    .expect("the rule fixture should compile");
    catalog
        .rule("checkout_policy")
        .expect("the compiled fixture should contain its rule")
        .clone()
}

fn typed_columns(bindings: &BTreeMap<String, RuleType>) -> SqlBindings {
    SqlBindings::new(bindings.iter().map(|(name, type_)| {
        (
            name.clone(),
            SqlBinding::expression(SqlExpression::column("input", name, type_.clone())),
        )
    }))
}

fn has_raw_boolean_operator(sql: &str) -> bool {
    // Arithmetic range guards legitimately use `BETWEEN ... AND ...`. Raw
    // profile boolean lowering, by contrast, would retain this parenthesized
    // binary shape instead of the required `CASE` expression.
    sql.contains(") AND (") || sql.contains(") OR (")
}

#[test]
fn decision_lowerer_keeps_rows_private_and_emits_a_closed_first_hit_program() {
    let catalog = compile_catalog(
        &[],
        &[DecisionTableDefinition {
            name: "shipping_route".to_owned(),
            revision: "shipping-v1".to_owned(),
            inputs: map([("amount", RuleType::Int), ("region", RuleType::String)]),
            output: map([("rank", RuleType::Int), ("route", RuleType::String)]),
            hit_policy: HitPolicy::First,
            rows: vec![
                DecisionRow {
                    id: "priority".to_owned(),
                    description: None,
                    when: map([
                        ("amount", "amount > 100".to_owned()),
                        ("region", "region == 'west'".to_owned()),
                    ]),
                    output: json!({"rank": 1, "route": "priority"}),
                },
                DecisionRow {
                    id: "default".to_owned(),
                    description: None,
                    when: map([("amount", "true".to_owned()), ("region", "true".to_owned())]),
                    output: json!({"rank": 2, "route": "standard"}),
                },
            ],
            test_cases: vec![],
        }],
    )
    .expect("the decision fixture compiles");
    let table = catalog
        .decision_table("shipping_route")
        .expect("the compiled decision exists");
    let lowered = lower_postgres_decision(
        table,
        &SqlBindings::new([
            (
                "amount".to_owned(),
                SqlBinding::expression(SqlExpression::column(
                    "_cmd_input",
                    "amount",
                    RuleType::Int,
                )),
            ),
            (
                "region".to_owned(),
                SqlBinding::expression(SqlExpression::column(
                    "_cmd_input",
                    "region",
                    RuleType::String,
                )),
            ),
        ]),
    )
    .expect("a compiled decision lowers without exposing its private AST");

    assert_eq!(lowered.name, "shipping_route");
    assert_eq!(lowered.revision, table.revision.0);
    assert_eq!(lowered.hit_policy, PostgresDecisionHitPolicy::First);
    assert_eq!(
        lowered
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["priority", "default"]
    );
    assert!(
        lowered.rows[0]
            .condition_sql
            .contains("\"_cmd_input\".\"amount\""),
        "decision input identifiers must be escaped inside Rules: {}",
        lowered.rows[0].condition_sql
    );
    assert_eq!(lowered.rows[0].output["rank"].type_, RuleType::Int);
    assert!(lowered.rows[0].output["rank"].sql.contains("1"));
    assert_eq!(lowered.rows[0].output["route"].type_, RuleType::String);
    assert!(lowered.rows[0].output["route"].sql.contains("priority"));
}

#[test]
fn decision_lowerer_retains_unique_policy_and_rejects_incomplete_bindings() {
    let catalog = compile_catalog(
        &[],
        &[DecisionTableDefinition {
            name: "unique_route".to_owned(),
            revision: "unique-v1".to_owned(),
            inputs: map([("amount", RuleType::Int)]),
            output: map([("route", RuleType::String)]),
            hit_policy: HitPolicy::Unique,
            rows: vec![DecisionRow {
                id: "only".to_owned(),
                description: None,
                when: map([("amount", "amount > 0".to_owned())]),
                output: json!({"route": "accepted"}),
            }],
            test_cases: vec![],
        }],
    )
    .expect("the unique decision fixture compiles");
    let table = catalog
        .decision_table("unique_route")
        .expect("the compiled decision exists");

    let missing = lower_postgres_decision(table, &SqlBindings::default())
        .expect_err("every typed decision input must have a closed SQL binding");
    assert!(matches!(
        missing,
        RuleError::MissingBinding { ref name } if name == "amount"
    ));

    let lowered = lower_postgres_decision(
        table,
        &SqlBindings::new([(
            "amount".to_owned(),
            SqlBinding::expression(SqlExpression::column("_cmd_input", "amount", RuleType::Int)),
        )]),
    )
    .expect("the complete unique decision binding lowers");
    assert_eq!(lowered.hit_policy, PostgresDecisionHitPolicy::Unique);
}

#[test]
fn lowerer_accepts_an_escaped_scalar_subquery_binding() {
    let bindings = map([("prior", RuleType::Int)]);
    let rule = compiled_rule(bindings, "prior > 0");
    let sql = lower_postgres(
        &rule,
        &SqlBindings::new([(
            "prior".to_owned(),
            SqlBinding::expression(SqlExpression::scalar_subquery(
                "prior step",
                "total value",
                RuleType::Int,
            )),
        )]),
    )
    .expect("a closed previous-step scalar binding lowers safely");

    assert!(sql.contains("(SELECT \"total value\" FROM \"prior step\" LIMIT 1)"));
}

#[test]
fn lowerer_parenthesizes_scalar_comparisons_and_arithmetic() {
    let bindings = map([
        ("amount", RuleType::Int),
        ("tax", RuleType::Int),
        ("quantity", RuleType::Int),
        ("discount", RuleType::Int),
    ]);
    let rule = compiled_rule(bindings.clone(), "amount + tax * quantity >= discount - 5");

    let sql = lower_postgres(&rule, &typed_columns(&bindings))
        .expect("typed SQL columns should lower a valid rule");

    insta::assert_snapshot!(sql);
}

#[test]
fn lowerer_uses_is_null_for_nullable_equality() {
    let bindings = map([("limit", RuleType::nullable(RuleType::Int))]);
    let rule = compiled_rule(
        bindings.clone(),
        "limit == null || limit != null || is_null(limit)",
    );

    let sql = lower_postgres(&rule, &typed_columns(&bindings))
        .expect("a nullable column should lower through SQL null predicates");

    insta::assert_snapshot!(sql);
    assert!(
        !has_raw_boolean_operator(&sql),
        "profile boolean operators must lower through CASE: {sql}"
    );
}

#[test]
fn lowerer_extracts_nested_typed_json_objects_and_lists() {
    let bindings = map([
        (
            "customer",
            object("Customer", map([("nickname", RuleType::String)])),
        ),
        ("lines", RuleType::List(Box::new(RuleType::Int))),
    ]);
    let rule = compiled_rule(
        bindings.clone(),
        "is_null(customer.nickname) && is_null(lines[3])",
    );

    let sql = lower_postgres(&rule, &typed_columns(&bindings))
        .expect("typed JSON columns should lower nullable static object and list access");

    insta::assert_snapshot!(sql);
    assert!(
        !has_raw_boolean_operator(&sql),
        "profile boolean operators must lower through CASE: {sql}"
    );
}

#[test]
fn lowerer_supports_each_allowed_function() {
    let bindings = map([
        ("name", RuleType::String),
        ("lines", RuleType::List(Box::new(RuleType::Int))),
        ("limit", RuleType::nullable(RuleType::Int)),
    ]);
    let rule = compiled_rule(
        bindings.clone(),
        "size(name) == 2 && size(lines) == 2 && is_null(limit) && startsWith(name, 'A') && endsWith(name, 'z')",
    );

    let sql = lower_postgres(&rule, &typed_columns(&bindings))
        .expect("all profile functions should lower from the typed AST");

    insta::assert_snapshot!(sql);
    assert!(
        !has_raw_boolean_operator(&sql),
        "profile boolean operators must lower through CASE: {sql}"
    );
}

#[test]
fn lowerer_short_circuits_runtime_division_with_typed_columns() {
    let bindings = map([("numerator", RuleType::Int), ("denominator", RuleType::Int)]);
    let mut client = postgres_client();

    for (source, expected) in [
        ("false && (numerator / denominator > 0)", false),
        ("true || (numerator / denominator > 0)", true),
    ] {
        let rule = compiled_rule(bindings.clone(), source);
        let expression = lower_postgres(&rule, &typed_columns(&bindings))
            .expect("typed SQL columns should lower short-circuited arithmetic");
        assert!(
            expression.contains("CASE WHEN"),
            "the operator must lower through CASE: {expression}"
        );
        assert!(
            !has_raw_boolean_operator(&expression),
            "the lowerer must not emit raw boolean operators: {expression}"
        );

        let actual = client
            .query_one(
                &format!(
                    "SELECT {expression} AS value FROM (VALUES (1::numeric, 0::numeric)) AS input(numerator, denominator)"
                ),
                &[],
            )
            .expect("CASE must avoid evaluating the zero typed-column divisor")
            .get::<_, Option<bool>>("value");

        assert_eq!(
            actual,
            Some(expected),
            "Postgres result mismatch for {source}"
        );
    }
}

#[test]
fn lowerer_rejects_a_declared_binding_without_a_typed_sql_expression() {
    let rule = compiled_rule(map([("amount", RuleType::Int)]), "amount > 0");

    let error = lower_postgres(&rule, &SqlBindings::default())
        .expect_err("a rule name must never become an implicit SQL identifier");

    assert!(matches!(
        error,
        RuleError::MissingBinding { ref name } if name == "amount"
    ));
}

#[test]
fn lower_postgres_value_matches_live_postgres() {
    let status = RuleType::Enum {
        name: "OrderStatus".to_owned(),
        symbols: vec!["draft".to_owned(), "submitted".to_owned()],
    };
    let customer = object("Customer", map([("nickname", RuleType::String)]));
    let enum_catalog = compile_catalog_with_declared_types(
        &map([("OrderStatus", status.clone())]),
        &[RuleDefinition {
            name: "checkout_policy".to_owned(),
            bindings: BTreeMap::new(),
            result: status.clone(),
            expression: "OrderStatus::draft".to_owned(),
        }],
        &[],
    )
    .expect("an enum value rule should compile");
    let enum_rule = enum_catalog
        .rule("checkout_policy")
        .expect("the enum value rule should exist")
        .clone();
    let cases = vec![
        (
            compiled_value_rule(RuleType::String, map([("name", RuleType::String)]), "name"),
            map([("name", json!("Ada"))]),
            RuleType::String,
            json!("Ada"),
        ),
        (
            compiled_value_rule(
                RuleType::Decimal,
                map([("price", RuleType::Decimal)]),
                "price / 2.0",
            ),
            map([("price", json!(2.4))]),
            RuleType::Decimal,
            json!(1.2),
        ),
        (enum_rule, BTreeMap::new(), status.clone(), json!("draft")),
        (
            compiled_value_rule(
                RuleType::nullable(RuleType::String),
                map([("customer", customer)]),
                "customer.nickname",
            ),
            map([("customer", json!({}))]),
            RuleType::nullable(RuleType::String),
            Value::Null,
        ),
        (
            compiled_value_rule(
                RuleType::nullable(RuleType::Int),
                map([("lines", RuleType::List(Box::new(RuleType::Int)))]),
                "lines[3]",
            ),
            map([("lines", json!([1]))]),
            RuleType::nullable(RuleType::Int),
            Value::Null,
        ),
    ];
    let mut client = postgres_client();

    for (rule, bindings, type_, value) in cases {
        let sql_bindings = SqlBindings::new(
            bindings
                .into_iter()
                .map(|(name, value)| (name, SqlBinding::literal(value))),
        );
        let lowered =
            lower_postgres_value(&rule, &sql_bindings).expect("a typed value rule should lower");
        assert_eq!(
            lowered.type_, type_,
            "lowered type mismatch for {}",
            rule.name
        );
        let actual = client
            .query_one(
                &format!(
                    "SELECT COALESCE(to_jsonb(({})), 'null'::jsonb)::text AS value",
                    lowered.sql
                ),
                &[],
            )
            .expect("the lowered typed value should execute in PostgreSQL")
            .get::<_, String>("value")
            .parse::<Value>()
            .expect("PostgreSQL JSON output should parse");

        assert_eq!(actual, value, "Postgres value mismatch for {}", rule.name);
    }
}

#[test]
fn object_value_rejects_missing_non_null_members_in_rust_and_postgres() {
    let customer = object("Customer", map([("name", RuleType::String)]));
    let rule = compiled_value_rule(customer.clone(), map([("customer", customer)]), "customer");
    let bindings = map([("customer", json!({}))]);
    let rust_error = evaluate_value(&rule, &bindings)
        .expect_err("a direct object value must retain required-member semantics");
    assert!(matches!(rust_error, RuleError::InvalidRuleResult { .. }));

    let sql_bindings = SqlBindings::new([("customer".to_owned(), SqlBinding::literal(json!({})))]);
    let mut client = postgres_client();
    let postgres_error = lower_postgres_value(&rule, &sql_bindings)
        .expect_err("Postgres value lowering must reject the same required member");
    assert!(matches!(
        postgres_error,
        RuleError::InvalidBinding { ref name, .. } if name == "customer"
    ));

    // The total-access contract is not weakened: a member consumer can still
    // observe this missing key as null. This direct value path is stricter
    // because `Customer!` cannot serialize a missing `string!` member.
    let member_rule = compiled_value_rule(
        RuleType::nullable(RuleType::String),
        map([(
            "customer",
            object("Customer", map([("name", RuleType::String)])),
        )]),
        "customer.name",
    );
    assert_postgres_matches_rust(
        &mut client,
        &member_rule,
        bindings,
        "total missing object member access",
    );
}

#[test]
fn object_value_allows_missing_nullable_members_in_rust_and_postgres() {
    let customer = object(
        "Customer",
        map([("nickname", RuleType::nullable(RuleType::String))]),
    );
    let rule = compiled_value_rule(customer.clone(), map([("customer", customer)]), "customer");
    let bindings = map([("customer", json!({}))]);
    let expected = json!({ "nickname": null });
    let rust_value = evaluate_value(&rule, &bindings)
        .expect("an absent nullable member must remain a JSON null");
    assert_eq!(rust_value.value, expected);

    let lowered = lower_postgres_value(
        &rule,
        &SqlBindings::new([("customer".to_owned(), SqlBinding::literal(json!({})))]),
    )
    .expect("Postgres value lowering must allow the same nullable member");
    let mut client = postgres_client();
    let postgres_value = client
        .query_one(
            &format!(
                "SELECT COALESCE(to_jsonb(({})), 'null'::jsonb)::text AS value",
                lowered.sql
            ),
            &[],
        )
        .expect("the nullable object value should execute in PostgreSQL")
        .get::<_, String>("value")
        .parse::<Value>()
        .expect("PostgreSQL JSON output should parse");

    assert_eq!(postgres_value, expected);
}

#[test]
fn lowerer_rejects_i128_arithmetic_overflow_like_the_rust_evaluator() {
    let decimal_maximum_at_scale_one = "17014118346046923173168730371588410572.7";
    let tiny_decimal_at_scale_4000 = format!("0.{}1", "0".repeat(3999));
    let cases = [
        (
            "integer result overflow",
            format!("{} + 1 > {}", i128::MAX, i128::MAX),
        ),
        (
            "decimal result overflow",
            format!("{decimal_maximum_at_scale_one} + 0.1 > 0.0"),
        ),
        (
            "decimal intermediate overflow",
            format!("{decimal_maximum_at_scale_one} * 10.0 > 0.0"),
        ),
        (
            "decimal scale alignment overflow",
            format!("{tiny_decimal_at_scale_4000} + 0.0 > 0.0"),
        ),
        (
            "decimal division numerator scale overflow",
            format!("0.0 / {tiny_decimal_at_scale_4000} > 0.0"),
        ),
    ];
    let mut client = postgres_client();

    for (case, source) in cases {
        let rule = compiled_rule(BTreeMap::new(), &source);
        let rust_error = evaluate_bool(&rule, &BTreeMap::new())
            .expect_err("the bounded Rust profile must reject arithmetic overflow");
        assert!(
            matches!(rust_error, RuleError::InvalidLiteral { .. }),
            "unexpected Rust rejection for {case}: {rust_error}",
        );

        let expression = lower_postgres(&rule, &SqlBindings::default())
            .expect("a validated arithmetic rule should lower to a runtime rejection");
        let postgres_error = client
            .query_one(&format!("SELECT {expression} AS value"), &[])
            .expect_err("Postgres must reject the same bounded arithmetic overflow");
        assert_eq!(
            postgres_error.as_db_error().map(|error| error.code()),
            Some(&SqlState::DIVISION_BY_ZERO),
            "overflow rejection for {case} must use the normal runtime arithmetic path: {postgres_error}",
        );
    }
}

#[test]
fn high_decimal_scale_remains_valid_when_no_bounded_power_is_required() {
    let tiny_decimal = format!("0.{}1", "0".repeat(3999));
    let rule = compiled_rule(BTreeMap::new(), &format!("{tiny_decimal} * 2.0 > -1.0"));
    assert!(
        evaluate_bool(&rule, &BTreeMap::new())
            .expect("multiplication does not require a bounded power-of-ten alignment"),
        "the exact positive high-scale product must remain greater than negative one",
    );

    let expression = lower_postgres(&rule, &SqlBindings::default())
        .expect("the high-scale decimal expression should lower");
    let postgres_value = postgres_client()
        .query_one(&format!("SELECT {expression} AS value"), &[])
        .expect("Postgres must not reject a scale that needs no bounded power")
        .get::<_, bool>("value");

    assert!(postgres_value);
}

#[test]
fn extreme_scale_decimal_ordering_matches_live_postgres() {
    let tiny = format!("0.{}1", "0".repeat(3999));
    let larger = format!("0.{}2", "0".repeat(1799));
    let smaller = format!("0.{}9", "0".repeat(1899));
    let mut client = postgres_client();
    let direct_postgres_value = client
        .query_one(
            &format!("SELECT (({tiny})::numeric * 2.0::numeric) > 0.0::numeric AS value"),
            &[],
        )
        .expect("Postgres should compare its exact numeric values directly")
        .get::<_, bool>("value");
    assert!(
        direct_postgres_value,
        "native Postgres numeric ordering is the differential ground truth",
    );
    let reconstruction_stages = client
        .query_one(
            &format!(
                "SELECT \
                    scale(trim_scale(({tiny})::numeric)) = 4000 AS literal_scale, \
                    ((2::numeric / power(10::numeric, 4000)) > 0) AS divided, \
                    ((2::numeric * power(10::numeric, -4000)) > 0) AS shifted, \
                    ((2::numeric * ('1e-' || 4000::text)::numeric) > 0) AS exponent_shifted"
            ),
            &[],
        )
        .expect("Postgres should expose each exact-decimal reconstruction stage");
    assert!(
        reconstruction_stages.get::<_, bool>("literal_scale"),
        "literal normalization must preserve the source scale",
    );
    assert!(
        !reconstruction_stages.get::<_, bool>("divided"),
        "numeric division rounds the reconstructed high-scale value to zero",
    );
    assert!(
        !reconstruction_stages.get::<_, bool>("shifted"),
        "numeric power also rounds the reconstructed high-scale value to zero",
    );
    assert!(
        reconstruction_stages.get::<_, bool>("exponent_shifted"),
        "numeric exponent input must preserve the high-scale value",
    );
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
        let rule = compiled_rule(BTreeMap::new(), &source);
        let expression = lower_postgres(&rule, &SqlBindings::default())
            .expect("an exact decimal comparison should lower");
        let postgres_value = client
            .query_one(&format!("SELECT {expression} AS value"), &[])
            .expect("the lowered decimal comparison should execute")
            .get::<_, bool>("value");
        let rust_value = evaluate_bool(&rule, &BTreeMap::new())
            .expect("the Rust decimal comparison should evaluate");

        assert_eq!(
            postgres_value, expected,
            "Donat-lowered Postgres mismatch for {case}; SQL: {expression}",
        );
        assert_eq!(
            rust_value, expected,
            "Rust decimal ordering mismatch for {case}",
        );
        assert_eq!(
            rust_value, postgres_value,
            "Rust/Postgres decimal ordering mismatch for {case}",
        );
    }
}

#[test]
fn timestamp_value_lowering_is_utc_canonical_outside_a_utc_session() {
    let rule = compiled_value_rule(
        RuleType::Timestamp,
        map([("created_at", RuleType::Timestamp)]),
        "created_at",
    );
    let mut client = postgres_client();
    client
        .batch_execute("SET TIME ZONE 'Asia/Kolkata'")
        .expect("the live differential must run outside UTC");

    for (case, timestamp) in [
        ("offset input", "2026-07-29T14:30:15.120+02:00"),
        ("already UTC input", "2026-07-29T12:30:15.120Z"),
    ] {
        let bindings = map([("created_at", json!(timestamp))]);
        let expected = json!("2026-07-29T12:30:15.12Z");
        let rust_value = evaluate_value(&rule, &bindings)
            .expect("the Rust evaluator should normalize the typed timestamp to UTC");
        assert_eq!(
            rust_value.value, expected,
            "Rust canonical timestamp mismatch for {case}",
        );
        let lowered = lower_postgres_value(
            &rule,
            &SqlBindings::new([(
                "created_at".to_owned(),
                SqlBinding::literal(json!(timestamp)),
            )]),
        )
        .expect("the timestamp value rule should lower");
        let postgres_value = client
            .query_one(
                &format!(
                    "SELECT COALESCE(to_jsonb(({})), 'null'::jsonb)::text AS value",
                    lowered.sql
                ),
                &[],
            )
            .expect("the lowered timestamp value should execute in PostgreSQL")
            .get::<_, String>("value")
            .parse::<Value>()
            .expect("PostgreSQL JSON output should parse");

        assert_eq!(
            postgres_value, expected,
            "Postgres canonical timestamp mismatch for {case}",
        );
    }
}

#[test]
fn lower_postgres_rejects_non_bool_rule() {
    let rule = compiled_value_rule(RuleType::String, map([("name", RuleType::String)]), "name");
    let error = lower_postgres(
        &rule,
        &SqlBindings::new([("name".to_owned(), SqlBinding::literal(json!("Ada")))]),
    )
    .expect_err("boolean lowering must reject a non-boolean rule");

    assert!(matches!(
        error,
        RuleError::InvalidRuleResult { ref rule, ref expected, ref actual }
            if rule == "checkout_policy" && expected == "bool" && actual == "string"
    ));
}

#[test]
fn lowerer_matches_rust_decimal_division_scale_and_truncation() {
    let mut client = postgres_client();

    for (case, source) in [
        ("positive repeating", "1.0 / 3.0 == 0.333333333333333333"),
        ("negative numerator", "-1.0 / 3.0 == -0.333333333333333333"),
        (
            "negative denominator",
            "1.0 / -3.0 == -0.333333333333333333",
        ),
        ("positive truncation", "1.0 / 6.0 == 0.166666666666666666"),
        (
            "normalized high scale",
            "1.0000000000000000001 / 3.0 == 0.3333333333333333333",
        ),
        (
            "underflow after truncation",
            "0.0000000000000000001 / 3.0 == 0.0",
        ),
    ] {
        let rule = compiled_rule(BTreeMap::new(), source);
        let bindings = BTreeMap::new();
        let expected = evaluate_bool(&rule, &bindings)
            .expect("the evaluator should accept the exact decimal division fixture");
        assert!(
            expected,
            "the {case} fixture must hold in the Rust evaluator"
        );
        let expression = lower_postgres(&rule, &SqlBindings::default())
            .expect("a closed decimal expression should lower");
        assert!(
            expression.contains("trunc(") && expression.contains("greatest(18"),
            "the {case} fixture must use Decimal::checked_div semantics: {expression}"
        );
        let actual = client
            .query_one(&format!("SELECT {expression} AS value"), &[])
            .expect("the lowerer should emit a valid PostgreSQL expression")
            .get::<_, Option<bool>>("value")
            .expect("a typed boolean rule must not produce SQL NULL");

        assert_eq!(actual, expected, "Postgres/Rust mismatch for {case}");
    }
}

#[test]
fn postgres_differential_matches_rust_for_bounded_generated_closed_contexts() {
    let context_bindings = map([
        ("amount", RuleType::Int),
        ("tax", RuleType::Int),
        ("quantity", RuleType::Int),
        ("discount", RuleType::Int),
        ("name", RuleType::String),
        ("lines", RuleType::List(Box::new(RuleType::Int))),
        ("limit", RuleType::nullable(RuleType::Int)),
        (
            "customer",
            object(
                "Customer",
                map([
                    (
                        "addresses",
                        RuleType::List(Box::new(object(
                            "Address",
                            map([("country", RuleType::String)]),
                        ))),
                    ),
                    ("tags", RuleType::List(Box::new(RuleType::Int))),
                    ("nickname", RuleType::String),
                ]),
            ),
        ),
    ]);
    let bool_rule = compiled_rule(
        context_bindings.clone(),
        "amount + tax * quantity >= discount - 5 && size(name) >= 2 && startsWith(name, 'A') && endsWith(name, 'z') && size(lines) == 2 && (limit == null || is_null(limit)) && is_null(customer.nickname)",
    );
    let string_rule = compiled_value_rule(RuleType::String, context_bindings.clone(), "name");
    let conditional_rule = compiled_value_rule(
        RuleType::String,
        context_bindings.clone(),
        "amount >= 0 ? 'matched' : 'fallback'",
    );
    let nullable_member_rule = compiled_value_rule(
        RuleType::nullable(RuleType::String),
        context_bindings.clone(),
        "customer.nickname",
    );
    let nullable_item_rule = compiled_value_rule(
        RuleType::nullable(RuleType::Int),
        context_bindings,
        "lines[3]",
    );
    let integer_rule = compiled_value_rule(
        RuleType::Int,
        map([
            ("left", RuleType::Int),
            ("addend", RuleType::Int),
            ("multiplier", RuleType::Int),
            ("divisor", RuleType::Int),
            ("threshold", RuleType::Int),
        ]),
        "-left + addend * multiplier / divisor",
    );
    let decimal_rule = compiled_value_rule(
        RuleType::Decimal,
        map([
            ("left", RuleType::Decimal),
            ("addend", RuleType::Decimal),
            ("multiplier", RuleType::Decimal),
            ("divisor", RuleType::Decimal),
            ("threshold", RuleType::Decimal),
        ]),
        "-left + addend * multiplier / divisor",
    );
    let status = RuleType::Enum {
        name: "OrderStatus".to_owned(),
        symbols: vec!["draft".to_owned(), "submitted".to_owned()],
    };
    let enum_catalog = compile_catalog_with_declared_types(
        &map([("OrderStatus", status.clone())]),
        &[RuleDefinition {
            name: "checkout_policy".to_owned(),
            bindings: BTreeMap::new(),
            result: status,
            expression: "OrderStatus::draft".to_owned(),
        }],
        &[],
    )
    .expect("a generated enum value rule should compile");
    let enum_rule = enum_catalog
        .rule("checkout_policy")
        .expect("the generated enum rule should exist")
        .clone();
    let mut client = postgres_client();
    let mut seed = 0x7A11_CE55_u64;

    for case_index in 0..128 {
        assert_postgres_matches_rust(
            &mut client,
            &bool_rule,
            generated_context_bindings(&mut seed),
            &format!("bool case {case_index}"),
        );
        assert_postgres_matches_rust(
            &mut client,
            &string_rule,
            generated_context_bindings(&mut seed),
            &format!("string case {case_index}"),
        );
        assert_postgres_matches_rust(
            &mut client,
            &conditional_rule,
            generated_context_bindings(&mut seed),
            &format!("conditional case {case_index}"),
        );
        assert_postgres_matches_rust(
            &mut client,
            &nullable_member_rule,
            generated_context_bindings(&mut seed),
            &format!("nullable member case {case_index}"),
        );
        assert_postgres_matches_rust(
            &mut client,
            &nullable_item_rule,
            generated_context_bindings(&mut seed),
            &format!("nullable item case {case_index}"),
        );
        assert_postgres_matches_rust(
            &mut client,
            &integer_rule,
            generated_integer_bindings(&mut seed),
            &format!("integer arithmetic case {case_index}"),
        );
        assert_postgres_matches_rust(
            &mut client,
            &decimal_rule,
            generated_decimal_bindings(&mut seed),
            &format!("decimal arithmetic case {case_index}"),
        );
        assert_postgres_matches_rust(
            &mut client,
            &enum_rule,
            BTreeMap::new(),
            &format!("enum case {case_index}"),
        );
    }
}

fn postgres_client() -> Client {
    let pg_url = std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15433/postgres".to_owned());
    Client::connect(&pg_url, NoTls)
        .expect("the focused rules differential requires PostgreSQL at PG_URL")
}

fn assert_postgres_matches_rust(
    client: &mut Client,
    rule: &donat_rules::CompiledRule,
    bindings: BTreeMap<String, Value>,
    case: &str,
) {
    let expected = evaluate_value(rule, &bindings)
        .expect("the generated values must remain inside the typed profile");
    let sql_bindings = SqlBindings::new(
        bindings
            .iter()
            .map(|(name, value)| (name.clone(), SqlBinding::literal(value.clone()))),
    );
    let lowered = lower_postgres_value(rule, &sql_bindings)
        .expect("every generated closed context should lower safely");
    assert_eq!(
        lowered.type_, expected.type_,
        "Postgres/Rust type mismatch for {case}"
    );
    let actual = client
        .query_one(
            &format!(
                "SELECT COALESCE(to_jsonb(({})), 'null'::jsonb)::text AS value",
                lowered.sql
            ),
            &[],
        )
        .expect("the lowerer should emit a valid PostgreSQL expression")
        .get::<_, String>("value")
        .parse::<Value>()
        .expect("PostgreSQL JSON output should parse");

    assert_eq!(
        actual,
        expected.value,
        "Postgres/Rust canonical JSON mismatch for {case}; bindings: {}",
        serde_json::to_string(&bindings).expect("generated bindings serialize"),
    );
}

fn generated_context_bindings(seed: &mut u64) -> BTreeMap<String, Value> {
    let amount = next_i64(seed, -20, 20);
    let tax = next_i64(seed, -5, 5);
    let quantity = next_i64(seed, 0, 5);
    let discount = next_i64(seed, -10, 20);
    let name = if next_i64(seed, 0, 1) == 0 {
        "Az"
    } else {
        "Bz"
    };
    let country = if next_i64(seed, 0, 1) == 0 {
        "US"
    } else {
        "CA"
    };
    let limit = if next_i64(seed, 0, 1) == 0 {
        Value::Null
    } else {
        json!(next_i64(seed, -10, 10))
    };

    map([
        ("amount", json!(amount)),
        ("tax", json!(tax)),
        ("quantity", json!(quantity)),
        ("discount", json!(discount)),
        ("name", json!(name)),
        ("lines", json!([1, 2])),
        ("limit", limit),
        (
            "customer",
            json!({
                "addresses": [{"country": country}],
                "tags": [1, 2],
            }),
        ),
    ])
}

fn generated_integer_bindings(seed: &mut u64) -> BTreeMap<String, Value> {
    map([
        ("left", json!(next_i64(seed, -20, 20))),
        ("addend", json!(next_i64(seed, -10, 10))),
        ("multiplier", json!(next_i64(seed, -5, 5))),
        ("divisor", json!(nonzero_i64(seed))),
        ("threshold", json!(next_i64(seed, -30, 30))),
    ])
}

fn generated_decimal_bindings(seed: &mut u64) -> BTreeMap<String, Value> {
    let left_values = ["1.25", "2.75", "10.01"];
    let addend_values = ["0.1", "0.2", "0.3"];
    let multiplier_values = ["1.0", "2.0", "3.0"];
    let divisor_values = ["0.1", "1.0", "3.0"];
    let threshold_values = ["0.1", "1.25", "2.75", "10.01"];

    map([
        ("left", generated_decimal(seed, &left_values)),
        ("addend", generated_decimal(seed, &addend_values)),
        ("multiplier", generated_decimal(seed, &multiplier_values)),
        ("divisor", generated_decimal(seed, &divisor_values)),
        ("threshold", generated_decimal(seed, &threshold_values)),
    ])
}

fn generated_decimal(seed: &mut u64, values: &[&str]) -> Value {
    let index = next_i64(seed, 0, values.len() as i64 - 1) as usize;
    serde_json::from_str(values[index]).expect("the decimal fixture must be valid JSON")
}

fn next_i64(seed: &mut u64, min: i64, max: i64) -> i64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    min + ((*seed >> 32) % (max - min + 1) as u64) as i64
}

fn nonzero_i64(seed: &mut u64) -> i64 {
    let value = next_i64(seed, -5, 5);
    if value == 0 { 1 } else { value }
}
