//! Unit tests for the IR: the crate is pure data (Serialize + Clone +
//! Debug), so these tests pin the structural invariants downstream code
//! (sqlgen snapshots, the planner) relies on — BoolExp composition,
//! Scalar::Json passthrough, externally-tagged serialization shapes, and
//! deep-clone equivalence. No database.

use donat_ir::*;
use serde_json::json;

fn table(name: &str) -> Table {
    Table {
        schema: "public".into(),
        name: name.into(),
    }
}

fn column(alias: &str, col: &str) -> OutputField {
    OutputField {
        alias: alias.into(),
        value: FieldValue::Column {
            column: col.into(),
            pg_type: "integer".into(),
        },
    }
}

fn select(from_table: &str, fields: Vec<OutputField>, predicate: Option<BoolExp>) -> SelectQuery {
    SelectQuery {
        from: FromSource::Table(table(from_table)),
        fields,
        predicate,
        order_by: vec![],
        limit: None,
        nodes_limit: None,
        offset: None,
        distinct_on: vec![],
        single: false,
    }
}

#[test]
fn scalar_json_passthrough() {
    // Scalar::Json must carry arbitrary JSON to sqlgen unchanged.
    let value = json!({"a": [1, "x", null, 2.5], "b": {"nested": true}});
    let scalar = Scalar::Json(value.clone());
    assert_eq!(scalar.as_json(), &value);
    assert_eq!(scalar, Scalar::Json(value.clone()));
    assert_ne!(scalar, Scalar::Json(json!(null)));

    // Externally tagged: {"Json": <value>} — the shape sqlgen snapshots see.
    assert_eq!(serde_json::to_value(&scalar).unwrap(), json!({"Json": value}));
}

#[test]
fn bool_exp_composition_nests_without_flattening() {
    // And/Or/Not compose structurally; nothing collapses or reorders.
    let exp = BoolExp::And(vec![
        BoolExp::Compare {
            column: "id".into(),
            pg_type: "integer".into(),
            op: CompareOp::Eq(Scalar::Json(json!(1))),
        },
        BoolExp::Or(vec![
            BoolExp::Not(Box::new(BoolExp::Compare {
                column: "name".into(),
                pg_type: "text".into(),
                op: CompareOp::IsNull(true),
            })),
            BoolExp::Compare {
                column: "age".into(),
                pg_type: "integer".into(),
                op: CompareOp::In(vec![Scalar::Json(json!(2)), Scalar::Json(json!(3))]),
            },
        ]),
    ]);

    let v = serde_json::to_value(&exp).unwrap();
    assert_eq!(v["And"][0]["Compare"]["column"], json!("id"));
    assert_eq!(v["And"][0]["Compare"]["op"]["Eq"]["Json"], json!(1));
    let or = &v["And"][1]["Or"];
    assert_eq!(or[0]["Not"]["Compare"]["op"]["IsNull"], json!(true));
    assert_eq!(
        or[1]["Compare"]["op"]["In"],
        json!([{"Json": 2}, {"Json": 3}])
    );
}

#[test]
fn bool_exp_relationship_predicate_keeps_join_pairs_in_order() {
    let exp = BoolExp::Relationship {
        table: table("author"),
        join: vec![
            ("author_id".into(), "id".into()),
            ("tenant".into(), "tenant".into()),
        ],
        predicate: Box::new(BoolExp::Compare {
            column: "name".into(),
            pg_type: "text".into(),
            op: CompareOp::Like(Scalar::Json(json!("%a%"))),
        }),
    };
    let v = serde_json::to_value(&exp).unwrap();
    assert_eq!(
        v["Relationship"]["join"],
        json!([["author_id", "id"], ["tenant", "tenant"]])
    );
    assert_eq!(
        v["Relationship"]["predicate"]["Compare"]["op"]["Like"]["Json"],
        json!("%a%")
    );
}

#[test]
fn table_equality_is_schema_and_name() {
    assert_eq!(table("author"), table("author"));
    assert_ne!(table("author"), table("article"));
    assert_ne!(
        table("author"),
        Table {
            schema: "other".into(),
            name: "author".into()
        }
    );
}

