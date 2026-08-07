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

/// The filter surface a client can discover has to be the filter surface the
/// engine accepts. `_ilike` is how every search box over this API is written —
/// a schema that omits it tells generated clients, IDEs and agents that the
/// store cannot be searched, while the engine answers the query perfectly well.
#[test]
fn text_filters_the_engine_accepts_are_published_by_introspection() {
    let s = Suite::new("introspection_text_filters").start();
    s.setup_v1q(&format!("{INTROSPECTION}/setup.yaml"));

    let (status, body) = s.post(
        "/v1/graphql",
        &serde_json::json!({
            "query": "query { \
                text: __type(name: \"String_comparison_exp\") { inputFields { name } } \
                number: __type(name: \"Int_comparison_exp\") { inputFields { name } } \
            }"
        }),
        &[("X-Donat-Role".to_owned(), "user".to_owned())],
    );
    assert_eq!(status, 200, "introspection status: {body}");

    let names = |field: &str| -> Vec<String> {
        body["data"][field]["inputFields"]
            .as_array()
            .unwrap_or_else(|| panic!("{field} is an input object: {body}"))
            .iter()
            .map(|input| input["name"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    let text = names("text");
    // Postgres has a regex engine, so the operations built on it are part of
    // what a client may discover here too.
    for operator in [
        "_eq",
        "_in",
        "_is_null",
        "_like",
        "_nlike",
        "_ilike",
        "_nilike",
        "_similar",
        "_nsimilar",
        "_regex",
        "_iregex",
        "_nregex",
        "_niregex",
    ] {
        assert!(
            text.contains(&operator.to_owned()),
            "String_comparison_exp must publish {operator}: {text:?}"
        );
    }
    // A pattern operator on a number is not a filter the engine can honour, so
    // it is not one the schema may offer.
    let number = names("number");
    for operator in ["_like", "_ilike", "_regex"] {
        assert!(
            !number.contains(&operator.to_owned()),
            "Int_comparison_exp must not publish {operator}: {number:?}"
        );
    }

    s.teardown_v1q(&format!("{INTROSPECTION}/teardown.yaml"));
}
