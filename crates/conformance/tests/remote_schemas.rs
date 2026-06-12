//! Ported from tests-py test_remote_schema_permissions.py.
//!
//! Module-level pytestmark: `admin_secret` + hge_env
//! `HASURA_GRAPHQL_ENABLE_REMOTE_SCHEMA_PERMISSIONS=true`. Every class uses
//! the `graphql_service_1/2/3` fixtures (node.js upstreams), replaced here
//! by the Rust stub in support/remote_stub.rs; their URLs reach the engine
//! through the same env vars the python fixtures export
//! (`GRAPHQL_SERVICE_1/2/3` via `hge_fixture_env`), which the engine
//! resolves from `{{ENV_VAR}}` templates in remote schema urls.
//!
//! Excluded (no-admin-role design rule — fixtures with no-role steps):
//! - TestRemoteSchemaPermissionsExecution::
//!   test_execution_with_subset_of_fields_exposed_to_role —
//!   execution_with_partial_fields_exposed_to_role.yaml step [2]
//!   ("run the above query as admin") posts to /v1/graphql without a role.
//! - TestRemoteSchemaPermissionsExecution::
//!   test_execution_with_subset_of_arguments_exposed_to_role —
//!   execution_with_partial_args_exposed_to_role.yaml step [2]
//!   ("run the above query as the admin role") has no role header.
//! - TestRemoteSchemaPermissionsExecutionPro (whole class) — enterprise
//!   edition + redis caching fixtures (pro-only), and its fixture
//!   execution_with_partial_fields_exposed_to_role_cached.yaml is the
//!   cached twin of an admin-step fixture; out of scope.
//! - TestCustomizedRemoteSchemaPermissionsExecution (both tests) —
//!   schema_customization/execution_with_partial_fields_exposed_to_role.yaml
//!   step [4] and schema_customization/
//!   execution_with_partial_args_exposed_to_role.yaml step [2] are no-role
//!   (admin) /v1/graphql requests; the classes' role-based steps share the
//!   same fixture files, so the whole tests are excluded.

use dist_conformance::{Running, Suite, Transport};
use serde_json::json;

#[path = "support/remote_stub.rs"]
mod remote_stub;
use remote_stub::Service;

const PERMS: &str = "queries/remote_schemas/permissions";
const SECRET: &str = "remote-schemas-secret";

/// Start the three upstream stubs and an engine wired to them, mirroring
/// `usefixtures('graphql_service_1', 'graphql_service_2',
/// 'graphql_service_3')` + the module pytestmark.
fn start_engine(name: &str) -> Running {
    let s1 = remote_stub::start(Service::One);
    let s2 = remote_stub::start(Service::Two);
    let s3 = remote_stub::start(Service::Three);
    Suite::new(name)
        .admin_secret(SECRET)
        .env("HASURA_GRAPHQL_ENABLE_REMOTE_SCHEMA_PERMISSIONS", "true")
        .env("GRAPHQL_SERVICE_1", &s1.url)
        .env("GRAPHQL_SERVICE_2", &s2.url)
        .env("GRAPHQL_SERVICE_3", &s3.url)
        .start()
}

fn admin_headers() -> Vec<(String, String)> {
    vec![("X-Hasura-Admin-Secret".to_string(), SECRET.to_string())]
}

/// TestAddRemoteSchemaPermissions — uses `per_method_tests_db_state`:
/// setup.yaml (add_remote_schema) -> /v1/query before EVERY test method,
/// teardown.yaml (remove_remote_schema) after it.
#[test]
fn add_remote_schema_permissions() {
    let s = start_engine("remote_perms_add");
    let per_method = |body: &dyn Fn()| {
        s.setup_v1q(&format!("{PERMS}/setup.yaml"));
        body();
        s.teardown_v1q(&format!("{PERMS}/teardown.yaml"));
    };

    // test_add_permission_with_valid_subset_of_fields
    per_method(&|| {
        s.apply(
            &format!("{PERMS}/add_permission_with_valid_subset_of_fields.yaml"),
            "/v1/metadata",
        );
    });

    // test_update_remote_schema_details_with_permissions_set
    per_method(&|| {
        s.apply(
            &format!("{PERMS}/add_permission_with_valid_subset_of_fields.yaml"),
            "/v1/metadata",
        );
        s.apply(
            &format!("{PERMS}/update_remote_schema/update_schema.yaml"),
            "/v1/metadata",
        );
        let (code, resp) = s.post(
            "/v1/metadata",
            &json!({"type": "export_metadata", "args": {}}),
            &admin_headers(),
        );
        assert!(code < 300, "export_metadata failed ({code}): {resp}");
        let def = &resp["remote_schemas"][0]["definition"];
        assert_eq!(def["url"], json!("{{GRAPHQL_SERVICE_2}}"), "{resp}");
        assert_eq!(
            resp["remote_schemas"][0]["comment"],
            json!("this is from update query"),
            "{resp}"
        );
        assert_eq!(def["timeout_seconds"], json!(120), "{resp}");
        s.apply(
            &format!("{PERMS}/update_remote_schema/revert_to_original_config.yaml"),
            "/v1/metadata",
        );
    });

    // test_update_remote_schema_details_with_permissions_set_with_error:
    // updating to GRAPHQL_SERVICE_3 (User.user_id: Float, incompatible with
    // the kept role SDL's Int) must fail with 400.
    per_method(&|| {
        s.apply(
            &format!("{PERMS}/add_permission_with_valid_subset_of_fields.yaml"),
            "/v1/metadata",
        );
        let body = dist_conformance::load_fixture(
            &dist_conformance::fixture_root()
                .join(format!("{PERMS}/update_remote_schema/update_schema_error.yaml")),
        )
        .expect("loading update_schema_error fixture");
        let (code, resp) = s.post("/v1/metadata", &body, &admin_headers());
        assert_eq!(code, 400, "expected 400 from update_schema_error: {resp}");
    });

    // test_add_permission_with_valid_subset_of_arguments
    per_method(&|| {
        s.apply(
            &format!("{PERMS}/add_permission_with_valid_subset_of_arguments.yaml"),
            "/v1/metadata",
        );
    });

    // test_role_based_schema_*_validation + test_preset_directive_validation:
    // check_query_f without a transport argument -> http only.
    //
    for f in [
        "role_based_schema_enum_validations.yaml",
        "role_based_schema_scalar_validation.yaml",
        "role_based_schema_interface_validation.yaml",
        "role_based_schema_union_validation.yaml",
        "role_based_schema_input_object_validation.yaml",
        "role_based_schema_object_validation.yaml",
        "argument_preset_validation.yaml",
    ] {
        per_method(&|| {
            s.check_query_f(&format!("{PERMS}/{f}"), Transport::Http);
        });
    }
}

