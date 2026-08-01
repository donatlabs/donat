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

struct MutationSelectOptions<'a> {
    cte: &'a str,
    dml: &'a str,
    check: Option<&'a BoolExp>,
    check_path: &'a str,
    extra_ctes: Vec<String>,
    extra_checks: Vec<(String, &'a BoolExp, String, Vec<RelationshipCteOverride>)>,
    /// Ordered per-role value validators over the written rows.
    validators: &'a [RowValidator],
    output: &'a MutationOutput,
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
        let (tail, _) = self.render_select_tail(&q, &alias, outer);

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
        let (tail, rendered_order) = self.render_select_tail(q, &alias, outer);
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
        let (tail, _) = self.render_select_tail(q, &inner_alias, outer);
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
    fn render_select_tail(
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
    let cast = quote_type_name(pg_type);
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
fn command_to_sql(ctx: &mut Ctx, command: &CommandMutation) -> String {
    assert!(
        matches!(ctx.dialect, donat_backend::AnyDialect::Postgres(_)),
        "commands have only a Postgres renderer"
    );
    assert!(
        command.effects.is_empty() || command.idempotency.is_some(),
        "command effects require one durable command execution generation"
    );
    for (index, effect) in command.effects.iter().enumerate() {
        let (source, position, invocation) = match effect {
            ResolvedCommandEffect::StartProcess(effect) => (
                &effect.source,
                effect.effect_position,
                effect.command_invocation_id,
            ),
            ResolvedCommandEffect::SignalProcess(effect) => (
                &effect.source,
                effect.effect_position,
                effect.command_invocation_id,
            ),
        };
        assert_eq!(
            source, &command.identity.source,
            "resolved command effect crossed its source boundary"
        );
        assert_eq!(
            usize::try_from(position).ok(),
            Some(index),
            "resolved command effect positions must be canonical and contiguous"
        );
        assert_eq!(
            invocation,
            CommandInvocationIdSource::CurrentExecution,
            "resolved command effects may reference only the current execution"
        );
    }

    if let Some(error_path) = command.steps.iter().find_map(|step| match step {
        CommandExecutionStep::InsertMany {
            items, error_path, ..
        } if items
            .as_json()
            .as_array()
            .is_none_or(|items| items.iter().any(|item| !item.is_object())) =>
        {
            Some(error_path)
        }
        _ => None,
    }) {
        return format!(
            "SELECT {schema}.{function}({code}, {path}, {message}) AS {root}",
            schema = quote_ident("donat"),
            function = quote_ident("raise_graphql_error"),
            code = quote_lit("validation-failed"),
            path = quote_lit(error_path),
            message = quote_lit("insert_many items must be a list of objects"),
            root = quote_ident("root"),
        );
    }

    let mut ctes = Vec::new();
    let mut effect_policy_gate = "TRUE".to_owned();
    for effect in &command.effects {
        let ResolvedCommandEffect::StartProcess(effect) = effect else {
            continue;
        };
        if effect.start_policy != ProcessStartPolicy::RejectRetired {
            continue;
        }
        let cte = format!("_cmd_effect_policy_gate_{}", effect.effect_position);
        let path = command
            .idempotency
            .as_ref()
            .expect("effect-bearing command has idempotency")
            .error_path
            .as_str();
        ctes.push(format!(
            "{cte} AS MATERIALIZED (SELECT TRUE AS {ok} WHERE CASE WHEN ({precondition}) THEN ({schema}.{raise_error}('validation-failed', {path}, {message}) IS NULL) ELSE FALSE END)",
            cte = quote_ident(&cte),
            ok = quote_ident("ok"),
            precondition = effect_policy_gate,
            schema = quote_ident("donat"),
            raise_error = quote_ident("raise_graphql_error"),
            path = quote_lit(path),
            message = quote_lit(&format!(
                "process '{}.{}' does not accept new starts",
                effect.source, effect.process_name
            )),
        ));
        effect_policy_gate = format!("({effect_policy_gate}) AND {}", command_gate_exists(&cte));
    }
    let guard_cte = "_cmd_guard_gate";
    ctes.push(command_rule_gate_cte(
        guard_cte,
        &command.guards,
        &effect_policy_gate,
    ));
    let guard_gate = command_gate_exists(guard_cte);
    let (execution_gate, invocation) = match &command.idempotency {
        Some(idempotency) => {
            let scope = command_canonical_json_array(ctx, &idempotency.scope);
            let scope_hash = command_hash(&scope);
            let input = command_jsonb_literal(&idempotency.input);
            let input_hash = command_hash(&input);
            let key = command_key_text(&idempotency.key);
            let expires_at = idempotency
                .retention_seconds
                .map(|seconds| format!("statement_timestamp() + {} * interval '1 second'", seconds))
                .unwrap_or_else(|| "'infinity'::timestamptz".to_owned());
            let identity = command_identity_text(&command.identity);
            let legacy_identity = command_legacy_identity_text(&command.identity.name);
            let legacy_gate_cte = "_cmd_legacy_identity_gate";
            ctes.push(format!(
                "{cte} AS MATERIALIZED (SELECT TRUE AS {ok} FROM {guard_cte} WHERE CASE WHEN EXISTS (SELECT 1 FROM {schema}.{journal} WHERE {stored_identity} = {legacy_identity} AND {command_name} = {name} AND {scope_hash_col} = {scope_hash} AND {key_col} = {key}) OR EXISTS (SELECT 1 FROM {schema}.{claims} WHERE {stored_identity} = {legacy_identity} AND {command_name} = {name} AND {scope_hash_col} = {scope_hash} AND {key_col} = {key}) THEN ({schema}.{raise_error}('validation-failed', {error_path}, {message}) IS NULL) ELSE TRUE END)",
                cte = quote_ident(legacy_gate_cte),
                ok = quote_ident("ok"),
                guard_cte = quote_ident(guard_cte),
                schema = quote_ident("donat"),
                journal = quote_ident("command_invocations"),
                claims = quote_ident("command_invocation_claims"),
                stored_identity = quote_ident("command_identity"),
                legacy_identity = quote_lit(&legacy_identity),
                command_name = quote_ident("command_name"),
                name = quote_lit(&command.identity.name),
                scope_hash_col = quote_ident("scope_hash"),
                scope_hash = scope_hash,
                key_col = quote_ident("key"),
                key = key,
                raise_error = quote_ident("raise_graphql_error"),
                error_path = quote_lit(&idempotency.error_path),
                message = quote_lit(
                    "legacy idempotency key cannot be replayed safely after command identity migration"
                ),
            ));
            ctes.push(format!(
                "{cte} AS (INSERT INTO {schema}.{table} AS {target} ({command_identity}, {command_name}, {scope_hash_col}, {key_col}, {claim_state}, {expires_at_col}) SELECT {identity}, {name}, {scope_hash}, {key}, 'first', {expires_at} FROM {legacy_gate_cte} ON CONFLICT ({command_identity}, {scope_hash_col}, {key_col}) DO UPDATE SET {claim_state} = CASE WHEN {target}.{expires_at_col} <= statement_timestamp() THEN 'first' ELSE 'replay' END, {expires_at_col} = CASE WHEN {target}.{expires_at_col} <= statement_timestamp() THEN EXCLUDED.{expires_at_col} ELSE {target}.{expires_at_col} END RETURNING {command_identity}, {command_name}, {scope_hash_col}, {key_col}, {claim_state})",
                cte = quote_ident("_cmd_claim"),
                schema = quote_ident("donat"),
                table = quote_ident("command_invocation_claims"),
                target = quote_ident("_cmd_claim_target"),
                command_identity = quote_ident("command_identity"),
                command_name = quote_ident("command_name"),
                scope_hash_col = quote_ident("scope_hash"),
                key_col = quote_ident("key"),
                claim_state = quote_ident("claim_state"),
                expires_at_col = quote_ident("expires_at"),
                identity = quote_lit(&identity),
                name = quote_lit(&command.identity.name),
                legacy_gate_cte = quote_ident(legacy_gate_cte),
            ));
            let identity_gate = command_gate_exists(legacy_gate_cte);
            (
                format!(
                    "({identity_gate}) AND EXISTS (SELECT 1 FROM {claim} WHERE {claim_state} = 'first')",
                    identity_gate = identity_gate,
                    claim = quote_ident("_cmd_claim"),
                    claim_state = quote_ident("claim_state"),
                ),
                Some(CommandInvocationSql {
                    input_hash,
                    error_path: idempotency.error_path.clone(),
                    expires_at,
                }),
            )
        }
        None => (guard_gate, None),
    };

    let mut step_execution_gate = execution_gate.clone();
    for (index, step) in command.steps.iter().enumerate() {
        if let CommandExecutionStep::Assert { rule, .. } = step {
            let assert_cte = format!("_cmd_assert_gate_{index}");
            ctes.push(command_rule_gate_cte(
                &assert_cte,
                std::slice::from_ref(rule),
                &step_execution_gate,
            ));
            step_execution_gate = format!(
                "({step_execution_gate}) AND {assert_gate}",
                assert_gate = command_gate_exists(&assert_cte),
            );
        } else if let CommandExecutionStep::AssertWhen {
            condition, rule, ..
        } = step
        {
            let assert_cte = format!("_cmd_assert_gate_{index}");
            ctes.push(command_conditional_rule_gate_cte(
                &assert_cte,
                condition,
                rule,
                &step_execution_gate,
            ));
            step_execution_gate = format!(
                "({step_execution_gate}) AND {assert_gate}",
                assert_gate = command_gate_exists(&assert_cte),
            );
        } else {
            for (gate_cte, gate) in
                command_pre_step_gate_ctes(ctx, index, step, &step_execution_gate)
            {
                ctes.push(gate_cte);
                step_execution_gate = format!("({step_execution_gate}) AND {gate}");
            }
            let mut current_step_gate = step_execution_gate.clone();
            if let Some(condition) = command_step_condition(step) {
                let condition_cte = format!("_cmd_condition_gate_{index}");
                ctes.push(command_condition_gate_cte(
                    &condition_cte,
                    condition,
                    &step_execution_gate,
                ));
                current_step_gate = format!(
                    "({step_execution_gate}) AND EXISTS (SELECT 1 FROM {condition_cte} WHERE {enabled})",
                    condition_cte = quote_ident(&condition_cte),
                    enabled = quote_ident("enabled"),
                );
            }
            let Some(cte) = command_step_cte(ctx, step, &current_step_gate) else {
                continue;
            };
            ctes.push(cte);
            if let Some((required_cte, required_gate)) = command_required_step_gate_cte(
                index,
                step,
                &step_execution_gate,
                &current_step_gate,
            ) {
                ctes.push(required_cte);
                step_execution_gate = format!("({step_execution_gate}) AND {required_gate}");
            }
            for (gate_cte, gate) in
                command_post_step_gate_ctes(ctx, index, step, &step_execution_gate)
            {
                ctes.push(gate_cte);
                step_execution_gate = format!("({step_execution_gate}) AND {gate}");
            }
        }
    }

    let final_gate_cte = "_cmd_final_gate";
    ctes.push(command_rule_gate_cte(
        final_gate_cte,
        &[],
        &step_execution_gate,
    ));

    let result = command_full_result_json(ctx, command);
    ctes.push(format!(
        "{cte} AS (SELECT ({result})::jsonb AS {column} FROM {final_gate})",
        cte = quote_ident("_cmd_result"),
        column = quote_ident("result"),
        final_gate = quote_ident(final_gate_cte),
    ));

    let result_source = match &invocation {
        Some(invocation) => {
            ctes.push(format!(
                "{cte} AS (INSERT INTO {schema}.{table} ({command_identity}, {command_name}, {scope_hash}, {key}, {invocation_id}, {input_fingerprint}, {result_col}, {status}, {expires_at_col}) SELECT {claim}.{command_identity}, {claim}.{command_name}, {claim}.{scope_hash}, {claim}.{key}, gen_random_uuid(), {input_hash}, {result_cte}.{result_col}, 'succeeded', {expires_at} FROM {claim} CROSS JOIN {result_cte} WHERE {claim}.{claim_state} = 'first' ON CONFLICT ({command_identity}, {scope_hash}, {key}) DO UPDATE SET {invocation_id} = EXCLUDED.{invocation_id}, {input_fingerprint} = EXCLUDED.{input_fingerprint}, {result_col} = EXCLUDED.{result_col}, {status} = EXCLUDED.{status}, {expires_at_col} = EXCLUDED.{expires_at_col} RETURNING {result_col}, {input_fingerprint}, {invocation_id})",
                cte = quote_ident("_cmd_store_first"),
                schema = quote_ident("donat"),
                table = quote_ident("command_invocations"),
                invocation_id = quote_ident("invocation_id"),
                result_col = quote_ident("result"),
                status = quote_ident("status"),
                result_cte = quote_ident("_cmd_result"),
                claim = quote_ident("_cmd_claim"),
                command_identity = quote_ident("command_identity"),
                command_name = quote_ident("command_name"),
                scope_hash = quote_ident("scope_hash"),
                key = quote_ident("key"),
                input_fingerprint = quote_ident("input_fingerprint"),
                input_hash = invocation.input_hash,
                claim_state = quote_ident("claim_state"),
                expires_at_col = quote_ident("expires_at"),
                expires_at = invocation.expires_at,
            ));
            ctes.push(format!(
                "{cte} AS (INSERT INTO {schema}.{table} ({command_identity}, {command_name}, {scope_hash}, {key}, {invocation_id}, {input_fingerprint}, {result_col}, {status}, {expires_at_col}) SELECT {claim}.{command_identity}, {claim}.{command_name}, {claim}.{scope_hash}, {claim}.{key}, gen_random_uuid(), {input_hash}, 'null'::jsonb, 'succeeded', {expires_at} FROM {claim} WHERE {claim}.{claim_state} = 'replay' ON CONFLICT ({command_identity}, {scope_hash}, {key}) DO UPDATE SET {key} = EXCLUDED.{key} RETURNING {result_col}, {input_fingerprint}, {invocation_id})",
                cte = quote_ident("_cmd_store_replay"),
                schema = quote_ident("donat"),
                table = quote_ident("command_invocations"),
                invocation_id = quote_ident("invocation_id"),
                command_identity = quote_ident("command_identity"),
                command_name = quote_ident("command_name"),
                scope_hash = quote_ident("scope_hash"),
                key = quote_ident("key"),
                input_fingerprint = quote_ident("input_fingerprint"),
                input_hash = invocation.input_hash,
                result_col = quote_ident("result"),
                status = quote_ident("status"),
                expires_at_col = quote_ident("expires_at"),
                expires_at = invocation.expires_at,
                claim = quote_ident("_cmd_claim"),
                claim_state = quote_ident("claim_state"),
            ));
            ctes.push(format!(
                "{cte} AS (SELECT {result_col}, {input_fingerprint}, {invocation_id} FROM {store_first} UNION ALL SELECT {result_col}, {input_fingerprint}, {invocation_id} FROM {store_replay})",
                cte = quote_ident("_cmd_invocation"),
                result_col = quote_ident("result"),
                input_fingerprint = quote_ident("input_fingerprint"),
                invocation_id = quote_ident("invocation_id"),
                store_first = quote_ident("_cmd_store_first"),
                store_replay = quote_ident("_cmd_store_replay"),
            ));
            for effect in &command.effects {
                ctes.push(command_effect_cte(ctx, effect));
            }
            format!(
                "(SELECT {result} FROM {invocation})",
                result = quote_ident("result"),
                invocation = quote_ident("_cmd_invocation"),
            )
        }
        None => format!(
            "(SELECT {result} FROM {cte})",
            result = quote_ident("result"),
            cte = quote_ident("_cmd_result"),
        ),
    };

    let guarded_result = command_rejections(
        ctx,
        command,
        &execution_gate,
        invocation.as_ref(),
        result_source,
    );
    ctes.push(format!(
        "{cte} AS (SELECT ({guarded_result})::jsonb AS {result})",
        cte = quote_ident("_cmd_checked"),
        result = quote_ident("result"),
    ));
    let projected = command_project_result(
        ctx,
        &format!(
            "(SELECT {result} FROM {cte})",
            result = quote_ident("result"),
            cte = quote_ident("_cmd_checked"),
        ),
        &command.selection,
    );
    if invocation.is_some() {
        format!(
            "WITH {} SELECT {projected} AS root, (SELECT {invocation_id} FROM {invocation}) AS {invocation_id}, COALESCE((SELECT {claim_state} = 'replay' FROM {claim}), FALSE) AS {replayed}",
            ctes.join(", "),
            invocation_id = quote_ident("invocation_id"),
            invocation = quote_ident("_cmd_invocation"),
            claim_state = quote_ident("claim_state"),
            claim = quote_ident("_cmd_claim"),
            replayed = quote_ident("replayed"),
        )
    } else {
        format!("WITH {} SELECT {projected} AS root", ctes.join(", "))
    }
}

struct CommandInvocationSql {
    input_hash: String,
    error_path: String,
    expires_at: String,
}

fn command_effect_cte(ctx: &mut Ctx, effect: &ResolvedCommandEffect) -> String {
    match effect {
        ResolvedCommandEffect::StartProcess(effect) => {
            let input = command_effect_object(ctx, &effect.input);
            let idempotency_key = command_effect_key(ctx, &effect.semantic_idempotency_key);
            let caller_role = effect
                .caller_role
                .as_deref()
                .map(quote_lit)
                .unwrap_or_else(|| "NULL::text".to_owned());
            let caller_session = effect.caller_role.as_ref().map_or_else(
                || "NULL::jsonb".to_owned(),
                |_| command_effect_object(ctx, &effect.caller_session_variables),
            );
            format!(
                "{cte} AS (INSERT INTO {schema}.{table} ({source_name}, {process_name}, {revision}, {input_json}, {caller_role_col}, {caller_session_col}, {command_invocation_id}, {effect_position}, {idempotency_key_col}, {status}) SELECT {source}, {process}, {process_revision}, {input}, {caller_role}, {caller_session}, {store}.{invocation_id}, {position}, {idempotency_key}, 'pending' FROM {store} RETURNING {id})",
                cte = quote_ident(&format!("_cmd_effect_{}", effect.effect_position)),
                schema = quote_ident("donat"),
                table = quote_ident("process_start_requests"),
                source_name = quote_ident("source_name"),
                process_name = quote_ident("process_name"),
                revision = quote_ident("revision"),
                input_json = quote_ident("input_json"),
                caller_role_col = quote_ident("caller_role"),
                caller_session_col = quote_ident("caller_session_json"),
                command_invocation_id = quote_ident("command_invocation_id"),
                effect_position = quote_ident("effect_position"),
                idempotency_key_col = quote_ident("idempotency_key"),
                status = quote_ident("status"),
                source = quote_lit(&effect.source),
                process = quote_lit(&effect.process_name),
                process_revision = quote_lit(&effect.process_revision),
                input = input,
                caller_role = caller_role,
                caller_session = caller_session,
                store = quote_ident("_cmd_store_first"),
                invocation_id = quote_ident("invocation_id"),
                position = effect.effect_position,
                idempotency_key = idempotency_key,
                id = quote_ident("id"),
            )
        }
        ResolvedCommandEffect::SignalProcess(effect) => {
            let correlation = command_effect_object(ctx, &effect.correlation);
            let payload = command_effect_object(ctx, &effect.payload);
            let idempotency_key = command_effect_key(ctx, &effect.semantic_idempotency_key);
            format!(
                "{cte} AS (INSERT INTO {schema}.{table} ({source_name}, {process_name}, {process_revision_col}, {signal_name}, {correlation_json}, {payload_json}, {command_invocation_id}, {effect_position}, {idempotency_key_col}, {status}) SELECT {source}, {process}, {process_revision}, {signal}, {correlation}, {payload}, {store}.{invocation_id}, {position}, {idempotency_key}, 'pending' FROM {store} RETURNING {id})",
                cte = quote_ident(&format!("_cmd_effect_{}", effect.effect_position)),
                schema = quote_ident("donat"),
                table = quote_ident("process_signal_requests"),
                source_name = quote_ident("source_name"),
                process_name = quote_ident("process_name"),
                process_revision_col = quote_ident("process_revision"),
                signal_name = quote_ident("signal_name"),
                correlation_json = quote_ident("correlation_json"),
                payload_json = quote_ident("payload_json"),
                command_invocation_id = quote_ident("command_invocation_id"),
                effect_position = quote_ident("effect_position"),
                idempotency_key_col = quote_ident("idempotency_key"),
                status = quote_ident("status"),
                source = quote_lit(&effect.source),
                process = quote_lit(&effect.process_name),
                process_revision = quote_lit(&effect.process_revision),
                signal = quote_lit(&effect.signal_name),
                correlation = correlation,
                payload = payload,
                store = quote_ident("_cmd_store_first"),
                invocation_id = quote_ident("invocation_id"),
                position = effect.effect_position,
                idempotency_key = idempotency_key,
                id = quote_ident("id"),
            )
        }
    }
}

fn command_effect_object(
    ctx: &mut Ctx,
    fields: &std::collections::BTreeMap<String, CommandExecutionValue>,
) -> String {
    let fields = fields
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                format!("to_jsonb({})", command_value_sql(ctx, value)),
            )
        })
        .collect::<Vec<_>>();
    format!("({})::jsonb", json_object(&ctx.dialect, &fields))
}