#[test]
fn root_field_select_serializes_alias_and_single_flag() {
    let mut query = select("author", vec![column("id", "id")], None);
    query.single = true; // by_pk roots return one nullable object
    query.limit = Some(1);
    let root = RootField::Select {
        alias: "author_by_pk".into(),
        query,
    };
    let v = serde_json::to_value(&root).unwrap();
    assert_eq!(v["Select"]["alias"], json!("author_by_pk"));
    assert_eq!(v["Select"]["query"]["single"], json!(true));
    assert_eq!(v["Select"]["query"]["limit"], json!(1));
    assert_eq!(
        v["Select"]["query"]["from"]["Table"],
        json!({"schema": "public", "name": "author"})
    );
}

#[test]
fn object_relationship_field_nests_a_single_row_query() {
    let inner = SelectQuery {
        single: true, // object relationships are single-row selects
        ..select("author", vec![column("name", "name")], None)
    };
    let field = OutputField {
        alias: "author".into(),
        value: FieldValue::Object {
            query: inner,
            join: vec![("author_id".into(), "id".into())],
        },
    };
    let v = serde_json::to_value(&field).unwrap();
    assert_eq!(v["value"]["Object"]["query"]["single"], json!(true));
    assert_eq!(v["value"]["Object"]["join"], json!([["author_id", "id"]]));
}

#[test]
fn order_by_and_direction_serialize_as_variant_names() {
    let ob = OrderBy {
        target: OrderByTarget::Column("id".into()),
        direction: OrderDirection::Desc,
        nulls: NullsOrder::First,
    };
    let v = serde_json::to_value(&ob).unwrap();
    assert_eq!(v, json!({"target": {"Column": "id"}, "direction": "Desc", "nulls": "First"}));
    assert_eq!(OrderDirection::Asc, OrderDirection::Asc);
    assert_ne!(NullsOrder::First, NullsOrder::Last);
}

#[test]
fn insert_mutation_rows_align_with_columns_and_none_is_null() {
    // Invariant the executor relies on: every row has one entry per
    // insertion column; None serializes to JSON null (rendered DEFAULT).
    let insert = InsertMutation {
        table: table("author"),
        columns: vec![("id".into(), "integer".into()), ("name".into(), "text".into())],
        rows: vec![
            vec![Some(Scalar::Json(json!(1))), Some(Scalar::Json(json!("a")))],
            vec![None, Some(Scalar::Json(json!("b")))],
        ],
        on_conflict: None,
        check: Some(BoolExp::Compare {
            column: "id".into(),
            pg_type: "integer".into(),
            op: CompareOp::Gt(Scalar::Json(json!(0))),
        }),
        check_path: "$.selectionSet.insert_author.args.objects".into(),
        output: MutationOutput::Response(vec![MutationResponseField::AffectedRows {
            alias: "affected_rows".into(),
        }]),
    };
    for row in &insert.rows {
        assert_eq!(row.len(), insert.columns.len());
    }
    let root = MutationRoot::Insert {
        alias: "insert_author".into(),
        insert,
    };
    let v = serde_json::to_value(&root).unwrap();
    assert_eq!(v["Insert"]["insert"]["rows"][1][0], json!(null));
    assert_eq!(v["Insert"]["insert"]["rows"][0][1], json!({"Json": "a"}));
    assert_eq!(
        v["Insert"]["insert"]["output"]["Response"][0]["AffectedRows"]["alias"],
        json!("affected_rows")
    );
}

#[test]
fn clone_is_deep_and_serialization_equivalent() {
    let original = RootField::Select {
        alias: "authors".into(),
        query: select(
            "author",
            vec![
                column("id", "id"),
                OutputField {
                    alias: "articles".into(),
                    value: FieldValue::Array {
                        query: select(
                            "article",
                            vec![column("title", "title")],
                            Some(BoolExp::Compare {
                                column: "published".into(),
                                pg_type: "boolean".into(),
                                op: CompareOp::Eq(Scalar::Json(json!(true))),
                            }),
                        ),
                        join: vec![("id".into(), "author_id".into())],
                        aggregate: false,
                    },
                },
            ],
            Some(BoolExp::Exists {
                table: table("account"),
                predicate: Box::new(BoolExp::Compare {
                    column: "active".into(),
                    pg_type: "boolean".into(),
                    op: CompareOp::Eq(Scalar::Json(json!(true))),
                }),
            }),
        ),
    };
    let cloned = original.clone();
    assert_eq!(
        serde_json::to_value(&original).unwrap(),
        serde_json::to_value(&cloned).unwrap()
    );
    // Debug derives exist and agree too.
    assert_eq!(format!("{original:?}"), format!("{cloned:?}"));
}
