//! Unit tests for the pure parts of the kit: fixture loading (`!include`),
//! the tests-py-faithful response comparison, and migration-file validation.
//! They need neither Postgres nor a running engine.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::json;

use std::collections::BTreeMap;

use super::*;
use crate::model::substitute;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "donat_conformance_fixture_{tag}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn duplicate_migration_versions_are_rejected() {
    let dir = tempdir("duplicate_migration_versions");
    write(&dir, "V2__first.sql", "SELECT 1;");
    write(&dir, "V2__second.sql", "SELECT 2;");

    let err = apply_sql_migration_dir("postgresql://unused", &dir)
        .expect_err("duplicate migration versions must be rejected");
    assert!(
        format!("{err:#}").contains("duplicate version 2"),
        "{err:#}"
    );

    std::fs::remove_dir_all(dir).expect("remove migration test directory");
}

#[test]
fn load_fixture_resolves_string_include_relative_to_file() {
    // The quoted-string spelling, resolved against the *including*
    // file's directory — including transitively from a subdirectory.
    let dir = tempdir("string");
    write(
        &dir,
        "suite/case.yaml",
        "setup: \"!include sub/inner.yaml\"\nname: top\n",
    );
    write(
        &dir,
        "suite/sub/inner.yaml",
        "deep: \"!include leaf.yaml\"\n",
    );
    write(&dir, "suite/sub/leaf.yaml", "- 1\n- two\n");

    let v = load_fixture(&dir.join("suite/case.yaml")).unwrap();
    assert_eq!(v["name"], json!("top"));
    assert_eq!(v["setup"]["deep"], json!([1, "two"]));
}

#[test]
fn load_fixture_resolves_real_yaml_tag_include() {
    let dir = tempdir("tag");
    write(&dir, "case.yaml", "steps: !include steps.yaml\n");
    write(&dir, "steps.yaml", "- url: /v1/graphql\n  status: 200\n");

    let v = load_fixture(&dir.join("case.yaml")).unwrap();
    assert_eq!(v["steps"][0]["url"], json!("/v1/graphql"));
    assert_eq!(v["steps"][0]["status"], json!(200));
}

#[test]
fn load_fixture_missing_include_target_errors() {
    let dir = tempdir("missing");
    write(&dir, "case.yaml", "setup: \"!include nope.yaml\"\n");
    let err = load_fixture(&dir.join("case.yaml")).unwrap_err();
    assert!(format!("{err:#}").contains("nope.yaml"), "got: {err:#}");
}

#[test]
fn load_fixture_preserves_numbers_and_non_string_keys() {
    let dir = tempdir("scalars");
    write(
        &dir,
        "case.yaml",
        "int: 5\nbig: 18446744073709551615\nfloat: 1.5\nmap:\n  1: one\n",
    );
    let v = load_fixture(&dir.join("case.yaml")).unwrap();
    assert_eq!(v["int"], json!(5));
    assert_eq!(v["big"], json!(18446744073709551615u64));
    assert_eq!(v["float"], json!(1.5));
    // Non-string YAML keys are stringified.
    assert_eq!(v["map"]["1"], json!("one"));
}

// ----------------------------------------- json/response matching

#[test]
fn numbers_coerce_across_int_and_float() {
    assert!(json_matches(&json!(1), &json!(1.0), None));
    assert!(json_matches(&json!(1.0), &json!(1), None));
    assert!(!json_matches(&json!(1), &json!(2.0), None));
    assert!(json_matches(&json!({"n": 1}), &json!({"n": 1.0}), None));
}

#[test]
fn objects_without_selection_tree_compare_unordered() {
    let exp = json!({"a": 1, "b": 2});
    let act = json!({"b": 2, "a": 1});
    assert!(json_matches(&exp, &act, None));
}

#[test]
fn object_key_set_mismatch_fails() {
    // Missing, extra, and renamed keys all fail even order-insensitively.
    assert!(!json_matches(
        &json!({"a": 1}),
        &json!({"a": 1, "b": 2}),
        None
    ));
    assert!(!json_matches(
        &json!({"a": 1, "b": 2}),
        &json!({"a": 1}),
        None
    ));
    assert!(!json_matches(&json!({"a": 1}), &json!({"b": 1}), None));
}

