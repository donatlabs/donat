//! Hand-built IR -> SQL unit tests for shapes the pipeline fixtures don't
//! reach: mutation check-expression wrapping, inherited-role cell guards,
//! DEFAULT/on_conflict rendering, stringify-numerics, and quoting rules.

use donat_ir::*;
use donat_sqlgen::{
    mutation_to_sql, operation_to_sql, operation_to_sql_opts, quote_ident, quote_lit,
};
use serde_json::json;

fn table(schema: &str, name: &str) -> Table {
    Table {
        schema: schema.into(),
        name: name.into(),
    }
}

fn column(alias: &str, column: &str, pg_type: &str) -> OutputField {
    OutputField {
        alias: alias.to_string(),
        value: FieldValue::Column {
            column: column.to_string(),
            pg_type: pg_type.to_string(),
        },
    }
}

fn select(fields: Vec<OutputField>, predicate: Option<BoolExp>, single: bool) -> SelectQuery {
    SelectQuery {
        from: FromSource::Table(table("public", "author")),
        fields,
        predicate,
        order_by: vec![],
        limit: None,
        nodes_limit: None,
        offset: None,
        distinct_on: vec![],
        single,
    }
}

fn eq(column: &str, pg_type: &str, value: serde_json::Value) -> BoolExp {
    BoolExp::Compare {
        column: column.to_string(),
        pg_type: pg_type.to_string(),
        op: CompareOp::Eq(Scalar::Json(value)),
    }
}

#[test]
fn by_pk_single_select_is_one_scalar_subquery() {
    let sql = operation_to_sql(&[RootField::Select {
        alias: "author_by_pk".into(),
        query: select(
            vec![column("id", "id", "int4"), column("name", "name", "text")],
            Some(eq("id", "int4", json!(7))),
            true,
        ),
    }]);
    // Single rows render LIMIT 1 with no json_agg wrapping.
    assert!(sql.contains("LIMIT 1"), "missing LIMIT 1 in: {sql}");
    assert!(
        !sql.contains("json_agg"),
        "single row must not aggregate: {sql}"
    );
    insta::assert_snapshot!(sql);
}

#[test]
fn guarded_column_renders_case_when() {
    // Inherited-role cell guard: NULL unless the granting parents' filter
    // passes for the row.
    let sql = operation_to_sql(&[RootField::Select {
        alias: "author".into(),
        query: select(
            vec![OutputField {
                alias: "name".into(),
                value: FieldValue::ColumnGuarded {
                    column: "name".into(),
                    pg_type: "text".into(),
                    guard: eq("id", "int4", json!(2)),
                },
            }],
            None,
            false,
        ),
    }]);
    insta::assert_snapshot!(sql);
}

#[test]
fn aggregate_column_guard_wraps_case() {
    let query = select(
        vec![OutputField {
            alias: "aggregate".into(),
            value: FieldValue::Aggregate {
                fields: vec![AggregateField {
                    alias: "max".into(),
                    op: AggregateOp::ColumnOp {
                        op: "max".into(),
                        columns: vec![AggregateColumn {
                            alias: "id".into(),
                            column: "id".into(),
                            pg_type: "int4".into(),
                            guard: Some(eq("id", "int4", json!(2))),
                        }],
                    },
                }],
            },
        }],
        None,
        false,
    );
    let sql = operation_to_sql(&[RootField::Select {
        alias: "author_aggregate".into(),
        query,
    }]);
    insta::assert_snapshot!(sql);
}

#[test]
fn insert_check_expression_wraps_check_violation() {
    let insert = InsertMutation {
        table: table("public", "author"),
        columns: vec![("name".into(), "text".into())],
        rows: vec![vec![Some(Scalar::Json(json!("bob")))]],
        on_conflict: None,
        check: Some(BoolExp::Compare {
            column: "name".into(),
            pg_type: "text".into(),
            op: CompareOp::Neq(Scalar::Json(json!("admin"))),
        }),
        check_path: "$.selectionSet.insert_author.args.objects".into(),
        output: MutationOutput::Response(vec![MutationResponseField::AffectedRows {
            alias: "affected_rows".into(),
        }]),
    };
    let sql = mutation_to_sql(&MutationRoot::Insert {
        alias: "insert_author".into(),
        insert,
    });
    // The check is enforced in-statement via donat.check_violation,
    // whose payload carries the GraphQL error path.
    assert!(
        sql.contains("donat.check_violation"),
        "missing check wrap in: {sql}"
    );
    assert!(
        sql.contains("$.selectionSet.insert_author.args.objects"),
        "check payload must carry the error path: {sql}"
    );
    insta::assert_snapshot!(sql);
}

#[test]
fn update_check_expression_wraps_check_violation() {
    let update = UpdateMutation {
        table: table("public", "author"),
        sets: vec![SetOp::Set {
            column: "name".into(),
            pg_type: "text".into(),
            value: Scalar::Json(json!("eve")),
        }],
        predicate: Some(eq("id", "int4", json!(1))),
        check: Some(BoolExp::Compare {
            column: "name".into(),
            pg_type: "text".into(),
            op: CompareOp::Neq(Scalar::Json(json!("admin"))),
        }),
        check_path: "$".into(),
        output: MutationOutput::Response(vec![MutationResponseField::AffectedRows {
            alias: "affected_rows".into(),
        }]),
    };
    let sql = mutation_to_sql(&MutationRoot::Update {
        alias: "update_author".into(),
        update,
    });
    assert!(
        sql.contains("donat.check_violation"),
        "missing check wrap in: {sql}"
    );
    insta::assert_snapshot!(sql);
}

