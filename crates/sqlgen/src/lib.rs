//! SQL generation (milestone M4) — the core trick of Donat v2.
//!
//! Compiles a whole operation (all root fields) into ONE Postgres statement
//! that returns the final GraphQL `data` object as a single `json` value.
//! `json` (not `jsonb`) everywhere: it preserves key insertion order, which
//! the conformance suite asserts against the selection-set order.
//!
//! Literals are inlined with strict quoting (`'` doubling; Postgres has
//! `standard_conforming_strings = on` by default, so backslashes are inert)
//! and cast to the column's pg type. Parameterized execution can replace
//! this later without touching the IR.

use donat_ir::*;

/// Compile one operation: `SELECT json_build_object('field1', (...), ...)`.
pub fn operation_to_sql(roots: &[RootField]) -> String {
    operation_to_sql_opts(roots, false)
}

/// `stringify_numerics` renders bigint/numeric columns as text
/// (Donat's --stringify-numeric-types).
pub fn operation_to_sql_opts(roots: &[RootField], stringify_numerics: bool) -> String {
    operation_to_sql_full(
        roots,
        stringify_numerics,
        donat_backend::AnyDialect::Postgres(donat_backend::PostgresDialect),
    )
}

/// Like [`operation_to_sql`], but compiling for an explicit backend dialect.
/// The Postgres dialect produces byte-identical SQL to [`operation_to_sql`].
pub fn operation_to_sql_with(roots: &[RootField], dialect: donat_backend::AnyDialect) -> String {
    operation_to_sql_full(roots, false, dialect)
}

/// Compile one operation for an explicit backend while honoring Donat's
/// `--stringify-numeric-types` response option.
pub fn operation_to_sql_opts_with(
    roots: &[RootField],
    stringify_numerics: bool,
    dialect: donat_backend::AnyDialect,
) -> String {
    operation_to_sql_full(roots, stringify_numerics, dialect)
}

fn operation_to_sql_full(
    roots: &[RootField],
    stringify_numerics: bool,
    dialect: donat_backend::AnyDialect,
) -> String {
    let mut ctx = Ctx {
        next_alias: 0,
        stringify_numerics,
        dialect,
    };
    let pairs: Vec<(String, String)> = roots
        .iter()
        .map(|r| match r {
            RootField::Select { alias, query } => (alias.clone(), ctx.select_expr(query, None)),
            RootField::Connection { alias, conn } => {
                (alias.clone(), ctx.connection_expr(conn, None))
            }
            RootField::Typename { alias, value } => {
                (alias.clone(), typename_literal(&ctx.dialect, value))
            }
        })
        .collect();
    format!("SELECT {} AS root", json_object(&ctx.dialect, &pairs))
}

/// base64 without the newlines Postgres' encode() inserts.
fn b64(expr: &str) -> String {
    format!("replace(encode(convert_to({expr}, 'UTF8'), 'base64'), chr(10), '')")
}

struct Ctx {
    next_alias: usize,
    stringify_numerics: bool,
    /// Backend dialect used for the four backend-divergent leaf renderings
    /// (`scalar_sql`, `json_object`, `json_array_agg`, `to_json_text`). The
    /// identifier/literal/limit ops are backend-identical and stay as free
    /// functions.
    dialect: donat_backend::AnyDialect,
}

#[derive(Debug, Clone)]
struct RelationshipCteOverride {
    table: Table,
    /// Join condition pairs: (local column on the outer row, remote column on
    /// the relationship target).
    join: Vec<(String, String)>,
    cte: String,
}

/// Join condition pairs against an enclosing table alias:
/// (local column on the outer table, remote column on the inner table).
type OuterJoin<'a> = (&'a [(String, String)], &'a str);

impl Ctx {
    fn alias(&mut self) -> String {
        let n = self.next_alias;
        self.next_alias += 1;
        format!("_t{n}")
    }

    /// Relay cursor for the current row: base64 of {"pk" : v}.
    fn cursor_expr(&mut self, alias: &str, pk: &[(String, String)]) -> String {
        let pairs: Vec<String> = pk
            .iter()
            .map(|(col, _)| {
                format!(
                    "{} || to_json({})::text",
                    quote_lit(&format!("\"{col}\" : ")),
                    qualified(alias, col)
                )
            })
            .collect();
        let body = pairs.join(" || ', ' || ");
        b64(&format!("'{{' || {body} || '}}'"))
    }

    /// Relay global id: base64 of [1, "schema", "table", pk...].
    fn global_id_expr(
        &mut self,
        alias: &str,
        schema: &str,
        table: &str,
        pk: &[(String, String)],
    ) -> String {
        let mut parts = vec![format!("'[1, \"{schema}\", \"{table}\"'")];
        for (col, _) in pk {
            parts.push(format!("', ' || to_json({})::text", qualified(alias, col)));
        }
        let body = parts.join(" || ");
        b64(&format!("{body} || ']'"))
    }

    /// A parenthesized scalar subquery producing a connection's JSON value.
    fn connection_expr(&mut self, conn: &Connection, outer: Option<OuterJoin>) -> String {
        let dialect = self.dialect;
        let alias = self.alias();
        let row_json = self.row_json(&conn.query.fields, &alias);
        let cursor = self.cursor_expr(&alias, &conn.pk);

        // Deterministic ordering: append pk (reversed when paging back).
        let mut q = conn.query.clone();
        let backward = conn.page.as_ref().is_some_and(|p| p.backward);
        for (col, _) in &conn.pk {
            if !q
                .order_by
                .iter()
                .any(|ob| matches!(&ob.target, OrderByTarget::Column(c) if c == col))
            {
                q.order_by.push(OrderBy {
                    target: OrderByTarget::Column(col.clone()),
                    direction: if backward {
                        OrderDirection::Desc
                    } else {
                        OrderDirection::Asc
                    },
                    nulls: NullsOrder::Last,
                });
            }
        }
        if let Some(page) = &conn.page {
            q.limit = Some(page.size + 1);
        }
        let (tail, _) = self.from_where_order(&q, &alias, outer);

        let arr = self.alias();
        let raw = format!("{}.a", quote_ident(&arr));
        // The visible page: size rows of the size+1 fetched, re-reversed
        // for backward iteration.
        let a = match &conn.page {
            None => raw.clone(),
            Some(page) => {
                let order = if page.backward { "t.i DESC" } else { "t.i ASC" };
                // Only the json_agg leaf is delegated; the surrounding
                // json_array_elements(...) WITH ORDINALITY size-limited wrapper
                // stays inline (no leaf for that shape).
                let agg = json_array_agg(&dialect, "t.e", Some(order));
                format!(
                    "(SELECT {agg} FROM json_array_elements({raw}) WITH ORDINALITY AS t(e, i) WHERE t.i <= {size})",
                    size = page.size
                )
            }
        };
        let has_more = format!(
            "(json_array_length({raw}) > {})",
            conn.page.as_ref().map(|p| p.size).unwrap_or(u64::MAX)
        );
        let pairs: Vec<(String, String)> = conn
            .fields
            .iter()
            .map(|f| match f {
                ConnectionField::Typename { alias, value } => {
                    (alias.clone(), typename_literal(&dialect, value))
                }
                ConnectionField::PageInfo { alias, fields } => {
                    let inner: Vec<(String, String)> = fields
                        .iter()
                        .map(|(fa, name)| {
                            let value = match name.as_str() {
                                "startCursor" => format!("({a}->0->>'cursor')"),
                                "endCursor" => format!(
                                    "({a}->(json_array_length({a})-1)->>'cursor')"
                                ),
                                "hasNextPage" => match &conn.page {
                                    Some(p) if !p.backward => has_more.clone(),
                                    Some(p) if p.has_other_side => "true".to_string(),
                                    _ => "false".to_string(),
                                },
                                "hasPreviousPage" => match &conn.page {
                                    Some(p) if p.backward => has_more.clone(),
                                    Some(p) if p.has_other_side => "true".to_string(),
                                    _ => "false".to_string(),
                                },
                                _ => "null".to_string(),
                            };
                            (fa.clone(), value)
                        })
                        .collect();
                    (alias.clone(), json_object(&dialect, &inner))
                }
                ConnectionField::Edges { alias, fields } => {
                    // Re-project the prebuilt edges array onto the selection.
                    let inner: Vec<(String, String)> = fields
                        .iter()
                        .map(|ef| match ef {
                            EdgeField::Cursor { alias } => {
                                (alias.clone(), "e.value->'cursor'".to_string())
                            }
                            EdgeField::Node { alias } => {
                                (alias.clone(), "e.value->'node'".to_string())
                            }
                            EdgeField::Typename { alias, value } => {
                                (alias.clone(), typename_literal(&dialect, value))
                            }
                        })
                        .collect();
                    // The json_build_object leaf is delegated; the coalesce of a
                    // SELECT json_agg(...) subquery has no leaf and stays inline.
                    (
                        alias.clone(),
                        format!(
                            "coalesce((SELECT json_agg({}) FROM json_array_elements({a}) AS e), '[]'::json)",
                            json_object(&dialect, &inner)
                        ),
                    )
                }
            })
            .collect();

        // The relay edges array (json_agg of cursor/node objects, coalesced to
        // []) is a clean array-agg leaf; the cursor/node object is a leaf too.
        let edge_obj = json_object(
            &dialect,
            &[
                (
                    "cursor".to_string(),
                    format!("{ed}.c", ed = quote_ident(&format!("{arr}_e"))),
                ),
                (
                    "node".to_string(),
                    format!("{ed}.n", ed = quote_ident(&format!("{arr}_e"))),
                ),
            ],
        );
        format!(
            "(SELECT {obj} FROM (SELECT {agg} AS a FROM (SELECT {cursor} AS c, {row_json} AS n {tail}) AS {ed}) AS {arr_q})",
            obj = json_object(&dialect, &pairs),
            agg = json_array_agg(&dialect, &edge_obj, None),
            ed = quote_ident(&format!("{arr}_e")),
            arr_q = quote_ident(&arr),
        )
    }

    /// A parenthesized scalar subquery producing this select's JSON value.
    fn select_expr(&mut self, q: &SelectQuery, outer: Option<OuterJoin>) -> String {
        if q.fields.iter().any(|f| {
            matches!(
                f.value,
                FieldValue::Aggregate { .. } | FieldValue::Nodes { .. }
            )
        }) {
            return self.aggregate_expr(q, outer);
        }

        let alias = self.alias();
        let row_json = self.row_json(&q.fields, &alias);
        let (tail, rendered_order) = self.from_where_order(q, &alias, outer);
        let distinct = distinct_clause(q, &alias);

        if q.single {
            format!("(SELECT {distinct}{row_json} {tail} LIMIT 1)")
        } else {
            let elem = self.alias();
            let e = quote_ident(&elem);
            let stable_order = if matches!(
                self.dialect,
                donat_backend::AnyDialect::Clickhouse(_) | donat_backend::AnyDialect::Mysql(_)
            ) {
                rendered_order
                    .as_ref()
                    .map(|_| format!("{e}.{}", quote_ident("__donat_ord")))
            } else {
                None
            };
            let row_projection = match rendered_order.as_deref() {
                Some(order)
                    if matches!(
                        self.dialect,
                        donat_backend::AnyDialect::Clickhouse(_)
                            | donat_backend::AnyDialect::Mysql(_)
                    ) =>
                {
                    format!(
                        "{row_json} AS j, row_number() OVER (ORDER BY {order}) AS {}",
                        quote_ident("__donat_ord")
                    )
                }
                _ => format!("{row_json} AS j"),
            };
            format!(
                "(SELECT {agg} FROM (SELECT {distinct}{row_projection} {tail}) AS {e})",
                agg = json_array_agg(&self.dialect, &format!("{e}.j"), stable_order.as_deref()),
            )
        }
    }