#[test]
fn arrays_require_equal_length_and_order() {
    assert!(json_matches(&json!([1, 2]), &json!([1, 2]), None));
    assert!(!json_matches(&json!([1, 2]), &json!([2, 1]), None));
    assert!(!json_matches(&json!([1, 2]), &json!([1, 2, 3]), None));
}

#[test]
fn data_key_order_is_enforced_per_selection_tree() {
    let query = "query { a b }";
    let exp = json!({"data": {"a": 1, "b": 2}});
    let in_order = json!({"data": {"a": 1, "b": 2}});
    let reordered = json!({"data": {"b": 2, "a": 1}});
    assert!(response_matches(&exp, &in_order, Some(query)));
    assert!(!response_matches(&exp, &reordered, Some(query)));
}

#[test]
fn nested_selection_order_is_enforced_per_level() {
    let query = "query { items { x y } }";
    let exp = json!({"data": {"items": [{"x": 1, "y": 2}]}});
    let good = json!({"data": {"items": [{"x": 1, "y": 2}]}});
    let bad = json!({"data": {"items": [{"y": 2, "x": 1}]}});
    assert!(response_matches(&exp, &good, Some(query)));
    assert!(!response_matches(&exp, &bad, Some(query)));
}

#[test]
fn aliases_key_the_selection_tree() {
    // The response key is the alias; ordering is enforced on aliases.
    let query = "query { first: item { v } second: item { v } }";
    let exp = json!({"data": {"first": {"v": 1}, "second": {"v": 2}}});
    let good = json!({"data": {"first": {"v": 1}, "second": {"v": 2}}});
    let swapped = json!({"data": {"second": {"v": 2}, "first": {"v": 1}}});
    assert!(response_matches(&exp, &good, Some(query)));
    assert!(!response_matches(&exp, &swapped, Some(query)));
}

#[test]
fn fragment_spread_fields_join_the_selection_tree() {
    let query = "
        query { item { ...F } }
        fragment F on Item { p q }
    ";
    let exp = json!({"data": {"item": {"p": 1, "q": 2}}});
    let good = json!({"data": {"item": {"p": 1, "q": 2}}});
    let bad = json!({"data": {"item": {"q": 2, "p": 1}}});
    assert!(response_matches(&exp, &good, Some(query)));
    assert!(
        !response_matches(&exp, &bad, Some(query)),
        "fragment fields must take part in order enforcement"
    );
}

#[test]
fn inline_fragment_fields_join_the_selection_tree() {
    let query = "query { item { ... on Item { p q } } }";
    let exp = json!({"data": {"item": {"p": 1, "q": 2}}});
    let bad = json!({"data": {"item": {"q": 2, "p": 1}}});
    assert!(!response_matches(&exp, &bad, Some(query)));
}

#[test]
fn jsonb_value_under_data_leaf_is_not_order_enforced() {
    // `payload` is a leaf field (no sub-selection): its object value is
    // a jsonb column, where Postgres does not guarantee key order.
    let query = "query { item { payload } }";
    let exp = json!({"data": {"item": {"payload": {"x": 1, "y": 2}}}});
    let act = json!({"data": {"item": {"payload": {"y": 2, "x": 1}}}});
    assert!(response_matches(&exp, &act, Some(query)));
}

#[test]
fn keys_outside_the_selection_tree_are_not_order_enforced() {
    // Only keys present in the selection tree participate in the
    // relative-order check (collapse_order_not_selset semantics).
    let query = "query { a }";
    let exp = json!({"data": {"extra": 0, "a": 1}});
    let act = json!({"data": {"a": 1, "extra": 0}});
    assert!(response_matches(&exp, &act, Some(query)));
}

#[test]
fn errors_compare_unordered() {
    // `errors` is outside `data`: key order inside error objects is free.
    let query = "query { a }";
    let exp = json!({"errors": [{
        "message": "boom",
        "extensions": {"code": "x", "path": "$"}
    }]});
    let act = json!({"errors": [{
        "extensions": {"path": "$", "code": "x"},
        "message": "boom"
    }]});
    assert!(response_matches(&exp, &act, Some(query)));
    // ...but error values still have to match.
    let wrong = json!({"errors": [{
        "message": "other",
        "extensions": {"code": "x", "path": "$"}
    }]});
    assert!(!response_matches(&exp, &wrong, Some(query)));
}

