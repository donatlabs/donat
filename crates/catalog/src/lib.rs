//! Postgres introspection (milestone M1).
//!
//! The single place that knows how to read `pg_catalog`. Produces a
//! [`Catalog`] snapshot — tables, columns with their SQL types and
//! nullability, primary keys and foreign keys — which the planner combines
//! with metadata. Nothing downstream talks to `pg_catalog` directly.
//!
//! The snapshot types live in `donat-catalog-types` (wasm-safe) and are
//! re-exported here so existing `donat_catalog::Catalog` paths keep working.

use std::collections::BTreeMap;

use tokio_postgres::Client;

pub use donat_catalog_types::{
    Catalog, ColumnInfo, ForeignKey, FunctionArg, FunctionInfo, TableInfo,
};

const COLUMNS_SQL: &str = r#"
SELECT n.nspname, c.relname, a.attname, t.typname,
       NOT a.attnotnull AS nullable,
       a.atthasdef AS has_default
FROM pg_attribute a
JOIN pg_class c ON a.attrelid = c.oid
JOIN pg_namespace n ON c.relnamespace = n.oid
JOIN pg_type t ON a.atttypid = t.oid
WHERE c.relkind IN ('r', 'v', 'm', 'f', 'p')
  AND a.attnum > 0
  AND NOT a.attisdropped
  AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'hdb_catalog')
  AND n.nspname NOT LIKE 'pg_toast%'
  AND n.nspname NOT LIKE 'pg_temp%'
ORDER BY n.nspname, c.relname, a.attnum
"#;

const PRIMARY_KEYS_SQL: &str = r#"
SELECT n.nspname, c.relname, a.attname
FROM pg_constraint con
JOIN pg_class c ON con.conrelid = c.oid
JOIN pg_namespace n ON c.relnamespace = n.oid
CROSS JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord)
JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum
WHERE con.contype = 'p'
ORDER BY n.nspname, c.relname, k.ord
"#;

const FOREIGN_KEYS_SQL: &str = r#"
SELECT con.conname, n.nspname, c.relname,
       fn.nspname AS fschema, fc.relname AS ftable,
       a.attname AS col, fa.attname AS fcol
FROM pg_constraint con
JOIN pg_class c ON con.conrelid = c.oid
JOIN pg_namespace n ON c.relnamespace = n.oid
JOIN pg_class fc ON con.confrelid = fc.oid
JOIN pg_namespace fn ON fc.relnamespace = fn.oid
CROSS JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord)
JOIN LATERAL unnest(con.confkey) WITH ORDINALITY AS fk(attnum, ord)
  ON fk.ord = k.ord
JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum
JOIN pg_attribute fa ON fa.attrelid = fc.oid AND fa.attnum = fk.attnum
WHERE con.contype = 'f'
ORDER BY con.conname, k.ord
"#;

const FUNCTIONS_SQL: &str = r#"
SELECT n.nspname,
       p.proname,
       p.proretset,
       p.pronargs::int4,
       p.pronargdefaults::int4,
       rt.typname AS ret_type,
       rn.nspname AS ret_rel_schema,
       rc.relname AS ret_rel_name,
       coalesce(p.proargnames, '{}'::text[]) AS arg_names,
       (SELECT coalesce(array_agg(at.typname ORDER BY a.ord), '{}'::name[])
        FROM unnest(p.proargtypes) WITH ORDINALITY AS a(oid, ord)
        JOIN pg_type at ON at.oid = a.oid) AS arg_types,
       (SELECT coalesce(array_agg(coalesce(an.nspname || '.' || ac.relname, '') ORDER BY a.ord), '{}'::text[])
        FROM unnest(p.proargtypes) WITH ORDINALITY AS a(oid, ord)
        JOIN pg_type at ON at.oid = a.oid
        LEFT JOIN pg_class ac ON ac.oid = at.typrelid AND ac.relkind IN ('r', 'v', 'm', 'p')
        LEFT JOIN pg_namespace an ON ac.relnamespace = an.oid) AS arg_composites
