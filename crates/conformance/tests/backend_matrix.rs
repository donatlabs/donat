use donat_backend::capabilities::JsonOps;
use donat_conformance::{BackendId, FixtureColumn, FixtureColumnType, Suite, TableFixture};
use serde_json::json;

const AUTHOR_COLUMNS: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "name",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
];

const SPECIAL_COLUMNS: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "text_value",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
    FixtureColumn {
        name: "boundary",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: false,
    },
];

const JSON_COLUMNS: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "payload",
        ty: FixtureColumnType::Json,
        nullable: true,
        primary_key: false,
    },
];

const ARTICLE_COLUMNS: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "title",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
    FixtureColumn {
        name: "author_id",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: false,
    },
];

fn post(suite: &donat_conformance::Running, query: &str) -> serde_json::Value {
    post_as(suite, "user", query)
}

fn post_as(
    suite: &donat_conformance::Running,
    role: &str,
    query: &str,
) -> serde_json::Value {
    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({ "query": query }),
        &[("X-Donat-Role".to_string(), role.to_string())],
    );
    assert_eq!(status, 200, "response: {body}");
    body
}

#[test]
fn core_read_contract() {
    let backend = BackendId::selected().expect("selected backend");
    let suite = Suite::new("matrix_core_reads").start();
    let long_text = format!("O'Brien\\path:{}", "x".repeat(1500));
    suite.install_table(&TableFixture {
        name: "author",
        columns: AUTHOR_COLUMNS,
        rows: vec![vec![json!(1), json!("Alice")], vec![json!(2), json!("Bob")]],
        role: "user",
        allow_aggregations: true,
        mutations: false,
    });
    suite.install_table(&TableFixture {
        name: "article",
        columns: ARTICLE_COLUMNS,
        rows: vec![
            vec![json!(10), json!("A"), json!(1)],
            vec![json!(11), json!("B"), json!(1)],
            vec![json!(12), json!("C"), json!(2)],
        ],
        role: "user",
        allow_aggregations: true,
        mutations: false,
    });
    suite.add_select_permission(
        "article",
        "limited",
        json!(["id", "title"]),
        json!({ "author_id": { "_eq": 1 } }),
        false,
    );
    let relationships_supported = backend.capabilities().relationships;
    if relationships_supported {
        suite.add_relationship("article", "author", "author", &[("author_id", "id")], false);
        suite.add_relationship("author", "articles", "article", &[("id", "author_id")], true);
    } else {
        eprintln!(
            "backend={} unsupported-by-capability: relationships",
            backend.as_str()
        );
    }
    suite.install_table(&TableFixture {
        name: "special_value",
        columns: SPECIAL_COLUMNS,
        rows: vec![
            vec![
                json!(1),
                json!(long_text.clone()),
                json!(9_223_372_036_854_775_807i64),
            ],
            vec![json!(2), json!("plain"), json!(0)],
        ],
        role: "user",
        allow_aggregations: false,
        mutations: false,
    });
    let json_supported = backend.capabilities().json_ops != JsonOps::None;
    if json_supported {
        suite.install_table(&TableFixture {
            name: "json_value",
            columns: JSON_COLUMNS,
            rows: vec![
                vec![json!(1), json!({ "nested": ["quoted", 1], "enabled": true })],
                vec![json!(2), json!(null)],
            ],
            role: "user",
            allow_aggregations: false,
            mutations: false,
        });
    } else {
        eprintln!(
            "backend={} unsupported-by-capability: json_ops",
            backend.as_str()
        );
    }

    let introspection = post(
        &suite,
        "{ __type(name: \"query_root\") { fields { name } } }",
    );
    assert!(
        introspection["data"]["__type"]["fields"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field["name"] == "author")),
        "query_root fields: {introspection}"
    );
    assert_eq!(
        post(
            &suite,
            "{ author(order_by: {id: desc}) { id name __typename } __typename }"
        ),
        json!({ "data": {
            "author": [
                { "id": 2, "name": "Bob", "__typename": "author" },
                { "id": 1, "name": "Alice", "__typename": "author" }
            ],
            "__typename": "query_root"
        }})
    );
    assert_eq!(
        post(
            &suite,
            "{ article(where: {title: {_in: [\"A\", \"C\"]}}, order_by: {id: asc}, limit: 1, offset: 1) { id title } }"
        ),
        json!({ "data": { "article": [{ "id": 12, "title": "C" }] } })
    );
    assert_eq!(
        post_as(
            &suite,
            "limited",
            "{ article(order_by: {id: asc}) { id title } }"
        ),
        json!({ "data": { "article": [
            { "id": 10, "title": "A" },
            { "id": 11, "title": "B" }
        ]}})
    );
    let hidden = post_as(&suite, "limited", "{ article { author_id } }");
    assert_eq!(
        hidden,
        json!({ "errors": [{
            "extensions": {
                "path": "$.selectionSet.article.selectionSet.author_id",
                "code": "validation-failed"
            },
            "message": "field 'author_id' not found in type: 'article'"
        }]})
    );
    if relationships_supported {
        assert_eq!(
            post(
                &suite,
                "{ article(order_by: {id: asc}) { id author { name } } }"
            ),
            json!({ "data": { "article": [
                { "id": 10, "author": { "name": "Alice" } },
                { "id": 11, "author": { "name": "Alice" } },
                { "id": 12, "author": { "name": "Bob" } }
            ]}})
        );
        assert_eq!(
            post(
                &suite,
                "{ author(order_by: {id: asc}) { id articles(order_by: {id: asc}) { id title } } }"
            ),
            json!({ "data": { "author": [
                { "id": 1, "articles": [
                    { "id": 10, "title": "A" },
                    { "id": 11, "title": "B" }
                ]},
                { "id": 2, "articles": [{ "id": 12, "title": "C" }] }
            ]}})
        );
    }
    assert_eq!(
        post(
            &suite,
            "{ special_value(order_by: {id: asc}) { id text_value boundary } }"
        ),
        json!({ "data": { "special_value": [
            {
                "id": 1,
                "text_value": long_text,
                "boundary": 9_223_372_036_854_775_807i64
            },
            { "id": 2, "text_value": "plain", "boundary": 0 }
        ]}})
    );
    if json_supported {
        assert_eq!(
            post(&suite, "{ json_value(order_by: {id: asc}) { id payload } }"),
            json!({ "data": { "json_value": [
                { "id": 1, "payload": { "nested": ["quoted", 1], "enabled": true } },
                { "id": 2, "payload": null }
            ]}})
        );
    }
    assert_eq!(
        post(&suite, "{ author_by_pk(id: 1) { id name } }"),
        json!({ "data": { "author_by_pk": { "id": 1, "name": "Alice" } } })
    );
    assert_eq!(
        post(
            &suite,
            "{ author_aggregate { __typename aggregate { __typename count } nodes { id } } }"
        ),
        json!({ "data": { "author_aggregate": {
            "__typename": "author_aggregate",
            "aggregate": { "__typename": "author_aggregate_fields", "count": 2 },
            "nodes": [{ "id": 1 }, { "id": 2 }]
        }}})
    );
}