#[test]
fn top_level_response_keys_compare_unordered() {
    let query = "query { a }";
    let exp = json!({"data": {"a": 1}, "errors": [{"message": "partial"}]});
    let act = json!({"errors": [{"message": "partial"}], "data": {"a": 1}});
    assert!(response_matches(&exp, &act, Some(query)));
}

#[test]
fn unparsable_query_disables_order_enforcement() {
    assert!(sel_tree_from_query("not a graphql query {{{").is_none());
    let exp = json!({"data": {"a": 1, "b": 2}});
    let act = json!({"data": {"b": 2, "a": 1}});
    assert!(response_matches(
        &exp,
        &act,
        Some("not a graphql query {{{")
    ));
    assert!(response_matches(&exp, &act, None));
}

#[test]
fn sel_tree_covers_operations_and_marks_leaves() {
    let tree = sel_tree_from_query("mutation { insert_x { affected_rows } }").unwrap();
    assert!(tree.contains_key("insert_x"));
    let child = tree.get("insert_x").unwrap().as_ref().unwrap();
    assert!(child.contains_key("affected_rows"));
    // Leaf fields carry no sub-tree.
    assert!(child.get("affected_rows").unwrap().is_none());
}

// ---------------------------------------------------- subset_matches

#[test]
fn subset_ignores_keys_the_expectation_does_not_mention() {
    assert!(subset_matches(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
    assert!(!subset_matches(&json!({"a": 1, "b": 2}), &json!({"a": 1})));
    assert!(!subset_matches(&json!({"a": 1}), &json!({"a": 2})));
}

#[test]
fn subset_arrays_are_a_claim_about_length() {
    assert!(subset_matches(
        &json!([{"a": 1}]),
        &json!([{"a": 1, "b": 2}])
    ));
    assert!(!subset_matches(&json!([]), &json!([1])));
    assert!(!subset_matches(&json!([1]), &json!([1, 1])));
}

#[test]
fn subset_matchers_any_and_present() {
    assert!(subset_matches(&json!({"id": "@any"}), &json!({"id": null})));
    assert!(!subset_matches(
        &json!({"id": "@present"}),
        &json!({"id": null})
    ));
    assert!(subset_matches(
        &json!({"id": "@present"}),
        &json!({"id": 7})
    ));
    assert!(!subset_matches(&json!({"id": "@present"}), &json!({})));
    assert!(!subset_matches(&json!({"id": "@any"}), &json!({})));
}

#[test]
fn subset_numbers_coerce_like_fixtures() {
    assert!(subset_matches(&json!(1), &json!(1.0)));
    assert!(!subset_matches(&json!("1"), &json!(1)));
}

// ---------------------------------------------------- substitute

#[test]
fn a_whole_string_reference_keeps_the_captured_type() {
    let vars = BTreeMap::from([
        ("amount".to_string(), json!(2659)),
        ("id".to_string(), json!("abc")),
    ]);
    assert_eq!(
        substitute(&json!({"amount_minor": "${amount}"}), &vars).unwrap(),
        json!({"amount_minor": 2659})
    );
    assert_eq!(
        substitute(&json!("order ${id} for ${amount}"), &vars).unwrap(),
        json!("order abc for 2659")
    );
    assert!(substitute(&json!("${missing}"), &vars).is_err());
    assert!(substitute(&json!("x ${missing}"), &vars).is_err());
}

// ---------------------------------------------------- matchers

#[test]
fn typed_matchers() {
    let uuid = json!("550e8400-e29b-41d4-a716-446655440900");
    assert!(subset_matches(&json!("@uuid"), &uuid));
    assert!(!subset_matches(&json!("@uuid"), &json!("not-a-uuid")));
    assert!(subset_matches(&json!("@number"), &json!(2.5)));
    assert!(!subset_matches(&json!("@number"), &json!("2")));
    assert!(subset_matches(&json!("@string"), &json!("x")));
    assert!(subset_matches(&json!("@bool"), &json!(false)));
    assert!(subset_matches(&json!("@gt 0"), &json!(1)));
    assert!(!subset_matches(&json!("@gt 0"), &json!(0)));
    assert!(subset_matches(&json!("@gte 0"), &json!(0)));
    assert!(subset_matches(&json!("@lt 10"), &json!(9.5)));
    assert!(subset_matches(&json!("@lte 10"), &json!(10)));
    assert!(!subset_matches(&json!("@gt 0"), &json!("1")));
    assert!(subset_matches(
        &json!("@regex ^TRACK-\\d+$"),
        &json!("TRACK-1")
    ));
    assert!(!subset_matches(
        &json!("@regex ^TRACK-\\d+$"),
        &json!("track-1")
    ));
    assert!(subset_matches(&json!("@len 2"), &json!([1, 2])));
    assert!(subset_matches(&json!("@len 3"), &json!("abc")));
    assert!(!subset_matches(&json!("@len 2"), &json!([1])));
}

#[test]
fn a_literal_at_sign_is_not_a_matcher() {
    assert!(subset_matches(&json!("@handle"), &json!("@handle")));
    assert!(!subset_matches(&json!("@handle"), &json!("other")));
}

// ---------------------------------------------------- vars and for items

#[test]
fn dotted_references_reach_into_a_bound_item() {
    let vars = BTreeMap::from([("item".to_string(), json!({"role": "staff", "n": 2}))]);
    assert_eq!(
        substitute(
            &json!({"as": {"role": "${item.role}"}, "count": "${item.n}"}),
            &vars
        )
        .unwrap(),
        json!({"as": {"role": "staff"}, "count": 2})
    );
    assert!(substitute(&json!("${item.missing}"), &vars).is_err());
}

#[test]
fn a_for_step_does_not_nest() {
    let raw = json!({"for": [1], "do": [{"for": [2], "do": []}]});
    assert!(crate::model::Step::parse(raw).is_err());
}

#[test]
fn an_await_on_a_query_parses_as_a_polled_sql_step() {
    // `await: {sql: …}` and `await: {row: …}` are different shapes of the same
    // step kind, and an untagged enum resolves them by which key is present.
    let raw = json!({
        "await": {
            "sql": "select status from notification.delivery where channel = 'email'",
            "expect": [{"status": "sent"}],
            "capture": {"status": "status"},
        }
    });
    let Ok(crate::model::Step::Await(step)) = crate::model::Step::parse(raw) else {
        panic!("an await step with a sql query is an await step");
    };
    let crate::model::Await::Rows {
        sql,
        expect,
        capture,
    } = step.what
    else {
        panic!("a sql key selects the polled-query variant, not the first-row one");
    };
    assert!(sql.contains("notification.delivery"));
    assert_eq!(expect, vec![json!({"status": "sent"})]);
    assert_eq!(capture.get("status").map(String::as_str), Some("status"));

    // No `expect` is not "wait for anything": a wait with nothing to wait for
    // would spin until the deadline, so the shape refuses it.
    assert!(crate::model::Step::parse(json!({"await": {"sql": "select 1"}})).is_err());
}

#[test]
fn a_config_takes_one_migrations_directory_or_several() {
    // A deployment applies more than one versioned set as soon as it adopts a
    // module, and the order is the deployment's: the module's schema, then the
    // application's, whose binding migration replaces a view the first created.
    let root = tempdir("app_config");
    write(
        &root,
        "donat.test.yaml",
        "metadata: metadata\nmigrations:\n  - ../module/migrations\n  - migrations\n",
    );
    let many = crate::AppTestConfig::load(&root).expect("a list of directories loads");
    assert_eq!(
        many.migrations,
        vec![root.join("../module/migrations"), root.join("migrations")],
        "each entry resolves against the config file's directory, in order"
    );

    // One string still means a list of one — the shape every application that
    // adopts nothing already writes.
    write(&root, "donat.test.yaml", "migrations: migrations\n");
    let one = crate::AppTestConfig::load(&root).expect("a single directory loads");
    assert_eq!(one.migrations, vec![root.join("migrations")]);
    assert_eq!(
        one.metadata,
        root.join("metadata"),
        "metadata has a default"
    );

    // And an application with no migrations of its own says nothing.
    write(&root, "donat.test.yaml", "metadata: metadata\n");
    let none = crate::AppTestConfig::load(&root).expect("no migrations loads");
    assert!(none.migrations.is_empty());
}
