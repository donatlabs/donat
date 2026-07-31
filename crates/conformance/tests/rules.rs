//! Deploy-time conformance for the declarative `rules.yaml` wrapper.
//!
//! These are Donat-owned fixtures. They exercise the binary's validate path,
//! not an HTTP, GraphQL, REST, or MCP rules surface.

use std::path::{Path, PathBuf};
use std::process::Command;

use donat_conformance::{engine_binary, pg_admin_url};

const DUPLICATE_NAMES: &str = include_str!("../fixtures/rules/duplicate_names.yaml");
const INVALID_SOURCE: &str = include_str!("../fixtures/rules/invalid_source.yaml");
const INVALID_TYPE: &str = include_str!("../fixtures/rules/invalid_type.yaml");
const INVALID_TYPE_LOCATION: &str = include_str!("../fixtures/rules/invalid_type_location.yaml");
const INVALID_DECISION_CONDITION_LOCATION: &str =
    include_str!("../fixtures/rules/invalid_decision_condition_location.yaml");
const MISSING_FIRST_DEFAULT: &str = include_str!("../fixtures/rules/missing_first_default.yaml");
const UNIQUE_NO_MATCH: &str = include_str!("../fixtures/rules/unique_no_match.yaml");
const UNIQUE_MULTIPLE_MATCHES: &str =
    include_str!("../fixtures/rules/unique_multiple_matches.yaml");
const FAILING_TEST_CASE: &str = include_str!("../fixtures/rules/failing_test_case.yaml");
const OBJECT_ENUM_VALID: &str = include_str!("../fixtures/rules/object_enum_valid.yaml");
const TYPE_CYCLE: &str = include_str!("../fixtures/rules/type_cycle.yaml");
const DUPLICATE_TYPE: &str = include_str!("../fixtures/rules/duplicate_type.yaml");
const PRIMITIVE_TYPE_COLLISION: &str =
    include_str!("../fixtures/rules/primitive_type_collision.yaml");
const UNKNOWN_TYPE_REFERENCE: &str = include_str!("../fixtures/rules/unknown_type_reference.yaml");
const AMBIGUOUS_TYPE_BODY: &str = include_str!("../fixtures/rules/ambiguous_type_body.yaml");

fn with_db(admin_url: &str, db: &str) -> String {
    let (prefix, _) = admin_url.rsplit_once('/').expect("PG_URL has a db path");
    format!("{prefix}/{db}")
}

fn fresh_db(name: &str) -> String {
    let admin = pg_admin_url();
    let mut client = postgres::Client::connect(&admin, postgres::NoTls)
        .expect("connect to PG_URL (is postgres up?)");
    client
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .expect("drop rules test database");
    client
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .expect("create rules test database");
    with_db(&admin, name)
}

fn metadata_dir(case: &str, rules: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dist_rules_{case}_{}_{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("main")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("databases")).expect("create metadata directory");
    std::fs::write(dir.join("version.yaml"), "version: 3\n").expect("write version");
    std::fs::write(
        dir.join("databases/databases.yaml"),
        "- name: default\n  kind: postgres\n  configuration:\n    connection_info:\n      database_url:\n        from_env: DONAT_DATABASE_URL\n  tables: []\n",
    )
    .expect("write source metadata");
    if let Some(rules) = rules {
        std::fs::write(dir.join("rules.yaml"), rules).expect("write rules metadata");
    }
    dir
}

fn validate(db_url: &str, metadata_dir: &Path) -> (bool, String) {
    let output = Command::new(engine_binary())
        .args(["validate", "--metadata-dir"])
        .arg(metadata_dir)
        .env("DONAT_DATABASE_URL", db_url)
        .output()
        .expect("run donat validate");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), combined)
}

#[test]
fn validate_rejects_invalid_declarative_rule_metadata_with_the_rules_path() {
    let db = fresh_db("conf_rules_validate");
    let cases = [
        ("duplicate_names", DUPLICATE_NAMES, "duplicate rule name"),
        ("invalid_source", INVALID_SOURCE, "undeclared binding"),
        ("invalid_type", INVALID_TYPE, "unsupported rule type"),
        (
            "missing_first_default",
            MISSING_FIRST_DEFAULT,
            "missing its final all-true default row",
        ),
        ("unique_no_match", UNIQUE_NO_MATCH, "test case"),
        (
            "unique_multiple_matches",
            UNIQUE_MULTIPLE_MATCHES,
            "test case",
        ),
        ("failing_test_case", FAILING_TEST_CASE, "test case"),
    ];

    for (case, rules, expected) in cases {
        let metadata = metadata_dir(case, Some(rules));
        let (ok, output) = validate(&db, &metadata);
        assert!(
            !ok,
            "{case}: malformed rules metadata unexpectedly validated:\n{output}"
        );
        assert!(
            output.contains("rules.yaml"),
            "{case}: validate must identify the metadata path:\n{output}"
        );
        assert!(
            output.contains(expected),
            "{case}: validate must identify the failed rule contract ({expected}):\n{output}"
        );
    }
}

