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
const MISSING_FIRST_DEFAULT: &str = include_str!("../fixtures/rules/missing_first_default.yaml");
const UNIQUE_NO_MATCH: &str = include_str!("../fixtures/rules/unique_no_match.yaml");
const UNIQUE_MULTIPLE_MATCHES: &str =
    include_str!("../fixtures/rules/unique_multiple_matches.yaml");
const FAILING_TEST_CASE: &str = include_str!("../fixtures/rules/failing_test_case.yaml");

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
fn validate_accepts_an_ordinary_metadata_directory_without_rules() {
    let db = fresh_db("conf_rules_no_rules");
    let metadata = metadata_dir("no_rules", None);
    let (ok, output) = validate(&db, &metadata);
    assert!(ok, "metadata without rules must remain valid:\n{output}");
}
