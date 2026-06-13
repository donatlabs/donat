//! Ported from tests-py test_graphql_queries.py (aggregate-permission and
//! relay-permission suites) and test_graphql_introspection.py
//! (`TestGraphqlIntrospection`: user-role and admin introspection).
//!
//! Only `test_introspection_user` (role-scoped) is ported. The other
//! introspection methods (`test_introspection`,
//! `test_introspection_directive_is_repeatable`) are no-role admin requests
//! — out of scope: this engine has no admin role.

use donat_conformance::{Suite, Transport};

const AGG_PERM: &str = "queries/graphql_query/agg_perm";
const RELAY_PERMS: &str = "queries/graphql_query/relay/permissions";
const INTROSPECTION: &str = "queries/graphql_introspection";

#[test]
fn graphql_query_agg_perm_postgres_mssql() {
    // Class is parametrized over http+websocket in pytest; Both replicates that.
    let s = Suite::new("agg_perm_pg_mssql").start();
    s.setup_v1q(&format!("{AGG_PERM}/setup.yaml"));

    for f in [
        "author_agg_articles.yaml",
        "article_agg_fail.yaml",
        "author_articles_agg_fail.yaml",
        "author_post_agg_order_by.yaml",
    ] {
        s.check_query_f(&format!("{AGG_PERM}/{f}"), Transport::Both);
    }
    s.check_query_f(
        &format!("{AGG_PERM}/article_agg_with_role_without_select_access.yaml"),
        Transport::Both,
    );
    s.check_query_f(
        &format!("{AGG_PERM}/article_agg_with_filter.yaml"),
        Transport::Both,
    );

    s.teardown_v1q(&format!("{AGG_PERM}/teardown.yaml"));
}

#[test]
fn graphql_query_agg_perm_postgres() {
    // Class is parametrized over http+websocket in pytest; Both replicates that.
    let s = Suite::new("agg_perm_pg").start();
    s.setup_v1q(&format!("{AGG_PERM}/setup.yaml"));

    s.check_query_f(
        &format!("{AGG_PERM}/article_agg_with_role_with_select_access.yaml"),
        Transport::Both,
    );

    s.teardown_v1q(&format!("{AGG_PERM}/teardown.yaml"));
}

#[test]
fn relay_queries_permissions() {
    // Class is parametrized over http+websocket in pytest; Both replicates that.
    let s = Suite::new("relay_perms").start();
    s.setup_v1q(&format!("{RELAY_PERMS}/setup.yaml"));

    for f in [
        "author_connection.yaml",
        "author_node.yaml",
        "author_node_null.yaml",
        // _test_relay_pagination(.., '/article_pagination/forward', 2)
        "article_pagination/forward/page_1.yaml",
        "article_pagination/forward/page_2.yaml",
        // _test_relay_pagination(.., '/article_pagination/backward', 2)
        "article_pagination/backward/page_1.yaml",
        "article_pagination/backward/page_2.yaml",
    ] {
        s.check_query_f(&format!("{RELAY_PERMS}/{f}"), Transport::Both);
    }

    s.teardown_v1q(&format!("{RELAY_PERMS}/teardown.yaml"));
}

#[test]
fn graphql_introspection() {
    let s = Suite::new("introspection").start();
    s.setup_v1q(&format!("{INTROSPECTION}/setup.yaml"));

    // test_introspection_user: user-role introspection, fixed-body fixture.
    // pytest calls check_query_f without the transport param -> http only.
    // (test_introspection / test_introspection_directive_is_repeatable are
    // no-role admin requests — out of scope: this engine has no admin role.)
    s.check_query_f(
        &format!("{INTROSPECTION}/introspection_user_role.yaml"),
        Transport::Http,
    );

    s.teardown_v1q(&format!("{INTROSPECTION}/teardown.yaml"));
}
