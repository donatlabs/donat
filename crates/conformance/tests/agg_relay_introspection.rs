//! Ported from tests-py test_graphql_queries.py (aggregate-permission and
//! relay-permission suites) and test_graphql_introspection.py
//! (`TestGraphqlIntrospection`: user-role and admin introspection).
//!
//! The engine implements Hasura's admin role: a request to a trusted/no-secret
//! connection without `X-Hasura-Role` is `admin` (full schema access). The
//! pytest admin introspection methods (`test_introspection`,
//! `test_introspection_directive_is_repeatable`) inspect the response in Python
//! rather than diffing against a fixed body, so they are replicated as Rust
//! assertions over the engine's introspection response (issued as a no-role,
//! i.e. admin, request).

use dist_conformance::{Suite, Transport, fixture_root, load_fixture};
use serde_json::Value as Json;

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
    s.check_query_f(
        &format!("{INTROSPECTION}/introspection_user_role.yaml"),
        Transport::Http,
    );

    // test_introspection: admin (no-role) full-schema introspection. pytest
    // inspects the response in Python; replicate the same assertions in Rust.
    {
        let resp = post_introspection(&s, &format!("{INTROSPECTION}/introspection.yaml"));
        let types = resp["data"]["__schema"]["types"]
            .as_array()
            .expect("__schema.types array");

        let mut has_article = false;
        let mut has_article_author_fk_rel = false;
        let mut has_article_author_manual_rel = false;
        for t in types {
            if t["name"] == Json::String("article".into()) {
                has_article = true;
                for fld in t["fields"].as_array().expect("article.fields array") {
                    match fld["name"].as_str() {
                        Some("author_obj_rel_manual") => {
                            has_article_author_manual_rel = true;
                            assert_eq!(
                                fld["type"]["kind"], "OBJECT",
                                "author_obj_rel_manual type.kind"
                            );
                        }
                        Some("author_obj_rel_fk") => {
                            has_article_author_fk_rel = true;
                            // FK object relationship on a NOT NULL column
                            // (`article.author_id INTEGER NOT NULL`) is
                            // non-nullable, like Hasura.
                            assert_eq!(
                                fld["type"]["kind"], "NON_NULL",
                                "author_obj_rel_fk type.kind"
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(
            has_article,
            "admin introspection exposes the `article` type"
        );
        assert!(
            has_article_author_fk_rel,
            "article exposes author_obj_rel_fk"
        );
        assert!(
            has_article_author_manual_rel,
            "article exposes author_obj_rel_manual"
        );
    }

    // test_introspection_directive_is_repeatable: admin (no-role) request;
    // every directive must report isRepeatable == false.
    {
        let resp = post_introspection(
            &s,
            &format!("{INTROSPECTION}/introspection_directive_is_repeatable.yaml"),
        );
        let directives = resp["data"]["__schema"]["directives"]
            .as_array()
            .expect("__schema.directives array");
        assert!(
            !directives.is_empty(),
            "admin introspection exposes directives"
        );
        for d in directives {
            assert_eq!(
                d["isRepeatable"],
                Json::Bool(false),
                "directive {} isRepeatable",
                d["name"]
            );
        }
    }

    s.teardown_v1q(&format!("{INTROSPECTION}/teardown.yaml"));
}

/// Issue an introspection fixture as a no-role (admin) request and return the
/// decoded GraphQL response. The fixture carries no `X-Hasura-Role`, so the
/// engine treats it as admin; we POST the fixture's `query` body to its `url`.
fn post_introspection(s: &dist_conformance::Running, rel: &str) -> Json {
    let conf = load_fixture(&fixture_root().join(rel)).expect("loading introspection fixture");
    let url = conf["url"].as_str().expect("fixture url");
    let body = conf["query"].clone();
    let (code, resp) = s.post(url, &body, &[]);
    assert_eq!(code, 200, "{rel}: introspection HTTP status\n{resp}");
    assert!(
        resp.get("errors").is_none(),
        "{rel}: introspection returned errors\n{resp}"
    );
    resp
}
