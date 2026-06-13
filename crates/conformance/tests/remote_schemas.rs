//! Ported from tests-py test_remote_schema_permissions.py
//! (`TestRemoteSchemaPermissionsExecution`): role-scoped execution against a
//! remote GraphQL schema.
//!
//! The upstream is a native Rust stub (`remote_graphql`) replacing tests-py's
//! Node/Apollo `remote_schema_perms.js`; the engine reaches it through the
//! `GRAPHQL_SERVICE_1` env var (`url: "{{GRAPHQL_SERVICE_1}}"`). The role's SDL
//! comes from `add_remote_schema_permissions`; the engine validates each
//! request against it and forwards only what the role may see. tests-py runs
//! the "admin" steps with no role — out of scope here (no admin role), so the
//! ported fixtures keep only the role-scoped steps.

use donat_conformance::{Running, Suite, Transport};
use serde_json::json;

const PERMS: &str = "queries/remote_schemas/permissions";

/// The role's view of the upstream schema: `user_id` (but not `gimmeText`) on
/// User, and `messages(where:)` (but not `includes:`). The omissions drive the
/// validation-error cases in the execution fixtures.
const USER_SDL: &str = r#"
type User {
  user_id: Int
}

type Message {
  id: Int!
  name: String!
  msg: String!
}

input MessageWhereInpObj {
  id: IntCompareObj
  name: StringCompareObj
}

input IntCompareObj { eq: Int gt: Int lt: Int }
input StringCompareObj { eq: String }

type Query {
  hello: String
  messages(where: MessageWhereInpObj): [Message]
  user(user_id: Int!): User
}

schema { query: Query }
"#;

fn remote_suite() -> Running {
    let s = Suite::new("remote_schema_perms")
        .with_remote_graphql("GRAPHQL_SERVICE_1")
        .start();

    s.post(
        "/v1/metadata",
        &json!({
            "type": "add_remote_schema",
            "args": {
                "name": "my-remote-schema",
                "definition": {
                    "url": "{{GRAPHQL_SERVICE_1}}",
                    "forward_client_headers": false
                }
            }
        }),
        &[],
    );
    s.post(
        "/v1/metadata",
        &json!({
            "type": "add_remote_schema_permissions",
            "args": {
                "remote_schema": "my-remote-schema",
                "role": "user",
                "definition": { "schema": USER_SDL }
            }
        }),
        &[],
    );
    s
}

#[test]
fn remote_schema_permissions_execution() {
    let s = remote_suite();
    // user-role: exposed field/arg succeed (forwarded to the stub), unexposed
    // field/arg fail validation locally.
    s.check_query_f(&format!("{PERMS}/execution_with_partial_fields_exposed_to_role.yaml"), Transport::Http);
    s.check_query_f(&format!("{PERMS}/execution_with_partial_args_exposed_to_role.yaml"), Transport::Http);
    // a role with no remote-schema permission can't see the fields at all.
    s.check_query_f(&format!("{PERMS}/unknown_role_execution.yaml"), Transport::Http);
}

const CUSTOM: &str = "queries/remote_schemas/permissions/schema_customization";

/// A customized remote schema (`TestCustomizedRemoteSchemaPermissionsExecution`):
/// namespace `my_remote_schema`, type prefix `Foo`, field prefix `foo_` on
/// User. The engine unwraps the namespace, translates the customized
/// type/field names back to upstream ones (validating against the role SDL),
/// forwards, then re-applies the customized spelling to the response.
fn customization_suite(name: &str, permission_fixture: &str) -> Running {
    let s = Suite::new(name)
        .with_remote_graphql("GRAPHQL_SERVICE_1")
        .start();
    s.setup_v1q(&format!("{CUSTOM}/setup.yaml"));
    s.setup_v1q(&format!("{CUSTOM}/{permission_fixture}"));
    s
}

#[test]
fn customized_remote_schema_partial_fields() {
    let s = customization_suite(
        "remote_schema_custom_fields",
        "add_permission_with_valid_subset_of_fields.yaml",
    );
    // Exposed `foo_user_id` (and fragments on `FooUser`) forward as `user_id`;
    // unexposed `foo_gimmeText` fails validation against the customized SDL.
    s.check_query_f(&format!("{CUSTOM}/execution_with_partial_fields_exposed_to_role.yaml"), Transport::Http);
}

#[test]
fn customized_remote_schema_partial_args() {
    let s = customization_suite(
        "remote_schema_custom_args",
        "add_permission_with_valid_subset_of_arguments.yaml",
    );
    s.check_query_f(&format!("{CUSTOM}/execution_with_partial_args_exposed_to_role.yaml"), Transport::Http);
}

const PRESETS: &str = "queries/remote_schemas/permissions/argument_presets";

/// `TestRemoteSchemaPermissionsArgumentPresets`: a role SDL with `@preset`
/// directives on arguments / input-object fields. The engine injects the
/// presets into the forwarded query and hides preset args from the role.
fn preset_suite(name: &str, permission_fixture: &str) -> Running {
    let s = Suite::new(name)
        .with_remote_graphql("GRAPHQL_SERVICE_1")
        .start();
    s.setup_v1q(&format!("{PRESETS}/setup.yaml"));
    s.setup_v1q(&format!("{PRESETS}/{permission_fixture}"));
    s
}

#[test]
fn remote_schema_static_argument_presets() {
    let s = preset_suite(
        "remote_schema_static_preset",
        "add_permission_with_static_preset_argument.yaml",
    );
    s.check_query_f(&format!("{PRESETS}/execution_with_static_preset_args.yaml"), Transport::Http);
}

#[test]
fn remote_schema_session_argument_presets() {
    let s = preset_suite(
        "remote_schema_session_preset",
        "add_permission_with_session_preset_argument.yaml",
    );
    s.check_query_f(&format!("{PRESETS}/execution_with_session_preset_args.yaml"), Transport::Http);
}
