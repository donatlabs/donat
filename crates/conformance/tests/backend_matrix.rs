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

fn post(suite: &donat_conformance::Running, query: &str) -> serde_json::Value {
    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({ "query": query }),
        &[("X-Donat-Role".to_string(), "user".to_string())],
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
    });
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
