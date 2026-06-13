//! Ported from tests-py test_graphql_queries.py (role-based suites only;
//! admin/no-role tests are out of scope per the no-admin-role design rule).

use dist_conformance::{Suite, Transport};

const PERMS: &str = "queries/graphql_query/permissions";

#[test]
fn graphql_query_permissions() {
    let s = Suite::new("query_permissions").start();
    s.setup_v1q(&format!("{PERMS}/setup.yaml"));

    // Class is parametrized over http+websocket in pytest; Both replicates that.
    let both = [
        "user_select_query_unpublished_articles.yaml",
        "user_select_query_article_author.yaml",
        "user_can_query_other_users_published_articles.yaml",
        "anonymous_can_only_get_published_articles.yaml",
        "anonymous_can_only_get_published_articles_v1alpha1.yaml",
        // Step [0] role=user (column hidden), step [1] no role = admin
        // (full access) — Hasura admin parity.
        "user_cannot_access_remarks_col.yaml",
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
        "in_and_nin.yaml",
        "iregex.yaml",
    ];
    for f in both {
        s.check_query_f(&format!("{PERMS}/{f}"), Transport::Both);
    }
    // pytest calls this one without the transport param -> http only.
    s.check_query_f(
        &format!("{PERMS}/user_should_not_be_able_to_access_books_by_pk.yaml"),
        Transport::Http,
    );
    for f in [
        "select_articles_without_required_headers.yaml",
        "reader_author.yaml",
        "tutor_get_students.yaml",
        "tutor_get_students_session.yaml",
    ] {
        s.check_query_f(&format!("{PERMS}/{f}"), Transport::Both);
    }

    s.teardown_v1q(&format!("{PERMS}/teardown.yaml"));
}
