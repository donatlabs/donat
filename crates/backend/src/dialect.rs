//! SQL dialect rendering: the leaf string-rendering primitives a backend
//! must provide. The [`PostgresDialect`] implementation is byte-identical to
//! the engine's current Postgres rendering in `crates/sqlgen` so it can be
//! dropped in later without changing emitted SQL.

/// Backend-specific rendering of SQL syntax fragments.
pub trait Dialect {
    /// Quote an identifier (table/column/alias) for safe inlining.
    fn quote_ident(&self, ident: &str) -> String;
    /// Quote a string literal for safe inlining.
    fn quote_literal(&self, lit: &str) -> String;
    /// Render the trailing `LIMIT`/`OFFSET` clause (with leading spaces),
    /// or the empty string when neither bound is present.
    fn limit_offset(&self, limit: Option<u64>, offset: Option<u64>) -> String;
    /// Render a JSON scalar as a SQL literal compatible with the column's
    /// native type. Mirrors sqlgen's `scalar_sql`: NULL / TRUE / FALSE,
    /// numbers and strings cast to the backend type, JSON arrays/objects
    /// targeting json/jsonb, and the geometry/geography GeoJSON special-case.
    fn render_scalar(&self, scalar: &donat_ir::Scalar, native_type: &str) -> String;

    /// Assemble a JSON object from `(raw key, value expr)` pairs. The key is
    /// quoted internally; values are inlined verbatim. (LEAF op #1 in the
    /// JSON-assembly inventory.)
    fn json_object(&self, pairs: &[(String, String)]) -> String;

    /// Aggregate a set of rows into a JSON array, with an optional
    /// `ORDER BY` clause body, coalescing the empty set to `[]`.
    /// (LEAF op #2/#8 in the JSON-assembly inventory.)
    fn json_array_agg(&self, row_expr: &str, order_by: Option<&str>) -> String;

    /// Render an expression as a JSON string (text). (LEAF op #7 in the
    /// JSON-assembly inventory.)
    fn to_json_text(&self, expr: &str) -> String;

    /// Render the `NULLS FIRST` / `NULLS LAST` suffix for an `ORDER BY` item,
    /// WITH a single leading space, or the empty string when the backend has
    /// no explicit null-ordering syntax. `nulls_first` selects which ordering
    /// was requested. Postgres and SQLite emit the explicit clause (matching
    /// sqlgen's historical output byte-for-byte); MySQL has no `NULLS`
    /// ordering clause (it is a syntax error there) and returns the empty
    /// string, falling back to MySQL's default null placement.
    fn null_ordering(&self, nulls_first: bool) -> String {
        if nulls_first {
            " NULLS FIRST".to_string()
        } else {
            " NULLS LAST".to_string()
        }
    }
}

/// Postgres dialect. Output matches `crates/sqlgen` exactly.
#[derive(Debug, Clone, Copy)]
pub struct PostgresDialect;

impl Dialect for PostgresDialect {
    fn quote_ident(&self, ident: &str) -> String {
        // Mirrors sqlgen::quote_ident: double-quote, doubling embedded `"`.
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn quote_literal(&self, lit: &str) -> String {
        // Mirrors sqlgen::quote_lit: single-quote, doubling embedded `'`.
        format!("'{}'", lit.replace('\'', "''"))
    }

    fn limit_offset(&self, limit: Option<u64>, offset: Option<u64>) -> String {
        // Mirrors sqlgen's tail assembly: append " LIMIT {n}" then
        // " OFFSET {n}", each only when present.
        let mut sql = String::new();
        if let Some(limit) = limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }
        sql
    }

    fn render_scalar(&self, scalar: &donat_ir::Scalar, native_type: &str) -> String {
        // Byte-for-byte port of sqlgen's `scalar_sql`. The geometry/geography
        // GeoJSON-object case is diverted to the inlined `geometry_sql` logic
        // (see `render_geometry` below).
        if matches!(native_type, "geometry" | "geography") && scalar.as_json().is_object() {
            return self.render_geometry(scalar, native_type);
        }
        let ty = self.quote_ident(native_type);
        match scalar.as_json() {
            serde_json::Value::Null => "NULL".into(),
            serde_json::Value::Bool(b) => {
                if *b {
                    "TRUE".into()
                } else {
                    "FALSE".into()
                }
            }
            serde_json::Value::Number(n) => format!("({n})::{ty}"),
            serde_json::Value::String(s) => format!("({})::{ty}", self.quote_literal(s)),
            // arrays/objects target json/jsonb columns
            other => format!("({})::{ty}", self.quote_literal(&other.to_string())),
        }
    }

    fn json_object(&self, pairs: &[(String, String)]) -> String {
        // Mirrors sqlgen's inlined `json_build_object('k', v, …)`: each key is
        // rendered via quote_literal, then key/value alternate, joined by ", ".
        let body: Vec<String> = pairs
            .iter()
            .map(|(key, value)| format!("{}, {value}", self.quote_literal(key)))
            .collect();
        format!("json_build_object({})", body.join(", "))
    }

    fn json_array_agg(&self, row_expr: &str, order_by: Option<&str>) -> String {
        // Mirrors sqlgen's inlined `coalesce(json_agg(x [ORDER BY ob]), '[]'::json)`.
        match order_by {
            Some(ob) => format!("coalesce(json_agg({row_expr} ORDER BY {ob}), '[]'::json)"),
            None => format!("coalesce(json_agg({row_expr}), '[]'::json)"),
        }
    }

