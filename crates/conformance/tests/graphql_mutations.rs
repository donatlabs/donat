//! Ported from tests-py test_graphql_mutations.py (permission classes).
//! No-role requests on a trusted connection are the `admin` superuser
//! (admin role is now implemented), so admin steps are in scope.
//!
//! These pytest classes use `per_class_db_schema_for_mutation_tests` +
//! `per_method_db_data_for_mutation_tests`: `schema_setup.yaml` is applied
//! once per class, then EVERY test case is wrapped in `values_setup.yaml` /
//! `values_teardown.yaml` (mutations mutate data), and `schema_teardown.yaml`
//! runs at the end — all via /v1/query (default backend, metadata API v1).

use donat_conformance::Suite;
use donat_conformance::{Running, Transport};

/// One mutation case, wrapped in the per-method data fixtures.
fn check_mutation(s: &Running, dir: &str, file: &str) {
    s.apply_if_exists(&format!("{dir}/values_setup.yaml"), "/v1/query");
    s.check_query_f(&format!("{dir}/{file}"), Transport::Http);
    s.apply_if_exists(&format!("{dir}/values_teardown.yaml"), "/v1/query");
}

const INSERT: &str = "queries/graphql_mutation/insert/permissions";

#[test]
fn graphql_insert_permission() {
    let s = Suite::new("insert_perms").start();
    s.setup_v1q(&format!("{INSERT}/schema_setup.yaml"));

    // pytest calls check_query_f without a transport param -> http only.
    let cases = [
        "article_on_conflict_user_role.yaml",
        "article_on_conflict_restricted_role.yaml",
        "article_on_conflict_constraint_on_user_role_error.yaml",
        "author_on_conflict_ignore_user_role.yaml",
        "user_article_on_conflict_error_missing_article_constraint.yaml",
        "user_article_error_unexpected_on_conflict_action.yaml",
        "user_article_unexpected_on_conflict_constraint_error.yaml",
        "address_permission_error.yaml",
        "author_user_role_insert_check_perm_success.yaml",
        "author_user_role_insert_check_is_registered_fail.yaml",
        "author_user_role_insert_check_user_id_fail.yaml",
        "author_student_role_insert_check_bio_success.yaml",
        "author_student_role_insert_check_bio_fail.yaml",
        "company_user_role.yaml",
        "company_user_role_on_conflict.yaml",
        "resident_user.yaml",
        "resident_infant.yaml",
        "resident_infant_fail.yaml",
        "resident_5_modifies_resident_6_upsert.yaml",
        // resident_on_conflict_where.yaml: no-role (admin) request — out of
        // scope (this engine has no admin role).
        "blog_on_conflict_update_preset.yaml",
        "insert_article_arr_sess_var_editor_allowed_user_id.yaml",
        // Status-only known-diff (fixture patched 400 -> 200, see COVERAGE.md):
        "insert_article_arr_sess_var_editors_err_not_allowed_user_id.yaml",
        "seller_insert_computer_has_keys_all_pass.yaml",
        // Status-only known-diff (fixture patched 400 -> 200, see COVERAGE.md):
        "seller_insert_computer_has_keys_all_fail.yaml",
        "developer_insert_has_keys_any_pass.yaml",
        // Status-only known-diff (fixture patched 400 -> 200, see COVERAGE.md):
        "developer_insert_has_keys_any_fail.yaml",
        "user_insert_account_success.yaml",
        "user_insert_account_fail.yaml",
        // The backend_user cases use check_query_admin_secret, which adds the
        // admin-secret header only when one is configured; this suite (like
        // the pytest default run) has none, so they run as plain role'd cases.
        "backend_user_insert_fail.yaml",
        "backend_user_insert_pass.yaml",
        "backend_user_insert_invalid_bool.yaml",
        "user_with_no_backend_privilege.yaml",
        // backend_user_no_admin_secret_fail.yaml: pytest skips it unless
        // admin-secret + JWT/webhook auth is configured — out of scope here.
        "leads_upsert_check_with_headers.yaml",
        "column_comparison_across_tables.yaml",
    ];
    for f in cases {
        check_mutation(&s, INSERT, f);
    }

    s.teardown_v1q(&format!("{INSERT}/schema_teardown.yaml"));
}

const UPDATE: &str = "queries/graphql_mutation/update/permissions";

#[test]
fn graphql_update_permissions() {
    let s = Suite::new("update_perms").start();
    s.setup_v1q(&format!("{UPDATE}/schema_setup.yaml"));

    let cases = [
        "user_update_author.yaml",
        "user_can_update_unpublished_article.yaml",
        "user_cannot_update_published_article_version.yaml",
        "user_cannot_update_another_users_article.yaml",
        "user_cannot_publish.yaml",
        "user_cannot_update_id_col_article.yaml",
        "user_update_resident_preset.yaml",
        "user_update_resident_preset_session_var.yaml",
        "user_account_update_success.yaml",
        "user_account_update_no_rows.yaml",
    ];
    for f in cases {
        check_mutation(&s, UPDATE, f);
    }

    s.teardown_v1q(&format!("{UPDATE}/schema_teardown.yaml"));
}

const DELETE: &str = "queries/graphql_mutation/delete/permissions";

#[test]
fn graphql_delete_permissions() {
    let s = Suite::new("delete_perms").start();
    s.setup_v1q(&format!("{DELETE}/schema_setup.yaml"));

    let cases = [
        "author_can_delete_his_articles.yaml",
        "author_cannot_delete_other_users_articles.yaml",
        "resident_delete_without_select_perm_fail.yaml",
        "agent_delete_perm_arr_sess_var.yaml",
        "agent_delete_perm_arr_sess_var_fail.yaml",
        "user_delete_author.yaml",
        "user_delete_author_by_pk.yaml",
        "user_delete_account_success.yaml",
        "user_delete_account_no_rows.yaml",
    ];
    for f in cases {
        check_mutation(&s, DELETE, f);
    }

    s.teardown_v1q(&format!("{DELETE}/schema_teardown.yaml"));
}