fn command_effect_key(ctx: &mut Ctx, value: &CommandExecutionValue) -> String {
    format!("(to_jsonb({}) #>> '{{}}')", command_value_sql(ctx, value))
}

/// Materialize one boolean gate before any dependent command DML. A false Rule
/// raises the existing structured GraphQL rejection while this CTE is still the
/// source of the dependent write, so a `BEFORE` trigger cannot run first.
fn command_rule_gate_cte(cte: &str, rules: &[CommandRule], precondition: &str) -> String {
    let mut condition = "TRUE".to_owned();
    for rule in rules.iter().rev() {
        condition = format!(
            "CASE WHEN ({rule_sql}) IS TRUE THEN ({condition}) ELSE (donat.raise_graphql_error('validation-failed', {path}, {message}) IS NULL) END",
            rule_sql = rule.sql,
            path = quote_lit(&rule.error_path),
            message = quote_lit(&rule.message),
        );
    }
    format!(
        "{cte} AS MATERIALIZED (SELECT TRUE AS {ok} WHERE CASE WHEN ({precondition}) THEN ({condition}) ELSE FALSE END)",
        cte = quote_ident(cte),
        ok = quote_ident("ok"),
    )
}

fn command_condition_sql(condition: &CommandCondition) -> String {
    match condition {
        CommandCondition::ArgumentEquals {
            argument,
            expected,
            pg_type,
        } => format!(
            "{} IS NOT DISTINCT FROM {}",
            scalar_sql(
                &donat_backend::AnyDialect::Postgres(donat_backend::PostgresDialect),
                argument,
                pg_type,
            ),
            scalar_sql(
                &donat_backend::AnyDialect::Postgres(donat_backend::PostgresDialect),
                expected,
                pg_type,
            ),
        ),
    }
}

fn command_condition_gate_cte(
    cte: &str,
    condition: &CommandCondition,
    precondition: &str,
) -> String {
    format!(
        "{cte} AS MATERIALIZED (SELECT ({condition}) IS TRUE AS {enabled} WHERE {precondition})",
        cte = quote_ident(cte),
        condition = command_condition_sql(condition),
        enabled = quote_ident("enabled"),
    )
}

fn command_conditional_rule_gate_cte(
    cte: &str,
    condition: &CommandCondition,
    rule: &CommandRule,
    precondition: &str,
) -> String {
    format!(
        "{cte} AS MATERIALIZED (SELECT TRUE AS {ok} WHERE CASE WHEN ({precondition}) THEN CASE WHEN ({condition}) IS NOT TRUE OR ({rule_sql}) IS TRUE THEN TRUE ELSE (donat.raise_graphql_error('validation-failed', {path}, {message}) IS NULL) END ELSE FALSE END)",
        cte = quote_ident(cte),
        ok = quote_ident("ok"),
        condition = command_condition_sql(condition),
        rule_sql = rule.sql,
        path = quote_lit(&rule.error_path),
        message = quote_lit(&rule.message),
    )
}

fn command_step_condition(step: &CommandExecutionStep) -> Option<&CommandCondition> {
    match step {
        CommandExecutionStep::UpdateWhen { condition, .. }
        | CommandExecutionStep::InsertWhen { condition, .. } => Some(condition),
        _ => None,
    }
}

fn command_gate_exists(cte: &str) -> String {
    format!("EXISTS (SELECT 1 FROM {})", quote_ident(cte))
}

fn command_business_gate_cte(
    cte: &str,
    precondition: &str,
    rejection: &str,
    error_path: &str,
    message: &str,
) -> (String, String) {
    let sql = format!(
        "{gate} AS MATERIALIZED (SELECT TRUE AS {ok} WHERE CASE WHEN ({precondition}) THEN CASE WHEN ({rejection}) THEN (donat.raise_graphql_error('validation-failed', {path}, {message}) IS NULL) ELSE TRUE END ELSE FALSE END)",
        gate = quote_ident(cte),
        ok = quote_ident("ok"),
        path = quote_lit(error_path),
        message = quote_lit(message),
    );
    (sql, command_gate_exists(cte))
}

fn command_permission_gate_cte(
    cte: &str,
    precondition: &str,
    rejection: &str,
    error_path: &str,
) -> (String, String) {
    let payload = serde_json::json!({
        "path": error_path,
        "message": "check constraint of an insert/update permission has failed",
    })
    .to_string();
    let sql = format!(
        "{gate} AS MATERIALIZED (SELECT TRUE AS {ok} WHERE CASE WHEN ({precondition}) THEN CASE WHEN ({rejection}) THEN (donat.check_violation({payload}) IS NULL) ELSE TRUE END ELSE FALSE END)",
        gate = quote_ident(cte),
        ok = quote_ident("ok"),
        payload = quote_lit(&payload),
    );
    (sql, command_gate_exists(cte))
}