FROM pg_proc p
JOIN pg_namespace n ON p.pronamespace = n.oid
JOIN pg_type rt ON p.prorettype = rt.oid
LEFT JOIN pg_class rc ON rc.oid = rt.typrelid AND rc.relkind IN ('r', 'v', 'm', 'p')
LEFT JOIN pg_namespace rn ON rc.relnamespace = rn.oid
WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'hdb_catalog')
  AND n.nspname NOT LIKE 'pg_toast%'
  AND n.nspname NOT LIKE 'pg_temp%'
  AND p.prokind = 'f'
"#;

/// Take a full snapshot of user-visible relations.
pub async fn introspect(client: &Client) -> Result<Catalog, tokio_postgres::Error> {
    let mut catalog = Catalog::default();

    for row in client.query(COLUMNS_SQL, &[]).await? {
        let schema: String = row.get(0);
        let table: String = row.get(1);
        let key = format!("{schema}.{table}");
        let entry = catalog.tables.entry(key).or_insert_with(|| TableInfo {
            schema,
            name: table,
            columns: vec![],
            primary_key: vec![],
            foreign_keys: vec![],
        });
        entry.columns.push(ColumnInfo {
            name: row.get(2),
            pg_type: row.get(3),
            nullable: row.get(4),
            has_default: row.get(5),
        });
    }

    for row in client.query(PRIMARY_KEYS_SQL, &[]).await? {
        let schema: String = row.get(0);
        let table: String = row.get(1);
        if let Some(info) = catalog.tables.get_mut(&format!("{schema}.{table}")) {
            info.primary_key.push(row.get(2));
        }
    }

    for row in client.query(FOREIGN_KEYS_SQL, &[]).await? {
        let conname: String = row.get(0);
        let schema: String = row.get(1);
        let table: String = row.get(2);
        let Some(info) = catalog.tables.get_mut(&format!("{schema}.{table}")) else {
            continue;
        };
        let fk = match info
            .foreign_keys
            .iter_mut()
            .find(|fk| fk.constraint_name == conname)
        {
            Some(fk) => fk,
            None => {
                info.foreign_keys.push(ForeignKey {
                    constraint_name: conname,
                    column_mapping: BTreeMap::new(),
                    referenced_schema: row.get(3),
                    referenced_table: row.get(4),
                });
                info.foreign_keys.last_mut().unwrap()
            }
        };
        fk.column_mapping.insert(row.get(5), row.get(6));
    }

    for row in client.query(FUNCTIONS_SQL, &[]).await? {
        let schema: String = row.get(0);
        let name: String = row.get(1);
        let returns_set: bool = row.get(2);
        let nargs: i32 = row.get(3);
        let ndefaults: i32 = row.get(4);
        let ret_type: String = row.get(5);
        let ret_rel_schema: Option<String> = row.get(6);
        let ret_rel_name: Option<String> = row.get(7);
        let arg_names: Vec<String> = row.get(8);
        let arg_types: Vec<String> = row.get(9);
        let arg_composites: Vec<String> = row.get(10);
        let first_default = (nargs - ndefaults).max(0) as usize;

        let returns_table = ret_rel_schema.zip(ret_rel_name);
        let args = arg_types
            .iter()
            .enumerate()
            .map(|(i, pg_type)| FunctionArg {
                name: arg_names.get(i).filter(|n| !n.is_empty()).cloned(),
                has_default: i >= first_default,
                pg_type: pg_type.clone(),
                composite_of: arg_composites
                    .get(i)
                    .filter(|c| !c.is_empty())
                    .and_then(|c| {
                        c.split_once('.')
                            .map(|(s, t)| (s.to_string(), t.to_string()))
                    }),
            })
            .collect();

        catalog.functions.insert(
            format!("{schema}.{name}"),
            FunctionInfo {
                schema,
                name,
                args,
                returns_scalar: if returns_table.is_none() {
                    Some(ret_type)
                } else {
                    None
                },
                returns_table,
                returns_set,
            },
        );
    }

    Ok(catalog)
}