#[test]
fn core_mutation_contract() {
    let backend = BackendId::selected().expect("selected backend");
    if !backend.capabilities().mutations {
        eprintln!(
            "backend={} unsupported-by-capability: mutations",
            backend.as_str()
        );
        return;
    }

    let suite = Suite::new("matrix_core_mutations").start();
    suite.install_table(&TableFixture {
        name: "note",
        columns: AUTHOR_COLUMNS,
        rows: vec![],
        role: "user",
        allow_aggregations: false,
        mutations: true,
    });

    assert_eq!(
        post(
            &suite,
            r#"mutation {
                insert_note(objects: [
                    { id: 1, name: "first" },
                    { id: 2, name: "second" }
                ]) { affected_rows returning { id name } }
            }"#
        ),
        json!({ "data": { "insert_note": {
            "affected_rows": 2,
            "returning": [
                { "id": 1, "name": "first" },
                { "id": 2, "name": "second" }
            ]
        }}})
    );
    assert_eq!(
        post(
            &suite,
            r#"mutation {
                update_note(where: {id: {_eq: 1}}, _set: {name: "edited"}) {
                    affected_rows returning { id name }
                }
            }"#
        ),
        json!({ "data": { "update_note": {
            "affected_rows": 1,
            "returning": [{ "id": 1, "name": "edited" }]
        }}})
    );
    assert_eq!(
        post(
            &suite,
            "mutation { delete_note(where: {id: {_eq: 2}}) { affected_rows returning { id } } }"
        ),
        json!({ "data": { "delete_note": {
            "affected_rows": 1,
            "returning": [{ "id": 2 }]
        }}})
    );
    assert_eq!(
        post(&suite, "{ note(order_by: {id: asc}) { id name } }"),
        json!({ "data": { "note": [{ "id": 1, "name": "edited" }] } })
    );
}
