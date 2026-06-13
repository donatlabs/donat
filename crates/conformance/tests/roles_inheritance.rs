//! Ported inherited-roles suites:
//! - tests-py test_graphql_queries.py: TestGraphQLInheritedRolesSchema,
//!   TestGraphQLInheritedRolesPostgres
//! - tests-py test_roles_inheritance.py: TestGraphQLMutationRolesInheritance,
//!   TestCustomFunctionPermissionsInheritance,
//!   TestNestedInheritedRolesSelectPermissions
//!
//! test_roles_inheritance.py carries module-level
//! `pytestmark = [admin_secret, hge_env(ENABLE_REMOTE_SCHEMA_PERMISSIONS=true)]`,
//! so every class from that module would get `.admin_secret()` + that env var.
//!
//! NOTE: the admin_secret mark is purely environmental for these classes —
//! tests-py sends the secret alongside explicit X-Donat-Role headers,
//! which yields the same trusted-role session a secretless engine
//! produces, and no fixture asserts on the secret itself. The suites below
//! therefore run without it; the hge_env marks are still replicated via
//! `.env()`. (Suite::start() authenticates its bootstrap calls when
//! `.admin_secret()` is set, so using it here would also work.)

use donat_conformance::{Running, Suite, Transport};

const INHERITED: &str = "queries/graphql_query/permissions/inherited_roles";
const NESTED: &str = "queries/graphql_query/permissions/roles_inheritance";
const MUTATION: &str = "queries/graphql_mutation/roles_inheritance";
const FUNCTION: &str = "queries/graphql_mutation/functions/permissions/roles_inheritance";

/// `setup_metadata_api_version = "v2"` setup order (per PORTING.md):
/// pre_setup -> /v1/metadata, schema_setup -> /v2/query, setup -> /v1/metadata.
fn v2_setup(s: &Running, dir: &str) {
    s.apply_if_exists(&format!("{dir}/pre_setup.yaml"), "/v1/metadata");
    s.apply_if_exists(&format!("{dir}/schema_setup.yaml"), "/v2/query");
    s.apply_if_exists(&format!("{dir}/setup.yaml"), "/v1/metadata");
}

/// Mirrored teardown: teardown -> /v1/metadata, schema_teardown -> /v2/query,
/// post_teardown -> /v1/metadata.
fn v2_teardown(s: &Running, dir: &str) {
    s.apply_if_exists(&format!("{dir}/teardown.yaml"), "/v1/metadata");
    s.apply_if_exists(&format!("{dir}/schema_teardown.yaml"), "/v2/query");
    s.apply_if_exists(&format!("{dir}/post_teardown.yaml"), "/v1/metadata");
}

/// TestGraphQLInheritedRolesSchema (test_graphql_queries.py).
/// Class is parametrized over http+websocket, but every check_query_f call
/// omits the transport argument -> http only.
#[test]
fn graphql_inherited_roles_schema() {
    let s = Suite::new("inherited_roles").start();
    v2_setup(&s, INHERITED);

    s.check_query_f(
        &format!("{INHERITED}/basic_inherited_roles.yaml"),
        Transport::Http,
    );
    s.check_query_f(
        &format!("{INHERITED}/inherited_role_with_some_roles_having_no_permissions.yaml"),
        Transport::Http,
    );

    // test_throw_error_when_roles_form_a_cycle DROPPED: it exported the
    // metadata, appended circular inherited roles, and asserted that the
    // replace_metadata management API rejects the cycle. That tests the
    // admin metadata API itself (export_metadata + replace_metadata), which
    // is being removed; the inherited-role DATA behavior is still covered by
    // the YAML-configured cases below.

    s.check_query_f(
        &format!("{INHERITED}/override_inherited_permission.yaml"),
        Transport::Http,
    );
    s.check_query_f(
        &format!("{INHERITED}/inherited_role_parent_is_another_inherited_role.yaml"),
        Transport::Http,
    );

    v2_teardown(&s, INHERITED);
}