/// Map a SQLite declared type to the nearest Postgres type name.
///
/// SQLite columns carry a free-form declared type (affinity is derived from
/// it). We normalise to a Postgres type *name* because schema-gen still keys
/// its scalar naming off pg type names — this pg-name mapping is the
/// pragmatic bridge until the IR ScalarType de-leak lands. Matching is
/// case-insensitive and any `(...)` size/precision suffix is stripped.
fn sqlite_type_to_pg(declared: &str) -> &'static str {
    let base = declared.split('(').next().unwrap_or(declared).trim();
    match base.to_ascii_uppercase().as_str() {
        "INTEGER" | "INT" => "int4",
        "BIGINT" => "int8",
        "REAL" | "DOUBLE" | "FLOAT" => "float8",
        "TEXT" | "VARCHAR" | "CHAR" | "CLOB" | "" => "text",
        "BLOB" => "bytea",
        "NUMERIC" | "DECIMAL" => "numeric",
        "BOOLEAN" | "BOOL" => "bool",
        "DATE" => "date",
        "DATETIME" | "TIMESTAMP" => "timestamp",
        _ => "text",
    }
}

/// Take a full snapshot of a SQLite database, producing the same [`Catalog`]
/// shape as the Postgres [`introspect`] so downstream schema-gen and planning
/// run unchanged. Every table lives in the synthetic `"main"` schema
/// (SQLite's default database); SQLite has no stored functions, so
/// [`Catalog::functions`] stays empty.
pub fn sqlite_introspect(conn: &rusqlite::Connection) -> rusqlite::Result<Catalog> {
    let mut catalog = Catalog::default();

    // Tables (skip SQLite's internal bookkeeping tables).
    let table_names: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    for table in table_names {
        let mut columns = Vec::new();
        // (cid, name, type, notnull, dflt_value, pk) ordered by pk index.
        let mut pk: Vec<(i64, String)> = Vec::new();

        {
            let mut stmt = conn.prepare(&format!("PRAGMA table_info('{table}')"))?;
            let rows = stmt.query_map([], |row| {
                let name: String = row.get(1)?;
                let decl_type: String = row.get(2)?;
                let notnull: i64 = row.get(3)?;
                let dflt: Option<String> = row.get(4)?;
                let pk_index: i64 = row.get(5)?;
                Ok((name, decl_type, notnull, dflt, pk_index))
            })?;
            for row in rows {
                let (name, decl_type, notnull, dflt, pk_index) = row?;
                if pk_index > 0 {
                    pk.push((pk_index, name.clone()));
                }
                columns.push(ColumnInfo {
                    name,
                    pg_type: sqlite_type_to_pg(&decl_type).to_string(),
                    nullable: notnull == 0,
                    has_default: dflt.is_some(),
                });
            }
        }

        pk.sort_by_key(|(idx, _)| *idx);
        let primary_key = pk.into_iter().map(|(_, name)| name).collect();

        // Foreign keys: (id, seq, table, from, to, ...). Group by id.
        let mut foreign_keys: Vec<ForeignKey> = Vec::new();
        let mut fk_ids: Vec<i64> = Vec::new();
        {
            let mut stmt = conn.prepare(&format!("PRAGMA foreign_key_list('{table}')"))?;
            let rows = stmt.query_map([], |row| {
                let id: i64 = row.get(0)?;
                let ref_table: String = row.get(2)?;
                let from: String = row.get(3)?;
                let to: String = row.get(4)?;
                Ok((id, ref_table, from, to))
            })?;
            for row in rows {
                let (id, ref_table, from, to) = row?;
                let pos = match fk_ids.iter().position(|&i| i == id) {
                    Some(pos) => pos,
                    None => {
                        fk_ids.push(id);
                        // SQLite FKs are unnamed — synthesise a stable name.
                        foreign_keys.push(ForeignKey {
                            constraint_name: format!("{table}_{id}_fkey"),
                            column_mapping: BTreeMap::new(),
                            referenced_schema: "main".to_string(),
                            referenced_table: ref_table,
                        });
                        foreign_keys.len() - 1
                    }
                };
                foreign_keys[pos].column_mapping.insert(from, to);
            }
        }

        catalog.tables.insert(
            format!("main.{table}"),
            TableInfo {
                schema: "main".to_string(),
                name: table,
                columns,
                primary_key,
                foreign_keys,
            },
        );
    }

    Ok(catalog)
}

