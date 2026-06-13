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

fn operation_to_sql_full(
    roots: &[RootField],
    stringify_numerics: bool,
    dialect: donat_backend::AnyDialect,
) -> String {
    let mut ctx = Ctx { next_alias: 0, stringify_numerics, dialect };
    let pairs: Vec<(String, String)> = roots
        .iter()
        .map(|r| match r {
            RootField::Select { alias, query } => {
                (alias.clone(), ctx.select_expr(query, None))
            }
            RootField::Connection { alias, conn } => {
                (alias.clone(), ctx.connection_expr(conn, None))
            }
            RootField::Typename { alias, value } => {
                (alias.clone(), format!("{}::text", quote_lit(value)))
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
        let mut parts = vec![format!(
            "'[1, \"{schema}\", \"{table}\"'"
        )];
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
            if !q.order_by.iter().any(
                |ob| matches!(&ob.target, OrderByTarget::Column(c) if c == col),
            ) {
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
        let tail = self.from_where_order(&q, &alias, outer);

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
                    (alias.clone(), format!("{}::text", quote_lit(value)))
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
                                (alias.clone(), format!("{}::text", quote_lit(value)))
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
        let edge_obj = json_object(&dialect, &[
            ("cursor".to_string(), format!("{ed}.c", ed = quote_ident(&format!("{arr}_e")))),
            ("node".to_string(), format!("{ed}.n", ed = quote_ident(&format!("{arr}_e")))),
        ]);
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
        if q.fields
            .iter()
            .any(|f| matches!(f.value, FieldValue::Aggregate { .. } | FieldValue::Nodes { .. }))
        {
            return self.aggregate_expr(q, outer);
        }

        let alias = self.alias();
        let row_json = self.row_json(&q.fields, &alias);
        let tail = self.from_where_order(q, &alias, outer);
        let distinct = distinct_clause(q, &alias);

        if q.single {
            format!("(SELECT {distinct}{row_json} {tail} LIMIT 1)")
        } else {
            let elem = self.alias();
            let e = quote_ident(&elem);
            format!(
                "(SELECT {agg} FROM (SELECT {distinct}{row_json} AS j {tail}) AS {e})",
                agg = json_array_agg(&self.dialect, &format!("{e}.j"), None),
            )
        }
    }

    /// `<t>_aggregate` (root or relationship): aggregate + nodes over one
    /// filtered row set.
    fn aggregate_expr(&mut self, q: &SelectQuery, outer: Option<OuterJoin>) -> String {
        let dialect = self.dialect;
        let inner_alias = self.alias();
        let tail = self.from_where_order(q, &inner_alias, outer);
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
                    FieldValue::Typename { value } => to_json_text(&dialect, &quote_lit(value)),
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
                    AggregateOp::Count { distinct, columns } => {
                        if columns.is_empty() {
                            "COUNT(*)".to_string()
                        } else {
                            let cols: Vec<String> = columns
                                .iter()
                                .map(|c| qualified(table_alias, c))
                                .collect();
                            let d = if *distinct { "DISTINCT " } else { "" };
                            // Multiple columns need a row constructor.
                            let expr = if cols.len() == 1 {
                                cols.join(", ")
                            } else {
                                format!("({})", cols.join(", "))
                            };
                            format!("COUNT({d}{expr})")
                        }
                    }
                    AggregateOp::ColumnOp { op, columns } => {
                        let inner: Vec<(String, String)> = columns
                            .iter()
                            .map(|c| {
                                let col = qualified(table_alias, &c.column);
                                let expr = match &c.guard {
                                    Some(guard) => {
                                        let cond =
                                            self.bool_exp(guard, table_alias, table_alias);
                                        format!("CASE WHEN {cond} THEN {col} ELSE NULL END")
                                    }
                                    None => col,
                                };
                                (c.alias.clone(), format!("{op}({expr})"))
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
    ) -> String {
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

        if !q.order_by.is_empty() {
            let items: Vec<String> = q
                .order_by
                .iter()
                .map(|ob| {
                    let target = match &ob.target {
                        OrderByTarget::Column(c) => qualified(alias, c),
                        OrderByTarget::Relationship { table, join, column, predicate } => {
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
                    let nulls = match ob.nulls {
                        NullsOrder::First => "NULLS FIRST",
                        NullsOrder::Last => "NULLS LAST",
                    };
                    format!("{target} {dir} {nulls}")
                })
                .collect();
            sql.push_str(&format!(" ORDER BY {}", items.join(", ")));
        }

        use donat_backend::Dialect;
        sql.push_str(&donat_backend::PostgresDialect.limit_offset(q.limit, q.offset));
        sql
    }

    fn row_json(&mut self, fields: &[OutputField], table_alias: &str) -> String {
        let dialect = self.dialect;
        let pairs: Vec<(String, String)> = fields
            .iter()
            .map(|f| {
                let value = match &f.value {
                    FieldValue::ColumnGuarded { column, pg_type, guard } => {
                        let col = self.column_output(table_alias, column, pg_type);
                        let cond = self.bool_exp(guard, table_alias, table_alias);
                        format!("CASE WHEN {cond} THEN {col} ELSE NULL END")
                    }
                    FieldValue::Column { column, pg_type } => {
                        let col = qualified(table_alias, column);
                        match pg_type.as_str() {
                            // Donat renders geometry as GeoJSON with the
                            // long CRS form (options bit 4).
                            "geometry" | "geography" => {
                                format!("ST_AsGeoJSON({col}, 15, 4)::json")
                            }
                            "int8" | "numeric" if self.stringify_numerics => {
                                format!("({col})::text")
                            }
                            _ => col,
                        }
                    }
                    FieldValue::Typename { value } => format!("{}::text", quote_lit(value)),
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
                    FieldValue::RemoteJoin { .. } => "NULL::json".to_string(),
                    FieldValue::ComputedScalar { schema, name, args, guard } => {
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
                                let cond =
                                    self.bool_exp(guard, table_alias, table_alias);
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
        match pg_type {
            "geometry" | "geography" => format!("ST_AsGeoJSON({col}, 15, 4)::json"),
            "int8" | "numeric" if self.stringify_numerics => format!("({col})::text"),
            _ => col,
        }
    }

    fn bool_exp(&mut self, exp: &BoolExp, alias: &str, root: &str) -> String {
        let dialect = self.dialect;
        match exp {
            BoolExp::And(exps) => {
                if exps.is_empty() {
                    "TRUE".into()
                } else {
                    let parts: Vec<String> =
                        exps.iter().map(|e| self.bool_exp(e, alias, root)).collect();
                    format!("({})", parts.join(" AND "))
                }
            }
            BoolExp::Or(exps) => {
                if exps.is_empty() {
                    "FALSE".into()
                } else {
                    let parts: Vec<String> =
                        exps.iter().map(|e| self.bool_exp(e, alias, root)).collect();
                    format!("({})", parts.join(" OR "))
                }
            }
            BoolExp::Not(inner) => format!("(NOT {})", self.bool_exp(inner, alias, root)),
            BoolExp::Compare { column, pg_type, op } => {
                let col = qualified(alias, column);
                self.compare(&col, pg_type, op, alias, root)
            }
            BoolExp::Relationship { table, join, predicate } => {
                let ra = self.alias();
                let mut conds: Vec<String> = join
                    .iter()
                    .map(|(local, remote)| {
                        format!("{} = {}", qualified(&ra, remote), qualified(alias, local))
                    })
                    .collect();
                conds.push(self.bool_exp(predicate, &ra, root));
                format!(
                    "EXISTS (SELECT 1 FROM {}.{} AS {} WHERE {})",
                    quote_ident(&table.schema),
                    quote_ident(&table.name),
                    quote_ident(&ra),
                    conds.join(" AND ")
                )
            }
            BoolExp::ComputedCompare { schema, name, args, pg_type, op } => {
                let rendered: Vec<String> =
                    args.iter().map(|a| row_function_arg(&dialect, a, alias)).collect();
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
            BoolExp::RowFunctionExists { schema, name, args, predicate } => {
                let ra = self.alias();
                let rendered: Vec<String> =
                    args.iter().map(|a| row_function_arg(&dialect, a, alias)).collect();
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

    fn compare(&mut self, col: &str, pg_type: &str, op: &CompareOp, alias: &str, root: &str) -> String {
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
            CompareOp::CompareColumn { sql_op, column, root: use_root } => {
                let base = if *use_root { root } else { alias };
                format!("{col} {sql_op} {}", qualified(base, column))
            }
            CompareOp::CompareColumnRel { sql_op, table, join, column } => {
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
                let func = if *three_d { "ST_3DDWithin" } else { "ST_DWithin" };
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
    let mut ctx = Ctx { next_alias: 0, stringify_numerics, dialect };
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
                            SetOp::Set { column, pg_type, value } => sets.push(format!(
                                "{} = {}",
                                quote_ident(column),
                                scalar_sql(&dialect, value, pg_type)
                            )),
                            SetOp::Inc { column, pg_type, value } => sets.push(format!(
                                "{} = {}.{} + {}",
                                quote_ident(column),
                                quote_ident(&insert.table.name),
                                quote_ident(column),
                                scalar_sql(&dialect, value, pg_type)
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
            ctx.mutation_select(
                "ins",
                &stmt,
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
                    SetOp::Set { column, pg_type, value } => {
                        format!("{} = {}", quote_ident(column), scalar_sql(&dialect, value, pg_type))
                    }
                    SetOp::Inc { column, pg_type, value } => format!(
                        "{} = {} + {}",
                        quote_ident(column),
                        quote_ident(column),
                        scalar_sql(&dialect, value, pg_type)
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
    /// Response key for the `returning` array, when the selection asked for it.
    pub returning_alias: Option<String>,
    /// Response key for `affected_rows`, when selected.
    pub affected_rows_alias: Option<String>,
    /// `(alias, value)` for a `__typename` field on the mutation response.
    pub typename: Option<(String, String)>,
    /// `(alias, value)` when the root is a `__typename` mutation root itself.
    pub root_typename: Option<(String, String)>,
    /// Error path reported on a check violation (carried into the executor's
    /// permission-error body).
    pub check_path: String,
}

/// Build the [`SqliteMutationPlan`] for an insert/update/delete mutation root.
/// Renders with the SQLite dialect; `on_conflict` is not supported on SQLite
/// (different `ON CONFLICT` grammar) and triggers a panic by design — the
/// planner does not surface it on this path and the carve-out defers it.
pub fn sqlite_mutation_plan(root: &MutationRoot) -> SqliteMutationPlan {
    let dialect = donat_backend::AnyDialect::Sqlite(donat_backend::SqliteDialect);
    let mut ctx = Ctx { next_alias: 0, stringify_numerics: false, dialect };
    match root {
        MutationRoot::Typename { value, .. } => SqliteMutationPlan {
            dml_sql: String::new(),
            returning_alias: None,
            affected_rows_alias: None,
            typename: None,
            root_typename: Some((String::new(), value.clone())),
            check_path: "$".into(),
        },
        MutationRoot::FunctionCall { .. } => {
            panic!("volatile function mutations are not supported on sqlite")
        }
        MutationRoot::Insert { insert, .. } => {
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
                    SetOp::Set { column, pg_type, value } => {
                        format!("{} = {}", quote_ident(column), scalar_sql(&dialect, value, pg_type))
                    }
                    SetOp::Inc { column, pg_type, value } => format!(
                        "{} = {} + {}",
                        quote_ident(column),
                        quote_ident(column),
                        scalar_sql(&dialect, value, pg_type)
                    ),
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
            ctx.sqlite_finish(dml, update.check.as_ref(), &update.check_path, &update.output)
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
        let dialect = self.dialect;
        let mut returning_alias = None;
        let mut affected_rows_alias = None;
        let mut typename = None;
        // Determine the node fields (the per-row `returning { ... }` selection).
        // A SingleRow output (`insert_one` / `_by_pk`) also produces a node.
        let node_fields: Vec<OutputField> = match output {
            MutationOutput::Response(fields) => {
                let mut node_fields = vec![];
                for f in fields {
                    match f {
                        MutationResponseField::AffectedRows { alias } => {
                            affected_rows_alias = Some(alias.clone());
                        }
                        MutationResponseField::Typename { alias, value } => {
                            typename = Some((alias.clone(), value.clone()));
                        }
                        MutationResponseField::Returning { alias, fields } => {
                            returning_alias = Some(alias.clone());
                            node_fields = fields.clone();
                        }
                    }
                }
                node_fields
            }
            MutationOutput::SingleRow(fields) => {
                // `insert_<t>_one` / `_by_pk`: the row itself is the node;
                // there is no affected_rows. Represent it as a returning alias
                // so the executor still folds rows; the server's SingleRow
                // handling for sqlite is out of scope for this carve-out.
                returning_alias = Some("returning".to_string());
                fields.clone()
            }
        };

        let node_expr = self.sqlite_node_json(&node_fields);
        let violated = match check {
            Some(check) => {
                format!("CASE WHEN NOT ({}) THEN 1 ELSE 0 END", self.sqlite_bare_bool(check))
            }
            None => "0".to_string(),
        };
        let _ = dialect;
        let dml_sql = format!("{dml} RETURNING {node_expr} AS node, {violated} AS violated");
        SqliteMutationPlan {
            dml_sql,
            returning_alias,
            affected_rows_alias,
            typename,
            root_typename: None,
            check_path: check_path.to_string(),
        }
    }

    /// Build a `json_object(...)` over the requested returning fields using
    /// BARE column names. Only column / typename leaves are expressible in a
    /// SQLite top-level RETURNING; nested relationships/computed/aggregate
    /// fields cannot be (they require correlated subqueries the grammar
    /// rejects), so they are refused explicitly.
    fn sqlite_node_json(&mut self, fields: &[OutputField]) -> String {
        let dialect = self.dialect;
        let pairs: Vec<(String, String)> = fields
            .iter()
            .map(|f| {
                let value = match &f.value {
                    FieldValue::Column { column, .. } => quote_ident(column),
                    FieldValue::ColumnGuarded { column, guard, .. } => {
                        let cond = self.sqlite_bare_bool(guard);
                        format!("CASE WHEN {cond} THEN {} ELSE NULL END", quote_ident(column))
                    }
                    FieldValue::Typename { value } => quote_lit(value),
                    other => panic!(
                        "field {:?} is not expressible in a sqlite top-level RETURNING",
                        std::mem::discriminant(other)
                    ),
                };
                (f.alias.clone(), value)
            })
            .collect();
        json_object(&dialect, &pairs)
    }

    /// Render a permission BoolExp over BARE column names for use inside a
    /// SQLite RETURNING `CASE`. Covers the connectives plus the scalar
    /// comparison operators a permission check uses; constructs that need an
    /// alias-qualified subquery (relationship/exists/computed/column-to-column)
    /// are rejected — a SQLite check carve-out does not support them.
    fn sqlite_bare_bool(&mut self, exp: &BoolExp) -> String {
        let dialect = self.dialect;
        match exp {
            BoolExp::And(exps) => {
                if exps.is_empty() {
                    "1".into()
                } else {
                    let parts: Vec<String> = exps.iter().map(|e| self.sqlite_bare_bool(e)).collect();
                    format!("({})", parts.join(" AND "))
                }
            }
            BoolExp::Or(exps) => {
                if exps.is_empty() {
                    "0".into()
                } else {
                    let parts: Vec<String> = exps.iter().map(|e| self.sqlite_bare_bool(e)).collect();
                    format!("({})", parts.join(" OR "))
                }
            }
            BoolExp::Not(inner) => format!("(NOT {})", self.sqlite_bare_bool(inner)),
            BoolExp::Compare { column, pg_type, op } => {
                let col = quote_ident(column);
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
        let dialect = self.dialect;
        let cte_ident = quote_ident(cte);
        let result = match output {
            MutationOutput::Response(fields) => {
                let pairs: Vec<(String, String)> = fields
                    .iter()
                    .map(|f| match f {
                        MutationResponseField::AffectedRows { alias } => (
                            alias.clone(),
                            format!("(SELECT count(*) FROM {cte_ident})"),
                        ),
                        MutationResponseField::Typename { alias, value } => {
                            (alias.clone(), format!("{}::text", quote_lit(value)))
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

        let guarded = match check {
            Some(check) => {
                let violated = format!(
                    "(SELECT count(*) FROM {cte_ident} WHERE NOT ({}))",
                    self.bool_exp(check, cte, cte)
                );
                // The message carries the GraphQL error path as JSON; the
                // executor unpacks it into the Donat error shape.
                let payload = serde_json::json!({
                    "path": check_path,
                    "message": "check constraint of an insert/update permission has failed",
                })
                .to_string();
                format!(
                    "CASE WHEN {violated} > 0 THEN donat.check_violation({}) ELSE {result} END",
                    quote_lit(&payload)
                )
            }
            None => result,
        };
        format!("WITH {cte_ident} AS ({dml}) SELECT {guarded} AS root")
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

/// Render an expression as a JSON string (LEAF op #7).
fn to_json_text(dialect: &donat_backend::AnyDialect, expr: &str) -> String {
    use donat_backend::Dialect;
    dialect.to_json_text(expr)
}

/// Render a JSON scalar as a SQL literal cast to the column's type.
/// Delegates to the active backend dialect's `render_scalar`, which holds the
/// byte-for-byte rendering (including the geometry/geography GeoJSON case).
fn scalar_sql(dialect: &donat_backend::AnyDialect, scalar: &Scalar, pg_type: &str) -> String {
    use donat_backend::Dialect;
    dialect.render_scalar(scalar, pg_type)
}

#[cfg(test)]
mod dialect_dispatch_tests {
    use super::*;
    use donat_backend::{AnyDialect, PostgresDialect};

    fn sample_roots() -> Vec<RootField> {
        let cols = vec![
            OutputField {
                alias: "id".into(),
                value: FieldValue::Column { column: "id".into(), pg_type: "int4".into() },
            },
            OutputField {
                alias: "name".into(),
                value: FieldValue::Column { column: "name".into(), pg_type: "text".into() },
            },
        ];
        let query = |single: bool| SelectQuery {
            from: FromSource::Table(Table { schema: "public".into(), name: "author".into() }),
            fields: cols.clone(),
            predicate: Some(BoolExp::Compare {
                column: "id".into(),
                pg_type: "int4".into(),
                op: CompareOp::Eq(Scalar::Json(serde_json::json!(7))),
            }),
            order_by: vec![],
            limit: Some(10),
            nodes_limit: None,
            offset: Some(2),
            distinct_on: vec![],
            single,
        };
        vec![
            RootField::Select { alias: "author_by_pk".into(), query: query(true) },
            RootField::Select { alias: "authors".into(), query: query(false) },
        ]
    }

    #[test]
    fn operation_to_sql_with_postgres_equals_default_wrapper() {
        // The dialect-explicit entry point with the Postgres dialect must
        // produce byte-identical SQL to the default (Postgres-wrapper) entry
        // point. Guards the dispatch refactor: the Postgres path is unchanged.
        let roots = sample_roots();
        let default = operation_to_sql(&roots);
        let explicit =
            operation_to_sql_with(&roots, AnyDialect::Postgres(PostgresDialect));
        assert_eq!(default, explicit);
    }

    #[test]
    fn mutation_to_sql_with_postgres_equals_default_wrapper() {
        let root = MutationRoot::Insert {
            alias: "insert_author".into(),
            insert: InsertMutation {
                table: Table { schema: "public".into(), name: "author".into() },
                columns: vec![("name".into(), "text".into())],
                rows: vec![vec![Some(Scalar::Json(serde_json::json!("Ada")))]],
                on_conflict: None,
                check: None,
                check_path: "$".into(),
                output: MutationOutput::Response(vec![
                    MutationResponseField::AffectedRows { alias: "affected_rows".into() },
                    MutationResponseField::Returning {
                        alias: "returning".into(),
                        fields: vec![OutputField {
                            alias: "id".into(),
                            value: FieldValue::Column { column: "id".into(), pg_type: "int4".into() },
                        }],
                    },
                ]),
            },
        };
        let default = mutation_to_sql(&root);
        let explicit = mutation_to_sql_with(&root, AnyDialect::Postgres(PostgresDialect));
        assert_eq!(default, explicit);
    }
}