#[test]
fn insert_missing_values_render_default_and_do_nothing() {
    let insert = InsertMutation {
        table: table("public", "author"),
        columns: vec![("id".into(), "int4".into()), ("name".into(), "text".into())],
        // Ragged objects: absent values must become DEFAULT, not NULL.
        rows: vec![
            vec![None, Some(Scalar::Json(json!("a")))],
            vec![Some(Scalar::Json(json!(2))), None],
        ],
        on_conflict: Some(OnConflict {
            constraint: "author_pkey".into(),
            update_columns: vec![],
            predicate: None,
            set_ops: vec![],
        }),
        check: None,
        check_path: "$".into(),
        output: MutationOutput::Response(vec![MutationResponseField::AffectedRows {
            alias: "affected_rows".into(),
        }]),
    };
    let sql = mutation_to_sql(&MutationRoot::Insert {
        alias: "insert_author".into(),
        insert,
    });
    insta::assert_snapshot!(sql);
}

#[test]
fn on_conflict_do_update_applies_filter_and_presets() {
    let insert = InsertMutation {
        table: table("public", "author"),
        columns: vec![("id".into(), "int4".into()), ("name".into(), "text".into())],
        rows: vec![vec![
            Some(Scalar::Json(json!(1))),
            Some(Scalar::Json(json!("a"))),
        ]],
        on_conflict: Some(OnConflict {
            constraint: "author_pkey".into(),
            update_columns: vec!["name".into()],
            // The update permission's filter restricts which existing rows
            // may be overwritten; its presets are re-applied.
            predicate: Some(eq("id", "int4", json!(1))),
            set_ops: vec![SetOp::Set {
                column: "updated_by".into(),
                pg_type: "text".into(),
                value: Scalar::Json(json!("1")),
            }],
        }),
        check: None,
        check_path: "$".into(),
        output: MutationOutput::Response(vec![MutationResponseField::AffectedRows {
            alias: "affected_rows".into(),
        }]),
    };
    let sql = mutation_to_sql(&MutationRoot::Insert {
        alias: "insert_author".into(),
        insert,
    });
    insta::assert_snapshot!(sql);
}

#[test]
fn empty_in_and_nin_render_constants() {
    let sql = operation_to_sql(&[RootField::Select {
        alias: "author".into(),
        query: select(
            vec![column("id", "id", "int4")],
            Some(BoolExp::And(vec![
                BoolExp::Compare {
                    column: "id".into(),
                    pg_type: "int4".into(),
                    op: CompareOp::In(vec![]),
                },
                BoolExp::Compare {
                    column: "name".into(),
                    pg_type: "text".into(),
                    op: CompareOp::Nin(vec![]),
                },
            ])),
            false,
        ),
    }]);
    // `_in: []` matches nothing, `_nin: []` matches everything.
    assert!(
        sql.contains("(FALSE AND TRUE)"),
        "wrong empty-list rendering: {sql}"
    );
}

#[test]
fn stringify_numerics_casts_bigint_and_numeric_to_text() {
    let roots = [RootField::Select {
        alias: "author".into(),
        query: select(
            vec![
                column("big", "big", "int8"),
                column("price", "price", "numeric"),
                column("name", "name", "text"),
            ],
            None,
            false,
        ),
    }];
    let sql = operation_to_sql_opts(&roots, true);
    assert!(
        sql.contains("(\"_t0\".\"big\")::text"),
        "int8 not stringified: {sql}"
    );
    assert!(
        sql.contains("(\"_t0\".\"price\")::text"),
        "numeric not stringified: {sql}"
    );
    assert!(
        sql.contains("'name', \"_t0\".\"name\""),
        "text must stay uncast: {sql}"
    );
    // Without the option the casts must not appear.
    let plain = operation_to_sql_opts(&roots, false);
    assert!(
        !plain.contains("::text"),
        "unexpected stringify cast: {plain}"
    );
}

#[test]
fn quote_helpers_double_embedded_quotes() {
    assert_eq!(quote_ident("author"), "\"author\"");
    assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    assert_eq!(quote_lit("plain"), "'plain'");
    assert_eq!(quote_lit("O'Brien"), "'O''Brien'");
}

#[test]
fn st_dwithin_renders_2d_and_3d_functions() {
    // The only upstream 3D fixture is admin-bound (out of conformance
    // scope), so the SQL rendering of both variants is pinned here.
    let st = |three_d: bool| BoolExp::Compare {
        column: "geom_col".to_string(),
        pg_type: "geometry".to_string(),
        op: CompareOp::StDWithin {
            distance: Scalar::Json(json!(5)),
            from: Scalar::Json(json!({"type": "Point", "coordinates": [1.0, 2.0]})),
            three_d,
        },
    };
    let sql = operation_to_sql(&[RootField::Select {
        alias: "rows".to_string(),
        query: select(vec![column("id", "id", "integer")], Some(st(false)), false),
    }]);
    assert!(sql.contains("ST_DWithin("), "missing 2D function: {sql}");
    assert!(
        !sql.contains("ST_3DDWithin("),
        "unexpected 3D function: {sql}"
    );

    let sql = operation_to_sql(&[RootField::Select {
        alias: "rows".to_string(),
        query: select(vec![column("id", "id", "integer")], Some(st(true)), false),
    }]);
    assert!(sql.contains("ST_3DDWithin("), "missing 3D function: {sql}");
}