    fn to_json_text(&self, expr: &str) -> String {
        // Mirrors sqlgen's inlined `to_json(x::text)`.
        format!("to_json({expr}::text)")
    }
}

impl PostgresDialect {
    /// Byte-for-byte port of sqlgen's `geometry_sql`: GeoJSON objects (or
    /// strings holding GeoJSON, e.g. from session variables) go through
    /// ST_GeomFromGeoJSON; other strings are assumed to be WKT/EWKT.
    fn render_geometry(&self, value: &donat_ir::Scalar, native_type: &str) -> String {
        let cast = self.quote_ident(native_type);
        match value.as_json() {
            serde_json::Value::Object(_) => format!(
                "(ST_GeomFromGeoJSON({}))::{cast}",
                self.quote_literal(&value.as_json().to_string())
            ),
            serde_json::Value::String(s) if s.trim_start().starts_with('{') => {
                format!("(ST_GeomFromGeoJSON({}))::{cast}", self.quote_literal(s))
            }
            serde_json::Value::String(s) => format!("({})::{cast}", self.quote_literal(s)),
            other => format!("({})::{cast}", self.quote_literal(&other.to_string())),
        }
    }
}

/// SQLite dialect. The identifier/literal/limit syntax is shared with
/// Postgres; only the JSON-assembly and scalar-rendering leaves differ
/// (no typed casts, json1 builtins). The chosen renderings below are design
/// choices pinned by unit tests and validated against a real SQLite in the
/// harness slice.
#[derive(Debug, Clone, Copy)]
pub struct SqliteDialect;

impl Dialect for SqliteDialect {
    fn quote_ident(&self, ident: &str) -> String {
        // SQLite uses the same double-quoted identifier syntax as Postgres.
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn quote_literal(&self, lit: &str) -> String {
        // SQLite uses the same single-quoted string literal syntax.
        format!("'{}'", lit.replace('\'', "''"))
    }

    fn limit_offset(&self, limit: Option<u64>, offset: Option<u64>) -> String {
        // Identical LIMIT/OFFSET tail to Postgres.
        let mut sql = String::new();
        if let Some(limit) = limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }
        sql
    }

    fn render_scalar(&self, scalar: &donat_ir::Scalar, _native_type: &str) -> String {
        // SQLite has no typed casts, so the native type is intentionally
        // ignored: values are rendered as bare literals.
        // validated against real sqlite in the harness slice
        match scalar.as_json() {
            serde_json::Value::Null => "NULL".into(),
            // No boolean type in SQLite: TRUE/FALSE are 1/0.
            serde_json::Value::Bool(b) => {
                if *b {
                    "1".into()
                } else {
                    "0".into()
                }
            }
            // No cast: bare numeric literal.
            serde_json::Value::Number(n) => format!("{n}"),
            serde_json::Value::String(s) => self.quote_literal(s),
            // JSON object/array: wrap in json(...) so json1 validates/minifies
            // the literal. (Geometry is not a SQLite capability and won't
            // reach here; this is the same quoted-text fallback.)
            // validated against real sqlite in the harness slice
            other => format!("json({})", self.quote_literal(&other.to_string())),
        }
    }

    fn json_object(&self, pairs: &[(String, String)]) -> String {
        // json1's json_object('k', v, …): keys quoted, values inlined.
        let body: Vec<String> = pairs
            .iter()
            .map(|(key, value)| format!("{}, {value}", self.quote_literal(key)))
            .collect();
        format!("json_object({})", body.join(", "))
    }

    fn json_array_agg(&self, row_expr: &str, order_by: Option<&str>) -> String {
        // json1's json_group_array(...), coalesced to an empty json array.
        //
        // The row expression is itself a JSON object/array built by
        // `json_object(...)`/`json_group_array(...)`, which SQLite returns as
        // TEXT. Feeding that text straight to json_group_array would treat it
        // as a string scalar and JSON-escape it, double-encoding the nested
        // value (the array would come back as `["{\"id\":1}"]`). Wrapping the
        // row in `json(...)` reparses the text so the nested structure is
        // preserved as real JSON. (validated against real sqlite)
        match order_by {
            Some(ob) => {
                format!("coalesce(json_group_array(json({row_expr}) ORDER BY {ob}), json_array())")
            }
            None => format!("coalesce(json_group_array(json({row_expr})), json_array())"),
        }
    }

    fn to_json_text(&self, expr: &str) -> String {
        // json1's json_quote(...) renders a value as a JSON string.
        format!("json_quote({expr})")
    }
}

/// MySQL dialect (8.0.14+). Differs from Postgres/SQLite in three notable
/// ways: identifiers are quoted with backticks; string literals must escape
/// backslashes (MySQL processes C-style escapes inside `'...'` by default);
/// and JSON assembly uses text concatenation. MySQL's binary JSON object type
/// reorders keys by key length, which violates GraphQL selection-order output.
/// SQLgen therefore passes already-serialized JSON values to these leaves;
/// `CONCAT`/`GROUP_CONCAT` preserve both field and row order without Rust
/// post-processing. These renderings are validated against a real MySQL 8 in
/// the e2e harness slice (`crates/server/tests/mysql_e2e.rs`).
#[derive(Debug, Clone, Copy)]
pub struct MySqlDialect;

impl Dialect for MySqlDialect {
    fn quote_ident(&self, ident: &str) -> String {
        // MySQL quotes identifiers with backticks; an embedded backtick is
        // escaped by doubling it.
        format!("`{}`", ident.replace('`', "``"))
    }