    /// `<t>_aggregate` (root or relationship): aggregate + nodes over one
    /// filtered row set.
    fn aggregate_expr(&mut self, q: &SelectQuery, outer: Option<OuterJoin>) -> String {
        let dialect = self.dialect;
        let inner_alias = self.alias();
        let (tail, _) = self.from_where_order(q, &inner_alias, outer);
        let distinct = distinct_clause(q, &inner_alias);
        let outer_alias = self.alias();
        let oa = quote_ident(&outer_alias);

        let pairs: Vec<(String, String)> = q
            .fields
            .iter()
            .map(|f| {
                let value = match &f.value {
                    FieldValue::Aggregate { fields } => self.aggregate_json(fields, &outer_alias),
                    FieldValue::Nodes { fields } => {
                        if let Some(nodes_limit) = q.nodes_limit {
                            // The permission limit caps visible rows but
                            // not aggregates: nodes get their own select.
                            let limit = Some(q.limit.map_or(nodes_limit, |l| l.min(nodes_limit)));
                            let nodes_query = SelectQuery {
                                from: q.from.clone(),
                                fields: fields.clone(),
                                predicate: q.predicate.clone(),
                                order_by: q.order_by.clone(),
                                limit,
                                nodes_limit: None,
                                offset: q.offset,
                                distinct_on: q.distinct_on.clone(),
                                single: false,
                            };
                            self.select_expr(&nodes_query, outer)
                        } else {
                            let row = self.row_json(fields, &outer_alias);
                            json_array_agg(&dialect, &row, None)
                        }
                    }
                    FieldValue::Typename { value } => typename_literal(&dialect, value),
                    other => panic!("non-aggregate field in aggregate select: {other:?}"),
                };
                (f.alias.clone(), value)
            })
            .collect();

        format!(
            "(SELECT {obj} FROM (SELECT {distinct}* {tail}) AS {oa})",
            obj = json_object(&dialect, &pairs),
        )
    }

    fn aggregate_json(&mut self, fields: &[AggregateField], table_alias: &str) -> String {
        let dialect = self.dialect;
        let pairs: Vec<(String, String)> = fields
            .iter()
            .map(|f| {
                let value = match &f.op {
                    AggregateOp::Typename { value } => typename_literal(&dialect, value),
                    AggregateOp::Count { distinct, columns } => {
                        let value = if columns.is_empty() {
                            "COUNT(*)".to_string()
                        } else {
                            let cols: Vec<String> =
                                columns.iter().map(|c| qualified(table_alias, c)).collect();
                            let d = if *distinct { "DISTINCT " } else { "" };
                            // Multiple columns need a row constructor.
                            let expr = if cols.len() == 1 {
                                cols.join(", ")
                            } else {
                                format!("({})", cols.join(", "))
                            };
                            format!("COUNT({d}{expr})")
                        };
                        match self.dialect {
                            donat_backend::AnyDialect::Clickhouse(_) => {
                                clickhouse_json_column(&value, "int8", false)
                            }
                            donat_backend::AnyDialect::Mysql(_) => {
                                mysql_json_column(&value, "int8", false)
                            }
                            _ => value,
                        }
                    }
                    AggregateOp::ColumnOp { op, columns } => {
                        let inner: Vec<(String, String)> = columns
                            .iter()
                            .map(|c| {
                                let col = qualified(table_alias, &c.column);
                                let expr = match &c.guard {
                                    Some(guard) => {
                                        let cond = self.bool_exp(guard, table_alias, table_alias);
                                        format!("CASE WHEN {cond} THEN {col} ELSE NULL END")
                                    }
                                    None => col,
                                };
                                let value = if matches!(
                                    self.dialect,
                                    donat_backend::AnyDialect::Clickhouse(_)
                                ) {
                                    format!("{}OrNull({expr})", clickhouse_aggregate_function(op))
                                } else {
                                    format!("{op}({expr})")
                                };
                                let value = match self.dialect {
                                    donat_backend::AnyDialect::Clickhouse(_) => {
                                        clickhouse_json_column(
                                            &value,
                                            &c.pg_type,
                                            self.stringify_numerics,
                                        )
                                    }
                                    donat_backend::AnyDialect::Mysql(_) => mysql_json_column(
                                        &value,
                                        &c.pg_type,
                                        self.stringify_numerics,
                                    ),
                                    _ => value,
                                };
                                (c.alias.clone(), value)
                            })
                            .collect();
                        json_object(&dialect, &inner)
                    }
                };
                (f.alias.clone(), value)
            })
            .collect();
        json_object(&dialect, &pairs)
    }

    /// `FROM .. WHERE .. ORDER BY .. LIMIT .. OFFSET ..` for one select.
    fn from_where_order(
        &mut self,
        q: &SelectQuery,
        alias: &str,
        outer: Option<OuterJoin>,
    ) -> (String, Option<String>) {
        let dialect = self.dialect;
        let from_item = match &q.from {
            FromSource::Table(t) => {
                format!("{}.{}", quote_ident(&t.schema), quote_ident(&t.name))
            }
            FromSource::Function { schema, name, args } => {
                let rendered: Vec<String> = args
                    .iter()
                    .map(|a| {
                        let value = scalar_sql(&dialect, &a.value, &a.pg_type);
                        match &a.name {
                            Some(arg_name) => {
                                format!("{} => {value}", quote_ident(arg_name))
                            }
                            None => value,
                        }
                    })
                    .collect();
                format!(
                    "{}.{}({})",
                    quote_ident(schema),
                    quote_ident(name),
                    rendered.join(", ")
                )
            }
            FromSource::RowFunction { schema, name, args } => {
                let outer_alias = outer
                    .map(|(_, a)| a)
                    .expect("row function requires an enclosing row");
                let rendered: Vec<String> = args
                    .iter()
                    .map(|a| row_function_arg(&dialect, a, outer_alias))
                    .collect();
                format!(
                    "{}.{}({})",
                    quote_ident(schema),
                    quote_ident(name),
                    rendered.join(", ")
                )
            }
        };
        let mut sql = format!("FROM {from_item} AS {}", quote_ident(alias));

        let mut conds: Vec<String> = vec![];
        if let Some((join, outer_alias)) = outer {
            for (local, remote) in join {
                conds.push(format!(
                    "{} = {}",
                    qualified(alias, remote),
                    qualified(outer_alias, local)
                ));
            }
        }
        if let Some(pred) = &q.predicate {
            conds.push(self.bool_exp(pred, alias, alias));
        }
        if !conds.is_empty() {
            sql.push_str(&format!(" WHERE {}", conds.join(" AND ")));
        }

        let rendered_order = if !q.order_by.is_empty() {
            let items: Vec<String> = q
                .order_by
                .iter()
                .map(|ob| {
                    let target = match &ob.target {
                        OrderByTarget::Column(c) => qualified(alias, c),
                        OrderByTarget::Relationship {
                            table,
                            join,
                            column,
                            predicate,
                        } => {
                            let ra = self.alias();
                            let mut conds: Vec<String> = join
                                .iter()
                                .map(|(local, remote)| {
                                    format!(
                                        "{} = {}",
                                        qualified(&ra, remote),
                                        qualified(alias, local)
                                    )
                                })
                                .collect();
                            if let Some(pred) = predicate {
                                conds.push(self.bool_exp(pred, &ra, &ra));
                            }
                            format!(
                                "(SELECT {} FROM {}.{} AS {} WHERE {} LIMIT 1)",
                                qualified(&ra, column),
                                quote_ident(&table.schema),
                                quote_ident(&table.name),
                                quote_ident(&ra),
                                conds.join(" AND ")
                            )
                        }
                        OrderByTarget::RelationshipAggregate {
                            table,
                            join,
                            function,
                            column,
                            predicate,
                        } => {
                            let ra = self.alias();
                            let mut conds: Vec<String> = join
                                .iter()
                                .map(|(local, remote)| {
                                    format!(
                                        "{} = {}",
                                        qualified(&ra, remote),
                                        qualified(alias, local)
                                    )
                                })
                                .collect();
                            if let Some(pred) = predicate {
                                conds.push(self.bool_exp(pred, &ra, &ra));
                            }
                            let agg = match column {
                                Some(c) => format!("{function}({})", qualified(&ra, c)),
                                None => "count(*)".to_string(),
                            };
                            format!(
                                "(SELECT {agg} FROM {}.{} AS {} WHERE {})",
                                quote_ident(&table.schema),
                                quote_ident(&table.name),
                                quote_ident(&ra),
                                conds.join(" AND ")
                            )
                        }
                    };
                    let dir = match ob.direction {
                        OrderDirection::Asc => "ASC",
                        OrderDirection::Desc => "DESC",
                    };
                    // Null-ordering is a backend-divergent leaf: Postgres and
                    // SQLite emit `NULLS FIRST/LAST` (the dialect's default
                    // body reproduces sqlgen's historical output byte-for-byte),
                    // while MySQL omits it (the clause is a parse error there).
                    let nulls = {
                        use donat_backend::Dialect;
                        self.dialect
                            .null_ordering(matches!(ob.nulls, NullsOrder::First))
                    };
                    format!("{target} {dir}{nulls}")
                })
                .collect();
            let rendered = items.join(", ");
            sql.push_str(&format!(" ORDER BY {rendered}"));
            Some(rendered)
        } else {
            None
        };

        use donat_backend::Dialect;
        sql.push_str(&self.dialect.limit_offset(q.limit, q.offset));
        (sql, rendered_order)
    }

