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
}