fn command_pre_step_gate_ctes(
    ctx: &mut Ctx,
    index: usize,
    step: &CommandExecutionStep,
    precondition: &str,
) -> Vec<(String, String)> {
    let CommandExecutionStep::UpdateMany {
        name,
        table,
        input_cte,
        primary_key,
        guards,
        check,
        filter,
        error_path,
        ..
    } = step
    else {
        return Vec::new();
    };

    let input_alias = "_cmd_input";
    let target_alias = "_cmd_target";
    let input_keys = primary_key
        .iter()
        .map(|assignment| {
            command_value_sql_scoped(ctx, &assignment.value, input_alias, target_alias)
        })
        .collect::<Vec<_>>();
    let unique_rows = if input_keys.is_empty() {
        "0".to_owned()
    } else {
        format!(
            "(SELECT count(*) FROM (SELECT {keys} FROM {input} AS {input_alias} GROUP BY {keys}) AS {unique_alias})",
            keys = input_keys.join(", "),
            input = quote_ident(input_cte),
            input_alias = quote_ident(input_alias),
            unique_alias = quote_ident("_cmd_unique_input"),
        )
    };
    let input_count = format!("(SELECT count(*) FROM {})", quote_ident(input_cte));
    let duplicate_gate = command_business_gate_cte(
        &format!("_cmd_unique_input_gate_{index}"),
        precondition,
        &format!("{input_count} <> {unique_rows}"),
        error_path,
        &format!("command update_many step '{name}' contains duplicate input primary keys"),
    );
    let mut gates = vec![duplicate_gate];

    if let Some(check) = check {
        let prior_gate = format!(
            "({precondition}) AND {}",
            gates
                .last()
                .map(|(_, gate)| gate.as_str())
                .expect("the duplicate-input gate was just added")
        );
        let mut predicates =
            command_update_many_join_predicates(ctx, primary_key, target_alias, input_alias);
        predicates.extend(command_update_many_join_predicates(
            ctx,
            guards,
            target_alias,
            input_alias,
        ));
        if let Some(filter) = filter {
            predicates.push(ctx.bool_exp(filter, target_alias, target_alias));
        }
        predicates.push(format!("({}) IS NOT TRUE", check.sql));
        let rejection = format!(
            "EXISTS (SELECT 1 FROM {table} AS {target} JOIN {input} AS {input_alias} ON {join} WHERE {predicate})",
            table = command_table_sql(table),
            target = quote_ident(target_alias),
            input = quote_ident(input_cte),
            input_alias = quote_ident(input_alias),
            join =
                command_update_many_join_predicates(ctx, primary_key, target_alias, input_alias,)
                    .join(" AND "),
            predicate = predicates.join(" AND "),
        );
        gates.push(command_business_gate_cte(
            &format!("_cmd_update_check_gate_{index}"),
            &prior_gate,
            &rejection,
            &check.error_path,
            &check.message,
        ));
    }

    gates
}