    fn row_json(&mut self, fields: &[OutputField], table_alias: &str) -> String {
        let dialect = self.dialect;
        let pairs: Vec<(String, String)> = fields
            .iter()
            .map(|f| {
                let value = match &f.value {
                    FieldValue::ColumnGuarded {
                        column,
                        pg_type,
                        guard,
                    } => {
                        let col = self.column_output(table_alias, column, pg_type);
                        let cond = self.bool_exp(guard, table_alias, table_alias);
                        format!("CASE WHEN {cond} THEN {col} ELSE NULL END")
                    }
                    FieldValue::Column { column, pg_type } => {
                        self.column_output(table_alias, column, pg_type)
                    }
                    FieldValue::Typename { value } => typename_literal(&dialect, value),
                    FieldValue::Object { query, join } => {
                        self.select_expr(query, Some((join, table_alias)))
                    }
                    FieldValue::Array { query, join, .. } => {
                        self.select_expr(query, Some((join, table_alias)))
                    }
                    FieldValue::RelayGlobalId { schema, table, pk } => {
                        let schema = schema.clone();
                        let table = table.clone();
                        let pk = pk.clone();
                        self.global_id_expr(table_alias, &schema, &table, &pk)
                    }
                    FieldValue::NestedConnection { conn } => {
                        self.connection_expr(conn, Some((&conn.join, table_alias)))
                    }
                    // The placeholder is replaced after the source query by
                    // the remote-join resolver. PostgreSQL needs the JSON
                    // cast to keep `json_build_object` typed; the portable
                    // backends accept a plain SQL NULL in their JSON object
                    // builders and reject PostgreSQL's `::json` syntax.
                    FieldValue::RemoteJoin { .. } => match dialect {
                        donat_backend::AnyDialect::Postgres(_) => "NULL::json".to_string(),
                        _ => "NULL".to_string(),
                    },
                    FieldValue::ComputedScalar {
                        schema,
                        name,
                        args,
                        guard,
                    } => {
                        let rendered: Vec<String> = args
                            .iter()
                            .map(|a| row_function_arg(&dialect, a, table_alias))
                            .collect();
                        let call = format!(
                            "{}.{}({})",
                            quote_ident(schema),
                            quote_ident(name),
                            rendered.join(", ")
                        );
                        match guard {
                            Some(guard) => {
                                let cond = self.bool_exp(guard, table_alias, table_alias);
                                format!("CASE WHEN {cond} THEN {call} ELSE NULL END")
                            }
                            None => call,
                        }
                    }
                    FieldValue::Aggregate { .. } | FieldValue::Nodes { .. } => {
                        panic!("aggregate fields must go through aggregate_expr")
                    }
                };
                (f.alias.clone(), value)
            })
            .collect();
        json_object(&dialect, &pairs)
    }

    /// Column output expression with type-specific casts.
    fn column_output(&mut self, table_alias: &str, column: &str, pg_type: &str) -> String {
        let col = qualified(table_alias, column);
        if matches!(self.dialect, donat_backend::AnyDialect::Clickhouse(_)) {
            return clickhouse_json_column(&col, pg_type, self.stringify_numerics);
        }
        if matches!(self.dialect, donat_backend::AnyDialect::Mysql(_)) {
            return mysql_json_column(&col, pg_type, self.stringify_numerics);
        }
        if matches!(self.dialect, donat_backend::AnyDialect::Sqlite(_)) {
            return sqlite_json_column(&col, pg_type, self.stringify_numerics);
        }
        match pg_type {
            "geometry" | "geography" => format!("ST_AsGeoJSON({col}, 15, 4)::json"),
            "int8" | "numeric" if self.stringify_numerics => format!("({col})::text"),
            _ => col,
        }
    }

    fn bool_exp(&mut self, exp: &BoolExp, alias: &str, root: &str) -> String {
        self.bool_exp_with_relationship_ctes(exp, alias, root, &[])
    }

    fn bool_exp_with_relationship_ctes(
        &mut self,
        exp: &BoolExp,
        alias: &str,
        root: &str,
        relationship_ctes: &[RelationshipCteOverride],
    ) -> String {
        let dialect = self.dialect;
        match exp {
            BoolExp::And(exps) => {
                if exps.is_empty() {
                    "TRUE".into()
                } else {
                    let parts: Vec<String> = exps
                        .iter()
                        .map(|e| {
                            self.bool_exp_with_relationship_ctes(e, alias, root, relationship_ctes)
                        })
                        .collect();
                    format!("({})", parts.join(" AND "))
                }
            }
            BoolExp::Or(exps) => {
                if exps.is_empty() {
                    "FALSE".into()
                } else {
                    let parts: Vec<String> = exps
                        .iter()
                        .map(|e| {
                            self.bool_exp_with_relationship_ctes(e, alias, root, relationship_ctes)
                        })
                        .collect();
                    format!("({})", parts.join(" OR "))
                }
            }
            BoolExp::Not(inner) => format!(
                "(NOT {})",
                self.bool_exp_with_relationship_ctes(inner, alias, root, relationship_ctes)
            ),
            BoolExp::Compare {
                column,
                pg_type,
                op,
            } => {
                let col = qualified(alias, column);
                self.compare(&col, pg_type, op, alias, root)
            }
            BoolExp::Relationship {
                table,
                join,
                predicate,
            } => {
                let ra = self.alias();
                let from = relationship_ctes
                    .iter()
                    .find(|override_| override_.table == *table && override_.join == *join)
                    .map(|override_| quote_ident(&override_.cte))
                    .unwrap_or_else(|| {
                        format!(
                            "{}.{}",
                            quote_ident(&table.schema),
                            quote_ident(&table.name)
                        )
                    });
                let mut conds: Vec<String> = join
                    .iter()
                    .map(|(local, remote)| {
                        format!("{} = {}", qualified(&ra, remote), qualified(alias, local))
                    })
                    .collect();
                conds.push(self.bool_exp_with_relationship_ctes(
                    predicate,
                    &ra,
                    root,
                    relationship_ctes,
                ));
                format!(
                    "EXISTS (SELECT 1 FROM {from} AS {} WHERE {})",
                    quote_ident(&ra),
                    conds.join(" AND ")
                )
            }
            BoolExp::ComputedCompare {
                schema,
                name,
                args,
                pg_type,
                op,
            } => {
                let rendered: Vec<String> = args
                    .iter()
                    .map(|a| row_function_arg(&dialect, a, alias))
                    .collect();
                let expr = format!(
                    "{}.{}({})",
                    quote_ident(schema),
                    quote_ident(name),
                    rendered.join(", ")
                );
                self.compare(&expr, pg_type, op, alias, root)
            }
            BoolExp::Exists { table, predicate } => {
                let ra = self.alias();
                let pred = self.bool_exp(predicate, &ra, &ra);
                format!(
                    "EXISTS (SELECT 1 FROM {}.{} AS {} WHERE {})",
                    quote_ident(&table.schema),
                    quote_ident(&table.name),
                    quote_ident(&ra),
                    pred
                )
            }
            BoolExp::RowFunctionExists {
                schema,
                name,
                args,
                predicate,
            } => {
                let ra = self.alias();
                let rendered: Vec<String> = args
                    .iter()
                    .map(|a| row_function_arg(&dialect, a, alias))
                    .collect();
                let pred = self.bool_exp(predicate, &ra, root);
                format!(
                    "EXISTS (SELECT 1 FROM {}.{}({}) AS {} WHERE {})",
                    quote_ident(schema),
                    quote_ident(name),
                    rendered.join(", "),
                    quote_ident(&ra),
                    pred
                )
            }
        }
    }

    fn compare(
        &mut self,
        col: &str,
        pg_type: &str,
        op: &CompareOp,
        alias: &str,
        root: &str,
    ) -> String {
        let dialect = self.dialect;
        let lit = |s: &Scalar| scalar_sql(&dialect, s, pg_type);
        match op {
            CompareOp::Eq(v) => format!("{col} = {}", lit(v)),
            CompareOp::Neq(v) => format!("{col} <> {}", lit(v)),
            CompareOp::Gt(v) => format!("{col} > {}", lit(v)),
            CompareOp::Lt(v) => format!("{col} < {}", lit(v)),
            CompareOp::Gte(v) => format!("{col} >= {}", lit(v)),
            CompareOp::Lte(v) => format!("{col} <= {}", lit(v)),
            CompareOp::In(vs) => {
                if vs.is_empty() {
                    "FALSE".into()
                } else {
                    let items: Vec<String> = vs.iter().map(lit).collect();
                    format!("{col} IN ({})", items.join(", "))
                }
            }
            CompareOp::Nin(vs) => {
                if vs.is_empty() {
                    "TRUE".into()
                } else {
                    let items: Vec<String> = vs.iter().map(lit).collect();
                    format!("{col} NOT IN ({})", items.join(", "))
                }
            }
            CompareOp::Like(v) => format!("{col} LIKE {}", lit(v)),
            CompareOp::Nlike(v) => format!("{col} NOT LIKE {}", lit(v)),
            CompareOp::Ilike(v) => format!("{col} ILIKE {}", lit(v)),
            CompareOp::Nilike(v) => format!("{col} NOT ILIKE {}", lit(v)),
            CompareOp::Similar(v) => format!("{col} SIMILAR TO {}", lit(v)),
            CompareOp::Nsimilar(v) => format!("{col} NOT SIMILAR TO {}", lit(v)),
            CompareOp::Regex(v) => format!("{col} ~ {}", lit(v)),
            CompareOp::Iregex(v) => format!("{col} ~* {}", lit(v)),
            CompareOp::Nregex(v) => format!("{col} !~ {}", lit(v)),
            CompareOp::Niregex(v) => format!("{col} !~* {}", lit(v)),
            CompareOp::IsNull(true) => format!("{col} IS NULL"),
            CompareOp::IsNull(false) => format!("{col} IS NOT NULL"),
            CompareOp::CompareColumn {
                sql_op,
                column,
                root: use_root,
            } => {
                let base = if *use_root { root } else { alias };
                format!("{col} {sql_op} {}", qualified(base, column))
            }
            CompareOp::CompareColumnRel {
                sql_op,
                table,
                join,
                column,
            } => {
                let ra = self.alias();
                let conds: Vec<String> = join
                    .iter()
                    .map(|(local, remote)| {
                        format!("{} = {}", qualified(&ra, remote), qualified(alias, local))
                    })
                    .collect();
                format!(
                    "{col} {sql_op} (SELECT {} FROM {}.{} AS {} WHERE {} LIMIT 1)",
                    qualified(&ra, column),
                    quote_ident(&table.schema),
                    quote_ident(&table.name),
                    quote_ident(&ra),
                    conds.join(" AND ")
                )
            }
            CompareOp::HasKey(v) => format!("{col} ? {}", scalar_sql(&dialect, v, "text")),
            CompareOp::HasKeysAny(keys) => format!("{col} ?| {}", text_array(keys)),
            CompareOp::HasKeysAll(keys) => format!("{col} ?& {}", text_array(keys)),
            CompareOp::Contains(v) => format!("{col} @> {}", scalar_sql(&dialect, v, "jsonb")),
            CompareOp::ContainedIn(v) => format!("{col} <@ {}", scalar_sql(&dialect, v, "jsonb")),
            CompareOp::StOp { function, value } => {
                format!("{function}({col}, {})", geometry_sql(value, pg_type))
            }
            CompareOp::StDWithin {
                distance,
                from,
                three_d,
            } => {
                let func = if *three_d {
                    "ST_3DDWithin"
                } else {
                    "ST_DWithin"
                };
                format!(
                    "{func}({col}, {}, {})",
                    geometry_sql(from, pg_type),
                    scalar_sql(&dialect, distance, "float8")
                )
            }
        }
    }
}

fn text_array(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| quote_lit(s)).collect();
    format!("array[{}]::text[]", quoted.join(", "))
}

/// A geometry/geography literal: GeoJSON objects (or strings holding
/// GeoJSON, e.g. from session variables) go through ST_GeomFromGeoJSON;
/// other strings are assumed to be WKT/EWKT.
fn geometry_sql(value: &Scalar, pg_type: &str) -> String {
    let cast = quote_ident(pg_type);
    match value.as_json() {
        serde_json::Value::Object(_) => format!(
            "(ST_GeomFromGeoJSON({}))::{cast}",
            quote_lit(&value.as_json().to_string())
        ),
        serde_json::Value::String(s) if s.trim_start().starts_with('{') => {
            format!("(ST_GeomFromGeoJSON({}))::{cast}", quote_lit(s))
        }
        serde_json::Value::String(s) => format!("({})::{cast}", quote_lit(s)),
        other => format!("({})::{cast}", quote_lit(&other.to_string())),
    }
}