/// Take a full snapshot of a MySQL database (the schema named by `schema`,
/// e.g. `"donat"`), producing the same [`Catalog`] shape as the Postgres
/// [`introspect`] so downstream schema-gen and planning run unchanged. The
/// MySQL database name plays the role Postgres schemas / SQLite's `"main"`
/// play; pass it explicitly so callers control which database is tracked.
///
/// Column types are read from `information_schema.columns.DATA_TYPE` and
/// normalised to the nearest Postgres type *name* via [`mysql_type_to_pg`] —
/// the same pragmatic pg-name bridge SQLite uses, because schema-gen still
/// keys scalar naming off pg type names. MySQL has no stored row-returning
/// functions modelled here, so [`Catalog::functions`] stays empty.
pub fn mysql_introspect(conn: &mut mysql::Conn, schema: &str) -> mysql::Result<Catalog> {
    use mysql::prelude::Queryable;

    let mut catalog = Catalog::default();

    // Columns, ordered by table then ordinal position. COLUMN_DEFAULT is SQL
    // NULL when the column has no default, so deserialize it as Option.
    let cols: Vec<(String, String, String, String, Option<String>)> = conn.exec(
        "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_DEFAULT \
         FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = ? \
         ORDER BY TABLE_NAME, ORDINAL_POSITION",
        (schema,),
    )?;
    for (table, column, data_type, is_nullable, default) in cols {
        let key = format!("{schema}.{table}");
        let entry = catalog.tables.entry(key).or_insert_with(|| TableInfo {
            schema: schema.to_string(),
            name: table.clone(),
            columns: vec![],
            primary_key: vec![],
            foreign_keys: vec![],
        });
        entry.columns.push(ColumnInfo {
            name: column,
            pg_type: mysql_type_to_pg(&data_type).to_string(),
            // IS_NULLABLE is the string 'YES' or 'NO'.
            nullable: is_nullable.eq_ignore_ascii_case("YES"),
            has_default: default.is_some(),
        });
    }

    // Primary keys, in key ordinal order.
    let pks: Vec<(String, String)> = conn.exec(
        "SELECT TABLE_NAME, COLUMN_NAME \
         FROM information_schema.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND CONSTRAINT_NAME = 'PRIMARY' \
         ORDER BY TABLE_NAME, ORDINAL_POSITION",
        (schema,),
    )?;
    for (table, column) in pks {
        if let Some(info) = catalog.tables.get_mut(&format!("{schema}.{table}")) {
            info.primary_key.push(column);
        }
    }

    // Foreign keys: KEY_COLUMN_USAGE rows with a REFERENCED_TABLE_NAME, grouped
    // by constraint name and ordered by position within the constraint.
    let fks: Vec<(String, String, String, String, String)> = conn.exec(
        "SELECT CONSTRAINT_NAME, TABLE_NAME, COLUMN_NAME, \
                REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME \
         FROM information_schema.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL \
         ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
        (schema,),
    )?;
    // We need the referenced column per row too; fetch it in the same query
    // order. (Selected separately to keep the tuple arity readable.)
    let fk_refs: Vec<String> = conn.exec(
        "SELECT REFERENCED_COLUMN_NAME \
         FROM information_schema.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL \
         ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
        (schema,),
    )?;
    for ((conname, table, from_col, ref_schema, ref_table), to_col) in
        fks.into_iter().zip(fk_refs)
    {
        let Some(info) = catalog.tables.get_mut(&format!("{schema}.{table}")) else {
            continue;
        };
        let fk = match info
            .foreign_keys
            .iter_mut()
            .find(|fk| fk.constraint_name == conname)
        {
            Some(fk) => fk,
            None => {
                info.foreign_keys.push(ForeignKey {
                    constraint_name: conname,
                    column_mapping: BTreeMap::new(),
                    referenced_schema: ref_schema,
                    referenced_table: ref_table,
                });
                info.foreign_keys.last_mut().unwrap()
            }
        };
        fk.column_mapping.insert(from_col, to_col);
    }

    Ok(catalog)
}