/// TestGraphQLInheritedRolesPostgres (test_graphql_queries.py) — DB-specific
/// subset over the same fixture dir; check_query_f without transport -> http.
#[test]
fn graphql_inherited_roles_postgres() {
    let s = Suite::new("inherited_roles_pg").start();
    v2_setup(&s, INHERITED);

    s.check_query_f(
        &format!("{INHERITED}/basic_inherited_roles.yaml"),
        Transport::Http,
    );
    s.check_query_f(
        &format!("{INHERITED}/inherited_role_with_some_roles_having_no_permissions.yaml"),
        Transport::Http,
    );

    v2_teardown(&s, INHERITED);
}

/// TestNestedInheritedRolesSelectPermissions (test_roles_inheritance.py).
/// Mutation-style fixtures (per_class_db_schema_for_mutation_tests +
/// per_method_db_data_for_mutation_tests) with v2 metadata setup; the fixture
/// dir has no values_setup/values_teardown files, so per-method data setup is
/// a no-op.
#[test]
fn nested_inherited_roles_select_permissions() {
    let s = Suite::new("nested_inherited")
        .env("DONAT_GRAPHQL_ENABLE_REMOTE_SCHEMA_PERMISSIONS", "true")
        .start();
    v2_setup(&s, NESTED);

    s.check_query_f(
        &format!("{NESTED}/nested_inherited_roles_column_permissions.yaml"),
        Transport::Http,
    );

    v2_teardown(&s, NESTED);
}

/// TestGraphQLMutationRolesInheritance (test_roles_inheritance.py).
/// v2 class-scoped schema setup; values_setup.yaml/values_teardown.yaml run
/// around every test method via /v1/query.
#[test]
fn graphql_mutation_roles_inheritance() {
    let s = Suite::new("mutation_inheritance")
        .env("DONAT_GRAPHQL_ENABLE_REMOTE_SCHEMA_PERMISSIONS", "true")
        .start();
    v2_setup(&s, MUTATION);

    let per_method = |case: &str| {
        s.setup_v1q(&format!("{MUTATION}/values_setup.yaml"));
        s.check_query_f(&format!("{MUTATION}/{case}"), Transport::Http);
        s.teardown_v1q(&format!("{MUTATION}/values_teardown.yaml"));
    };

    per_method("inheritance_from_single_parent.yaml");
    // resolve_inconsistent_permission.yaml DROPPED: it drives the metadata
    // CONSISTENCY management API (get_inconsistent_metadata) and a runtime
    // create_update_permission/drop pivot as test steps — both are part of
    // the admin metadata API that is being removed. The underlying
    // inherited-permission DATA behavior is covered by the YAML-configured
    // inheritance cases.
    per_method("inherited_mutation_permission_for_nested_roles.yaml");
    // override_inherited_permission.yaml: step [1] is a no-role (admin)
    // update_articles mutation — out of scope (this engine has no admin role).

    v2_teardown(&s, MUTATION);
}

/// TestCustomFunctionPermissionsInheritance (test_roles_inheritance.py).
/// Class adds hge_env INFER_FUNCTION_PERMISSIONS=false on top of the module
/// marks; setup_metadata_api_version = "2" (non-"v1") -> the same v2 path.
/// No values_setup/values_teardown files in the dir.
#[test]
fn custom_function_permissions_inheritance() {
    let s = Suite::new("function_perm_inheritance")
        .env("DONAT_GRAPHQL_ENABLE_REMOTE_SCHEMA_PERMISSIONS", "true")
        .env("DONAT_GRAPHQL_INFER_FUNCTION_PERMISSIONS", "false")
        .start();
    v2_setup(&s, FUNCTION);

    s.check_query_f(
        &format!("{FUNCTION}/multiple_parents_inheritance.yaml"),
        Transport::Http,
    );
    // override_inherited_permission.yaml DROPPED: its pivot is a runtime
    // create_function_permission for `inherited_role2` applied as a test STEP
    // (step[0] expects the role to be denied, step[1] grants the permission
    // via the metadata API, step[2] expects success, step[3] revokes). That
    // depends on mutating metadata against the running engine — part of the
    // admin metadata API being removed — and cannot be expressed as boot
    // metadata (the same role must be both denied and allowed in one engine).
    // multiple_parents_inheritance.yaml already covers inherited function
    // permission DATA behavior.

    v2_teardown(&s, FUNCTION);
}
