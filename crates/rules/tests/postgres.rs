use std::collections::BTreeMap;

use donat_rules::{
    RuleDefinition, RuleError, RuleType, SqlBinding, SqlBindings, SqlExpression, compile_catalog,
    evaluate_bool, lower_postgres,
};
use postgres::{Client, NoTls};
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
    let catalog = compile_catalog(
        &[RuleDefinition {
            name: "checkout_policy".to_owned(),
            bindings,
            result: RuleType::Bool,
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
}

#[test]
fn lowerer_extracts_nested_typed_json_objects_and_lists() {
    let bindings = map([(
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
            ]),
        ),
    )]);
    let rule = compiled_rule(
        bindings.clone(),
        "customer.addresses[0].country == 'US' && size(customer.tags) == 2",
    );

    let sql = lower_postgres(&rule, &typed_columns(&bindings))
        .expect("typed JSON columns should lower static object and list access");

    insta::assert_snapshot!(sql);
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
            expression.contains("trim_scale(trunc"),
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
                ]),
            ),
        ),
    ]);
    let context_rule = compiled_rule(
        context_bindings,
        "amount + tax * quantity >= discount - 5 && size(name) >= 2 && startsWith(name, 'A') && endsWith(name, 'z') && size(lines) == 2 && (limit == null || is_null(limit)) && customer.addresses[0].country == 'US' && size(customer.tags) == 2",
    );
    let integer_rule = compiled_rule(
        map([
            ("left", RuleType::Int),
            ("addend", RuleType::Int),
            ("multiplier", RuleType::Int),
            ("divisor", RuleType::Int),
            ("threshold", RuleType::Int),
        ]),
        "-left + addend * multiplier / divisor >= threshold - 5",
    );
    let decimal_rule = compiled_rule(
        map([
            ("left", RuleType::Decimal),
            ("addend", RuleType::Decimal),
            ("multiplier", RuleType::Decimal),
            ("divisor", RuleType::Decimal),
            ("threshold", RuleType::Decimal),
        ]),
        "-left + addend * multiplier / divisor >= threshold - 0.333333333333333333",
    );
    let mut client = postgres_client();
    let mut seed = 0x7A11_CE55_u64;

    for case_index in 0..128 {
        assert_postgres_matches_rust(
            &mut client,
            &context_rule,
            generated_context_bindings(&mut seed),
            &format!("typed context case {case_index}"),
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
    let expected = evaluate_bool(rule, &bindings)
        .expect("the generated values must remain inside the typed profile");
    let sql_bindings = SqlBindings::new(
        bindings
            .iter()
            .map(|(name, value)| (name.clone(), SqlBinding::literal(value.clone()))),
    );
    let expression = lower_postgres(rule, &sql_bindings)
        .expect("every generated closed context should lower safely");
    let actual = client
        .query_one(&format!("SELECT {expression} AS value"), &[])
        .expect("the lowerer should emit a valid PostgreSQL expression")
        .get::<_, Option<bool>>("value")
        .expect("a typed boolean rule must not produce SQL NULL");

    assert_eq!(
        actual,
        expected,
        "Postgres/Rust rule mismatch for {case}; bindings: {}",
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