/// Compile one mutation root field into one SQL statement. The statement
/// computes the GraphQL value of the field as a single `json` column named
/// `root`. Permission check expressions are enforced in-statement via
/// `donat.check_violation(...)`, which raises SQLSTATE 23514.
pub fn mutation_to_sql(root: &MutationRoot) -> String {
    mutation_to_sql_opts(root, false)
}

pub fn mutation_to_sql_opts(root: &MutationRoot, stringify_numerics: bool) -> String {
    mutation_to_sql_full(
        root,
        stringify_numerics,
        donat_backend::AnyDialect::Postgres(donat_backend::PostgresDialect),
    )
}

/// Like [`mutation_to_sql`], but compiling for an explicit backend dialect.
/// The Postgres dialect produces byte-identical SQL to [`mutation_to_sql`].
pub fn mutation_to_sql_with(root: &MutationRoot, dialect: donat_backend::AnyDialect) -> String {
    mutation_to_sql_full(root, false, dialect)
}

fn mutation_to_sql_full(
    root: &MutationRoot,
    stringify_numerics: bool,
    dialect: donat_backend::AnyDialect,
) -> String {
    let mut ctx = Ctx {
        next_alias: 0,
        stringify_numerics,
        dialect,
    };
    let dialect = ctx.dialect;
    match root {
        MutationRoot::Typename { value, .. } => {
            format!("SELECT {}::text AS root", quote_lit(value))
        }
        MutationRoot::FunctionCall { query, .. } => {
            format!("SELECT {} AS root", ctx.select_expr(query, None))
        }
        MutationRoot::Insert { insert, .. } => {
            let cols: Vec<String> = insert
                .columns
                .iter()
                .map(|(name, _)| quote_ident(name))
                .collect();
            let rows: Vec<String> = insert
                .rows
                .iter()
                .map(|row| {
                    let values: Vec<String> = row
                        .iter()
                        .zip(&insert.columns)
                        .map(|(v, (_, pg_type))| match v {
                            None => "DEFAULT".to_string(),
                            Some(s) => scalar_sql(&dialect, s, pg_type),
                        })
                        .collect();
                    format!("({})", values.join(", "))
                })
                .collect();
            let mut stmt = format!(
                "INSERT INTO {}.{} ({}) VALUES {}",
                quote_ident(&insert.table.schema),
                quote_ident(&insert.table.name),
                cols.join(", "),
                rows.join(", ")
            );
            if let Some(oc) = &insert.on_conflict {
                if oc.update_columns.is_empty() && oc.set_ops.is_empty() {
                    stmt.push_str(&format!(
                        " ON CONFLICT ON CONSTRAINT {} DO NOTHING",
                        quote_ident(&oc.constraint)
                    ));
                } else {
                    let mut sets: Vec<String> = oc
                        .update_columns
                        .iter()
                        .map(|c| format!("{} = EXCLUDED.{}", quote_ident(c), quote_ident(c)))
                        .collect();
                    for op in &oc.set_ops {
                        match op {
                            SetOp::Set {
                                column,
                                pg_type,
                                value,
                            } => sets.push(format!(
                                "{} = {}",
                                quote_ident(column),
                                scalar_sql(&dialect, value, pg_type)
                            )),
                            SetOp::Inc {
                                column,
                                pg_type,
                                value,
                            } => sets.push(format!(
                                "{} = {}.{} + {}",
                                quote_ident(column),
                                quote_ident(&insert.table.name),
                                quote_ident(column),
                                scalar_sql(&dialect, value, pg_type)
                            )),
                            SetOp::JsonbAppend { column, value } => sets.push(format!(
                                "{} = COALESCE({}.{}, '{{}}'::jsonb) || {}",
                                quote_ident(column),
                                quote_ident(&insert.table.name),
                                quote_ident(column),
                                scalar_sql(&dialect, value, "jsonb")
                            )),
                        }
                    }
                    stmt.push_str(&format!(
                        " ON CONFLICT ON CONSTRAINT {} DO UPDATE SET {}",
                        quote_ident(&oc.constraint),
                        sets.join(", ")
                    ));
                    if let Some(pred) = &oc.predicate {
                        // In DO UPDATE, the existing row is addressable by
                        // the table name.
                        let cond = ctx.bool_exp(pred, &insert.table.name, &insert.table.name);
                        stmt.push_str(&format!(" WHERE {cond}"));
                    }
                }
            }
            stmt.push_str(" RETURNING *");
            let mut extra_ctes = vec![];
            let mut extra_checks = vec![];
            for (idx, nested) in insert.nested_object_inserts.iter().enumerate() {
                let cte = format!("{}__nested_{idx}", nested.relationship_name);
                let mut cols: Vec<String> = nested
                    .column_mapping
                    .iter()
                    .map(|(_, child)| quote_ident(child))
                    .collect();
                cols.extend(nested.columns.iter().map(|(name, _)| quote_ident(name)));

                let mut values: Vec<String> = nested
                    .column_mapping
                    .iter()
                    .map(|(parent, _)| qualified("ins", parent))
                    .collect();
                values.extend(nested.row.iter().zip(&nested.columns).map(
                    |(value, (_, pg_type))| match value {
                        None => "DEFAULT".to_string(),
                        Some(s) => scalar_sql(&dialect, s, pg_type),
                    },
                ));
                extra_ctes.push(format!(
                    "{} AS (INSERT INTO {}.{} ({}) SELECT {} FROM {} RETURNING *)",
                    quote_ident(&cte),
                    quote_ident(&nested.table.schema),
                    quote_ident(&nested.table.name),
                    cols.join(", "),
                    values.join(", "),
                    quote_ident("ins")
                ));
                if let Some(check) = &nested.check {
                    let parent_join = nested
                        .column_mapping
                        .iter()
                        .map(|(parent, child)| (child.clone(), parent.clone()))
                        .collect();
                    extra_checks.push((
                        cte,
                        check,
                        nested.check_path.clone(),
                        vec![RelationshipCteOverride {
                            table: insert.table.clone(),
                            join: parent_join,
                            cte: "ins".to_string(),
                        }],
                    ));
                }
            }
            ctx.mutation_select_with_extra_ctes(
                "ins",
                &stmt,
                insert.check.as_ref(),
                &insert.check_path,
                extra_ctes,
                extra_checks,
                &insert.output,
            )
        }
        MutationRoot::Update { update, .. } => {
            let sets: Vec<String> = update
                .sets
                .iter()
                .map(|s| match s {
                    SetOp::Set {
                        column,
                        pg_type,
                        value,
                    } => {
                        format!(
                            "{} = {}",
                            quote_ident(column),
                            scalar_sql(&dialect, value, pg_type)
                        )
                    }
                    SetOp::Inc {
                        column,
                        pg_type,
                        value,
                    } => format!(
                        "{} = {} + {}",
                        quote_ident(column),
                        quote_ident(column),
                        scalar_sql(&dialect, value, pg_type)
                    ),
                    SetOp::JsonbAppend { column, value } => format!(
                        "{} = COALESCE({}, '{{}}'::jsonb) || {}",
                        quote_ident(column),
                        quote_ident(column),
                        scalar_sql(&dialect, value, "jsonb")
                    ),
                })
                .collect();
            let alias = "_upd_target".to_string();
            let mut stmt = format!(
                "UPDATE {}.{} AS {} SET {}",
                quote_ident(&update.table.schema),
                quote_ident(&update.table.name),
                quote_ident(&alias),
                sets.join(", ")
            );
            if let Some(pred) = &update.predicate {
                stmt.push_str(&format!(" WHERE {}", ctx.bool_exp(pred, &alias, &alias)));
            }
            stmt.push_str(" RETURNING *");
            ctx.mutation_select(
                "upd",
                &stmt,
                update.check.as_ref(),
                &update.check_path,
                &update.output,
            )
        }
        MutationRoot::Delete { delete, .. } => {
            let alias = "_del_target".to_string();
            let mut stmt = format!(
                "DELETE FROM {}.{} AS {}",
                quote_ident(&delete.table.schema),
                quote_ident(&delete.table.name),
                quote_ident(&alias)
            );
            if let Some(pred) = &delete.predicate {
                stmt.push_str(&format!(" WHERE {}", ctx.bool_exp(pred, &alias, &alias)));
            }
            stmt.push_str(" RETURNING *");
            ctx.mutation_select("del", &stmt, None, "$", &delete.output)
        }
    }
}

// ---------------------------------------------------------------------
// SQLite mutation path (M4 carve-out, see ADR 003)
// ---------------------------------------------------------------------

/// A selected field in a Rust-assembled mutation response object.
///
/// SQLite and MySQL cannot use the Postgres in-database mutation response
/// assembly path, so their executors retain these slots to preserve GraphQL
/// selection order exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationResponseSlot {
    Returning { alias: String },
    AffectedRows { alias: String },
    Typename { alias: String, value: String },
}

/// A planned SQLite mutation: one TOP-LEVEL DML statement whose RETURNING
/// clause yields, per affected row, a `node` JSON object (built from BARE
/// column names — SQLite RETURNING cannot reference an alias-qualified or
/// aggregated expression) and a `violated` flag (1 when the permission check
/// fails for that row, else 0). The executor runs `dml_sql` inside a
/// transaction, folds the rows into the response, and rolls back if any
/// `violated` flag is set. This replaces the Postgres CTE-wrapped, in-database
/// assembly, which SQLite's grammar forbids (DML in a CTE/subquery).
#[derive(Debug, Clone)]
pub struct SqliteMutationPlan {
    /// The single top-level DML to execute.
    pub dml_sql: String,
    /// Whether the GraphQL root returns one node directly instead of a
    /// response object containing a `returning` array.
    pub single_row_output: bool,
    /// Selected mutation response fields in GraphQL selection order.
    pub response_slots: Vec<MutationResponseSlot>,
    /// `(alias, value)` when the root is a `__typename` mutation root itself.
    pub root_typename: Option<(String, String)>,
    /// Error path reported on a check violation (carried into the executor's
    /// permission-error body).
    pub check_path: String,
}