    fn quote_literal(&self, lit: &str) -> String {
        // MySQL interprets backslash escape sequences inside single-quoted
        // strings by default (unlike Postgres/SQLite), so a literal backslash
        // must be doubled IN ADDITION to doubling the single quote. Order
        // matters: escape backslashes first, then quotes.
        format!("'{}'", lit.replace('\\', "\\\\").replace('\'', "''"))
    }

    fn limit_offset(&self, limit: Option<u64>, offset: Option<u64>) -> String {
        // MySQL 8 supports `LIMIT n OFFSET m`. MySQL requires a LIMIT to be
        // present whenever OFFSET is used; when only an offset is given we emit
        // a max-row LIMIT sentinel so the OFFSET clause is legal.
        let mut sql = String::new();
        match (limit, offset) {
            (Some(limit), Some(offset)) => {
                sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
            }
            (Some(limit), None) => {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            (None, Some(offset)) => {
                // OFFSET without LIMIT is a syntax error in MySQL; use the
                // documented "all rows" sentinel as the LIMIT.
                sql.push_str(&format!(" LIMIT 18446744073709551615 OFFSET {offset}"));
            }
            (None, None) => {}
        }
        sql
    }

    fn render_scalar(&self, scalar: &donat_ir::Scalar, _native_type: &str) -> String {
        // MySQL is loosely typed; like SQLite we render bare literals and let
        // MySQL coerce. JSON object/array values are wrapped in CAST(... AS
        // JSON) so they become real JSON values rather than string scalars.
        // (validated against real MySQL in the harness slice)
        match scalar.as_json() {
            serde_json::Value::Null => "NULL".into(),
            serde_json::Value::Bool(b) => {
                if *b {
                    "TRUE".into()
                } else {
                    "FALSE".into()
                }
            }
            serde_json::Value::Number(n) => format!("{n}"),
            serde_json::Value::String(s) => self.quote_literal(s),
            other => format!("CAST({} AS JSON)", self.quote_literal(&other.to_string())),
        }
    }

    fn json_object(&self, pairs: &[(String, String)]) -> String {
        if pairs.is_empty() {
            return self.quote_literal("{}");
        }
        let mut parts = vec![self.quote_literal("{")];
        for (index, (key, value)) in pairs.iter().enumerate() {
            if index > 0 {
                parts.push(self.quote_literal(","));
            }
            let key = serde_json::to_string(key).expect("JSON object key");
            parts.push(self.quote_literal(&format!("{key}:")));
            parts.push(format!(
                "COALESCE(CAST({value} AS CHAR), {})",
                self.quote_literal("null")
            ));
        }
        parts.push(self.quote_literal("}"));
        format!("CONCAT({})", parts.join(", "))
    }

    fn json_array_agg(&self, row_expr: &str, order_by: Option<&str>) -> String {
        let order = order_by
            .map(|order| format!(" ORDER BY {order}"))
            .unwrap_or_default();
        format!(
            "CONCAT('[', COALESCE(GROUP_CONCAT(CAST({row_expr} AS CHAR){order} SEPARATOR ','), ''), ']')"
        )
    }

    fn to_json_text(&self, expr: &str) -> String {
        format!("JSON_QUOTE(CAST({expr} AS CHAR))")
    }

    fn null_ordering(&self, _nulls_first: bool) -> String {
        // MySQL has no NULLS FIRST/LAST syntax (it is a parse error). Omit the
        // clause; MySQL sorts NULLs first for ASC and last for DESC by default.
        String::new()
    }
}

/// ClickHouse read-query dialect.
///
/// ClickHouse's experimental `JSON` type canonicalizes object keys, so GraphQL
/// response objects stay as ordered JSON text. SQLgen passes serialized scalar
/// and nested JSON values to these leaves; `concat`/`arrayStringConcat` keep
/// field and row order without Rust post-processing.
#[derive(Debug, Clone, Copy)]
pub struct ClickhouseDialect;

impl Dialect for ClickhouseDialect {
    fn quote_ident(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('\\', "\\\\").replace('`', "\\`"))
    }

    fn quote_literal(&self, lit: &str) -> String {
        format!("'{}'", lit.replace('\\', "\\\\").replace('\'', "\\'"))
    }

    fn limit_offset(&self, limit: Option<u64>, offset: Option<u64>) -> String {
        match (limit, offset) {
            (Some(limit), Some(offset)) => format!(" LIMIT {limit} OFFSET {offset}"),
            (Some(limit), None) => format!(" LIMIT {limit}"),
            (None, Some(offset)) => {
                format!(" LIMIT 18446744073709551615 OFFSET {offset}")
            }
            (None, None) => String::new(),
        }
    }

    fn render_scalar(&self, scalar: &donat_ir::Scalar, native_type: &str) -> String {
        let native_type = native_type.trim();
        if clickhouse_complex_type(native_type) {
            // ClickHouse does not support CAST(String AS Map/Tuple/Array),
            // while JSONExtract's type-name overload parses the same JSON
            // literal into the native complex value.
            let value = scalar.as_json();
            if value.is_null() {
                return "NULL".into();
            }
            let json = match value {
                serde_json::Value::String(value) => value.clone(),
                value => value.to_string(),
            };
            return format!(
                "JSONExtract({}, {})",
                self.quote_literal(&json),
                self.quote_literal(native_type)
            );
        }
        let native_type = clickhouse_cast_type(native_type);
        match scalar.as_json() {
            serde_json::Value::Null => "NULL".into(),
            serde_json::Value::Bool(value) => {
                if *value {
                    "true".into()
                } else {
                    "false".into()
                }
            }
            serde_json::Value::Number(value) => format!("CAST({value} AS {native_type})"),
            serde_json::Value::String(value) => {
                format!("CAST({} AS {native_type})", self.quote_literal(value))
            }
            value => format!(
                "CAST({} AS {native_type})",
                self.quote_literal(&value.to_string())
            ),
        }
    }