/// TestRemoteSchemaPermissionsExecution — same per-method db state.
/// Only test_execution_with_unknown_role is in scope (see module docs for
/// the two excluded admin-step tests).
#[test]
fn remote_schema_permissions_execution() {
    let s = start_engine("remote_perms_exec");

    // test_execution_with_subset_of_fields_exposed_to_role: excluded —
    // execution_with_partial_fields_exposed_to_role.yaml step [2] is a
    // no-role (admin) request, out of scope.
    // test_execution_with_subset_of_arguments_exposed_to_role: excluded —
    // execution_with_partial_args_exposed_to_role.yaml step [2] is a
    // no-role (admin) request, out of scope.

    // test_execution_with_unknown_role
    s.setup_v1q(&format!("{PERMS}/setup.yaml"));
    s.check_query_f(
        &format!("{PERMS}/unknown_role_execution.yaml"),
        Transport::Http,
    );
    s.teardown_v1q(&format!("{PERMS}/teardown.yaml"));
}

/// TestRemoteSchemaPermissionsArgumentPresets — per-method db state over
/// the argument_presets dir.
#[test]
fn remote_schema_permissions_argument_presets() {
    let s = start_engine("remote_perms_presets");
    let dir = format!("{PERMS}/argument_presets");
    let per_method = |body: &dyn Fn()| {
        s.setup_v1q(&format!("{dir}/setup.yaml"));
        body();
        s.teardown_v1q(&format!("{dir}/teardown.yaml"));
    };

    // test_execution_with_static_argument_preset
    per_method(&|| {
        s.apply(
            &format!("{dir}/add_permission_with_static_preset_argument.yaml"),
            "/v1/metadata",
        );
        s.check_query_f(
            &format!("{dir}/execution_with_static_preset_args.yaml"),
            Transport::Http,
        );
    });

    // test_execution_with_session_argument_preset
    per_method(&|| {
        s.apply(
            &format!("{dir}/add_permission_with_session_preset_argument.yaml"),
            "/v1/metadata",
        );
        s.check_query_f(
            &format!("{dir}/execution_with_session_preset_args.yaml"),
            Transport::Http,
        );
    });
}

/// TestRemoteRelationshipPermissions — no per_method_tests_db_state;
/// instead an autouse `transact` fixture runs setup_with_permissions.yaml
/// -> /v1/query before every test and teardown.yaml after it.
#[test]
fn remote_relationship_permissions() {
    let s = start_engine("remote_perms_rel");
    let dir = format!("{PERMS}/remote_relationships");
    let transact = |body: &dyn Fn()| {
        s.setup_v1q(&format!("{dir}/setup_with_permissions.yaml"));
        body();
        s.teardown_v1q(&format!("{dir}/teardown.yaml"));
    };

    // test_basic_relationship
    transact(&|| {
        s.setup_v1q(&format!("{dir}/setup_remote_rel_basic.yaml"));
        s.setup_v1q(&format!("{dir}/setup_remote_rel_basic_user.yaml"));
        s.check_query_f(
            &format!(
                "{dir}/basic_remote_relationship_without_remote_schema_permissions_configured.yaml"
            ),
            Transport::Http,
        );
        s.check_query_f(
            &format!(
                "{dir}/basic_remote_relationship_with_remote_schema_permissions_configured.yaml"
            ),
            Transport::Http,
        );
    });

    // test_complex_multiple_joins
    transact(&|| {
        s.setup_v1q(&format!("{dir}/setup_multiple_remote_rel.yaml"));
        s.check_query_f(
            &format!("{dir}/complex_multiple_joins.yaml"),
            Transport::Http,
        );
    });

    // test_remote_relationship_with_field_containing_preset_argument
    transact(&|| {
        s.setup_v1q(&format!("{dir}/setup_remote_rel_basic.yaml"));
        s.check_query_f(
            &format!("{dir}/derive_remote_relationship_with_joining_field_containing_preset.yaml"),
            Transport::Http,
        );
    });

    // test_partial_arguments_of_remote_relationship_from_preset
    transact(&|| {
        s.setup_v1q(&format!(
            "{dir}/setup_remote_rel_messages_single_field.yaml"
        ));
        s.check_query_f(
            &format!("{dir}/partial_arguments_from_preset.yaml"),
            Transport::Http,
        );
    });
}