/// Build the [`SqliteMutationPlan`] for an insert/update/delete mutation root.
/// Renders with the SQLite dialect. Unsupported mutation features are rejected
/// by the planner; the assertions below defend the SQL-generation boundary.
pub fn sqlite_mutation_plan(root: &MutationRoot) -> SqliteMutationPlan {
    let dialect = donat_backend::AnyDialect::Sqlite(donat_backend::SqliteDialect);
    let mut ctx = Ctx {
        next_alias: 0,
        stringify_numerics: false,
        dialect,
    };
    match root {
        MutationRoot::Typename { value, .. } => SqliteMutationPlan {
            dml_sql: String::new(),
            single_row_output: false,
            response_slots: vec![],
            root_typename: Some((String::new(), value.clone())),
            check_path: "$".into(),
        },
        MutationRoot::FunctionCall { .. } => {
            panic!("volatile function mutations are not supported on sqlite")
        }
        MutationRoot::Insert { insert, .. } => {
            assert!(
                insert.nested_object_inserts.is_empty(),
                "nested object inserts are not supported on sqlite mutations"
            );
            assert!(
                insert.on_conflict.is_none(),
                "on_conflict is not supported on sqlite mutations"
            );
            let cols: Vec<String> = insert
                .columns
                .iter()
                .map(|(name, _)| quote_ident(name))
                .collect();
            let rows: Vec<String> = insert
                .rows
                .iter()
                .map(|row| {
                    let values: Vec<String> = row
                        .iter()
                        .zip(&insert.columns)
                        .map(|(v, (_, pg_type))| match v {
                            None => "NULL".to_string(),
                            Some(s) => scalar_sql(&dialect, s, pg_type),
                        })
                        .collect();
                    format!("({})", values.join(", "))
                })
                .collect();
            let dml = format!(
                "INSERT INTO {}.{} ({}) VALUES {}",
                quote_ident(&insert.table.schema),
                quote_ident(&insert.table.name),
                cols.join(", "),
                rows.join(", ")
            );
            ctx.sqlite_finish(
                dml,
                insert.check.as_ref(),
                &insert.check_path,
                &insert.output,
            )
        }
        MutationRoot::Update { update, .. } => {
            let sets: Vec<String> = update
                .sets
                .iter()
                .map(|s| match s {
                    SetOp::Set {
                        column,
                        pg_type,
                        value,
                    } => {
                        format!(
                            "{} = {}",
                            quote_ident(column),
                            scalar_sql(&dialect, value, pg_type)
                        )
                    }
                    SetOp::Inc {
                        column,
                        pg_type,
                        value,
                    } => format!(
                        "{} = {} + {}",
                        quote_ident(column),
                        quote_ident(column),
                        scalar_sql(&dialect, value, pg_type)
                    ),
                    SetOp::JsonbAppend { .. } => {
                        panic!("jsonb append updates are not supported by sqlite sqlgen")
                    }
                })
                .collect();
            let alias = "_t".to_string();
            let mut dml = format!(
                "UPDATE {}.{} AS {} SET {}",
                quote_ident(&update.table.schema),
                quote_ident(&update.table.name),
                quote_ident(&alias),
                sets.join(", ")
            );
            if let Some(pred) = &update.predicate {
                dml.push_str(&format!(" WHERE {}", ctx.bool_exp(pred, &alias, &alias)));
            }
            ctx.sqlite_finish(
                dml,
                update.check.as_ref(),
                &update.check_path,
                &update.output,
            )
        }
        MutationRoot::Delete { delete, .. } => {
            let alias = "_t".to_string();
            let mut dml = format!(
                "DELETE FROM {}.{} AS {}",
                quote_ident(&delete.table.schema),
                quote_ident(&delete.table.name),
                quote_ident(&alias)
            );
            if let Some(pred) = &delete.predicate {
                dml.push_str(&format!(" WHERE {}", ctx.bool_exp(pred, &alias, &alias)));
            }
            ctx.sqlite_finish(dml, None, "$", &delete.output)
        }
    }
}

impl Ctx {
    /// Append the SQLite `RETURNING json_object(<bare cols>) AS node, <flag> AS
    /// violated` clause to a top-level DML and package it into a
    /// [`SqliteMutationPlan`]. `RETURNING` expressions must use BARE column
    /// names (no alias qualification, no aggregation) — hence the dedicated
    /// bare-column renderers below rather than reusing `row_json`/`bool_exp`'s
    /// alias-qualified output.
    fn sqlite_finish(
        &mut self,
        dml: String,
        check: Option<&BoolExp>,
        check_path: &str,
        output: &MutationOutput,
    ) -> SqliteMutationPlan {
        let single_row_output = matches!(output, MutationOutput::SingleRow(_));
        let mut response_slots = vec![];
        // Determine the node fields (the per-row `returning { ... }` selection).
        // A SingleRow output (`insert_one` / `_by_pk`) also produces a node.
        let node_fields: Vec<OutputField> = match output {
            MutationOutput::Response(fields) => {
                let mut node_fields = vec![];
                for f in fields {
                    match f {
                        MutationResponseField::AffectedRows { alias } => {
                            response_slots.push(MutationResponseSlot::AffectedRows {
                                alias: alias.clone(),
                            });
                        }
                        MutationResponseField::Typename { alias, value } => {
                            response_slots.push(MutationResponseSlot::Typename {
                                alias: alias.clone(),
                                value: value.clone(),
                            });
                        }
                        MutationResponseField::Returning { alias, fields } => {
                            response_slots.push(MutationResponseSlot::Returning {
                                alias: alias.clone(),
                            });
                            node_fields = fields.clone();
                        }
                    }
                }
                node_fields
            }
            MutationOutput::SingleRow(fields) => {
                // `insert_<t>_one` / `_by_pk`: the row itself is the node;
                // there is no affected_rows. The executor folds the returned
                // row and emits it directly when `single_row_output` is set.
                fields.clone()
            }
        };

        let node_expr = self.sqlite_node_json(&node_fields);
        let violated = match check {
            Some(check) => {
                format!(
                    "CASE WHEN ({}) THEN 0 ELSE 1 END",
                    self.sqlite_bare_bool(check)
                )
            }
            None => "0".to_string(),
        };
        let dml_sql = format!("{dml} RETURNING {node_expr} AS node, {violated} AS violated");
        SqliteMutationPlan {
            dml_sql,
            single_row_output,
            response_slots,
            root_typename: None,
            check_path: check_path.to_string(),
        }
    }

    /// Build a `json_object(...)` over the requested returning fields using
    /// BARE column names. Only column / typename leaves are expressible in a
    /// SQLite top-level RETURNING (and in a MySQL companion SELECT); nested
    /// relationships/computed/aggregate fields cannot be (they require
    /// correlated subqueries the SQLite grammar rejects / the carve-out does not
    /// model), so they are refused explicitly. Quoting goes through the active
    /// dialect: the SQLite dialect's identifier/literal syntax is byte-identical
    /// to the free `quote_ident`/`quote_lit`, so the SQLite output is unchanged,
    /// while MySQL gets its backtick-quoted identifiers.
    fn sqlite_node_json(&mut self, fields: &[OutputField]) -> String {
        use donat_backend::Dialect;
        let dialect = self.dialect;
        let pairs: Vec<(String, String)> = fields
            .iter()
            .map(|f| {
                let value = match &f.value {
                    FieldValue::Column { column, pg_type } => match dialect {
                        donat_backend::AnyDialect::Mysql(_) => {
                            mysql_json_column(&dialect.quote_ident(column), pg_type, false)
                        }
                        _ => sqlite_json_column(&dialect.quote_ident(column), pg_type, false),
                    },
                    FieldValue::ColumnGuarded {
                        column,
                        pg_type,
                        guard,
                    } => {
                        let cond = self.sqlite_bare_bool(guard);
                        let col = match dialect {
                            donat_backend::AnyDialect::Mysql(_) => {
                                mysql_json_column(&dialect.quote_ident(column), pg_type, false)
                            }
                            _ => sqlite_json_column(&dialect.quote_ident(column), pg_type, false),
                        };
                        format!("CASE WHEN {cond} THEN {col} ELSE NULL END")
                    }
                    FieldValue::Typename { value } => typename_literal(&dialect, value),
                    other => panic!(
                        "field {:?} is not expressible in a sqlite/mysql bare RETURNING",
                        std::mem::discriminant(other)
                    ),
                };
                (f.alias.clone(), value)
            })
            .collect();
        json_object(&dialect, &pairs)
    }

    /// Render a permission BoolExp over BARE column names for use inside a
    /// SQLite RETURNING `CASE` (or a MySQL companion-SELECT `CASE`). Covers the
    /// connectives plus the scalar comparison operators a permission check uses;
    /// constructs that need an alias-qualified subquery
    /// (relationship/exists/computed/column-to-column) are rejected — this
    /// carve-out does not support them. Identifier/literal quoting goes through
    /// the active dialect (byte-identical for SQLite, backticks for MySQL).
    fn sqlite_bare_bool(&mut self, exp: &BoolExp) -> String {
        use donat_backend::Dialect;
        let dialect = self.dialect;
        match exp {
            BoolExp::And(exps) => {
                if exps.is_empty() {
                    "1".into()
                } else {
                    let parts: Vec<String> =
                        exps.iter().map(|e| self.sqlite_bare_bool(e)).collect();
                    format!("({})", parts.join(" AND "))
                }
            }
            BoolExp::Or(exps) => {
                if exps.is_empty() {
                    "0".into()
                } else {
                    let parts: Vec<String> =
                        exps.iter().map(|e| self.sqlite_bare_bool(e)).collect();
                    format!("({})", parts.join(" OR "))
                }
            }
            BoolExp::Not(inner) => format!("(NOT {})", self.sqlite_bare_bool(inner)),
            BoolExp::Compare {
                column,
                pg_type,
                op,
            } => {
                let col = dialect.quote_ident(column);
                let lit = |s: &Scalar| scalar_sql(&dialect, s, pg_type);
                match op {
                    CompareOp::Eq(v) => format!("{col} = {}", lit(v)),
                    CompareOp::Neq(v) => format!("{col} <> {}", lit(v)),
                    CompareOp::Gt(v) => format!("{col} > {}", lit(v)),
                    CompareOp::Lt(v) => format!("{col} < {}", lit(v)),
                    CompareOp::Gte(v) => format!("{col} >= {}", lit(v)),
                    CompareOp::Lte(v) => format!("{col} <= {}", lit(v)),
                    CompareOp::In(vs) => {
                        if vs.is_empty() {
                            "0".into()
                        } else {
                            let items: Vec<String> = vs.iter().map(lit).collect();
                            format!("{col} IN ({})", items.join(", "))
                        }
                    }
                    CompareOp::Nin(vs) => {
                        if vs.is_empty() {
                            "1".into()
                        } else {
                            let items: Vec<String> = vs.iter().map(lit).collect();
                            format!("{col} NOT IN ({})", items.join(", "))
                        }
                    }
                    CompareOp::Like(v) => format!("{col} LIKE {}", lit(v)),
                    CompareOp::Nlike(v) => format!("{col} NOT LIKE {}", lit(v)),
                    CompareOp::IsNull(true) => format!("{col} IS NULL"),
                    CompareOp::IsNull(false) => format!("{col} IS NOT NULL"),
                    other => panic!(
                        "comparison {:?} is not supported in a sqlite mutation check",
                        std::mem::discriminant(other)
                    ),
                }
            }
            other => panic!(
                "bool-exp {:?} is not supported in a sqlite mutation check",
                std::mem::discriminant(other)
            ),
        }
    }
}

// ---------------------------------------------------------------------
// MySQL mutation path (companion SELECT, see ADR 004)
// ---------------------------------------------------------------------

