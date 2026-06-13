//! Ported from tests-py test_actions.py (synchronous actions).
//!
//! tests-py runs these as admin; this engine has no admin role, so each
//! action is granted to an explicit `tester` role (role-less requests fall
//! back to it via `HASURA_GRAPHQL_UNAUTHORIZED_ROLE`). A dedicated role avoids
//! the restrictive `user`-role permission the shared schema_setup installs.
//! The webhook handler is a
//! native Rust stub (`action_webhook`) mirroring `ActionsWebhookHandler` in
//! tests-py/context.py; the engine reaches it through `ACTION_WEBHOOK_HANDLER`.
//! Webhook handlers that call back into the engine run under the same role.

use donat_conformance::{Running, Suite, Transport};
use serde_json::{json, Value as Json};

const SYNC: &str = "queries/actions/sync";

/// Start an actions suite: webhook stub running, schema + custom types +
/// actions loaded, every action granted to the `tester` role, and table
/// permissions for `tester` so the webhook callbacks and output relationships
/// can read/write through the GraphQL API.
fn sync_suite(name: &str) -> Running {
    let s = Suite::new(name)
        .env("HASURA_GRAPHQL_UNAUTHORIZED_ROLE", "tester")
        .with_action_webhook()
        .start();
    s.setup_v1q(&format!("{SYNC}/schema_setup.yaml"));

    for action in [
        "create_user",
        "create_users",
        "mirror",
        "null_response",
        "omitted_response_field",
        "scalar_response",
        "pgscalar_response",
        "custom_scalar_response",
        "scalar_array_response",
        "custom_scalar_array_response",
        "recursive_output",
        "typed_nested_null",
        "intentional_error",
        "get_user_by_email",
        "get_users_by_email",
        "get_user_by_email_nested",
    ] {
        op(&s, json!({
            "type": "create_action_permission",
            "args": { "action": action, "role": "tester" }
        }));
    }

    // user/article table permissions for the role the callbacks run as.
    op(&s, json!({
        "type": "create_select_permission",
        "args": { "table": "user", "role": "tester",
                  "permission": { "columns": "*", "filter": {} } }
    }));
    op(&s, json!({
        "type": "create_insert_permission",
        "args": { "table": "user", "role": "tester",
                  "permission": { "columns": ["email", "name"], "check": {} } }
    }));
    op(&s, json!({
        "type": "create_select_permission",
        "args": { "table": "article", "role": "tester",
                  "permission": { "columns": "*", "filter": {} } }
    }));
    s
}

/// Apply a metadata/SQL op in-harness (accumulates into boot metadata before
/// the engine starts; runs SQL against the suite DB).
fn op(s: &Running, doc: Json) {
    s.post("/v1/query", &doc, &[]);
}

/// Reset the `user` table so auto-increment ids start at 1 again.
fn reset_users(s: &Running) {
    op(s, json!({
        "type": "run_sql",
        "args": { "sql": "DELETE FROM \"user\"; SELECT setval('user_id_seq', 1, false);" }
    }));
}

fn seed_user(s: &Running, name: &str, email: &str) {
    op(s, json!({
        "type": "run_sql",
        "args": { "sql": format!(
            "INSERT INTO \"user\"(name, email) VALUES ('{name}', '{email}')"
        ) }
    }));
}

/// Single-step sync action cases that resolve against the boot-time metadata.
/// (Multi-step `update_action`-then-query cases are out of scope: this engine
/// has no runtime metadata API — see tests/hasura/COVERAGE.md.)
#[test]
fn sync_actions() {
    let s = sync_suite("actions_sync");
    for file in [
        // Successful response shaping.
        "mirror_action_success.yaml",
        "mirror_action_unexpected_field.yaml",
        "null_response.yaml",
        "omitted_field_response_for_nullable_field.yaml",
        "get_scalar_action_output_type_success.yaml",
        "expecting_object_response_with_nested_null.yaml",
        "expecting_jsonb_response_success.yaml",
        "expecting_custom_scalar_response_success.yaml",
        "expecting_custom_scalar_array_response_success.yaml",
        "get_string_scalar_array_action_output_type_success.yaml",
        // query_action_recursive_output.yaml: Hasura omits (vs nulls) a
        // selected-but-absent nullable field only at deep nesting — an
        // inconsistency with the top-level omitted-field behaviour we follow.
        // Out of scope; see tests/hasura/COVERAGE.md.
        // Output-validation errors (internal diagnostic trimmed).
        "mirror_action_not_null.yaml",
        "mirror_action_no_field.yaml",
        // Webhook-error surfacing (handler 4xx with message/code/extensions).
        "extensions_code_both_codes.yaml",
        "extensions_code_only_extensions_code.yaml",
        "extensions_code_only_empty_extensions.yaml",
        "extensions_code_nothing.yaml",
        "extensions_code_toplevel_empty_extensions.yaml",
        "extensions_code_toplevel_no_extensions.yaml",
    ] {
        s.check_query_f(&format!("{SYNC}/{file}"), Transport::Http);
    }
}

/// Actions whose webhook handler calls back into the engine over GraphQL, and
/// whose output objects relate to tracked tables. The real end-to-end HTTP
/// hook: input → webhook → engine mutation/query → output shaping +
/// relationship resolution under the role's permissions.
#[test]
fn engine_callback_actions() {
    let s = sync_suite("actions_cb");

    // create_user: webhook inserts a user, output relationship reads it back.
    reset_users(&s);
    s.check_query_f(&format!("{SYNC}/create_user_success.yaml"), Transport::Http);

    reset_users(&s);
    s.check_query_f(&format!("{SYNC}/create_user_fail.yaml"), Transport::Http);

    // create_users: list output with per-row relationships.
    reset_users(&s);
    s.check_query_f(&format!("{SYNC}/create_users_success.yaml"), Transport::Http);

    reset_users(&s);
    s.check_query_f(&format!("{SYNC}/create_users_fail.yaml"), Transport::Http);

    // get_user_by_email: query action returning a single object (seed first).
    reset_users(&s);
    seed_user(&s, "Clarke", "clarke@gmail.com");
    s.check_query_f(&format!("{SYNC}/get_user_by_email_success.yaml"), Transport::Http);

    s.check_query_f(&format!("{SYNC}/get_user_by_email_fail.yaml"), Transport::Http);

    // get_users_by_email: query action returning a list.
    reset_users(&s);
    seed_user(&s, "Clarke 1", "clarke@gmail.com");
    seed_user(&s, "Clarke 2", "clarke@gmail.com");
    s.check_query_f(&format!("{SYNC}/get_users_by_email_success.yaml"), Transport::Http);

    // get_user_by_email_nested: nested custom objects (no table relationship),
    // shaped from the webhook around the engine-fetched id.
    reset_users(&s);
    seed_user(&s, "Clarke", "clarke@gmail.com");
    s.check_query_f(&format!("{SYNC}/get_user_by_email_nested_success.yaml"), Transport::Http);
}
