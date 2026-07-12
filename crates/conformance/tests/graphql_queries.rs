//! Ported from tests-py test_graphql_queries.py (role-based suites only;
//! admin/no-role tests are out of scope per the no-admin-role design rule).

use donat_conformance::{Suite, Transport};

const PERMS: &str = "queries/graphql_query/permissions";

#[test]
fn graphql_query_permissions() {
    let s = Suite::new("query_permissions").start();
    s.setup_v1q(&format!("{PERMS}/setup.yaml"));

    // Class is parametrized over http+websocket in pytest; Both replicates that.
    // Portable relationship/session-filter cases run in backend_matrix against
    // every capable datasource. This Postgres reference keeps only cases that
    // still require PostGIS, jsonb operators, functions, or computed fields.
    let both = [
        // user_cannot_access_remarks_col.yaml: step [1] is a no-role (admin)
        // request — out of scope (this engine has no admin role).
        "user_can_query_geometry_values_filter.yaml",
        "user_can_query_geometry_values_filter_session_vars.yaml",
        "user_can_query_jsonb_values_filter.yaml",
        "user_can_query_jsonb_values_filter_session_vars.yaml",
        "artist_select_query_Track_fail.yaml",
        "artist_select_query_Track.yaml",
        "artist_search_tracks.yaml",
        "artist_search_tracks_aggregate.yaml",
        "staff_passed_students.yaml",
        "user_query_auction.yaml",
        // jsonb_has_all is commented out in tests-py as well.
        "jsonb_has_any.yaml",
        "iregex.yaml",
    ];
    for f in both {
        s.check_query_f(&format!("{PERMS}/{f}"), Transport::Both);
    }
    // pytest calls this one without the transport param -> http only.
    for f in [
        "reader_author.yaml",
        "tutor_get_students.yaml",
        "tutor_get_students_session.yaml",
    ] {
        s.check_query_f(&format!("{PERMS}/{f}"), Transport::Both);
    }

    s.teardown_v1q(&format!("{PERMS}/teardown.yaml"));
}