/// How the MySQL executor recovers the `returning` set for a mutation root and
/// how it orders the DML vs. the companion SELECT. MySQL has no `RETURNING`, so
/// every variant pairs the DML with a companion `SELECT` whose `WHERE` the
/// executor builds at runtime (the executor knows `last_insert_id()` /
/// `affected_rows`, which sqlgen cannot).
#[derive(Debug, Clone)]
pub enum MySqlMutationKind {
    /// `INSERT`, then companion SELECT recovering the new rows. When the insert
    /// supplied the PK column(s), `pk_in_predicate` restricts the SELECT to the
    /// supplied values; otherwise the executor restricts by the
    /// `last_insert_id()` range over a single AUTO_INCREMENT PK (`pk_col`).
    Insert {
        /// Backtick-quoted PK column, used for the `last_insert_id()`-range
        /// `WHERE` when the insert omitted the PK (auto-increment recovery).
        pk_col: Option<String>,
        /// `<pk> IN (..)` predicate when the insert explicitly supplied the
        /// single PK column; the executor uses it verbatim as the companion
        /// `WHERE` and skips `last_insert_id()` recovery.
        pk_in_predicate: Option<String>,
    },
    /// `UPDATE ... WHERE <pred>`, then re-`SELECT ... WHERE <pred>`.
    Update { where_clause: Option<String> },
    /// `SELECT ... WHERE <pred>` FIRST (capture returning), then
    /// `DELETE ... WHERE <pred>`.
    Delete { where_clause: Option<String> },
    /// A `__typename`-only mutation root: no DML, no companion SELECT.
    Typename,
}

/// A planned MySQL mutation: the DML statement plus the companion SELECT that
/// recovers `returning` + the permission-`violated` flag (MySQL has no
/// `RETURNING`; see ADR 004). The executor runs these inside one transaction,
/// builds the companion `WHERE` from `kind` + runtime row-counts/ids, folds the
/// rows into the response, and rolls back if any row's `violated` flag is set.
#[derive(Debug, Clone)]
pub struct MySqlMutationPlan {
    /// The single DML to execute (`INSERT`/`UPDATE`/`DELETE`), no trailing
    /// `RETURNING` (MySQL has none). Empty for a `__typename` root.
    pub dml_sql: String,
    /// Whether the GraphQL root returns one node directly instead of a
    /// response object containing a `returning` array.
    pub single_row_output: bool,
    /// The companion `SELECT <node> AS node, <flag> AS violated FROM `s`.`t``,
    /// WITHOUT the trailing `WHERE` — the executor appends the restriction it
    /// derives from `kind`. Empty for a `__typename` root.
    pub companion_select: String,
    /// Recovery strategy + companion-`WHERE` building blocks.
    pub kind: MySqlMutationKind,
    /// Selected mutation response fields in GraphQL selection order.
    pub response_slots: Vec<MutationResponseSlot>,
    /// `(alias, value)` when the root is a `__typename` mutation root itself.
    pub root_typename: Option<(String, String)>,
    /// Error path reported on a check violation.
    pub check_path: String,
}

/// Build the [`MySqlMutationPlan`] for an insert/update/delete mutation root.
/// `pk` is the table's primary-key column names (from the catalog) — needed for
/// `last_insert_id()` recovery and for the supplied-PK `IN` predicate, which
/// the IR mutation does not carry. Unsupported mutation features are rejected
/// by the planner; the assertions below defend the SQL-generation boundary.
pub fn mysql_mutation_plan(root: &MutationRoot, pk: &[String]) -> MySqlMutationPlan {
    use donat_backend::Dialect;
    let dialect = donat_backend::AnyDialect::Mysql(donat_backend::MySqlDialect);
    let mut ctx = Ctx {
        next_alias: 0,
        stringify_numerics: false,
        dialect,
    };
    match root {
        MutationRoot::Typename { value, .. } => MySqlMutationPlan {
            dml_sql: String::new(),
            single_row_output: false,
            companion_select: String::new(),
            kind: MySqlMutationKind::Typename,
            response_slots: vec![],
            root_typename: Some((String::new(), value.clone())),
            check_path: "$".into(),
        },
        MutationRoot::FunctionCall { .. } => {
            panic!("volatile function mutations are not supported on mysql")
        }
        MutationRoot::Insert { insert, .. } => {
            assert!(
                insert.nested_object_inserts.is_empty(),
                "nested object inserts are not supported on mysql mutations"
            );
            assert!(
                insert.on_conflict.is_none(),
                "on_conflict is not yet supported on mysql mutations"
            );
            let table = format!(
                "{}.{}",
                dialect.quote_ident(&insert.table.schema),
                dialect.quote_ident(&insert.table.name)
            );
            let cols: Vec<String> = insert
                .columns
                .iter()
                .map(|(name, _)| dialect.quote_ident(name))
                .collect();
            let rows: Vec<String> = insert
                .rows
                .iter()
                .map(|row| {
                    let values: Vec<String> = row
                        .iter()
                        .zip(&insert.columns)
                        .map(|(v, (_, pg_type))| match v {
                            None => "DEFAULT".to_string(),
                            Some(s) => scalar_sql(&dialect, s, pg_type),
                        })
                        .collect();
                    format!("({})", values.join(", "))
                })
                .collect();
            let dml = format!(
                "INSERT INTO {table} ({}) VALUES {}",
                cols.join(", "),
                rows.join(", ")
            );

            // Recovery: supplied single PK -> IN (values); else last_insert_id().
            let single_pk = if pk.len() == 1 { Some(&pk[0]) } else { None };
            let pk_col = single_pk.map(|c| dialect.quote_ident(c));
            // Which IR column index, if any, holds the (single) PK?
            let pk_idx = single_pk
                .and_then(|pkname| insert.columns.iter().position(|(name, _)| name == pkname));
            // A supplied-PK IN predicate is usable only when every row gave a
            // non-DEFAULT value for that PK column.
            let pk_in_predicate = match (pk_col.as_ref(), pk_idx) {
                (Some(col), Some(idx)) => {
                    let mut vals = Vec::with_capacity(insert.rows.len());
                    let mut all_present = true;
                    for row in &insert.rows {
                        match &row[idx] {
                            Some(s) => {
                                let (_, ty) = &insert.columns[idx];
                                vals.push(scalar_sql(&dialect, s, ty));
                            }
                            None => {
                                all_present = false;
                                break;
                            }
                        }
                    }
                    if all_present && !vals.is_empty() {
                        Some(format!("{col} IN ({})", vals.join(", ")))
                    } else {
                        None
                    }
                }
                _ => None,
            };

            let companion =
                ctx.mysql_companion_select(&table, insert.check.as_ref(), &insert.output);
            MySqlMutationPlan {
                dml_sql: dml,
                single_row_output: companion.single_row_output,
                companion_select: companion.select,
                kind: MySqlMutationKind::Insert {
                    pk_col,
                    pk_in_predicate,
                },
                response_slots: companion.response_slots,
                root_typename: None,
                check_path: insert.check_path.clone(),
            }
        }
        MutationRoot::Update { update, .. } => {
            let table = format!(
                "{}.{}",
                dialect.quote_ident(&update.table.schema),
                dialect.quote_ident(&update.table.name)
            );
            let sets: Vec<String> = update
                .sets
                .iter()
                .map(|s| match s {
                    SetOp::Set {
                        column,
                        pg_type,
                        value,
                    } => format!(
                        "{} = {}",
                        dialect.quote_ident(column),
                        scalar_sql(&dialect, value, pg_type)
                    ),
                    SetOp::Inc {
                        column,
                        pg_type,
                        value,
                    } => format!(
                        "{} = {} + {}",
                        dialect.quote_ident(column),
                        dialect.quote_ident(column),
                        scalar_sql(&dialect, value, pg_type)
                    ),
                    SetOp::JsonbAppend { .. } => {
                        panic!("jsonb append updates are not supported by mysql sqlgen")
                    }
                })
                .collect();
            // The predicate is rendered over BARE columns so it is valid both in
            // the unaliased UPDATE and in the companion SELECT.
            let where_clause = update.predicate.as_ref().map(|p| ctx.mysql_bare_bool(p));
            let mut dml = format!("UPDATE {table} SET {}", sets.join(", "));
            if let Some(w) = &where_clause {
                dml.push_str(&format!(" WHERE {w}"));
            }
            let companion =
                ctx.mysql_companion_select(&table, update.check.as_ref(), &update.output);
            MySqlMutationPlan {
                dml_sql: dml,
                single_row_output: companion.single_row_output,
                companion_select: companion.select,
                kind: MySqlMutationKind::Update { where_clause },
                response_slots: companion.response_slots,
                root_typename: None,
                check_path: update.check_path.clone(),
            }
        }
        MutationRoot::Delete { delete, .. } => {
            let table = format!(
                "{}.{}",
                dialect.quote_ident(&delete.table.schema),
                dialect.quote_ident(&delete.table.name)
            );
            let where_clause = delete.predicate.as_ref().map(|p| ctx.mysql_bare_bool(p));
            let mut dml = format!("DELETE FROM {table}");
            if let Some(w) = &where_clause {
                dml.push_str(&format!(" WHERE {w}"));
            }
            let companion = ctx.mysql_companion_select(&table, None, &delete.output);
            MySqlMutationPlan {
                dml_sql: dml,
                single_row_output: companion.single_row_output,
                companion_select: companion.select,
                kind: MySqlMutationKind::Delete { where_clause },
                response_slots: companion.response_slots,
                root_typename: None,
                check_path: "$".into(),
            }
        }
    }
}

/// Intermediate result of [`Ctx::mysql_companion_select`].
struct MySqlCompanion {
    select: String,
    single_row_output: bool,
    response_slots: Vec<MutationResponseSlot>,
}

impl Ctx {
    /// Build the companion `SELECT <node> AS node, <violated> AS violated FROM
    /// <table>` (no `WHERE`; the executor appends the restriction). Reuses the
    /// BARE-column renderers (`sqlite_node_json` / `sqlite_bare_bool`) — the
    /// MySQL companion SELECT references columns by bare name exactly like a
    /// SQLite RETURNING — under the MySQL dialect (backtick quoting,
    /// `JSON_OBJECT`).
    fn mysql_companion_select(
        &mut self,
        table: &str,
        check: Option<&BoolExp>,
        output: &MutationOutput,
    ) -> MySqlCompanion {
        let single_row_output = matches!(output, MutationOutput::SingleRow(_));
        let mut response_slots = vec![];
        let node_fields: Vec<OutputField> = match output {
            MutationOutput::Response(fields) => {
                let mut node_fields = vec![];
                for f in fields {
                    match f {
                        MutationResponseField::AffectedRows { alias } => {
                            response_slots.push(MutationResponseSlot::AffectedRows {
                                alias: alias.clone(),
                            });
                        }
                        MutationResponseField::Typename { alias, value } => {
                            response_slots.push(MutationResponseSlot::Typename {
                                alias: alias.clone(),
                                value: value.clone(),
                            });
                        }
                        MutationResponseField::Returning { alias, fields } => {
                            response_slots.push(MutationResponseSlot::Returning {
                                alias: alias.clone(),
                            });
                            node_fields = fields.clone();
                        }
                    }
                }
                node_fields
            }
            MutationOutput::SingleRow(fields) => fields.clone(),
        };
        let node_expr = self.sqlite_node_json(&node_fields);
        let violated = match check {
            Some(check) => {
                format!(
                    "CASE WHEN ({}) THEN 0 ELSE 1 END",
                    self.sqlite_bare_bool(check)
                )
            }
            None => "0".to_string(),
        };
        MySqlCompanion {
            select: format!("SELECT {node_expr} AS node, {violated} AS violated FROM {table}"),
            single_row_output,
            response_slots,
        }
    }