    fn json_object(&self, pairs: &[(String, String)]) -> String {
        if pairs.is_empty() {
            return self.quote_literal("{}");
        }
        let mut parts = vec![self.quote_literal("{")];
        for (index, (key, value)) in pairs.iter().enumerate() {
            if index > 0 {
                parts.push(self.quote_literal(","));
            }
            let key = format!("{}:", serde_json::to_string(key).expect("JSON object key"));
            parts.push(self.quote_literal(&key));
            parts.push(format!("coalesce({value}, {})", self.quote_literal("null")));
        }
        parts.push(self.quote_literal("}"));
        format!("concat({})", parts.join(", "))
    }

    fn json_array_agg(&self, row_expr: &str, order_by: Option<&str>) -> String {
        match order_by {
            Some(order) => format!(
                "concat('[', arrayStringConcat(arrayMap(item -> item.2, arraySort(item -> item.1, groupArray(({order}, CAST({row_expr} AS String))))), ','), ']')"
            ),
            None => format!(
                "concat('[', arrayStringConcat(groupArray(CAST({row_expr} AS String)), ','), ']')"
            ),
        }
    }

    fn to_json_text(&self, expr: &str) -> String {
        format!("toJSONString({expr})")
    }
}

fn clickhouse_cast_type(logical_type: &str) -> &str {
    match logical_type {
        "int2" => "Int16",
        "int4" => "Int32",
        "int8" => "Int64",
        "float4" => "Float32",
        "float8" => "Float64",
        "numeric" => "Decimal(38, 9)",
        "bool" => "Bool",
        "text" | "varchar" | "bpchar" | "json" | "jsonb" => "String",
        "uuid" => "UUID",
        "timestamp" | "timestamptz" => "DateTime64(3)",
        "date" => "Date",
        other => other,
    }
}

fn clickhouse_complex_type(native_type: &str) -> bool {
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
    matches!(family.as_str(), "array" | "map" | "tuple")
}

/// A backend dialect selected at runtime. Implements [`Dialect`] by
/// delegating every method to the inner concrete dialect, so callers can hold
/// one value and stay dialect-agnostic.
#[derive(Debug, Clone, Copy)]
pub enum AnyDialect {
    Postgres(PostgresDialect),
    Sqlite(SqliteDialect),
    Mysql(MySqlDialect),
    Clickhouse(ClickhouseDialect),
}

impl Dialect for AnyDialect {
    fn quote_ident(&self, ident: &str) -> String {
        match self {
            AnyDialect::Postgres(d) => d.quote_ident(ident),
            AnyDialect::Sqlite(d) => d.quote_ident(ident),
            AnyDialect::Mysql(d) => d.quote_ident(ident),
            AnyDialect::Clickhouse(d) => d.quote_ident(ident),
        }
    }

    fn quote_literal(&self, lit: &str) -> String {
        match self {
            AnyDialect::Postgres(d) => d.quote_literal(lit),
            AnyDialect::Sqlite(d) => d.quote_literal(lit),
            AnyDialect::Mysql(d) => d.quote_literal(lit),
            AnyDialect::Clickhouse(d) => d.quote_literal(lit),
        }
    }

    fn limit_offset(&self, limit: Option<u64>, offset: Option<u64>) -> String {
        match self {
            AnyDialect::Postgres(d) => d.limit_offset(limit, offset),
            AnyDialect::Sqlite(d) => d.limit_offset(limit, offset),
            AnyDialect::Mysql(d) => d.limit_offset(limit, offset),
            AnyDialect::Clickhouse(d) => d.limit_offset(limit, offset),
        }
    }

    fn render_scalar(&self, scalar: &donat_ir::Scalar, native_type: &str) -> String {
        match self {
            AnyDialect::Postgres(d) => d.render_scalar(scalar, native_type),
            AnyDialect::Sqlite(d) => d.render_scalar(scalar, native_type),
            AnyDialect::Mysql(d) => d.render_scalar(scalar, native_type),
            AnyDialect::Clickhouse(d) => d.render_scalar(scalar, native_type),
        }
    }

    fn json_object(&self, pairs: &[(String, String)]) -> String {
        match self {
            AnyDialect::Postgres(d) => d.json_object(pairs),
            AnyDialect::Sqlite(d) => d.json_object(pairs),
            AnyDialect::Mysql(d) => d.json_object(pairs),
            AnyDialect::Clickhouse(d) => d.json_object(pairs),
        }
    }

    fn json_array_agg(&self, row_expr: &str, order_by: Option<&str>) -> String {
        match self {
            AnyDialect::Postgres(d) => d.json_array_agg(row_expr, order_by),
            AnyDialect::Sqlite(d) => d.json_array_agg(row_expr, order_by),
            AnyDialect::Mysql(d) => d.json_array_agg(row_expr, order_by),
            AnyDialect::Clickhouse(d) => d.json_array_agg(row_expr, order_by),
        }
    }

    fn to_json_text(&self, expr: &str) -> String {
        match self {
            AnyDialect::Postgres(d) => d.to_json_text(expr),
            AnyDialect::Sqlite(d) => d.to_json_text(expr),
            AnyDialect::Mysql(d) => d.to_json_text(expr),
            AnyDialect::Clickhouse(d) => d.to_json_text(expr),
        }
    }

