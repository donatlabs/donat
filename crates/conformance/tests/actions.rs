//! Ported from tests-py test_actions.py (synchronous actions).
//!
//! tests-py runs these as admin; this engine has no admin role, so each
//! action is granted to an explicit `user` role and role-less requests fall
//! back to it via `HASURA_GRAPHQL_UNAUTHORIZED_ROLE`. The webhook handler is a
//! native Rust stub (`action_webhook`) mirroring `ActionsWebhookHandler` in
//! tests-py/context.py; the engine reaches it through `ACTION_WEBHOOK_HANDLER`.

use dist_conformance::{Running, Suite, Transport};
use serde_json::json;

const SYNC: &str = "queries/actions/sync";

/// Start an actions suite: webhook stub running, schema + custom types +
/// actions loaded, and every action granted to the `user` role (the
/// unauthorized-role fallback) so role-less fixtures resolve.
fn sync_suite() -> Running {
    let s = Suite::new("actions_sync")
        .env("HASURA_GRAPHQL_UNAUTHORIZED_ROLE", "user")
        .with_action_webhook()
        .start();
    s.setup_v1q(&format!("{SYNC}/schema_setup.yaml"));
    // tests-py runs as admin; grant every action to the `user` role instead.
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
    ] {
        s.post(
            "/v1/query",
            &json!({
                "type": "create_action_permission",
                "args": { "action": action, "role": "user" }
            }),
            &[],
        );
    }
    s
}

/// Single-step sync action cases that resolve against the boot-time metadata.
/// (Multi-step `update_action`-then-query cases are out of scope: this engine
/// has no runtime metadata API — see tests/hasura/COVERAGE.md.)
#[test]
fn sync_actions() {
    let s = sync_suite();
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