fn command_post_step_gate_ctes(
    ctx: &mut Ctx,
    index: usize,
    step: &CommandExecutionStep,
    precondition: &str,
) -> Vec<(String, String)> {
    match step {
        CommandExecutionStep::ProjectMany {
            name,
            cte,
            maximum_rows,
            error_path,
            ..
        }
        | CommandExecutionStep::FixedRows {
            name,
            cte,
            maximum_rows,
            error_path,
            ..
        } => vec![command_business_gate_cte(
            &format!("_cmd_maximum_rows_gate_{index}"),
            precondition,
            &format!(
                "(SELECT count(*) FROM {}) > {}",
                quote_ident(cte),
                maximum_rows
            ),
            error_path,
            &format!("command step '{name}' exceeded maximum_rows {maximum_rows}"),
        )],
        CommandExecutionStep::Decision {
            name,
            cte,
            decision,
            error_path,
            ..
        } => command_decision_gate_ctes(index, name, cte, None, decision, error_path, precondition),
        CommandExecutionStep::DecisionMany {
            name,
            cte,
            input_cte,
            decision,
            error_path,
            ..
        } => command_decision_gate_ctes(
            index,
            name,
            cte,
            Some(input_cte),
            decision,
            error_path,
            precondition,
        ),
        CommandExecutionStep::AllocateMany {
            name,
            cte,
            input_cte,
            group_key,
            requested,
            allocated,
            backordered,
            groups,
            lines,
            group_order_by,
            line_order_by,
            maximum_rows,
            error_path,
            ..
        } => {
            let input_count = format!("(SELECT count(*) FROM {})", quote_ident(input_cte));
            let mut identity = vec![qualified("_cmd_input", "order_line_id")];
            identity.extend(
                group_key
                    .iter()
                    .map(|column| qualified("_cmd_input", &column.name)),
            );
            if lines
                .iter()
                .any(|column| column.name == "inventory_level_id")
            {
                identity.push(qualified("_cmd_input", "inventory_level_id"));
            }
            let unique_count = format!(
                "(SELECT count(*) FROM (SELECT {identity} FROM {input} AS {alias} GROUP BY {identity}) AS {unique})",
                identity = identity.join(", "),
                input = quote_ident(input_cte),
                alias = quote_ident("_cmd_input"),
                unique = quote_ident("_cmd_unique_allocation_input"),
            );
            let requested_consistency = format!(
                "EXISTS (SELECT 1 FROM {input} AS {alias} GROUP BY {line} HAVING min({requested}) IS DISTINCT FROM max({requested}))",
                input = quote_ident(input_cte),
                alias = quote_ident("_cmd_input"),
                line = qualified("_cmd_input", "order_line_id"),
                requested = qualified("_cmd_input", &requested.name),
            );
            let group_cte = format!("{cte}_groups");
            let line_cte = format!("{cte}_lines");
            let backorder_cte = format!("{cte}_backorders");
            let mut gates = vec![
                command_business_gate_cte(
                    &format!("_cmd_allocation_bound_gate_{index}"),
                    precondition,
                    &format!("{input_count} > {maximum_rows}"),
                    error_path,
                    &format!("command allocate_many step '{name}' exceeded its row bound"),
                ),
                command_business_gate_cte(
                    &format!("_cmd_allocation_duplicate_gate_{index}"),
                    precondition,
                    &format!("{input_count} <> {unique_count}"),
                    error_path,
                    &format!("command allocate_many step '{name}' contains duplicate candidates"),
                ),
                command_business_gate_cte(
                    &format!("_cmd_allocation_requested_gate_{index}"),
                    precondition,
                    &requested_consistency,
                    error_path,
                    &format!(
                        "command allocate_many step '{name}' has inconsistent requested quantities"
                    ),
                ),
                command_business_gate_cte(
                    &format!("_cmd_allocation_id_gate_{index}"),
                    precondition,
                    &format!(
                        "(SELECT count(*) FROM {groups}) <> (SELECT count(DISTINCT {allocation_id}) FROM {groups})",
                        groups = quote_ident(&group_cte),
                        allocation_id = quote_ident("allocation_id"),
                    ),
                    error_path,
                    &format!(
                        "command allocate_many step '{name}' produced duplicate allocation ids"
                    ),
                ),
            ];
            let conservation = format!(
                "EXISTS (SELECT 1 FROM {input} AS {source} LEFT JOIN (SELECT {line}, sum({allocated}) AS {allocated_sum} FROM {lines} GROUP BY {line}) AS {line_sum} USING ({line}) LEFT JOIN {backorders} AS {backorder} USING ({line}) GROUP BY {source}.{line}, {line_sum}.{allocated_sum}, {backorder}.{backordered} HAVING max({source}.{requested}) <> coalesce({line_sum}.{allocated_sum}, 0) + coalesce({backorder}.{backordered}, 0))",
                input = quote_ident(input_cte),
                source = quote_ident("_cmd_source"),
                line = quote_ident("order_line_id"),
                allocated = quote_ident(&allocated.name),
                allocated_sum = quote_ident("_cmd_allocated_sum"),
                lines = quote_ident(&line_cte),
                line_sum = quote_ident("_cmd_line_sum"),
                backorders = quote_ident(&backorder_cte),
                backorder = quote_ident("_cmd_backorder"),
                backordered = quote_ident(&backordered.name),
                requested = quote_ident(&requested.name),
            );
            gates.push(command_business_gate_cte(
                &format!("_cmd_allocation_conservation_gate_{index}"),
                precondition,
                &conservation,
                error_path,
                &format!("command allocate_many step '{name}' violated quantity conservation"),
            ));
            let duplicate_order = |rows: &str,
                                   order_by: &[CommandColumn],
                                   row_alias: &str,
                                   unique_alias: &str| {
                let keys = order_by
                    .iter()
                    .map(|column| qualified(row_alias, &column.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "(SELECT count(*) FROM {rows}) <> (SELECT count(*) FROM (SELECT {keys} FROM {rows} AS {row_alias} GROUP BY {keys}) AS {unique_alias})",
                    rows = quote_ident(rows),
                    row_alias = quote_ident(row_alias),
                    unique_alias = quote_ident(unique_alias),
                )
            };
            gates.push(command_business_gate_cte(
                &format!("_cmd_allocation_group_order_gate_{index}"),
                precondition,
                &duplicate_order(
                    &group_cte,
                    group_order_by,
                    "_cmd_group_order",
                    "_cmd_unique_group_order",
                ),
                error_path,
                &format!("command allocate_many step '{name}' produced duplicate group order keys"),
            ));
            gates.push(command_business_gate_cte(
                &format!("_cmd_allocation_line_order_gate_{index}"),
                precondition,
                &duplicate_order(
                    &line_cte,
                    line_order_by,
                    "_cmd_line_order",
                    "_cmd_unique_line_order",
                ),
                error_path,
                &format!("command allocate_many step '{name}' produced duplicate line order keys"),
            ));
            gates.push(command_business_gate_cte(
                &format!("_cmd_allocation_output_bound_gate_{index}"),
                precondition,
                &format!(
                    "(SELECT count(*) FROM {groups}) > {maximum_rows} OR (SELECT count(*) FROM {lines}) > {maximum_rows} OR (SELECT count(*) FROM {backorders}) > {maximum_rows}",
                    groups = quote_ident(&group_cte),
                    lines = quote_ident(&line_cte),
                    backorders = quote_ident(&backorder_cte),
                ),
                error_path,
                &format!("command allocate_many step '{name}' exceeded its output bound"),
            ));
            let _ = groups;
            gates
        }
        CommandExecutionStep::SelectMany {
            name,
            cte,
            order_by,
            require_non_empty,
            error_path,
            ..
        } => {
            let order_keys = order_by
                .iter()
                .enumerate()
                .map(|(order_index, _)| {
                    qualified("_cmd_ordered_rows", &format!("_cmd_order_{order_index}"))
                })
                .collect::<Vec<_>>();
            let unique_count = format!(
                "(SELECT count(*) FROM (SELECT {keys} FROM {step} AS {rows} GROUP BY {keys}) AS {unique_alias})",
                keys = order_keys.join(", "),
                step = quote_ident(cte),
                rows = quote_ident("_cmd_ordered_rows"),
                unique_alias = quote_ident("_cmd_unique_order"),
            );
            let row_count = format!("(SELECT count(*) FROM {})", quote_ident(cte));
            let unique_gate = command_business_gate_cte(
                &format!("_cmd_unique_order_gate_{index}"),
                precondition,
                &format!("{row_count} <> {unique_count}"),
                error_path,
                &format!("command select_many step '{name}' contains duplicate order keys"),
            );
            let mut gates = vec![unique_gate];
            if *require_non_empty {
                let prior_gate = format!(
                    "({precondition}) AND {}",
                    gates
                        .last()
                        .map(|(_, gate)| gate.as_str())
                        .expect("the unique-order gate was just added")
                );
                gates.push(command_business_gate_cte(
                    &format!("_cmd_non_empty_gate_{index}"),
                    &prior_gate,
                    &format!("{row_count} = 0"),
                    error_path,
                    &format!("command select_many step '{name}' requires at least one row"),
                ));
            }
            gates
        }
        CommandExecutionStep::UpdateMany {
            name,
            cte,
            input_cte,
            primary_key,
            require_each,
            permission_check,
            error_path,
            ..
        } => {
            let mut gates = Vec::new();
            if let Some(check) = permission_check {
                let rejection = format!(
                    "(SELECT count(*) FROM {step} WHERE ({check}) IS NOT TRUE) > 0",
                    step = quote_ident(cte),
                    check = ctx.bool_exp(check, cte, cte),
                );
                gates.push(command_permission_gate_cte(
                    &format!("_cmd_update_permission_gate_{index}"),
                    precondition,
                    &rejection,
                    error_path,
                ));
            }
            if *require_each {
                let prior_gate = match gates.last() {
                    Some((_, gate)) => format!("({precondition}) AND {gate}"),
                    None => precondition.to_owned(),
                };
                let input_alias = "_cmd_input";
                let input_keys = primary_key
                    .iter()
                    .map(|assignment| {
                        command_value_sql_scoped(ctx, &assignment.value, input_alias, "_cmd_target")
                    })
                    .collect::<Vec<_>>();
                let unique_count = if input_keys.is_empty() {
                    "0".to_owned()
                } else {
                    format!(
                        "(SELECT count(*) FROM (SELECT {keys} FROM {input} AS {input_alias} GROUP BY {keys}) AS {unique_alias})",
                        keys = input_keys.join(", "),
                        input = quote_ident(input_cte),
                        input_alias = quote_ident(input_alias),
                        unique_alias = quote_ident("_cmd_exact_unique_input"),
                    )
                };
                let input_count = format!("(SELECT count(*) FROM {})", quote_ident(input_cte));
                let affected_count = format!("(SELECT count(*) FROM {})", quote_ident(cte));
                gates.push(command_business_gate_cte(
                    &format!("_cmd_affected_each_gate_{index}"),
                    &prior_gate,
                    &format!(
                        "{unique_count} <> {input_count} OR {affected_count} <> {input_count}"
                    ),
                    error_path,
                    &format!("command update_many step '{name}' did not affect every input row"),
                ));
            }
            gates
        }
        _ => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn command_decision_gate_ctes(
    index: usize,
    name: &str,
    cte: &str,
    input_cte: Option<&str>,
    decision: &CommandDecision,
    error_path: &str,
    precondition: &str,
) -> Vec<(String, String)> {
    let matches_cte = format!("{cte}_matches");
    let expected = input_cte
        .map(|input| format!("(SELECT count(*) FROM {})", quote_ident(input)))
        .unwrap_or_else(|| "1".to_owned());
    let selected = format!("(SELECT count(*) FROM {})", quote_ident(cte));
    let no_match = command_business_gate_cte(
        &format!("_cmd_decision_no_match_gate_{index}"),
        precondition,
        &format!("{selected} <> {expected}"),
        error_path,
        &format!("command decision step '{name}' had no matching row"),
    );
    let mut gates = vec![no_match];
    if decision.hit_policy == CommandDecisionHitPolicy::Unique {
        let matches = format!("(SELECT count(*) FROM {})", quote_ident(&matches_cte));
        let prior = format!("({precondition}) AND {}", gates[0].1);
        gates.push(command_business_gate_cte(
            &format!("_cmd_decision_multiple_match_gate_{index}"),
            &prior,
            &format!("{matches} <> {expected}"),
            error_path,
            &format!("command decision step '{name}' matched multiple rows"),
        ));
    }
    gates
}

fn command_required_step_gate_cte(
    index: usize,
    step: &CommandExecutionStep,
    precondition: &str,
    step_precondition: &str,
) -> Option<(String, String)> {
    let (step_cte, error_path, message, conditional) = match step {
        CommandExecutionStep::SelectOne {
            name,
            cte,
            require_found: true,
            error_path,
            ..
        } => (
            cte,
            error_path,
            format!("command select_one step '{name}' did not find a row"),
            false,
        ),
        CommandExecutionStep::Update {
            name,
            cte,
            require_affected: true,
            error_path,
            ..
        } => (
            cte,
            error_path,
            format!("command update step '{name}' did not affect a row"),
            false,
        ),
        CommandExecutionStep::UpdateWhen {
            name,
            cte,
            require_affected: true,
            error_path,
            ..
        } => (
            cte,
            error_path,
            format!("command update_when step '{name}' did not affect a row"),
            true,
        ),
        CommandExecutionStep::Delete {
            name,
            cte,
            require_affected: true,
            error_path,
            ..
        } => (
            cte,
            error_path,
            format!("command delete step '{name}' did not affect a row"),
            false,
        ),
        _ => return None,
    };
    let gate_cte = format!("_cmd_required_gate_{index}");
    let required = if conditional {
        format!(
            "CASE WHEN ({step_precondition}) THEN EXISTS (SELECT 1 FROM {step}) ELSE TRUE END",
            step = quote_ident(step_cte),
        )
    } else {
        format!("EXISTS (SELECT 1 FROM {})", quote_ident(step_cte))
    };
    let sql = format!(
        "{gate} AS MATERIALIZED (SELECT TRUE AS {ok} WHERE CASE WHEN ({precondition}) THEN CASE WHEN {required} THEN TRUE ELSE (donat.raise_graphql_error('validation-failed', {path}, {message}) IS NULL) END ELSE FALSE END)",
        gate = quote_ident(&gate_cte),
        ok = quote_ident("ok"),
        required = required,
        path = quote_lit(error_path),
        message = quote_lit(&message),
    );
    let gate = command_gate_exists(&gate_cte);
    Some((sql, gate))
}

fn command_step_cte(
    ctx: &mut Ctx,
    step: &CommandExecutionStep,
    execution_gate: &str,
) -> Option<String> {
    match step {
        CommandExecutionStep::Assert { .. } | CommandExecutionStep::AssertWhen { .. } => None,
        CommandExecutionStep::ArgumentRows {
            cte,
            items,
            columns,
            minimum_items,
            maximum_items,
            error_path,
            ..
        } => {
            let items = scalar_sql(&ctx.dialect, items, "jsonb");
            let alias = "_cmd_argument_rows";
            let ordinal = "_cmd_ordinal";
            let definitions = columns
                .iter()
                .map(|column| {
                    format!(
                        "{} {}",
                        quote_ident(&column.name),
                        quote_type_name(&column.pg_type)
                    )
                })
                .collect::<Vec<_>>();
            let aliases = columns
                .iter()
                .map(|column| quote_ident(&column.name))
                .chain(std::iter::once(quote_ident(ordinal)))
                .collect::<Vec<_>>();
            let projection = std::iter::once(qualified(alias, ordinal))
                .chain(columns.iter().map(|column| qualified(alias, &column.name)))
                .collect::<Vec<_>>();
            let maximum_message =
                format!("command argument row-set exceeded maximum_items {maximum_items}");
            let minimum_message =
                format!("command argument row-set requires minimum_items {minimum_items}");
            let within_bound = format!(
                "CASE WHEN jsonb_array_length({items}) < {minimum_items} THEN (donat.raise_graphql_error('validation-failed', {path}, {minimum_message}) IS NULL) WHEN jsonb_array_length({items}) <= {maximum_items} THEN ({execution_gate}) ELSE (donat.raise_graphql_error('validation-failed', {path}, {maximum_message}) IS NULL) END",
                path = quote_lit(error_path),
                minimum_message = quote_lit(&minimum_message),
                maximum_message = quote_lit(&maximum_message),
            );
            let source = if definitions.is_empty() {
                format!(
                    "jsonb_array_elements({items}) WITH ORDINALITY AS {alias}({ignored}, {ordinal})",
                    alias = quote_ident(alias),
                    ignored = quote_ident("_cmd_ignored"),
                    ordinal = quote_ident(ordinal),
                )
            } else {
                format!(
                    "ROWS FROM(jsonb_to_recordset({items}) AS ({definitions})) WITH ORDINALITY AS {alias}({aliases})",
                    definitions = definitions.join(", "),
                    alias = quote_ident(alias),
                    aliases = aliases.join(", "),
                )
            };
            Some(format!(
                "{cte} AS MATERIALIZED (SELECT {projection} FROM {source} WHERE {within_bound} ORDER BY {ordinal})",
                cte = quote_ident(cte),
                projection = projection.join(", "),
                ordinal = qualified(alias, ordinal),
            ))
        }
        CommandExecutionStep::SelectOne {
            cte,
            table,
            by,
            returning,
            filter,
            ..
        } => {
            let alias = "_cmd_target";
            let mut predicates = command_predicates(ctx, by, alias);
            if let Some(filter) = filter {
                predicates.push(ctx.bool_exp(filter, alias, alias));
            }
            predicates.push(execution_gate.to_owned());
            let selected = command_returning_columns(returning, alias);
            Some(format!(
                "{cte} AS (SELECT {selected} FROM {table} AS {alias} WHERE {predicate} LIMIT 1)",
                cte = quote_ident(cte),
                table = command_table_sql(table),
                alias = quote_ident(alias),
                predicate = predicates.join(" AND "),
            ))
        }
        CommandExecutionStep::SelectMany {
            cte,
            table,
            equality,
            order_by,
            returning,
            filter,
            ..
        } => {
            let alias = "_cmd_target";
            let mut predicates = command_predicates(ctx, equality, alias);
            if let Some(filter) = filter {
                predicates.push(ctx.bool_exp(filter, alias, alias));
            }
            predicates.push(execution_gate.to_owned());
            let selected = command_returning_columns(returning, alias);
            let order = order_by
                .iter()
                .map(|column| qualified(alias, &column.name))
                .collect::<Vec<_>>();
            let private_order = order_by
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    format!(
                        "{} AS {}",
                        qualified(alias, &column.name),
                        quote_ident(&format!("_cmd_order_{index}"))
                    )
                })
                .collect::<Vec<_>>();
            let mut projection = vec![selected];
            projection.extend(private_order);
            projection.push(format!(
                "row_number() OVER (ORDER BY {})::bigint AS {}",
                order.join(", "),
                quote_ident("_cmd_ordinal")
            ));
            Some(format!(
                "{cte} AS MATERIALIZED (SELECT {projection} FROM {table} AS {alias} WHERE {predicate} ORDER BY {order})",
                cte = quote_ident(cte),
                projection = projection.join(", "),
                table = command_table_sql(table),
                alias = quote_ident(alias),
                predicate = predicates.join(" AND "),
                order = order.join(", "),
            ))
        }
        CommandExecutionStep::Aggregate {
            cte,
            input_cte,
            values,
            ..
        } => {
            let input_alias = "_cmd_input";
            let aggregates = values
                .iter()
                .map(|aggregate| command_aggregate_sql(aggregate, input_alias))
                .collect::<Vec<_>>();
            Some(format!(
                "{cte} AS (SELECT {aggregates} FROM {input} AS {input_alias} WHERE {execution_gate})",
                cte = quote_ident(cte),
                aggregates = aggregates.join(", "),
                input = quote_ident(input_cte),
                input_alias = quote_ident(input_alias),
            ))
        }
        CommandExecutionStep::Project { cte, values, .. } => {
            let projection = command_named_values_sql(ctx, values, "_cmd_input", "_cmd_target");
            Some(format!(
                "{cte} AS (SELECT {projection} WHERE {execution_gate})",
                cte = quote_ident(cte),
            ))
        }
        CommandExecutionStep::ProjectMany {
            cte,
            input_cte,
            values,
            ..
        } => {
            let input_alias = "_cmd_input";
            let projection = command_named_values_sql(ctx, values, input_alias, input_alias);
            Some(format!(
                "{cte} AS MATERIALIZED (SELECT {input_alias}.{ordinal}, {projection} FROM {input} AS {input_alias} WHERE {execution_gate} ORDER BY {input_alias}.{ordinal})",
                cte = quote_ident(cte),
                input_alias = quote_ident(input_alias),
                ordinal = quote_ident("_cmd_ordinal"),
                input = quote_ident(input_cte),
            ))
        }
        CommandExecutionStep::FixedRows {
            cte, columns, rows, ..
        } => {
            let aliases = std::iter::once(quote_ident("_cmd_ordinal"))
                .chain(columns.iter().map(|column| quote_ident(&column.name)))
                .collect::<Vec<_>>();
            let rows = rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let values = std::iter::once(format!("{}::bigint", index + 1))
                        .chain(row.iter().map(|value| command_value_sql(ctx, value)))
                        .collect::<Vec<_>>();
                    format!("({})", values.join(", "))
                })
                .collect::<Vec<_>>();
            Some(format!(
                "{cte} AS MATERIALIZED (SELECT * FROM (VALUES {rows}) AS {fixed}({aliases}) WHERE {execution_gate} ORDER BY {ordinal})",
                cte = quote_ident(cte),
                rows = rows.join(", "),
                fixed = quote_ident("_cmd_fixed"),
                aliases = aliases.join(", "),
                ordinal = quote_ident("_cmd_ordinal"),
            ))
        }
        CommandExecutionStep::Decision {
            cte,
            decision,
            input,
            returning,
            ..
        } => Some(command_decision_ctes(
            ctx,
            cte,
            None,
            decision,
            input,
            returning,
            &[],
            execution_gate,
        )),
        CommandExecutionStep::DecisionMany {
            cte,
            input_cte,
            decision,
            input,
            returning,
            order_by,
            ..
        } => Some(command_decision_ctes(
            ctx,
            cte,
            Some(input_cte),
            decision,
            input,
            returning,
            order_by,
            execution_gate,
        )),
        CommandExecutionStep::Insert {
            cte, table, object, ..
        }
        | CommandExecutionStep::InsertWhen {
            cte, table, object, ..
        } => {
            let columns: Vec<String> = object
                .iter()
                .map(|assignment| quote_ident(&assignment.column.name))
                .collect();
            let values: Vec<String> = object
                .iter()
                .map(|assignment| command_value_sql(ctx, &assignment.value))
                .collect();
            Some(format!(
                "{cte} AS (INSERT INTO {table} ({columns}) SELECT {values} WHERE {execution_gate} RETURNING *)",
                cte = quote_ident(cte),
                table = command_table_sql(table),
                columns = columns.join(", "),
                values = values.join(", "),
            ))
        }
        CommandExecutionStep::InsertMany {
            cte,
            table,
            items,
            item_fields,
            object,
            ..
        } => {
            let columns: Vec<String> = object
                .iter()
                .map(|assignment| quote_ident(&assignment.column.name))
                .collect();
            let item_rows = items.as_json().as_array().map(Vec::as_slice).unwrap_or(&[]);
            let mut item_ctes = item_rows
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let item = item.as_object()?;
                    let values = object
                        .iter()
                        .map(|assignment| command_value_sql_for_item(ctx, &assignment.value, item))
                        .collect::<Vec<_>>();
                    let item_source = command_item_source_sql(ctx, item_fields, item);
                    let item_cte = format!("{cte}_item_{index}");
                    Some(format!(
                        "{item_cte} AS (INSERT INTO {table} ({columns}) SELECT {values} FROM ({item_source}) AS {item_alias} WHERE {execution_gate} RETURNING *)",
                        item_cte = quote_ident(&item_cte),
                        table = command_table_sql(table),
                        columns = columns.join(", "),
                        values = values.join(", "),
                        item_alias = quote_ident("_cmd_item"),
                    ))
                })
                .collect::<Vec<_>>();
            if item_ctes.is_empty() {
                let empty_item = serde_json::Map::new();
                let values = object
                    .iter()
                    .map(|assignment| {
                        command_value_sql_for_item(ctx, &assignment.value, &empty_item)
                    })
                    .collect::<Vec<_>>();
                let item_source = command_item_source_sql(ctx, item_fields, &empty_item);
                let empty_item_cte = format!("{cte}_item_empty");
                Some(format!(
                    "{empty_item_cte} AS (INSERT INTO {table} ({columns}) SELECT {values} FROM ({item_source}) AS {item_alias} WHERE FALSE RETURNING *), {cte} AS (SELECT 0::bigint AS {ordinal}, {empty_item_cte}.* FROM {empty_item_cte} WHERE FALSE)",
                    empty_item_cte = quote_ident(&empty_item_cte),
                    table = command_table_sql(table),
                    columns = columns.join(", "),
                    values = values.join(", "),
                    item_alias = quote_ident("_cmd_item"),
                    cte = quote_ident(cte),
                    ordinal = quote_ident("_cmd_ordinal"),
                ))
            } else {
                let ordered_rows = item_ctes
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        let item_cte = format!("{cte}_item_{index}");
                        format!(
                            "SELECT {ordinal}::bigint AS {ordinal_column}, {item_cte}.* FROM {item_cte}",
                            ordinal = index + 1,
                            ordinal_column = quote_ident("_cmd_ordinal"),
                            item_cte = quote_ident(&item_cte),
                        )
                    })
                    .collect::<Vec<_>>();
                item_ctes.push(format!(
                    "{cte} AS ({ordered_rows})",
                    cte = quote_ident(cte),
                    ordered_rows = ordered_rows.join(" UNION ALL "),
                ));
                Some(item_ctes.join(", "))
            }
        }
        CommandExecutionStep::InsertRows {
            cte,
            table,
            input_cte,
            where_nonzero,
            object,
            returning,
            ..
        } => {
            let item_alias = "_cmd_item";
            let columns = object
                .iter()
                .map(|assignment| quote_ident(&assignment.column.name))
                .collect::<Vec<_>>();
            let values = object
                .iter()
                .map(|assignment| {
                    command_value_sql_scoped(ctx, &assignment.value, item_alias, "_cmd_target")
                })
                .collect::<Vec<_>>();
            let mut predicates = vec![execution_gate.to_owned()];
            if let Some(column) = where_nonzero {
                predicates.push(format!(
                    "coalesce({}::numeric, 0::numeric) <> 0::numeric",
                    qualified(item_alias, column)
                ));
            }
            let inserted = format!("{cte}_inserted");
            let order = returning
                .iter()
                .map(|column| qualified("_cmd_inserted", &column.name))
                .collect::<Vec<_>>();
            Some(format!(
                "{inserted} AS (INSERT INTO {table} ({columns}) SELECT {values} FROM {input} AS {item_alias} WHERE {predicates} RETURNING *), {cte} AS MATERIALIZED (SELECT row_number() OVER (ORDER BY {order})::bigint AS {ordinal}, {returning} FROM {inserted} AS {inserted_alias} ORDER BY {order})",
                inserted = quote_ident(&inserted),
                table = command_table_sql(table),
                columns = columns.join(", "),
                values = values.join(", "),
                input = quote_ident(input_cte),
                item_alias = quote_ident(item_alias),
                predicates = predicates.join(" AND "),
                cte = quote_ident(cte),
                order = order.join(", "),
                ordinal = quote_ident("_cmd_ordinal"),
                returning = command_returning_columns(returning, "_cmd_inserted"),
                inserted_alias = quote_ident("_cmd_inserted"),
            ))
        }
        CommandExecutionStep::Update {
            cte,
            table,
            predicate,
            set,
            filter,
            ..
        }
        | CommandExecutionStep::UpdateWhen {
            cte,
            table,
            predicate,
            set,
            filter,
            ..
        } => {
            let alias = "_cmd_target";
            let sets: Vec<String> = set
                .iter()
                .map(|assignment| {
                    format!(
                        "{} = {}",
                        quote_ident(&assignment.column.name),
                        command_value_sql(ctx, &assignment.value)
                    )
                })
                .collect();
            let mut predicates = command_predicates(ctx, predicate, alias);
            if let Some(filter) = filter {
                predicates.push(ctx.bool_exp(filter, alias, alias));
            }
            predicates.push(execution_gate.to_owned());
            Some(format!(
                "{cte} AS (UPDATE {table} AS {alias} SET {sets} WHERE {predicate} RETURNING *)",
                cte = quote_ident(cte),
                table = command_table_sql(table),
                alias = quote_ident(alias),
                sets = sets.join(", "),
                predicate = predicates.join(" AND "),
            ))
        }
        CommandExecutionStep::UpdateMany {
            cte,
            table,
            input_cte,
            primary_key,
            guards,
            assignments,
            check,
            filter,
            ..
        } => {
            let target_alias = "_cmd_target";
            let input_alias = "_cmd_input";
            let updated_alias = "_cmd_updated";
            let sets = assignments
                .iter()
                .map(|assignment| {
                    format!(
                        "{} = {}",
                        quote_ident(&assignment.column.name),
                        command_value_sql_scoped(ctx, &assignment.value, input_alias, target_alias,)
                    )
                })
                .collect::<Vec<_>>();
            let mut predicates =
                command_update_many_join_predicates(ctx, primary_key, target_alias, input_alias);
            predicates.extend(command_update_many_join_predicates(
                ctx,
                guards,
                target_alias,
                input_alias,
            ));
            if let Some(filter) = filter {
                predicates.push(ctx.bool_exp(filter, target_alias, target_alias));
            }
            if let Some(check) = check {
                predicates.push(format!("({}) IS TRUE", check.sql));
            }
            predicates.push(execution_gate.to_owned());

            let updated_cte = format!("{cte}_updated");
            let ordered_join = primary_key
                .iter()
                .map(|assignment| {
                    format!(
                        "{} = {}",
                        qualified(updated_alias, &assignment.column.name),
                        command_value_sql_scoped(
                            ctx,
                            &assignment.value,
                            input_alias,
                            updated_alias,
                        )
                    )
                })
                .collect::<Vec<_>>();
            Some(format!(
                "{updated_cte} AS (UPDATE {table} AS {target} SET {sets} FROM {input} AS {input_alias} WHERE {predicate} RETURNING {target}.*), {cte} AS (SELECT {input_alias}.{ordinal}, {updated_alias}.* FROM {input} AS {input_alias} JOIN {updated_cte} AS {updated_alias} ON {ordered_join} ORDER BY {input_alias}.{ordinal})",
                updated_cte = quote_ident(&updated_cte),
                table = command_table_sql(table),
                target = quote_ident(target_alias),
                sets = sets.join(", "),
                input = quote_ident(input_cte),
                input_alias = quote_ident(input_alias),
                predicate = predicates.join(" AND "),
                cte = quote_ident(cte),
                ordinal = quote_ident("_cmd_ordinal"),
                updated_alias = quote_ident(updated_alias),
                ordered_join = ordered_join.join(" AND "),
            ))
        }
        CommandExecutionStep::AllocateMany {
            cte,
            input_cte,
            request_id,
            group_key,
            requested,
            available,
            allocated,
            backordered,
            groups,
            lines,
            backorders,
            group_order_by,
            line_order_by,
            ..
        } => Some(command_allocate_many_ctes(
            ctx,
            cte,
            input_cte,
            request_id,
            group_key,
            requested,
            available,
            allocated,
            backordered,
            groups,
            lines,
            backorders,
            group_order_by,
            line_order_by,
            execution_gate,
        )),
        CommandExecutionStep::Delete {
            cte,
            table,
            predicate,
            filter,
            ..
        } => {
            let alias = "_cmd_target";
            let mut predicates = command_predicates(ctx, predicate, alias);
            if let Some(filter) = filter {
                predicates.push(ctx.bool_exp(filter, alias, alias));
            }
            predicates.push(execution_gate.to_owned());
            Some(format!(
                "{cte} AS (DELETE FROM {table} AS {alias} WHERE {predicate} RETURNING *)",
                cte = quote_ident(cte),
                table = command_table_sql(table),
                alias = quote_ident(alias),
                predicate = predicates.join(" AND "),
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn command_allocate_many_ctes(
    ctx: &mut Ctx,
    cte: &str,
    input_cte: &str,
    request_id: &CommandExecutionValue,
    group_key: &[CommandColumn],
    requested: &CommandColumn,
    available: &CommandColumn,
    allocated: &CommandColumn,
    backordered: &CommandColumn,
    groups: &[CommandColumn],
    lines: &[CommandColumn],
    backorders: &[CommandColumn],
    group_order_by: &[CommandColumn],
    line_order_by: &[CommandColumn],
    execution_gate: &str,
) -> String {
    let ranked_cte = format!("{cte}_ranked");
    let line_raw_cte = format!("{cte}_line_raw");
    let line_cte = format!("{cte}_lines");
    let backorder_cte = format!("{cte}_backorders");
    let group_raw_cte = format!("{cte}_group_raw");
    let group_cte = format!("{cte}_groups");
    let input_alias = "_cmd_input";
    let requested_sql = qualified(input_alias, &requested.name);
    let available_sql = qualified(input_alias, &available.name);
    let prior_available = format!(
        "coalesce(sum(greatest(({available})::numeric, 0::numeric)) OVER (PARTITION BY {line} ORDER BY {ordinal} ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING), 0::numeric)",
        available = available_sql,
        line = qualified(input_alias, "order_line_id"),
        ordinal = qualified(input_alias, "_cmd_ordinal"),
    );
    let allocated_sql = format!(
        "greatest(least(greatest(({available_sql})::numeric, 0::numeric), greatest(({requested_sql})::numeric - ({prior_available}), 0::numeric)), 0::numeric)::{}",
        quote_type_name(&allocated.pg_type),
    );
    let ranked = format!(
        "{ranked} AS MATERIALIZED (SELECT {input_alias}.*, {allocated_sql} AS {allocated} FROM {input} AS {input_alias} WHERE {execution_gate})",
        ranked = quote_ident(&ranked_cte),
        input_alias = quote_ident(input_alias),
        allocated = quote_ident(&allocated.name),
        input = quote_ident(input_cte),
    );
    let ranked_alias = "_cmd_ranked";
    let request_sql = command_value_sql(ctx, request_id);
    let group_json = format!(
        "jsonb_build_array({})",
        group_key
            .iter()
            .map(|column| format!("to_jsonb({})", qualified(ranked_alias, &column.name)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let digest = format!("md5((({request_sql})::text || ':' || ({group_json})::text))");
    let allocation_id = format!(
        "(substr({digest}, 1, 8) || '-' || substr({digest}, 9, 4) || '-' || substr({digest}, 13, 4) || '-' || substr({digest}, 17, 4) || '-' || substr({digest}, 21, 12))::uuid"
    );
    let mut line_projection = lines
        .iter()
        .map(|column| {
            let expression = if column.name == "allocation_id" {
                allocation_id.clone()
            } else {
                qualified(ranked_alias, &column.name)
            };
            format!("{expression} AS {}", quote_ident(&column.name))
        })
        .collect::<Vec<_>>();
    let group_aux = groups
        .iter()
        .filter(|column| {
            !lines.iter().any(|line| line.name == column.name)
                && !matches!(
                    column.name.as_str(),
                    "allocation_id" | "first_line_sequence" | "items"
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    line_projection.extend(group_aux.iter().map(|column| {
        format!(
            "{} AS {}",
            qualified(ranked_alias, &column.name),
            quote_ident(&column.name),
        )
    }));
    let line_raw = format!(
        "{line_raw} AS MATERIALIZED (SELECT {projection} FROM {ranked} AS {ranked_alias} WHERE {allocated}::numeric > 0::numeric)",
        line_raw = quote_ident(&line_raw_cte),
        projection = line_projection.join(", "),
        ranked = quote_ident(&ranked_cte),
        ranked_alias = quote_ident(ranked_alias),
        allocated = qualified(ranked_alias, &allocated.name),
    );
    let line_order = line_order_by
        .iter()
        .map(|column| qualified("_cmd_line", &column.name))
        .collect::<Vec<_>>();
    let mut line_columns = lines
        .iter()
        .map(|column| qualified("_cmd_line", &column.name))
        .collect::<Vec<_>>();
    line_columns.extend(
        group_aux
            .iter()
            .map(|column| qualified("_cmd_line", &column.name)),
    );
    let ordered_lines = format!(
        "{lines_cte} AS MATERIALIZED (SELECT row_number() OVER (ORDER BY {order})::bigint AS {ordinal}, {columns} FROM {line_raw} AS {alias} ORDER BY {order})",
        lines_cte = quote_ident(&line_cte),
        order = line_order.join(", "),
        ordinal = quote_ident("_cmd_ordinal"),
        columns = line_columns.join(", "),
        line_raw = quote_ident(&line_raw_cte),
        alias = quote_ident("_cmd_line"),
    );
    let backorder_projection = backorders
        .iter()
        .map(|column| {
            let expression = if column.name == backordered.name {
                format!(
                    "greatest(max({requested})::numeric - sum({allocated})::numeric, 0::numeric)::{}",
                    quote_type_name(&column.pg_type),
                    requested = qualified("_cmd_ranked", &requested.name),
                    allocated = qualified("_cmd_ranked", &allocated.name),
                )
            } else if column.name == requested.name {
                format!("max({})", qualified("_cmd_ranked", &requested.name))
            } else {
                format!(
                    "(array_agg({column} ORDER BY {ordinal}))[1]",
                    column = qualified("_cmd_ranked", &column.name),
                    ordinal = qualified("_cmd_ranked", "_cmd_ordinal"),
                )
            };
            format!("{expression} AS {}", quote_ident(&column.name))
        })
        .collect::<Vec<_>>();
    let backorder_order = backorders
        .iter()
        .find(|column| column.name == "order_line_id")
        .map(|column| quote_ident(&column.name))
        .unwrap_or_else(|| "1".to_owned());
    let backorder_rows = format!(
        "{backorders_cte} AS MATERIALIZED (SELECT row_number() OVER (ORDER BY {order_line})::bigint AS {ordinal}, {projection} FROM {ranked} AS {alias} GROUP BY {line} ORDER BY {order_line})",
        backorders_cte = quote_ident(&backorder_cte),
        order_line = backorder_order,
        ordinal = quote_ident("_cmd_ordinal"),
        projection = backorder_projection.join(", "),
        ranked = quote_ident(&ranked_cte),
        alias = quote_ident("_cmd_ranked"),
        line = qualified("_cmd_ranked", "order_line_id"),
    );
    let group_by = std::iter::once(qualified("_cmd_lines", "allocation_id"))
        .chain(
            group_key
                .iter()
                .map(|column| qualified("_cmd_lines", &column.name)),
        )
        .collect::<Vec<_>>();
    let line_json = command_row_json(ctx, lines, "_cmd_lines");
    let group_projection = groups
        .iter()
        .map(|column| {
            let expression = match column.name.as_str() {
                "allocation_id" => qualified("_cmd_lines", "allocation_id"),
                "first_line_sequence" => {
                    format!("min({})", qualified("_cmd_lines", "line_sequence"))
                }
                "items" => format!(
                    "jsonb_agg(({line_json})::jsonb ORDER BY {})",
                    qualified("_cmd_lines", "_cmd_ordinal")
                ),
                _ if group_key.iter().any(|key| key.name == column.name) => {
                    qualified("_cmd_lines", &column.name)
                }
                _ => format!(
                    "(array_agg({column} ORDER BY {ordinal}))[1]",
                    column = qualified("_cmd_lines", &column.name),
                    ordinal = qualified("_cmd_lines", "_cmd_ordinal"),
                ),
            };
            format!("{expression} AS {}", quote_ident(&column.name))
        })
        .collect::<Vec<_>>();
    let group_raw = format!(
        "{group_raw} AS MATERIALIZED (SELECT {projection} FROM {lines} AS {alias} GROUP BY {group_by})",
        group_raw = quote_ident(&group_raw_cte),
        projection = group_projection.join(", "),
        lines = quote_ident(&line_cte),
        alias = quote_ident("_cmd_lines"),
        group_by = group_by.join(", "),
    );
    let group_order = group_order_by
        .iter()
        .map(|column| qualified("_cmd_group", &column.name))
        .collect::<Vec<_>>();
    let group_columns = groups
        .iter()
        .map(|column| qualified("_cmd_group", &column.name))
        .collect::<Vec<_>>();
    let ordered_groups = format!(
        "{groups_cte} AS MATERIALIZED (SELECT row_number() OVER (ORDER BY {order})::bigint AS {ordinal}, {columns} FROM {group_raw} AS {alias} ORDER BY {order})",
        groups_cte = quote_ident(&group_cte),
        order = group_order.join(", "),
        ordinal = quote_ident("_cmd_ordinal"),
        columns = group_columns.join(", "),
        group_raw = quote_ident(&group_raw_cte),
        alias = quote_ident("_cmd_group"),
    );
    let summary = format!(
        "{cte} AS MATERIALIZED (SELECT (SELECT count(*) FROM {groups})::bigint AS {groups_count}, (SELECT count(*) FROM {lines})::bigint AS {lines_count}, (SELECT count(*) FROM {backorders})::bigint AS {backorders_count})",
        cte = quote_ident(cte),
        groups = quote_ident(&group_cte),
        lines = quote_ident(&line_cte),
        backorders = quote_ident(&backorder_cte),
        groups_count = quote_ident("_cmd_group_count"),
        lines_count = quote_ident("_cmd_line_count"),
        backorders_count = quote_ident("_cmd_backorder_count"),
    );
    [
        ranked,
        line_raw,
        ordered_lines,
        backorder_rows,
        group_raw,
        ordered_groups,
        summary,
    ]
    .join(", ")
}

fn command_named_values_sql(
    ctx: &mut Ctx,
    values: &[CommandNamedValue],
    item_alias: &str,
    current_alias: &str,
) -> String {
    values
        .iter()
        .map(|value| {
            format!(
                "{} AS {}",
                command_value_sql_scoped(ctx, &value.value, item_alias, current_alias),
                quote_ident(&value.name),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[allow(clippy::too_many_arguments)]
fn command_decision_ctes(
    ctx: &mut Ctx,
    cte: &str,
    input_cte: Option<&str>,
    decision: &CommandDecision,
    input: &[CommandNamedValue],
    returning: &[CommandColumn],
    order_by: &[CommandColumn],
    execution_gate: &str,
) -> String {
    let current_alias = if input_cte.is_some() {
        "_cmd_input"
    } else {
        "_cmd_target"
    };
    let input_projection = command_named_values_sql(ctx, input, "_cmd_input", current_alias);
    let source = match input_cte {
        Some(input_cte) => format!(
            "{input} AS {input_alias} CROSS JOIN LATERAL (SELECT {projection}) AS {decision_input}",
            input = quote_ident(input_cte),
            input_alias = quote_ident("_cmd_input"),
            projection = input_projection,
            decision_input = quote_ident("_cmd_decision_input"),
        ),
        None => format!(
            "(SELECT {projection}) AS {decision_input}",
            projection = input_projection,
            decision_input = quote_ident("_cmd_decision_input"),
        ),
    };
    let output_names = decision
        .rows
        .first()
        .map(|row| {
            row.output
                .iter()
                .map(|output| output.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let decision_rows = decision
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let output = output_names
                .iter()
                .map(|name| {
                    row.output
                        .iter()
                        .find(|output| output.name == *name)
                        .map(|output| {
                            format!(
                                "({})::{} AS {}",
                                output.sql,
                                quote_type_name(&output.column.pg_type),
                                quote_ident(name),
                            )
                        })
                        .unwrap_or_else(|| format!("NULL AS {}", quote_ident(name)))
                })
                .collect::<Vec<_>>();
            let mut projection = vec![
                format!(
                    "{}::bigint AS {}",
                    index + 1,
                    quote_ident("_cmd_decision_ordinal")
                ),
                format!(
                    "{}::text AS {}",
                    quote_lit(&row.id),
                    quote_ident("_cmd_decision_row_id")
                ),
            ];
            projection.extend(output);
            format!(
                "SELECT {projection} WHERE ({condition}) IS TRUE",
                projection = projection.join(", "),
                condition = row.condition_sql,
            )
        })
        .collect::<Vec<_>>();
    let matches_cte = format!("{cte}_matches");
    let mut matches_projection = Vec::new();
    if input_cte.is_some() {
        matches_projection.push(format!(
            "{} AS {}",
            qualified("_cmd_input", "_cmd_ordinal"),
            quote_ident("_cmd_input_ordinal"),
        ));
    }
    matches_projection.push(qualified("_cmd_decision_row", "_cmd_decision_ordinal"));
    matches_projection.push(qualified("_cmd_decision_row", "_cmd_decision_row_id"));
    matches_projection.extend(returning.iter().map(|column| {
        let source_alias = if output_names.iter().any(|name| name == &column.name) {
            "_cmd_decision_row"
        } else {
            "_cmd_input"
        };
        format!(
            "{} AS {}",
            qualified(source_alias, &column.name),
            quote_ident(&column.name),
        )
    }));
    let matches = format!(
        "{matches} AS MATERIALIZED (SELECT {projection} FROM {source} CROSS JOIN LATERAL ({rows}) AS {row_alias} WHERE {execution_gate})",
        matches = quote_ident(&matches_cte),
        projection = matches_projection.join(", "),
        rows = decision_rows.join(" UNION ALL "),
        row_alias = quote_ident("_cmd_decision_row"),
    );
    let selected = if input_cte.is_none() {
        let projection = returning
            .iter()
            .map(|column| qualified("_cmd_matches", &column.name))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{cte} AS (SELECT {projection} FROM {matches} AS {alias} ORDER BY {alias}.{decision_ordinal} LIMIT 1)",
            cte = quote_ident(cte),
            matches = quote_ident(&matches_cte),
            alias = quote_ident("_cmd_matches"),
            decision_ordinal = quote_ident("_cmd_decision_ordinal"),
        )
    } else {
        let chosen = "_cmd_chosen";
        let chosen_projection = returning
            .iter()
            .map(|column| qualified(chosen, &column.name))
            .collect::<Vec<_>>();
        let order = order_by
            .iter()
            .map(|column| qualified(chosen, &column.name))
            .collect::<Vec<_>>();
        format!(
            "{cte} AS MATERIALIZED (SELECT row_number() OVER (ORDER BY {order})::bigint AS {ordinal}, {projection} FROM (SELECT {matches_alias}.*, row_number() OVER (PARTITION BY {input_ordinal} ORDER BY {decision_ordinal}) AS {choice} FROM {matches} AS {matches_alias}) AS {chosen} WHERE {chosen}.{choice} = 1 ORDER BY {order})",
            cte = quote_ident(cte),
            order = order.join(", "),
            ordinal = quote_ident("_cmd_ordinal"),
            projection = chosen_projection.join(", "),
            matches_alias = quote_ident("_cmd_matches"),
            input_ordinal = qualified("_cmd_matches", "_cmd_input_ordinal"),
            decision_ordinal = qualified("_cmd_matches", "_cmd_decision_ordinal"),
            choice = quote_ident("_cmd_choice"),
            matches = quote_ident(&matches_cte),
            chosen = quote_ident(chosen),
        )
    };
    format!("{matches}, {selected}")
}

fn command_aggregate_sql(aggregate: &CommandAggregateIr, input_alias: &str) -> String {
    let (output, expression) = match aggregate {
        CommandAggregateIr::Count { output } => (output, "count(*)".to_owned()),
        CommandAggregateIr::Sum { output, input } => (
            output,
            format!("sum({})", qualified(input_alias, &input.name)),
        ),
        CommandAggregateIr::Min { output, input } => (
            output,
            format!("min({})", qualified(input_alias, &input.name)),
        ),
        CommandAggregateIr::Max { output, input } => (
            output,
            format!("max({})", qualified(input_alias, &input.name)),
        ),
        CommandAggregateIr::CountDistinct { output, input } => (
            output,
            format!("count(DISTINCT {})", qualified(input_alias, &input.name)),
        ),
    };
    format!(
        "({expression})::{} AS {}",
        quote_type_name(&output.pg_type),
        quote_ident(&output.name),
    )
}

fn command_table_sql(table: &Table) -> String {
    format!(
        "{}.{}",
        quote_ident(&table.schema),
        quote_ident(&table.name)
    )
}

fn command_returning_columns(columns: &[CommandColumn], alias: &str) -> String {
    if columns.is_empty() {
        "1 AS \"_cmd_exists\"".to_owned()
    } else {
        columns
            .iter()
            .map(|column| qualified(alias, &column.name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn command_predicates(
    ctx: &mut Ctx,
    assignments: &[CommandAssignment],
    alias: &str,
) -> Vec<String> {
    assignments
        .iter()
        .map(|assignment| {
            format!(
                "{} = {}",
                qualified(alias, &assignment.column.name),
                command_value_sql(ctx, &assignment.value)
            )
        })
        .collect()
}

fn command_value_sql(ctx: &mut Ctx, value: &CommandExecutionValue) -> String {
    command_value_sql_scoped(ctx, value, "_cmd_input", "_cmd_target")
}

fn command_value_sql_scoped(
    ctx: &mut Ctx,
    value: &CommandExecutionValue,
    item_alias: &str,
    current_alias: &str,
) -> String {
    match value {
        CommandExecutionValue::Scalar { value, pg_type } => {
            scalar_sql(&ctx.dialect, value, pg_type)
        }
        CommandExecutionValue::StepColumn { cte, column } => format!(
            "(SELECT {} FROM {} LIMIT 1)",
            quote_ident(&column.name),
            quote_ident(cte),
        ),
        CommandExecutionValue::StepRows { cte, columns } => {
            let row = command_row_json(ctx, columns, cte);
            let order = qualified(cte, "_cmd_ordinal");
            format!(
                "(SELECT {} FROM {})",
                json_array_agg(&ctx.dialect, &row, Some(&order)),
                quote_ident(cte),
            )
        }
        CommandExecutionValue::StepFieldRows {
            cte,
            field,
            columns,
            where_nonzero,
        } => {
            let source = format!("{cte}_{field}");
            let row = command_row_json(ctx, columns, &source);
            let order = qualified(&source, "_cmd_ordinal");
            let predicate = where_nonzero
                .as_ref()
                .map(|column| {
                    format!(
                        " WHERE coalesce({}::numeric, 0::numeric) <> 0::numeric",
                        qualified(&source, column)
                    )
                })
                .unwrap_or_default();
            format!(
                "(SELECT {aggregate} FROM {source}{predicate})",
                aggregate = json_array_agg(&ctx.dialect, &row, Some(&order)),
                source = quote_ident(&source),
            )
        }
        CommandExecutionValue::Item { field, .. } => qualified(item_alias, field),
        CommandExecutionValue::CurrentColumn { column } => qualified(current_alias, &column.name),
        CommandExecutionValue::Rule { sql, pg_type } => {
            format!("({sql})::{}", quote_type_name(pg_type))
        }
        CommandExecutionValue::DatabaseTime {
            function: CommandDatabaseTime::Now,
            pg_type,
        } => format!("statement_timestamp()::{}", quote_type_name(pg_type)),
    }
}

fn command_update_many_join_predicates(
    ctx: &mut Ctx,
    primary_key: &[CommandAssignment],
    target_alias: &str,
    input_alias: &str,
) -> Vec<String> {
    primary_key
        .iter()
        .map(|assignment| {
            format!(
                "{} = {}",
                qualified(target_alias, &assignment.column.name),
                command_value_sql_scoped(ctx, &assignment.value, input_alias, target_alias),
            )
        })
        .collect()
}

fn command_value_sql_for_item(
    ctx: &mut Ctx,
    value: &CommandExecutionValue,
    item: &serde_json::Map<String, serde_json::Value>,
) -> String {
    match value {
        CommandExecutionValue::Item { field, pg_type } => {
            let value = item.get(field).cloned().unwrap_or(serde_json::Value::Null);
            scalar_sql(&ctx.dialect, &Scalar::Json(value), pg_type)
        }
        _ => command_value_sql(ctx, value),
    }
}

fn command_item_source_sql(
    ctx: &mut Ctx,
    item_fields: &[CommandColumn],
    item: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let values = item_fields
        .iter()
        .map(|field| {
            let value = item
                .get(&field.name)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            format!(
                "{} AS {}",
                scalar_sql(&ctx.dialect, &Scalar::Json(value), &field.pg_type),
                quote_ident(&field.name),
            )
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        "SELECT 1 AS \"_cmd_item_present\"".to_owned()
    } else {
        format!("SELECT {}", values.join(", "))
    }
}

fn command_full_result_json(ctx: &mut Ctx, command: &CommandMutation) -> String {
    let pairs = command
        .result
        .iter()
        .map(|field| {
            (
                field.name.clone(),
                command_result_value_sql(ctx, &field.value),
            )
        })
        .collect::<Vec<_>>();
    json_object(&ctx.dialect, &pairs)
}

fn command_result_value_sql(ctx: &mut Ctx, value: &CommandResultValue) -> String {
    match value {
        CommandResultValue::StepRow { cte, many, columns } => {
            let row = command_row_json(ctx, columns, cte);
            if *many {
                // Every row-set renderer exposes this private ordinal alongside
                // the declared columns, so neither SELECT nor RETURNING plan
                // order can leak into the public JSON contract.
                let order = qualified(cte, "_cmd_ordinal");
                format!(
                    "(SELECT {} FROM {})",
                    json_array_agg(&ctx.dialect, &row, Some(&order)),
                    quote_ident(cte),
                )
            } else {
                format!("(SELECT {row} FROM {} LIMIT 1)", quote_ident(cte))
            }
        }
        CommandResultValue::StepColumn { cte, column } => format!(
            "(SELECT {} FROM {} LIMIT 1)",
            ctx.column_output(cte, &column.name, &column.logical_type),
            quote_ident(cte),
        ),
        CommandResultValue::Scalar { value, pg_type } => scalar_sql(&ctx.dialect, value, pg_type),
        CommandResultValue::Rule { sql, pg_type } => {
            format!("({sql})::{}", quote_type_name(pg_type))
        }
        CommandResultValue::ProjectedRows {
            cte,
            many,
            columns,
            maximum_items,
        } => {
            let pairs = columns
                .iter()
                .map(|projection| {
                    (
                        projection.name.clone(),
                        ctx.column_output(
                            cte,
                            &projection.source.name,
                            &projection.source.logical_type,
                        ),
                    )
                })
                .collect::<Vec<_>>();
            let row = json_object(&ctx.dialect, &pairs);
            let order = if *many {
                Some(qualified(cte, "_cmd_ordinal"))
            } else {
                None
            };
            let value = format!(
                "(SELECT {} FROM {})",
                json_array_agg(&ctx.dialect, &row, order.as_deref()),
                quote_ident(cte),
            );
            format!(
                "CASE WHEN (SELECT count(*) FROM {cte}) > {maximum_items} THEN donat.raise_graphql_error('validation-failed', '$', 'command result exceeded maximum_items')::jsonb ELSE ({value})::jsonb END",
                cte = quote_ident(cte),
            )
        }
        CommandResultValue::Array {
            value,
            maximum_items,
        } => {
            let json = command_jsonb_literal(value);
            format!(
                "CASE WHEN jsonb_array_length({json}) > {maximum_items} THEN donat.raise_graphql_error('validation-failed', '$', 'command result exceeded maximum_items')::jsonb ELSE {json} END"
            )
        }
    }
}

fn command_row_json(ctx: &mut Ctx, columns: &[CommandColumn], alias: &str) -> String {
    let pairs = columns
        .iter()
        .map(|column| {
            (
                column.name.clone(),
                ctx.column_output(alias, &column.name, &column.logical_type),
            )
        })
        .collect::<Vec<_>>();
    json_object(&ctx.dialect, &pairs)
}

fn command_rejections(
    ctx: &mut Ctx,
    command: &CommandMutation,
    execution_gate: &str,
    invocation: Option<&CommandInvocationSql>,
    result: String,
) -> String {
    let mut guarded = result;
    for rejection in command_rejection_checks(ctx, command).into_iter().rev() {
        guarded = match rejection {
            CommandRejection::Business {
                condition,
                path,
                message,
            } => format!(
                "CASE WHEN ({execution_gate}) AND ({condition}) THEN donat.raise_graphql_error('validation-failed', {path}, {message}) ELSE {guarded} END",
                path = quote_lit(&path),
                message = quote_lit(&message),
            ),
            CommandRejection::Permission { condition, path } => {
                let payload = serde_json::json!({
                    "path": path,
                    "message": "check constraint of an insert/update permission has failed",
                })
                .to_string();
                format!(
                    "CASE WHEN ({execution_gate}) AND ({condition}) THEN donat.check_violation({})::jsonb ELSE {guarded} END",
                    quote_lit(&payload),
                )
            }
        };
    }
    if let Some(invocation) = invocation {
        guarded = format!(
            "CASE WHEN NOT EXISTS (SELECT 1 FROM {cte} WHERE {fingerprint} = {input_hash}) THEN donat.raise_graphql_error('validation-failed', {path}, 'idempotency key was reused with different input') ELSE {guarded} END",
            cte = quote_ident("_cmd_invocation"),
            fingerprint = quote_ident("input_fingerprint"),
            input_hash = invocation.input_hash,
            path = quote_lit(&invocation.error_path),
        );
    }
    guarded
}

enum CommandRejection {
    Business {
        condition: String,
        path: String,
        message: String,
    },
    Permission {
        condition: String,
        path: String,
    },
}

/// The table and CTE of every command step that writes rows before `index`.
///
/// A command is one statement, so a row a later step references was created in
/// a data-modifying CTE and is not visible to a table read in the same
/// statement. A permission check that traverses a relationship to such a row
/// must therefore resolve it against that CTE, exactly as the nested-insert
/// path already does for GraphQL mutations.
fn command_written_tables(
    command: &CommandMutation,
    index: usize,
) -> Vec<(&Table, &str, Vec<&str>)> {
    command.steps[..index]
        .iter()
        .filter_map(|step| match step {
            CommandExecutionStep::Insert {
                cte,
                table,
                returning,
                ..
            }
            | CommandExecutionStep::InsertMany {
                cte,
                table,
                returning,
                ..
            }
            | CommandExecutionStep::InsertRows {
                cte,
                table,
                returning,
                ..
            }
            | CommandExecutionStep::InsertWhen {
                cte,
                table,
                returning,
                ..
            }
            | CommandExecutionStep::Update {
                cte,
                table,
                returning,
                ..
            }
            | CommandExecutionStep::UpdateWhen {
                cte,
                table,
                returning,
                ..
            } => Some((
                table,
                cte.as_str(),
                returning
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

/// Build one override per relationship in `check` whose target row this
/// statement is still creating. Matching on the expression's own join keeps the
/// override exact.
fn command_relationship_ctes(
    check: &BoolExp,
    written: &[(&Table, &str, Vec<&str>)],
) -> Vec<RelationshipCteOverride> {
    let mut overrides = Vec::new();
    collect_command_relationship_ctes(check, written, &mut overrides);
    overrides
}

fn collect_command_relationship_ctes(
    exp: &BoolExp,
    written: &[(&Table, &str, Vec<&str>)],
    overrides: &mut Vec<RelationshipCteOverride>,
) {
    match exp {
        BoolExp::And(exps) | BoolExp::Or(exps) => {
            for exp in exps {
                collect_command_relationship_ctes(exp, written, overrides);
            }
        }
        BoolExp::Not(exp) => collect_command_relationship_ctes(exp, written, overrides),
        BoolExp::Relationship {
            table,
            join,
            predicate,
        } => {
            // Only redirect when that step actually returns the columns the
            // join reads; otherwise the committed table remains the only place
            // the relationship can be resolved.
            if let Some((_, cte, _)) = written.iter().find(|(written, _, returning)| {
                *written == table
                    && join
                        .iter()
                        .all(|(_, remote)| returning.contains(&remote.as_str()))
            }) {
                overrides.push(RelationshipCteOverride {
                    table: table.clone(),
                    join: join.clone(),
                    cte: (*cte).to_owned(),
                });
            }
            collect_command_relationship_ctes(predicate, written, overrides);
        }
        _ => {}
    }
}

fn command_rejection_checks(ctx: &mut Ctx, command: &CommandMutation) -> Vec<CommandRejection> {
    let mut checks = Vec::new();
    for (index, step) in command.steps.iter().enumerate() {
        match step {
            CommandExecutionStep::Assert { .. } => {}
            CommandExecutionStep::SelectOne {
                name,
                cte,
                require_found,
                error_path,
                ..
            } if *require_found => checks.push(CommandRejection::Business {
                condition: format!("(SELECT count(*) FROM {}) = 0", quote_ident(cte)),
                path: error_path.clone(),
                message: format!("command select_one step '{name}' did not find a row"),
            }),
            CommandExecutionStep::Insert {
                cte,
                check,
                error_path,
                ..
            }
            | CommandExecutionStep::InsertMany {
                cte,
                check,
                error_path,
                ..
            }
            | CommandExecutionStep::InsertRows {
                cte,
                check,
                error_path,
                ..
            }
            | CommandExecutionStep::InsertWhen {
                cte,
                check,
                error_path,
                ..
            }
            | CommandExecutionStep::Update {
                cte,
                check,
                error_path,
                ..
            }
            | CommandExecutionStep::UpdateWhen {
                cte,
                check,
                error_path,
                ..
            } => {
                if let Some(check) = check {
                    let overrides =
                        command_relationship_ctes(check, &command_written_tables(command, index));
                    checks.push(CommandRejection::Permission {
                        condition: format!(
                            "(SELECT count(*) FROM {} WHERE ({}) IS NOT TRUE) > 0",
                            quote_ident(cte),
                            ctx.bool_exp_with_relationship_ctes(check, cte, cte, &overrides),
                        ),
                        path: error_path.clone(),
                    });
                }
                if let CommandExecutionStep::InsertMany {
                    allow_empty: false,
                    name,
                    ..
                } = step
                {
                    checks.push(CommandRejection::Business {
                        condition: format!("(SELECT count(*) FROM {}) = 0", quote_ident(cte)),
                        path: error_path.clone(),
                        message: format!(
                            "command insert_many step '{name}' requires at least one item"
                        ),
                    });
                }
                if let CommandExecutionStep::InsertRows {
                    allow_empty: false,
                    name,
                    ..
                } = step
                {
                    checks.push(CommandRejection::Business {
                        condition: format!("(SELECT count(*) FROM {}) = 0", quote_ident(cte)),
                        path: error_path.clone(),
                        message: format!(
                            "command insert_many step '{name}' requires at least one item"
                        ),
                    });
                }
                if let CommandExecutionStep::Update {
                    require_affected: true,
                    name,
                    ..
                } = step
                {
                    checks.push(CommandRejection::Business {
                        condition: format!("(SELECT count(*) FROM {}) = 0", quote_ident(cte)),
                        path: error_path.clone(),
                        message: format!("command update step '{name}' did not affect a row"),
                    });
                }
                if let CommandExecutionStep::UpdateWhen {
                    require_affected: true,
                    name,
                    condition,
                    ..
                } = step
                {
                    checks.push(CommandRejection::Business {
                        condition: format!(
                            "({}) IS TRUE AND (SELECT count(*) FROM {}) = 0",
                            command_condition_sql(condition),
                            quote_ident(cte)
                        ),
                        path: error_path.clone(),
                        message: format!("command update_when step '{name}' did not affect a row"),
                    });
                }
            }
            CommandExecutionStep::Delete {
                cte,
                require_affected: true,
                error_path,
                name,
                ..
            } => checks.push(CommandRejection::Business {
                condition: format!("(SELECT count(*) FROM {}) = 0", quote_ident(cte)),
                path: error_path.clone(),
                message: format!("command delete step '{name}' did not affect a row"),
            }),
            _ => {}
        }
    }
    checks
}

fn command_project_result(
    ctx: &mut Ctx,
    result: &str,
    selection: &[CommandResultSelection],
) -> String {
    if selection.is_empty() {
        // GraphQL normally requires a command selection set, but retaining a
        // dependency here makes every command rejection and journal update
        // observable even for a deliberately minimal IR fixture.
        return format!(
            "CASE WHEN ({result}) IS NULL THEN json_build_object() ELSE json_build_object() END"
        );
    }
    let pairs = selection
        .iter()
        .map(|selected| command_project_selection(ctx, result, selected))
        .collect::<Vec<_>>();
    json_object(&ctx.dialect, &pairs)
}

fn command_project_selection(
    ctx: &mut Ctx,
    result: &str,
    selection: &CommandResultSelection,
) -> (String, String) {
    match selection {
        CommandResultSelection::Scalar { alias, field } => {
            (alias.clone(), format!("({result} -> {})", quote_lit(field)))
        }
        CommandResultSelection::Object {
            alias,
            field,
            selections,
        } => {
            let value = format!("({result} -> {})", quote_lit(field));
            (
                alias.clone(),
                command_project_object(ctx, &value, selections),
            )
        }
        CommandResultSelection::List {
            alias,
            field,
            selections,
        } => {
            let value = format!("({result} -> {})", quote_lit(field));
            (alias.clone(), command_project_list(ctx, &value, selections))
        }
        CommandResultSelection::Typename { alias, value } => {
            (alias.clone(), typename_literal(&ctx.dialect, value))
        }
    }
}

fn command_project_object(
    ctx: &mut Ctx,
    value: &str,
    selections: &[CommandResultSelection],
) -> String {
    let projected = command_project_result(ctx, value, selections);
    format!(
        "CASE WHEN {value} IS NULL OR {value} = 'null'::jsonb THEN 'null'::jsonb ELSE ({projected})::jsonb END"
    )
}

fn command_project_list(
    ctx: &mut Ctx,
    value: &str,
    selections: &[CommandResultSelection],
) -> String {
    let alias = ctx.alias();
    let item = qualified(&alias, "value");
    let projected = command_project_object(ctx, &item, selections);
    format!(
        "(SELECT coalesce(jsonb_agg({projected}), '[]'::jsonb) FROM jsonb_array_elements(CASE WHEN jsonb_typeof({value}) = 'array' THEN {value} ELSE '[]'::jsonb END) AS {alias}(value))",
        alias = quote_ident(&alias),
    )
}

fn command_jsonb_literal(value: &Scalar) -> String {
    format!("({})::jsonb", quote_lit(&value.as_json().to_string()))
}

fn command_canonical_json_array(ctx: &mut Ctx, values: &[CommandExecutionValue]) -> String {
    let values = values
        .iter()
        .map(|value| format!("to_jsonb({})", command_value_sql(ctx, value)))
        .collect::<Vec<_>>();
    format!("jsonb_build_array({})", values.join(", "))
}

fn command_hash(json: &str) -> String {
    format!("decode(md5(({json})::text), 'hex')")
}

fn command_key_text(value: &Scalar) -> String {
    format!("({} #>> '{{}}')", command_jsonb_literal(value))
}

fn command_identity_text(identity: &CommandIdentity) -> String {
    fn component(value: &str) -> String {
        format!("{}:{value}", value.len())
    }

    format!(
        "v1:{}{}{}",
        component(&identity.source),
        component(&identity.name),
        component(&identity.role),
    )
}

fn command_legacy_identity_text(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut identity = String::with_capacity("legacy-unqualified:".len() + name.len() * 2);
    identity.push_str("legacy-unqualified:");
    for byte in name.bytes() {
        identity.push(char::from(HEX[usize::from(byte >> 4)]));
        identity.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    identity
}

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
        MutationRoot::Command { command, .. } => command_to_sql(&mut ctx, command),
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
            ctx.mutation_select(MutationSelectOptions {
                cte: INSERT_ROW_ALIAS,
                dml: &stmt,
                check: insert.check.as_ref(),
                check_path: &insert.check_path,
                extra_ctes,
                extra_checks,
                validators: &insert.validators,
                output: &insert.output,
            })
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
            ctx.mutation_select(MutationSelectOptions {
                cte: UPDATE_ROW_ALIAS,
                dml: &stmt,
                check: update.check.as_ref(),
                check_path: &update.check_path,
                extra_ctes: vec![],
                extra_checks: vec![],
                validators: &update.validators,
                output: &update.output,
            })
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
            ctx.mutation_select(MutationSelectOptions {
                cte: "del",
                dml: &stmt,
                check: None,
                check_path: "$",
                validators: &[],
                extra_ctes: vec![],
                extra_checks: vec![],
                output: &delete.output,
            })
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
        MutationRoot::Command { .. } => {
            panic!("command mutations are Postgres-only and have no SQLite renderer")
        }
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
        MutationRoot::Command { .. } => {
            panic!("command mutations are Postgres-only and have no MySQL renderer")
        }
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
    fn mutation_select(&mut self, options: MutationSelectOptions<'_>) -> String {
        let MutationSelectOptions {
            cte,
            dml,
            check,
            check_path,
            validators,
            extra_ctes,
            extra_checks,
            output,
        } = options;
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
        // Validators are wrapped first, so they end up innermost: a role that
        // may not write the row at all is told that, and is never handed a
        // message about a value it was not allowed to submit. Within the list
        // the wrap order is reversed, which leaves the first declared entry
        // outermost — document order is the reported order.
        for validator in validators.iter().rev() {
            let violated = format!(
                "(SELECT count(*) FROM {cte_ident} WHERE ({}) IS NOT TRUE)",
                validator.sql,
            );
            guarded = format!(
                "CASE WHEN {violated} > 0 THEN donat.raise_graphql_error('validation-failed', {path}, {message})::json ELSE {guarded} END",
                path = quote_lit(&validator.error_path),
                message = quote_lit(&validator.message),
            );
        }
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

/// Render a SQL type name that may be schema-qualified.
///
/// Catalog introspection reports a domain as `schema.name`. Quoting that whole
/// string as one identifier asks PostgreSQL for a type literally named
/// `"public.petshop_required_int8"`, which does not exist, so every cast
/// against a domain-typed column fails at execution time. Built-in names carry
/// no qualifier and are unchanged.
pub fn quote_type_name(pg_type: &str) -> String {
    donat_backend::PostgresDialect.quote_type_name(pg_type)
}

pub fn quote_lit(s: &str) -> String {
    use donat_backend::Dialect;
    donat_backend::PostgresDialect.quote_literal(s)
}

/// The CTE that holds the rows an INSERT wrote. Rule lowering happens in the
/// planner, before SQLgen picks a name, so the name is part of the contract
/// between them rather than a local choice here.
pub const INSERT_ROW_ALIAS: &str = "ins";

/// The CTE that holds the rows an UPDATE wrote.
pub const UPDATE_ROW_ALIAS: &str = "upd";

/// Render a qualified SQL column reference for the declarative rule lowerer.
///
/// The rule crate intentionally receives no raw identifier rendering API: both
/// components are quoted here before they can become a SQL fragment.
pub fn rule_qualified_column(alias: &str, column: &str) -> String {
    format!("{}.{}", quote_ident(alias), quote_ident(column))
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

    /// The gate a permission validator renders, and the order it renders in.
    ///
    /// Everything asserted here was previously pinned only by the conformance
    /// suite: a regression in the wrap order, the three-valued gate, the cast
    /// or the literal escaping would have been invisible to this crate.
    #[test]
    fn permission_validators_render_inside_the_check_in_document_order() {
        let insert = MutationRoot::Insert {
            alias: "insert_note".into(),
            insert: InsertMutation {
                table: Table {
                    schema: "public".into(),
                    name: "note".into(),
                },
                columns: vec![("body".into(), "text".into())],
                rows: vec![vec![Some(Scalar::Json(serde_json::json!("hi")))]],
                nested_object_inserts: vec![],
                on_conflict: None,
                check: Some(BoolExp::Compare {
                    column: "author_id".into(),
                    pg_type: "text".into(),
                    op: CompareOp::Eq(Scalar::Json(serde_json::json!("u1"))),
                }),
                check_path: "$.selectionSet.insert_note.args.objects".into(),
                validators: vec![
                    RowValidator {
                        sql: r#"length("ins"."body") >= 3"#.into(),
                        message: "body is too short".into(),
                        error_path: "$.selectionSet.insert_note.args.objects".into(),
                    },
                    RowValidator {
                        sql: r#"length("ins"."body") <= 400"#.into(),
                        // An apostrophe must survive as a doubled quote, not
                        // as a way out of the string literal.
                        message: "body can't exceed 400 characters".into(),
                        error_path: "$.selectionSet.insert_note.args.objects".into(),
                    },
                ],
                output: MutationOutput::Response(vec![MutationResponseField::AffectedRows {
                    alias: "affected_rows".into(),
                }]),
            },
        };
        let sql = mutation_to_sql(&insert);

        // Three-valued: a validator passes only on TRUE, so an unknown value
        // is a violation.
        assert!(
            sql.contains(r#"WHERE (length("ins"."body") >= 3) IS NOT TRUE"#),
            "{sql}"
        );
        // jsonb from the error helper must be cast to match the json the rest
        // of the expression produces.
        assert!(
            sql.contains("donat.raise_graphql_error('validation-failed'"),
            "{sql}"
        );
        assert!(sql.contains(")::json ELSE"), "{sql}");
        assert!(sql.contains("'body can''t exceed 400 characters'"), "{sql}");

        // The permission check is outermost, so a role that may not write the
        // row is never told about the value. Among validators the first
        // declared entry is outermost, so document order is reported order.
        let check_at = sql.find("donat.check_violation").expect("check gate");
        let first_at = sql.find("'body is too short'").expect("first validator");
        let second_at = sql
            .find("'body can''t exceed 400 characters'")
            .expect("second validator");
        assert!(
            check_at < first_at && first_at < second_at,
            "check must wrap the validators, and validators keep document order: {sql}"
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
                validators: vec![],
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
                validators: vec![],
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
