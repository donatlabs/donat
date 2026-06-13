//! Ported from tests-py test_v1_queries.py (legacy /v1/query data API).
//! No-role requests on a trusted connection are the `admin` superuser
//! (admin role is now implemented), so admin steps are in scope.
//!
//! The v1 data API has no websocket transport, so every case is Http.
//!
//! Note: tests-py's validate.py only WARNS when expected and actual are both
//! error bodies, so the v1 error *shapes* below were never enforced by
//! pytest. The native harness compares them strictly; the engine's v1 error
//! shapes were aligned to Hasura's exact bodies as part of this port.

use dist_conformance::{Suite, Transport};

const SELECT: &str = "queries/v1/select/permissions";
const COUNT: &str = "queries/v1/count/permissions";
const INSERT: &str = "queries/v1/insert/permissions";
const UPDATE: &str = "queries/v1/update/permissions";

/// `@usefixtures('postgis', 'per_class_tests_db_state')` — class-scoped
/// setup.yaml/teardown.yaml via /v1/query (postgis is created by the
/// harness at suite start).
#[test]
fn v1_select_permissions() {
    let s = Suite::new("v1_select_perms").start();
    s.setup_v1q(&format!("{SELECT}/setup.yaml"));
    for f in [
        "user_select_query_unpublished_articles.yaml",
        "user_can_query_other_users_published_articles.yaml",
        "anonymous_can_only_get_published_articles.yaml",
        // user_cannot_access_remarks_col.yaml: step[1] is a no-role (admin)
        // request — now covered (admin role implemented).
        "user_cannot_access_remarks_col.yaml",
        "user_can_query_geometry_values_filter.yaml",
        "user_can_query_geometry_values_filter_session_vars.yaml",
        "user_can_query_geog_filter.yaml",
        "user_can_query_geog_filter_session_vars.yaml",
        "user_can_query_jsonb_values_filter.yaml",
        "user_can_query_jsonb_values_filter_session_vars.yaml",
        "user_query_auction.yaml",
    ] {
        s.check_query_f(&format!("{SELECT}/{f}"), Transport::Http);
    }
    s.teardown_v1q(&format!("{SELECT}/teardown.yaml"));
}

/// `@usefixtures('per_class_tests_db_state')`.
#[test]
fn v1_count_permissions() {
    let s = Suite::new("v1_count_perms").start();
    s.setup_v1q(&format!("{COUNT}/setup.yaml"));
    s.check_query_f(
        &format!("{COUNT}/count_user_has_no_select_perm_error.yaml"),
        Transport::Http,
    );
    s.check_query_f(
        &format!("{COUNT}/count_users_unpublished_articles.yaml"),
        Transport::Http,
    );
    s.teardown_v1q(&format!("{COUNT}/teardown.yaml"));
}

/// `use_mutation_fixtures`: schema_setup.yaml once, then per test
/// values_setup.yaml -> case -> values_teardown.yaml (all /v1/query),
/// schema_teardown.yaml at the end. tests-py runs values_teardown
/// unconditionally (may_skip_test_teardown is ignored by this conftest).
fn run_mutation_suite(s: &dist_conformance::Running, dir: &str, cases: &[&str]) {
    s.setup_v1q(&format!("{dir}/schema_setup.yaml"));
    for f in cases {
        s.setup_v1q(&format!("{dir}/values_setup.yaml"));
        s.check_query_f(&format!("{dir}/{f}"), Transport::Http);
        s.teardown_v1q(&format!("{dir}/values_teardown.yaml"));
    }
    s.teardown_v1q(&format!("{dir}/schema_teardown.yaml"));
}

#[test]
fn v1_insert_permissions() {
    let s = Suite::new("v1_insert_perms").start();
    run_mutation_suite(
        &s,
        INSERT,
        &[
            "article_on_conflict_user_role.yaml",
            "author_on_conflict_ignore_user_role.yaml",
            "address_permission_error.yaml",
            "author_user_role_insert_check_perm_success.yaml",
            "author_user_role_insert_check_is_registered_fail.yaml",
            "author_user_role_insert_check_user_id_fail.yaml",
            "author_student_role_insert_check_bio_success.yaml",
            "author_student_role_insert_check_bio_fail.yaml",
            "resident_1_modifies_resident_2_upsert.yaml",
        ],
    );
}

#[test]
fn v1_update_permissions() {
    let s = Suite::new("v1_update_perms").start();
    run_mutation_suite(
        &s,
        UPDATE,
        &[
            "user_can_update_unpublished_article.yaml",
            "user_cannot_update_published_article_version.yaml",
            "user_cannot_update_another_users_article.yaml",
            "user_cannot_update_id_col_article.yaml",
            "user_update_resident_preset_error.yaml",
            "user_update_resident_preset.yaml",
            "user_update_resident_preset_session_var.yaml",
        ],
    );
}