    fn null_ordering(&self, nulls_first: bool) -> String {
        match self {
            AnyDialect::Postgres(d) => d.null_ordering(nulls_first),
            AnyDialect::Sqlite(d) => d.null_ordering(nulls_first),
            AnyDialect::Mysql(d) => d.null_ordering(nulls_first),
            AnyDialect::Clickhouse(d) => d.null_ordering(nulls_first),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ClickhouseDialect -----------------------------------------------

    #[test]
    fn clickhouse_quotes_identifiers_and_literals() {
        let d = ClickhouseDialect;
        assert_eq!(d.quote_ident("users"), "`users`");
        assert_eq!(d.quote_ident("a`b"), "`a\\`b`");
        assert_eq!(d.quote_literal("O'Hara"), "'O\\'Hara'");
        assert_eq!(d.quote_literal("a\\b"), "'a\\\\b'");
    }

    #[test]
    fn clickhouse_renders_pagination_and_scalars() {
        let d = ClickhouseDialect;
        assert_eq!(d.limit_offset(None, None), "");
        assert_eq!(d.limit_offset(Some(10), Some(5)), " LIMIT 10 OFFSET 5");
        assert_eq!(
            d.limit_offset(None, Some(5)),
            " LIMIT 18446744073709551615 OFFSET 5"
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(42)), "UInt64"),
            "CAST(42 AS UInt64)"
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!("O'Hara")), "String"),
            "CAST('O\\'Hara' AS String)"
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(42)), "int4"),
            "CAST(42 AS Int32)"
        );
    }

    #[test]
    fn clickhouse_renders_complex_literals_with_json_extract() {
        let d = ClickhouseDialect;
        assert_eq!(
            d.render_scalar(&s(serde_json::json!({"a": 1})), "Map(String, UInt64)"),
            "JSONExtract('{\"a\":1}', 'Map(String, UInt64)')"
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(["a", "b"])), "Array(String)"),
            "JSONExtract('[\"a\",\"b\"]', 'Array(String)')"
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!({"a": 1})), "Tuple(a UInt64)"),
            "JSONExtract('{\"a\":1}', 'Tuple(a UInt64)')"
        );
    }

    #[test]
    fn clickhouse_assembles_ordered_json_text_without_double_encoding() {
        let d = ClickhouseDialect;
        assert_eq!(
            d.json_object(&[
                ("id".to_string(), "toJSONString(_t0.id)".to_string()),
                ("name".to_string(), "toJSONString(_t0.name)".to_string()),
            ]),
            "concat('{', '\"id\":', coalesce(toJSONString(_t0.id), 'null'), ',', '\"name\":', coalesce(toJSONString(_t0.name), 'null'), '}')"
        );
        assert_eq!(d.json_object(&[]), "'{}'");
        assert_eq!(
            d.json_array_agg("_e.j", Some("_e.__donat_ord")),
            "concat('[', arrayStringConcat(arrayMap(item -> item.2, arraySort(item -> item.1, groupArray((_e.__donat_ord, CAST(_e.j AS String))))), ','), ']')"
        );
        assert_eq!(d.to_json_text("'User'"), "toJSONString('User')");
    }

    #[test]
    fn any_dialect_clickhouse_delegates() {
        let d = AnyDialect::Clickhouse(ClickhouseDialect);
        assert_eq!(d.quote_ident("users"), "`users`");
        assert_eq!(d.quote_literal("x"), "'x'");
        assert_eq!(d.limit_offset(Some(1), None), " LIMIT 1");
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(1)), "UInt8"),
            "CAST(1 AS UInt8)"
        );
        assert_eq!(
            d.json_object(&[("k".into(), "toJSONString(v)".into())]),
            "concat('{', '\"k\":', coalesce(toJSONString(v), 'null'), '}')"
        );
        assert_eq!(
            d.json_array_agg("x", None),
            "concat('[', arrayStringConcat(groupArray(CAST(x AS String)), ','), ']')"
        );
        assert_eq!(d.to_json_text("x"), "toJSONString(x)");
    }

    #[test]
    fn quote_ident_wraps_in_double_quotes() {
        let d = PostgresDialect;
        assert_eq!(d.quote_ident("users"), "\"users\"");
    }

    #[test]
    fn quote_ident_doubles_embedded_double_quotes() {
        let d = PostgresDialect;
        assert_eq!(d.quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn quote_ident_empty() {
        let d = PostgresDialect;
        assert_eq!(d.quote_ident(""), "\"\"");
    }

    #[test]
    fn quote_literal_wraps_in_single_quotes() {
        let d = PostgresDialect;
        assert_eq!(d.quote_literal("hello"), "'hello'");
    }

    #[test]
    fn quote_literal_doubles_embedded_single_quotes() {
        let d = PostgresDialect;
        assert_eq!(d.quote_literal("O'Hara"), "'O''Hara'");
    }

    #[test]
    fn quote_literal_empty() {
        let d = PostgresDialect;
        assert_eq!(d.quote_literal(""), "''");
    }

    #[test]
    fn limit_offset_matrix() {
        let d = PostgresDialect;
        // sqlgen emits " LIMIT {n}" then " OFFSET {n}", each only when present,
        // each with a single leading space; nothing when both are None.
        assert_eq!(d.limit_offset(None, None), "");
        assert_eq!(d.limit_offset(Some(10), None), " LIMIT 10");
        assert_eq!(d.limit_offset(None, Some(5)), " OFFSET 5");
        assert_eq!(d.limit_offset(Some(10), Some(5)), " LIMIT 10 OFFSET 5");
        assert_eq!(d.limit_offset(Some(0), Some(0)), " LIMIT 0 OFFSET 0");
    }

    fn s(v: serde_json::Value) -> donat_ir::Scalar {
        donat_ir::Scalar::Json(v)
    }

    #[test]
    fn render_scalar_null() {
        let d = PostgresDialect;
        assert_eq!(d.render_scalar(&s(serde_json::Value::Null), "text"), "NULL");
    }

    #[test]
    fn render_scalar_bool_true() {
        let d = PostgresDialect;
        assert_eq!(d.render_scalar(&s(serde_json::json!(true)), "bool"), "TRUE");
    }

    #[test]
    fn render_scalar_bool_false() {
        let d = PostgresDialect;
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(false)), "bool"),
            "FALSE"
        );
    }

    #[test]
    fn render_scalar_number() {
        let d = PostgresDialect;
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(42)), "int4"),
            "(42)::\"int4\""
        );
    }

    #[test]
    fn render_scalar_string_escapes() {
        let d = PostgresDialect;
        assert_eq!(
            d.render_scalar(&s(serde_json::json!("O'Hara")), "text"),
            "('O''Hara')::\"text\""
        );
    }

    #[test]
    fn render_scalar_json_object_and_array() {
        let d = PostgresDialect;
        // A JSON object targeting a jsonb column (not geometry/geography):
        // serialized via to_string and quoted.
        assert_eq!(
            d.render_scalar(&s(serde_json::json!({"a": 1})), "jsonb"),
            "('{\"a\":1}')::\"jsonb\""
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!([1, 2])), "jsonb"),
            "('[1,2]')::\"jsonb\""
        );
    }

    #[test]
    fn render_scalar_geometry_geojson_object() {
        let d = PostgresDialect;
        let geo = serde_json::json!({"type": "Point", "coordinates": [1, 2]});
        // geometry_sql casts via quote_ident(pg_type), i.e. `::"geometry"`.
        assert_eq!(
            d.render_scalar(&s(geo.clone()), "geometry"),
            format!("(ST_GeomFromGeoJSON('{}'))::\"geometry\"", geo.to_string())
        );
    }

    #[test]
    fn json_object_alternates_quoted_keys_and_values() {
        let d = PostgresDialect;
        assert_eq!(
            d.json_object(&[
                ("id".to_string(), "_t0.id".to_string()),
                ("name".to_string(), "_t0.name".to_string()),
            ]),
            "json_build_object('id', _t0.id, 'name', _t0.name)"
        );
    }

    #[test]
    fn json_object_fixed_keys_and_empty() {
        let d = PostgresDialect;
        // Fixed (raw) keys like cursor/node are quoted internally, matching
        // sqlgen's `json_build_object('cursor', …, 'node', …)`.
        assert_eq!(
            d.json_object(&[
                ("cursor".to_string(), "_t0.c".to_string()),
                ("node".to_string(), "_t0.n".to_string()),
            ]),
            "json_build_object('cursor', _t0.c, 'node', _t0.n)"
        );
        // No pairs -> empty argument list, byte-identical to inlined output.
        assert_eq!(d.json_object(&[]), "json_build_object()");
    }

    #[test]
    fn json_object_quotes_embedded_single_quotes_in_key() {
        let d = PostgresDialect;
        assert_eq!(
            d.json_object(&[("O'Hara".to_string(), "v".to_string())]),
            "json_build_object('O''Hara', v)"
        );
    }

    #[test]
    fn json_array_agg_without_order() {
        let d = PostgresDialect;
        assert_eq!(
            d.json_array_agg("_e.j", None),
            "coalesce(json_agg(_e.j), '[]'::json)"
        );
    }

    #[test]
    fn json_array_agg_with_order() {
        let d = PostgresDialect;
        assert_eq!(
            d.json_array_agg("t.e", Some("t.i ASC")),
            "coalesce(json_agg(t.e ORDER BY t.i ASC), '[]'::json)"
        );
    }

    #[test]
    fn to_json_text_casts_expr() {
        let d = PostgresDialect;
        assert_eq!(d.to_json_text("'User'"), "to_json('User'::text)");
    }

    #[test]
    fn render_scalar_geometry_wkt_string() {
        let d = PostgresDialect;
        // A non-object string on a geometry column is NOT the object
        // special-case (scalar_sql only diverts when as_json().is_object());
        // it renders as a plain cast.
        assert_eq!(
            d.render_scalar(&s(serde_json::json!("POINT(1 2)")), "geometry"),
            "('POINT(1 2)')::\"geometry\""
        );
    }

    // ---- SqliteDialect ----------------------------------------------------

    #[test]
    fn sqlite_quote_ident_matches_postgres() {
        let d = SqliteDialect;
        // SQLite uses the same double-quoted identifier syntax as Postgres.
        assert_eq!(d.quote_ident("users"), "\"users\"");
        assert_eq!(d.quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn sqlite_quote_literal_matches_postgres() {
        let d = SqliteDialect;
        assert_eq!(d.quote_literal("O'Hara"), "'O''Hara'");
    }

    #[test]
    fn sqlite_limit_offset_matches_postgres() {
        let d = SqliteDialect;
        assert_eq!(d.limit_offset(None, None), "");
        assert_eq!(d.limit_offset(Some(10), Some(5)), " LIMIT 10 OFFSET 5");
    }

    #[test]
    fn sqlite_render_scalar_null() {
        let d = SqliteDialect;
        assert_eq!(d.render_scalar(&s(serde_json::Value::Null), "TEXT"), "NULL");
    }

    #[test]
    fn sqlite_render_scalar_bool_to_int() {
        let d = SqliteDialect;
        // SQLite has no boolean type: TRUE/FALSE are 1/0.
        assert_eq!(d.render_scalar(&s(serde_json::json!(true)), "INTEGER"), "1");
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(false)), "INTEGER"),
            "0"
        );
    }

    #[test]
    fn sqlite_render_scalar_number_no_cast() {
        let d = SqliteDialect;
        // No typed casts in SQLite: bare numeric literal.
        assert_eq!(d.render_scalar(&s(serde_json::json!(42)), "INTEGER"), "42");
    }

    #[test]
    fn sqlite_render_scalar_string_quoted_no_cast() {
        let d = SqliteDialect;
        assert_eq!(
            d.render_scalar(&s(serde_json::json!("O'Hara")), "TEXT"),
            "'O''Hara'"
        );
    }

    #[test]
    fn sqlite_render_scalar_json_object_and_array() {
        let d = SqliteDialect;
        // JSON object/array wrapped in json(...) so json1 validates/minifies.
        assert_eq!(
            d.render_scalar(&s(serde_json::json!({"a": 1})), "JSON"),
            "json('{\"a\":1}')"
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!([1, 2])), "JSON"),
            "json('[1,2]')"
        );
    }

    #[test]
    fn sqlite_render_scalar_json_object_quotes_embedded_quote() {
        let d = SqliteDialect;
        // The serialized JSON text is single-quote-escaped before json(...).
        assert_eq!(
            d.render_scalar(&s(serde_json::json!({"a": "O'Hara"})), "JSON"),
            "json('{\"a\":\"O''Hara\"}')"
        );
    }

    #[test]
    fn sqlite_json_object_alternates_quoted_keys_and_values() {
        let d = SqliteDialect;
        assert_eq!(
            d.json_object(&[
                ("id".to_string(), "_t0.id".to_string()),
                ("name".to_string(), "_t0.name".to_string()),
            ]),
            "json_object('id', _t0.id, 'name', _t0.name)"
        );
    }

    #[test]
    fn sqlite_json_object_empty() {
        let d = SqliteDialect;
        assert_eq!(d.json_object(&[]), "json_object()");
    }

    #[test]
    fn sqlite_json_array_agg_without_order() {
        let d = SqliteDialect;
        // The row expression is reparsed with json(...) so the nested JSON
        // object/array is aggregated as real JSON rather than being
        // double-encoded into a string scalar.
        assert_eq!(
            d.json_array_agg("_e.j", None),
            "coalesce(json_group_array(json(_e.j)), json_array())"
        );
    }

    #[test]
    fn sqlite_json_array_agg_with_order() {
        let d = SqliteDialect;
        assert_eq!(
            d.json_array_agg("t.e", Some("t.i ASC")),
            "coalesce(json_group_array(json(t.e) ORDER BY t.i ASC), json_array())"
        );
    }

    #[test]
    fn sqlite_to_json_text_uses_json_quote() {
        let d = SqliteDialect;
        assert_eq!(d.to_json_text("'User'"), "json_quote('User')");
    }

    // ---- AnyDialect delegation -------------------------------------------

    #[test]
    fn any_dialect_postgres_delegates() {
        let d = AnyDialect::Postgres(PostgresDialect);
        assert_eq!(d.quote_ident("users"), "\"users\"");
        assert_eq!(d.quote_literal("x"), "'x'");
        assert_eq!(d.limit_offset(Some(1), None), " LIMIT 1");
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(1)), "int4"),
            "(1)::\"int4\""
        );
        assert_eq!(
            d.json_object(&[("k".into(), "v".into())]),
            "json_build_object('k', v)"
        );
        assert_eq!(
            d.json_array_agg("x", None),
            "coalesce(json_agg(x), '[]'::json)"
        );
        assert_eq!(d.to_json_text("x"), "to_json(x::text)");
    }

    #[test]
    fn any_dialect_sqlite_delegates() {
        let d = AnyDialect::Sqlite(SqliteDialect);
        assert_eq!(d.quote_ident("users"), "\"users\"");
        assert_eq!(d.render_scalar(&s(serde_json::json!(1)), "INTEGER"), "1");
        assert_eq!(
            d.json_object(&[("k".into(), "v".into())]),
            "json_object('k', v)"
        );
        assert_eq!(
            d.json_array_agg("x", None),
            "coalesce(json_group_array(json(x)), json_array())"
        );
        assert_eq!(d.to_json_text("x"), "json_quote(x)");
    }

    // ---- MySqlDialect -----------------------------------------------------

    #[test]
    fn mysql_quote_ident_uses_backticks() {
        let d = MySqlDialect;
        assert_eq!(d.quote_ident("users"), "`users`");
        // Embedded backtick is doubled.
        assert_eq!(d.quote_ident("a`b"), "`a``b`");
        assert_eq!(d.quote_ident(""), "``");
    }

    #[test]
    fn mysql_quote_literal_escapes_quote_and_backslash() {
        let d = MySqlDialect;
        assert_eq!(d.quote_literal("hello"), "'hello'");
        // Single quote doubled.
        assert_eq!(d.quote_literal("O'Hara"), "'O''Hara'");
        // Backslash doubled (MySQL processes C-style escapes by default).
        assert_eq!(d.quote_literal("a\\b"), "'a\\\\b'");
        // Both at once: backslash first, then quote.
        assert_eq!(d.quote_literal("a\\'b"), "'a\\\\''b'");
        assert_eq!(d.quote_literal(""), "''");
    }

    #[test]
    fn mysql_limit_offset_matrix() {
        let d = MySqlDialect;
        assert_eq!(d.limit_offset(None, None), "");
        assert_eq!(d.limit_offset(Some(10), None), " LIMIT 10");
        assert_eq!(d.limit_offset(Some(10), Some(5)), " LIMIT 10 OFFSET 5");
        // OFFSET without LIMIT requires the all-rows sentinel.
        assert_eq!(
            d.limit_offset(None, Some(5)),
            " LIMIT 18446744073709551615 OFFSET 5"
        );
        assert_eq!(d.limit_offset(Some(0), Some(0)), " LIMIT 0 OFFSET 0");
    }

    #[test]
    fn mysql_render_scalar_null_bool_number() {
        let d = MySqlDialect;
        assert_eq!(d.render_scalar(&s(serde_json::Value::Null), "int"), "NULL");
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(true)), "tinyint"),
            "TRUE"
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!(false)), "tinyint"),
            "FALSE"
        );
        assert_eq!(d.render_scalar(&s(serde_json::json!(42)), "int"), "42");
    }

    #[test]
    fn mysql_render_scalar_string_escapes() {
        let d = MySqlDialect;
        assert_eq!(
            d.render_scalar(&s(serde_json::json!("O'Hara")), "varchar"),
            "'O''Hara'"
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!("a\\b")), "varchar"),
            "'a\\\\b'"
        );
    }

    #[test]
    fn mysql_render_scalar_json_object_and_array() {
        let d = MySqlDialect;
        assert_eq!(
            d.render_scalar(&s(serde_json::json!({"a": 1})), "json"),
            "CAST('{\"a\":1}' AS JSON)"
        );
        assert_eq!(
            d.render_scalar(&s(serde_json::json!([1, 2])), "json"),
            "CAST('[1,2]' AS JSON)"
        );
    }

    #[test]
    fn mysql_json_object_preserves_declared_key_order() {
        let d = MySqlDialect;
        assert_eq!(
            d.json_object(&[
                ("id".to_string(), "_t0.id".to_string()),
                ("name".to_string(), "_t0.name".to_string()),
            ]),
            "CONCAT('{', '\"id\":', COALESCE(CAST(_t0.id AS CHAR), 'null'), ',', '\"name\":', COALESCE(CAST(_t0.name AS CHAR), 'null'), '}')"
        );
        assert_eq!(d.json_object(&[]), "'{}'");
    }

    #[test]
    fn mysql_json_array_agg_preserves_requested_order() {
        let d = MySqlDialect;
        // Rows are already JSON text. GROUP_CONCAT keeps them raw and avoids
        // MySQL binary-JSON key canonicalization.
        assert_eq!(
            d.json_array_agg("_e.j", None),
            "CONCAT('[', COALESCE(GROUP_CONCAT(CAST(_e.j AS CHAR) SEPARATOR ','), ''), ']')"
        );
        assert_eq!(
            d.json_array_agg("t.e", Some("t.i ASC")),
            "CONCAT('[', COALESCE(GROUP_CONCAT(CAST(t.e AS CHAR) ORDER BY t.i ASC SEPARATOR ','), ''), ']')"
        );
    }

    #[test]
    fn mysql_to_json_text_quotes_text() {
        let d = MySqlDialect;
        assert_eq!(d.to_json_text("'User'"), "JSON_QUOTE(CAST('User' AS CHAR))");
    }

    #[test]
    fn null_ordering_postgres_and_sqlite_keep_explicit_clause() {
        // Default trait body: byte-identical to sqlgen's historical output.
        assert_eq!(PostgresDialect.null_ordering(true), " NULLS FIRST");
        assert_eq!(PostgresDialect.null_ordering(false), " NULLS LAST");
        assert_eq!(SqliteDialect.null_ordering(true), " NULLS FIRST");
        assert_eq!(SqliteDialect.null_ordering(false), " NULLS LAST");
    }

    #[test]
    fn null_ordering_mysql_is_empty() {
        // MySQL has no NULLS FIRST/LAST syntax: the clause is omitted.
        assert_eq!(MySqlDialect.null_ordering(true), "");
        assert_eq!(MySqlDialect.null_ordering(false), "");
    }

    #[test]
    fn any_dialect_mysql_delegates() {
        let d = AnyDialect::Mysql(MySqlDialect);
        assert_eq!(d.quote_ident("users"), "`users`");
        assert_eq!(d.quote_literal("x"), "'x'");
        assert_eq!(d.limit_offset(Some(1), None), " LIMIT 1");
        assert_eq!(d.render_scalar(&s(serde_json::json!(1)), "int"), "1");
        assert_eq!(
            d.json_object(&[("k".into(), "v".into())]),
            "CONCAT('{', '\"k\":', COALESCE(CAST(v AS CHAR), 'null'), '}')"
        );
        assert_eq!(
            d.json_array_agg("x", None),
            "CONCAT('[', COALESCE(GROUP_CONCAT(CAST(x AS CHAR) SEPARATOR ','), ''), ']')"
        );
        assert_eq!(d.to_json_text("x"), "JSON_QUOTE(CAST(x AS CHAR))");
    }
}