/// Map a MySQL `information_schema.columns.DATA_TYPE` value to the nearest
/// Postgres type name (pg_catalog `typname` form). MySQL `DATA_TYPE` carries
/// no size/precision suffix (that lives in `COLUMN_TYPE`), so no stripping is
/// needed; matching is case-insensitive. This pg-name mapping is the same
/// pragmatic bridge SQLite uses (see [`sqlite_type_to_pg`]).
fn mysql_type_to_pg(data_type: &str) -> &'static str {
    match data_type.to_ascii_lowercase().as_str() {
        "tinyint" | "smallint" => "int2",
        "mediumint" | "int" | "integer" => "int4",
        "bigint" => "int8",
        "float" => "float4",
        "double" | "double precision" | "real" => "float8",
        "decimal" | "numeric" | "dec" | "fixed" => "numeric",
        "bool" | "boolean" => "bool",
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum"
        | "set" => "text",
        "json" => "json",
        "datetime" | "timestamp" => "timestamp",
        "date" => "date",
        "time" => "time",
        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" => "bytea",
        _ => "text",
    }
}

#[cfg(test)]
mod sqlite_tests {
    use super::*;

    #[test]
    fn sqlite_introspect_produces_pg_shaped_catalog() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE author (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                bio VARCHAR(255),
                rating REAL,
                active BOOLEAN DEFAULT 1
            );
            CREATE TABLE article (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                word_count BIGINT,
                author_id INTEGER NOT NULL REFERENCES author(id)
            );
            "#,
        )
        .unwrap();

        let catalog = sqlite_introspect(&conn).unwrap();

        // Tables keyed by "main.<name>".
        assert!(catalog.functions.is_empty());
        let author = catalog.table("main", "author").expect("author table");
        let article = catalog.table("main", "article").expect("article table");
        assert_eq!(author.schema, "main");
        assert_eq!(author.name, "author");

        // Column pg_type mappings.
        let col = |t: &TableInfo, n: &str| t.column(n).unwrap().clone();
        assert_eq!(col(author, "id").pg_type, "int4");
        assert_eq!(col(author, "name").pg_type, "text");
        assert_eq!(col(author, "bio").pg_type, "text"); // VARCHAR(255) -> text
        assert_eq!(col(author, "rating").pg_type, "float8");
        assert_eq!(col(author, "active").pg_type, "bool");
        assert_eq!(col(article, "word_count").pg_type, "int8");

        // Nullability / defaults.
        assert!(!col(author, "name").nullable);
        assert!(col(author, "bio").nullable);
        assert!(col(author, "active").has_default);
        assert!(!col(author, "name").has_default);

        // Primary keys.
        assert_eq!(author.primary_key, vec!["id".to_string()]);
        assert_eq!(article.primary_key, vec!["id".to_string()]);

        // Foreign key: synthetic name, main schema, column_mapping from->to.
        assert_eq!(article.foreign_keys.len(), 1);
        let fk = &article.foreign_keys[0];
        assert_eq!(fk.constraint_name, "article_0_fkey");
        assert_eq!(fk.referenced_schema, "main");
        assert_eq!(fk.referenced_table, "author");
        let mut expected = BTreeMap::new();
        expected.insert("author_id".to_string(), "id".to_string());
        assert_eq!(fk.column_mapping, expected);
    }
}