    /// Alias for [`Ctx::sqlite_bare_bool`] used by the MySQL update/delete
    /// predicate: the same bare-column rendering, under the MySQL dialect.
    fn mysql_bare_bool(&mut self, exp: &BoolExp) -> String {
        self.sqlite_bare_bool(exp)
    }
}

impl Ctx {
    /// Wrap a DML statement in a CTE and select the GraphQL response from
    /// its RETURNING set, enforcing the permission check expression.
    fn mutation_select(
        &mut self,
        cte: &str,
        dml: &str,
        check: Option<&BoolExp>,
        check_path: &str,
        output: &MutationOutput,
    ) -> String {
        self.mutation_select_with_extra_ctes(cte, dml, check, check_path, vec![], vec![], output)
    }

    fn mutation_select_with_extra_ctes(
        &mut self,
        cte: &str,
        dml: &str,
        check: Option<&BoolExp>,
        check_path: &str,
        extra_ctes: Vec<String>,
        extra_checks: Vec<(String, &BoolExp, String, Vec<RelationshipCteOverride>)>,
        output: &MutationOutput,
    ) -> String {
        let dialect = self.dialect;
        let cte_ident = quote_ident(cte);
        let result = match output {
            MutationOutput::Response(fields) => {
                let pairs: Vec<(String, String)> = fields
                    .iter()
                    .map(|f| match f {
                        MutationResponseField::AffectedRows { alias } => {
                            (alias.clone(), format!("(SELECT count(*) FROM {cte_ident})"))
                        }
                        MutationResponseField::Typename { alias, value } => {
                            (alias.clone(), typename_literal(&dialect, value))
                        }
                        MutationResponseField::Returning { alias, fields } => {
                            let row = self.row_json(fields, cte);
                            // json_agg leaf delegated; the (SELECT … FROM cte)
                            // wrapper has no leaf and stays inline.
                            (
                                alias.clone(),
                                format!(
                                    "(SELECT {} FROM {cte_ident})",
                                    json_array_agg(&dialect, &row, None)
                                ),
                            )
                        }
                    })
                    .collect();
                json_object(&dialect, &pairs)
            }
            MutationOutput::SingleRow(fields) => {
                let row = self.row_json(fields, cte);
                format!("(SELECT {row} FROM {cte_ident} LIMIT 1)")
            }
        };

        let mut guarded = result;
        for (check_cte, check, check_path, relationship_ctes) in extra_checks.into_iter().rev() {
            let check_cte_ident = quote_ident(&check_cte);
            let violated = format!(
                "(SELECT count(*) FROM {check_cte_ident} WHERE ({}) IS NOT TRUE)",
                self.bool_exp_with_relationship_ctes(
                    check,
                    &check_cte,
                    &check_cte,
                    &relationship_ctes,
                )
            );
            let payload = serde_json::json!({
                "path": check_path,
                "message": "check constraint of an insert/update permission has failed",
            })
            .to_string();
            guarded = format!(
                "CASE WHEN {violated} > 0 THEN donat.check_violation({}) ELSE {guarded} END",
                quote_lit(&payload)
            );
        }
        if let Some(check) = check {
            let violated = format!(
                "(SELECT count(*) FROM {cte_ident} WHERE ({}) IS NOT TRUE)",
                self.bool_exp(check, cte, cte)
            );
            // The message carries the GraphQL error path as JSON; the
            // executor unpacks it into the Donat error shape.
            let payload = serde_json::json!({
                "path": check_path,
                "message": "check constraint of an insert/update permission has failed",
            })
            .to_string();
            guarded = format!(
                "CASE WHEN {violated} > 0 THEN donat.check_violation({}) ELSE {guarded} END",
                quote_lit(&payload)
            );
        }
        let mut ctes = vec![format!("{cte_ident} AS ({dml})")];
        ctes.extend(extra_ctes);
        format!("WITH {} SELECT {guarded} AS root", ctes.join(", "))
    }
}

fn row_function_arg(
    dialect: &donat_backend::AnyDialect,
    arg: &RowFunctionArg,
    outer_alias: &str,
) -> String {
    match arg {
        // The enclosing FROM alias is a composite value of the table's
        // row type, which is exactly what the function expects.
        RowFunctionArg::Row => quote_ident(outer_alias),
        RowFunctionArg::SessionJson(json) => format!("({})::json", quote_lit(json)),
        RowFunctionArg::Value { value, pg_type } => scalar_sql(dialect, value, pg_type),
    }
}

/// `DISTINCT ON (cols) ` prefix for the row-producing SELECT, or empty.
fn distinct_clause(q: &SelectQuery, alias: &str) -> String {
    if q.distinct_on.is_empty() {
        String::new()
    } else {
        let cols: Vec<String> = q.distinct_on.iter().map(|c| qualified(alias, c)).collect();
        format!("DISTINCT ON ({}) ", cols.join(", "))
    }
}

fn qualified(alias: &str, column: &str) -> String {
    format!("{}.{}", quote_ident(alias), quote_ident(column))
}

fn sqlite_json_column(expression: &str, pg_type: &str, stringify_numerics: bool) -> String {
    match pg_type {
        "int8" | "numeric" if stringify_numerics => format!("CAST({expression} AS TEXT)"),
        "bool" => format!(
            "CASE WHEN {expression} IS NULL THEN NULL WHEN {expression} THEN json('true') ELSE json('false') END"
        ),
        "json" => format!("json({expression})"),
        _ => expression.to_string(),
    }
}

fn clickhouse_json_column(expression: &str, pg_type: &str, stringify_numerics: bool) -> String {
    if stringify_numerics && clickhouse_stringified_numeric_type(pg_type) {
        format!("toJSONString(toString({expression}))")
    } else {
        format!("toJSONString({expression})")
    }
}

fn clickhouse_stringified_numeric_type(native_type: &str) -> bool {
    let mut native_type = native_type.trim();
    loop {
        let inner = ["Nullable", "LowCardinality"]
            .into_iter()
            .find_map(|wrapper| {
                native_type
                    .strip_prefix(wrapper)
                    .and_then(|rest| rest.strip_prefix('('))
                    .and_then(|rest| rest.strip_suffix(')'))
            });
        match inner {
            Some(inner) => native_type = inner,
            None => break,
        }
    }
    let family = native_type
        .split_once('(')
        .map_or(native_type, |(family, _)| family)
        .to_ascii_lowercase();
    matches!(
        family.as_str(),
        "int8"
            | "numeric"
            | "int64"
            | "uint64"
            | "int128"
            | "uint128"
            | "int256"
            | "uint256"
            | "decimal"
            | "decimal32"
            | "decimal64"
            | "decimal128"
            | "decimal256"
    )
}

fn mysql_json_column(expression: &str, pg_type: &str, stringify_numerics: bool) -> String {
    match pg_type {
        "bool" => format!(
            "CASE WHEN {expression} IS NULL THEN NULL WHEN {expression} THEN 'true' ELSE 'false' END"
        ),
        "int8" | "numeric" if stringify_numerics => {
            format!("JSON_QUOTE(CAST({expression} AS CHAR))")
        }
        "text" | "varchar" | "bpchar" | "uuid" | "timestamp" | "timestamptz" | "date" | "time"
        | "bytea" | "inet" | "citext" => {
            format!("JSON_QUOTE(CAST({expression} AS CHAR))")
        }
        _ => format!("CAST({expression} AS CHAR)"),
    }
}

pub fn quote_ident(ident: &str) -> String {
    use donat_backend::Dialect;
    donat_backend::PostgresDialect.quote_ident(ident)
}

pub fn quote_lit(s: &str) -> String {
    use donat_backend::Dialect;
    donat_backend::PostgresDialect.quote_literal(s)
}

/// JSON object assembly (LEAF op #1). Delegates to the active backend
/// dialect; keys are raw and quoted internally, values are inlined verbatim.
fn json_object(dialect: &donat_backend::AnyDialect, pairs: &[(String, String)]) -> String {
    use donat_backend::Dialect;
    dialect.json_object(pairs)
}

/// JSON array aggregation (LEAF op #2/#8), coalescing empty to `[]`.
fn json_array_agg(
    dialect: &donat_backend::AnyDialect,
    row_expr: &str,
    order_by: Option<&str>,
) -> String {
    use donat_backend::Dialect;
    dialect.json_array_agg(row_expr, order_by)
}

/// Render a JSON scalar as a SQL literal cast to the column's type.
/// Delegates to the active backend dialect's `render_scalar`, which holds the
/// byte-for-byte rendering (including the geometry/geography GeoJSON case).
fn scalar_sql(dialect: &donat_backend::AnyDialect, scalar: &Scalar, pg_type: &str) -> String {
    use donat_backend::Dialect;
    dialect.render_scalar(scalar, pg_type)
}

fn typename_literal(dialect: &donat_backend::AnyDialect, value: &str) -> String {
    use donat_backend::Dialect;

    let literal = dialect.quote_literal(value);
    match dialect {
        donat_backend::AnyDialect::Postgres(_) => format!("{literal}::text"),
        donat_backend::AnyDialect::Clickhouse(_) => {
            format!("toJSONString(CAST({literal} AS String))")
        }
        donat_backend::AnyDialect::Sqlite(_) => literal,
        donat_backend::AnyDialect::Mysql(_) => format!("JSON_QUOTE(CAST({literal} AS CHAR))"),
    }
}

fn clickhouse_aggregate_function(op: &str) -> &str {
    match op {
        "stddev" => "stddev_samp",
        "variance" => "var_samp",
        other => other,
    }
}

#[cfg(test)]
mod dialect_dispatch_tests {
    use super::*;
    use donat_backend::{
        AnyDialect, ClickhouseDialect, MySqlDialect, PostgresDialect, SqliteDialect,
    };

    fn sample_roots() -> Vec<RootField> {
        let cols = vec![
            OutputField {
                alias: "id".into(),
                value: FieldValue::Column {
                    column: "id".into(),
                    pg_type: "int4".into(),
                },
            },
            OutputField {
                alias: "name".into(),
                value: FieldValue::Column {
                    column: "name".into(),
                    pg_type: "text".into(),
                },
            },
        ];
        let query = |single: bool| SelectQuery {
            from: FromSource::Table(Table {
                schema: "public".into(),
                name: "author".into(),
            }),
            fields: cols.clone(),
            predicate: Some(BoolExp::Compare {
                column: "id".into(),
                pg_type: "int4".into(),
                op: CompareOp::Eq(Scalar::Json(serde_json::json!(7))),
            }),
            order_by: vec![OrderBy {
                target: OrderByTarget::Column("id".into()),
                direction: OrderDirection::Asc,
                nulls: NullsOrder::Last,
            }],
            limit: Some(10),
            nodes_limit: None,
            offset: Some(2),
            distinct_on: vec![],
            single,
        };
        vec![
            RootField::Select {
                alias: "author_by_pk".into(),
                query: query(true),
            },
            RootField::Select {
                alias: "authors".into(),
                query: query(false),
            },
        ]
    }

