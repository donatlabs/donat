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
    /// Render a JSON scalar as a SQL literal cast to the column's native
    /// type. Mirrors sqlgen's `scalar_sql`: NULL / TRUE / FALSE, numbers and
    /// strings cast to `::"ty"`, JSON arrays/objects targeting json/jsonb,
    /// and the geometry/geography GeoJSON special-case.
    fn render_scalar(&self, scalar: &donat_ir::Scalar, native_type: &str) -> String;
}

/// Postgres dialect. Output matches `crates/sqlgen` exactly.
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
                if *b { "TRUE".into() } else { "FALSE".into() }
            }
            serde_json::Value::Number(n) => format!("({n})::{ty}"),
            serde_json::Value::String(s) => format!("({})::{ty}", self.quote_literal(s)),
            // arrays/objects target json/jsonb columns
            other => format!("({})::{ty}", self.quote_literal(&other.to_string())),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(d.render_scalar(&s(serde_json::json!(false)), "bool"), "FALSE");
    }

    #[test]
    fn render_scalar_number() {
        let d = PostgresDialect;
        assert_eq!(d.render_scalar(&s(serde_json::json!(42)), "int4"), "(42)::\"int4\"");
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
            format!(
                "(ST_GeomFromGeoJSON('{}'))::\"geometry\"",
                geo.to_string()
            )
        );
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
}
