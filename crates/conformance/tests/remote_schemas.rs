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

use dist_conformance::{Running, Suite, Transport};
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