#[test]
fn validate_reports_semantic_rule_locations() {
    let db = fresh_db("conf_rules_invalid_type_location");
    let metadata = metadata_dir("invalid_type_location", Some(INVALID_TYPE_LOCATION));
    let (ok, output) = validate(&db, &metadata);

    assert!(
        !ok,
        "invalid rule metadata unexpectedly validated:\n{output}"
    );
    assert!(
        output.contains("rules.yaml.rules[0].expression"),
        "validation must report the expression metadata path:\n{output}"
    );
    assert!(
        output.contains("bytes 0..7"),
        "validation must report the semantic source span:\n{output}"
    );
    assert!(
        output.contains("undeclared binding `missing`"),
        "validation must preserve the semantic diagnostic message:\n{output}"
    );
    assert!(
        output.contains("rule `undeclared_binding_location`"),
        "validation must identify the rule expression owner:\n{output}"
    );
}

#[test]
fn validate_reports_semantic_decision_condition_locations() {
    let db = fresh_db("conf_rules_invalid_decision_condition_location");
    let metadata = metadata_dir(
        "invalid_decision_condition_location",
        Some(INVALID_DECISION_CONDITION_LOCATION),
    );
    let (ok, output) = validate(&db, &metadata);

    assert!(
        !ok,
        "invalid decision condition metadata unexpectedly validated:\n{output}"
    );
    assert!(
        output.contains("rules.yaml.decision_tables[0].rows[0].when.amount"),
        "validation must report the decision condition metadata path:\n{output}"
    );
    assert!(
        output.contains("bytes 0..13"),
        "validation must report the decision condition source span:\n{output}"
    );
    assert!(
        output.contains("matching int or decimal operands"),
        "validation must preserve the decision condition semantic message:\n{output}"
    );
    assert!(
        output.contains("decision table `invoice_route` row `invalid_amount` condition `amount`"),
        "validation must identify the decision condition owner:\n{output}"
    );
}

#[test]
fn validate_accepts_an_ordinary_metadata_directory_without_rules() {
    let db = fresh_db("conf_rules_no_rules");
    let metadata = metadata_dir("no_rules", None);
    let (ok, output) = validate(&db, &metadata);
    assert!(ok, "metadata without rules must remain valid:\n{output}");
}

#[test]
fn validate_resolves_declared_object_and_enum_types_before_publishing_metadata() {
    let db = fresh_db("conf_rules_object_enum_types");
    let metadata = metadata_dir("object_enum_valid", Some(OBJECT_ENUM_VALID));
    let (ok, output) = validate(&db, &metadata);
    assert!(
        ok,
        "declared object and enum types must validate:\n{output}"
    );
}

#[test]
fn validate_rejects_non_finite_or_unknown_declared_rule_types() {
    let db = fresh_db("conf_rules_invalid_declared_types");
    let cases = [
        ("type_cycle", TYPE_CYCLE, "cycle"),
        (
            "duplicate_type",
            DUPLICATE_TYPE,
            "duplicate declared rule type",
        ),
        (
            "primitive_type_collision",
            PRIMITIVE_TYPE_COLLISION,
            "collides with scalar profile type",
        ),
        (
            "unknown_type_reference",
            UNKNOWN_TYPE_REFERENCE,
            "unknown declared rule type",
        ),
        (
            "ambiguous_type_body",
            AMBIGUOUS_TYPE_BODY,
            "exactly one of object, enum, or opaque_json",
        ),
    ];

    for (case, rules, expected) in cases {
        let metadata = metadata_dir(case, Some(rules));
        let (ok, output) = validate(&db, &metadata);
        assert!(
            !ok,
            "{case}: invalid declared types unexpectedly validated:\n{output}"
        );
        assert!(
            output.contains("rules.yaml.types"),
            "{case}: missing type path:\n{output}"
        );
        assert!(
            output.contains(expected),
            "{case}: missing error detail:\n{output}"
        );
    }
}

#[test]
fn validate_rule_errors_identify_the_metadata_path_without_echoing_expression_source() {
    let db = fresh_db("conf_rules_source_redaction");
    let source = "unexposed_rule_source_7e3c";
    let rules = format!(
        "rules:\n  - name: invalid_source_redaction\n    result: bool!\n    expression: \"{source} +\"\n"
    );
    let metadata = metadata_dir("source_redaction", Some(&rules));
    let (ok, output) = validate(&db, &metadata);

    assert!(
        !ok,
        "invalid rule metadata unexpectedly validated:\n{output}"
    );
    assert!(
        output.contains("rules.yaml"),
        "validation must retain the deploy-time metadata location:\n{output}"
    );
    assert!(
        !output.contains(source),
        "rule source must not appear in validation errors:\n{output}"
    );
}