    #[test]
    fn remote_join_placeholder_uses_portable_null_outside_postgres() {
        let query = SelectQuery {
            from: FromSource::Table(Table {
                schema: "public".into(),
                name: "author".into(),
            }),
            fields: vec![OutputField {
                alias: "joined".into(),
                value: FieldValue::RemoteJoin {
                    spec: RemoteJoinSpec {
                        schema: "remote".into(),
                        query: "query { message { name } }".into(),
                        variables: vec![],
                        root_field: "message".into(),
                    },
                },
            }],
            predicate: None,
            order_by: vec![],
            limit: None,
            nodes_limit: None,
            offset: None,
            distinct_on: vec![],
            single: false,
        };
        let roots = [RootField::Select {
            alias: "authors".into(),
            query,
        }];

        let postgres = operation_to_sql_with(&roots, AnyDialect::Postgres(PostgresDialect));
        let sqlite = operation_to_sql_with(&roots, AnyDialect::Sqlite(SqliteDialect));

        assert!(postgres.contains("NULL::json"), "{postgres}");
        assert!(sqlite.contains("'joined', NULL"), "{sqlite}");
        assert!(!sqlite.contains("::json"), "{sqlite}");
    }

    #[test]
    fn operation_to_sql_with_postgres_equals_default_wrapper() {
        // The dialect-explicit entry point with the Postgres dialect must
        // produce byte-identical SQL to the default (Postgres-wrapper) entry
        // point. Guards the dispatch refactor: the Postgres path is unchanged.
        let roots = sample_roots();
        let default = operation_to_sql(&roots);
        let explicit = operation_to_sql_with(&roots, AnyDialect::Postgres(PostgresDialect));
        assert_eq!(default, explicit);
    }

    #[test]
    fn operation_to_sql_with_clickhouse_uses_ordered_json_text_and_casts() {
        let mut roots = sample_roots();
        let list_root = roots.pop().expect("list root");
        let sql = operation_to_sql_with(&[list_root], AnyDialect::Clickhouse(ClickhouseDialect));

        // ClickHouse accepts standard double-quoted identifiers as well as
        // backticks; the shared query assembler currently emits the former.
        assert!(sql.contains("\"public\".\"author\""), "{sql}");
        assert!(sql.contains("concat('{',"), "{sql}");
        assert!(sql.contains("toJSONString(\"_t0\".\"id\")"), "{sql}");
        assert!(sql.contains("groupArray(("), "{sql}");
        assert!(sql.contains("arrayStringConcat("), "{sql}");
        assert!(sql.contains("row_number() OVER (ORDER BY"), "{sql}");
        assert!(sql.contains("arraySort("), "{sql}");
        assert!(
            !sql.contains(" AS JSON"),
            "JSON casts reorder object keys: {sql}"
        );
        assert!(sql.contains("CAST(7 AS Int32)"), "{sql}");
        assert!(sql.contains(" LIMIT 10 OFFSET 2"), "{sql}");
        assert!(!sql.contains(';'), "one SQL statement only: {sql}");
    }

    #[test]
    fn clickhouse_stringify_numerics_recognizes_wrapped_native_types() {
        for numeric in [
            "int8",
            "numeric",
            "UInt64",
            "Nullable(UInt128)",
            "LowCardinality(Nullable(Decimal256(76)))",
        ] {
            assert!(clickhouse_stringified_numeric_type(numeric), "{numeric}");
        }
        for non_numeric in ["UInt32", "Float64", "Nullable(String)"] {
            assert!(
                !clickhouse_stringified_numeric_type(non_numeric),
                "{non_numeric}"
            );
        }
    }

    #[test]
    fn operation_to_sql_with_sqlite_serializes_boolean_columns_as_json_booleans() {
        let query = SelectQuery {
            from: FromSource::Table(Table {
                schema: "main".into(),
                name: "article".into(),
            }),
            fields: vec![OutputField {
                alias: "is_published".into(),
                value: FieldValue::Column {
                    column: "is_published".into(),
                    pg_type: "bool".into(),
                },
            }],
            predicate: None,
            order_by: vec![],
            limit: None,
            nodes_limit: None,
            offset: None,
            distinct_on: vec![],
            single: false,
        };
        let sql = operation_to_sql_with(
            &[RootField::Select {
                alias: "article".into(),
                query,
            }],
            AnyDialect::Sqlite(SqliteDialect),
        );

        assert!(
            sql.contains("json('true')"),
            "true is not JSON boolean: {sql}"
        );
        assert!(
            sql.contains("json('false')"),
            "false is not JSON boolean: {sql}"
        );
        assert!(
            sql.contains("IS NULL THEN NULL"),
            "nullable booleans must remain null: {sql}"
        );
    }

    #[test]
    fn operation_to_sql_with_mysql_preserves_field_order_and_boolean_shape() {
        let query = SelectQuery {
            from: FromSource::Table(Table {
                schema: "app".into(),
                name: "article".into(),
            }),
            fields: vec![
                OutputField {
                    alias: "title".into(),
                    value: FieldValue::Column {
                        column: "title".into(),
                        pg_type: "text".into(),
                    },
                },
                OutputField {
                    alias: "is_published".into(),
                    value: FieldValue::Column {
                        column: "is_published".into(),
                        pg_type: "bool".into(),
                    },
                },
            ],
            predicate: None,
            order_by: vec![],
            limit: None,
            nodes_limit: None,
            offset: None,
            distinct_on: vec![],
            single: false,
        };
        let sql = operation_to_sql_with(
            &[RootField::Select {
                alias: "article".into(),
                query,
            }],
            AnyDialect::Mysql(MySqlDialect),
        );

        let title = sql.find("'\"title\":'").expect("title key");
        let published = sql.find("'\"is_published\":'").expect("is_published key");
        assert!(title < published, "selection order changed: {sql}");
        assert!(
            sql.contains("JSON_QUOTE(CAST(\"_t0\".\"title\" AS CHAR))"),
            "text column is not JSON-quoted: {sql}"
        );
        assert!(sql.contains("THEN 'true' ELSE 'false'"), "{sql}");
        assert!(
            !sql.contains("JSON_OBJECT"),
            "binary JSON reorders keys: {sql}"
        );
    }

    #[test]
    fn mysql_mutation_nodes_json_quote_typenames() {
        let typename_field = || OutputField {
            alias: "__typename".into(),
            value: FieldValue::Typename {
                value: "note".into(),
            },
        };
        let insert = |output| MutationRoot::Insert {
            alias: "insert_note".into(),
            insert: InsertMutation {
                table: Table {
                    schema: "donat".into(),
                    name: "note".into(),
                },
                columns: vec![("body".into(), "text".into())],
                rows: vec![vec![Some(Scalar::Json(serde_json::json!("hello")))]],
                nested_object_inserts: vec![],
                on_conflict: None,
                check: None,
                check_path: "$".into(),
                output,
            },
        };

        let returning = mysql_mutation_plan(
            &insert(MutationOutput::Response(vec![
                MutationResponseField::Returning {
                    alias: "returning".into(),
                    fields: vec![typename_field()],
                },
            ])),
            &["id".into()],
        );
        let single = mysql_mutation_plan(
            &insert(MutationOutput::SingleRow(vec![typename_field()])),
            &["id".into()],
        );

        for sql in [returning.companion_select, single.companion_select] {
            assert!(
                sql.contains("JSON_QUOTE(CAST('note' AS CHAR))"),
                "typename is not valid JSON text: {sql}"
            );
        }
    }

    #[test]
    fn mutation_to_sql_with_postgres_equals_default_wrapper() {
        let root = MutationRoot::Insert {
            alias: "insert_author".into(),
            insert: InsertMutation {
                table: Table {
                    schema: "public".into(),
                    name: "author".into(),
                },
                columns: vec![("name".into(), "text".into())],
                rows: vec![vec![Some(Scalar::Json(serde_json::json!("Ada")))]],
                nested_object_inserts: vec![],
                on_conflict: None,
                check: None,
                check_path: "$".into(),
                output: MutationOutput::Response(vec![
                    MutationResponseField::AffectedRows {
                        alias: "affected_rows".into(),
                    },
                    MutationResponseField::Returning {
                        alias: "returning".into(),
                        fields: vec![OutputField {
                            alias: "id".into(),
                            value: FieldValue::Column {
                                column: "id".into(),
                                pg_type: "int4".into(),
                            },
                        }],
                    },
                ]),
            },
        };
        let default = mutation_to_sql(&root);
        let explicit = mutation_to_sql_with(&root, AnyDialect::Postgres(PostgresDialect));
        assert_eq!(default, explicit);
    }

    #[test]
    fn operation_to_sql_with_clickhouse_renders_typenames_without_postgres_casts() {
        let query = SelectQuery {
            from: FromSource::Table(Table {
                schema: "analytics".into(),
                name: "author".into(),
            }),
            fields: vec![OutputField {
                alias: "__typename".into(),
                value: FieldValue::Typename {
                    value: "author".into(),
                },
            }],
            predicate: None,
            order_by: vec![],
            limit: Some(1),
            nodes_limit: None,
            offset: None,
            distinct_on: vec![],
            single: false,
        };
        let sql = operation_to_sql_with(
            &[
                RootField::Typename {
                    alias: "__typename".into(),
                    value: "query_root".into(),
                },
                RootField::Select {
                    alias: "author".into(),
                    query,
                },
            ],
            AnyDialect::Clickhouse(ClickhouseDialect),
        );

        assert!(!sql.contains("::text"), "Postgres cast leaked: {sql}");
        assert!(sql.contains("query_root"), "root typename missing: {sql}");
        assert!(sql.contains("author"), "row typename missing: {sql}");
    }

    #[test]
    fn clickhouse_uses_supported_statistical_aggregate_names() {
        let fields = [
            "stddev",
            "stddev_samp",
            "stddev_pop",
            "variance",
            "var_samp",
            "var_pop",
        ]
        .into_iter()
        .map(|op| AggregateField {
            alias: op.to_string(),
            op: AggregateOp::ColumnOp {
                op: op.to_string(),
                columns: vec![AggregateColumn {
                    alias: "id".into(),
                    column: "id".into(),
                    pg_type: "int4".into(),
                    guard: None,
                }],
            },
        })
        .collect();
        let query = SelectQuery {
            from: FromSource::Table(Table {
                schema: "analytics".into(),
                name: "author".into(),
            }),
            fields: vec![OutputField {
                alias: "aggregate".into(),
                value: FieldValue::Aggregate { fields },
            }],
            predicate: None,
            order_by: vec![],
            limit: None,
            nodes_limit: None,
            offset: None,
            distinct_on: vec![],
            single: false,
        };

        let sql = operation_to_sql_with(
            &[RootField::Select {
                alias: "author_aggregate".into(),
                query,
            }],
            AnyDialect::Clickhouse(ClickhouseDialect),
        );

        for function in [
            "stddev_sampOrNull",
            "stddev_popOrNull",
            "var_sampOrNull",
            "var_popOrNull",
        ] {
            assert!(sql.contains(function), "missing {function}: {sql}");
        }
        assert!(!sql.contains("stddevOrNull"), "unsupported function: {sql}");
        assert!(
            !sql.contains("varianceOrNull"),
            "unsupported function: {sql}"
        );
    }
}
