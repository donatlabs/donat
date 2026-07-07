//! Logical, backend-neutral scalar type system.
//!
//! De-leaks the stringly-typed Postgres `pg_type` (e.g. `int4`, `jsonb`)
//! reported by catalog introspection into a closed set of logical variants.
//! The variant set is derived from the native Postgres type names that
//! actually appear in `crates/catalog` and `crates/sqlgen` (pg_catalog
//! `typname` form), with `Other` as a catch-all for anything unmapped.

/// A logical scalar type, independent of any one backend's native naming.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScalarType {
    SmallInt,
    Int,
    BigInt,
    Float,
    Double,
    Numeric,
    Bool,
    Text,
    Uuid,
    Json,
    Jsonb,
    Timestamp,
    TimestampTz,
    Date,
    Time,
    Bytes,
    Geometry,
    Geography,
    /// Native type name that has no logical mapping yet.
    Other(String),
}

/// Map a native Postgres type name (pg_catalog `typname`, e.g. `int4`,
/// `varchar`, `jsonb`) to its logical [`ScalarType`]. Unknown names map to
/// [`ScalarType::Other`] carrying the original native name verbatim.
pub fn postgres_scalar(native: &str) -> ScalarType {
    match native {
        "int2" => ScalarType::SmallInt,
        "int4" => ScalarType::Int,
        "int8" => ScalarType::BigInt,
        "float4" => ScalarType::Float,
        "float8" => ScalarType::Double,
        "numeric" => ScalarType::Numeric,
        "bool" => ScalarType::Bool,
        "text" | "varchar" | "bpchar" => ScalarType::Text,
        "uuid" => ScalarType::Uuid,
        "json" => ScalarType::Json,
        "jsonb" => ScalarType::Jsonb,
        "timestamp" => ScalarType::Timestamp,
        "timestamptz" => ScalarType::TimestampTz,
        "date" => ScalarType::Date,
        "time" => ScalarType::Time,
        "bytea" => ScalarType::Bytes,
        "geometry" => ScalarType::Geometry,
        "geography" => ScalarType::Geography,
        other => ScalarType::Other(other.to_string()),
    }
}

/// Map a SQLite declared/native type name to its logical [`ScalarType`].
/// SQLite typing is dynamic and declared types are free-form, so matching is
/// case-insensitive over the common declared spellings. Unknown names map to
/// [`ScalarType::Other`] carrying the original native name verbatim.
pub fn sqlite_scalar(native: &str) -> ScalarType {
    match native.to_ascii_uppercase().as_str() {
        "INTEGER" | "INT" => ScalarType::Int,
        "REAL" | "DOUBLE" | "FLOAT" => ScalarType::Double,
        "TEXT" | "VARCHAR" | "CHAR" | "CLOB" => ScalarType::Text,
        "BLOB" => ScalarType::Bytes,
        "NUMERIC" | "DECIMAL" => ScalarType::Numeric,
        "BOOLEAN" => ScalarType::Bool,
        "DATE" => ScalarType::Date,
        "DATETIME" | "TIMESTAMP" => ScalarType::Timestamp,
        _ => ScalarType::Other(native.to_string()),
    }
}

/// Map a MySQL `information_schema.columns.DATA_TYPE` value to its logical
/// [`ScalarType`]. MySQL `DATA_TYPE` is the base type name without the size
/// suffix (e.g. `int`, `varchar`, `decimal`), reported in lower case, but we
/// match case-insensitively for safety. Unknown names map to
/// [`ScalarType::Other`] carrying the original native name verbatim.
pub fn mysql_scalar(native: &str) -> ScalarType {
    match native.to_ascii_lowercase().as_str() {
        "tinyint" | "smallint" => ScalarType::SmallInt,
        "mediumint" | "int" | "integer" => ScalarType::Int,
        "bigint" => ScalarType::BigInt,
        "float" => ScalarType::Float,
        "double" | "double precision" | "real" => ScalarType::Double,
        "decimal" | "numeric" | "dec" | "fixed" => ScalarType::Numeric,
        "bool" | "boolean" => ScalarType::Bool,
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set" => {
            ScalarType::Text
        }
        "json" => ScalarType::Json,
        "datetime" | "timestamp" => ScalarType::Timestamp,
        "date" => ScalarType::Date,
        "time" => ScalarType::Time,
        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" => {
            ScalarType::Bytes
        }
        _ => ScalarType::Other(native.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_integer_family() {
        assert_eq!(postgres_scalar("int2"), ScalarType::SmallInt);
        assert_eq!(postgres_scalar("int4"), ScalarType::Int);
        assert_eq!(postgres_scalar("int8"), ScalarType::BigInt);
    }

    #[test]
    fn maps_float_family() {
        assert_eq!(postgres_scalar("float4"), ScalarType::Float);
        assert_eq!(postgres_scalar("float8"), ScalarType::Double);
        assert_eq!(postgres_scalar("numeric"), ScalarType::Numeric);
    }

    #[test]
    fn maps_bool_and_text_family() {
        assert_eq!(postgres_scalar("bool"), ScalarType::Bool);
        assert_eq!(postgres_scalar("text"), ScalarType::Text);
        assert_eq!(postgres_scalar("varchar"), ScalarType::Text);
        assert_eq!(postgres_scalar("bpchar"), ScalarType::Text);
    }

    #[test]
    fn maps_uuid_and_json_family() {
        assert_eq!(postgres_scalar("uuid"), ScalarType::Uuid);
        assert_eq!(postgres_scalar("json"), ScalarType::Json);
        assert_eq!(postgres_scalar("jsonb"), ScalarType::Jsonb);
    }

    #[test]
    fn maps_temporal_family() {
        assert_eq!(postgres_scalar("timestamp"), ScalarType::Timestamp);
        assert_eq!(postgres_scalar("timestamptz"), ScalarType::TimestampTz);
        assert_eq!(postgres_scalar("date"), ScalarType::Date);
        assert_eq!(postgres_scalar("time"), ScalarType::Time);
    }

    #[test]
    fn maps_bytes_and_geo_family() {
        assert_eq!(postgres_scalar("bytea"), ScalarType::Bytes);
        assert_eq!(postgres_scalar("geometry"), ScalarType::Geometry);
        assert_eq!(postgres_scalar("geography"), ScalarType::Geography);
    }

    #[test]
    fn unknown_native_falls_back_to_other() {
        assert_eq!(
            postgres_scalar("citext"),
            ScalarType::Other("citext".to_string())
        );
        // Postgres array notation is not specially mapped here.
        assert_eq!(
            postgres_scalar("_int4"),
            ScalarType::Other("_int4".to_string())
        );
    }

    // ---- sqlite_scalar ----------------------------------------------------

    #[test]
    fn sqlite_maps_integer_family() {
        assert_eq!(sqlite_scalar("INTEGER"), ScalarType::Int);
        assert_eq!(sqlite_scalar("INT"), ScalarType::Int);
    }

    #[test]
    fn sqlite_maps_real_family() {
        assert_eq!(sqlite_scalar("REAL"), ScalarType::Double);
        assert_eq!(sqlite_scalar("DOUBLE"), ScalarType::Double);
        assert_eq!(sqlite_scalar("FLOAT"), ScalarType::Double);
    }

    #[test]
    fn sqlite_maps_text_family() {
        assert_eq!(sqlite_scalar("TEXT"), ScalarType::Text);
        assert_eq!(sqlite_scalar("VARCHAR"), ScalarType::Text);
        assert_eq!(sqlite_scalar("CHAR"), ScalarType::Text);
        assert_eq!(sqlite_scalar("CLOB"), ScalarType::Text);
    }

    #[test]
    fn sqlite_maps_blob_and_numeric() {
        assert_eq!(sqlite_scalar("BLOB"), ScalarType::Bytes);
        assert_eq!(sqlite_scalar("NUMERIC"), ScalarType::Numeric);
        assert_eq!(sqlite_scalar("DECIMAL"), ScalarType::Numeric);
    }

    #[test]
    fn sqlite_maps_bool_and_temporal() {
        assert_eq!(sqlite_scalar("BOOLEAN"), ScalarType::Bool);
        assert_eq!(sqlite_scalar("DATE"), ScalarType::Date);
        assert_eq!(sqlite_scalar("DATETIME"), ScalarType::Timestamp);
        assert_eq!(sqlite_scalar("TIMESTAMP"), ScalarType::Timestamp);
    }

    #[test]
    fn sqlite_is_case_insensitive() {
        assert_eq!(sqlite_scalar("integer"), ScalarType::Int);
        assert_eq!(sqlite_scalar("Text"), ScalarType::Text);
        assert_eq!(sqlite_scalar("Boolean"), ScalarType::Bool);
    }

    #[test]
    fn sqlite_unknown_falls_back_to_other_verbatim() {
        // Unknown declared type keeps its original (non-uppercased) spelling.
        assert_eq!(
            sqlite_scalar("citext"),
            ScalarType::Other("citext".to_string())
        );
    }

    // ---- mysql_scalar -----------------------------------------------------

    #[test]
    fn mysql_maps_integer_family() {
        assert_eq!(mysql_scalar("tinyint"), ScalarType::SmallInt);
        assert_eq!(mysql_scalar("smallint"), ScalarType::SmallInt);
        assert_eq!(mysql_scalar("mediumint"), ScalarType::Int);
        assert_eq!(mysql_scalar("int"), ScalarType::Int);
        assert_eq!(mysql_scalar("integer"), ScalarType::Int);
        assert_eq!(mysql_scalar("bigint"), ScalarType::BigInt);
    }

    #[test]
    fn mysql_maps_float_and_decimal_family() {
        assert_eq!(mysql_scalar("float"), ScalarType::Float);
        assert_eq!(mysql_scalar("double"), ScalarType::Double);
        assert_eq!(mysql_scalar("real"), ScalarType::Double);
        assert_eq!(mysql_scalar("decimal"), ScalarType::Numeric);
        assert_eq!(mysql_scalar("numeric"), ScalarType::Numeric);
    }

    #[test]
    fn mysql_maps_bool_and_text_family() {
        assert_eq!(mysql_scalar("bool"), ScalarType::Bool);
        assert_eq!(mysql_scalar("boolean"), ScalarType::Bool);
        assert_eq!(mysql_scalar("varchar"), ScalarType::Text);
        assert_eq!(mysql_scalar("char"), ScalarType::Text);
        assert_eq!(mysql_scalar("text"), ScalarType::Text);
        assert_eq!(mysql_scalar("longtext"), ScalarType::Text);
    }

    #[test]
    fn mysql_maps_json_temporal_and_binary() {
        assert_eq!(mysql_scalar("json"), ScalarType::Json);
        assert_eq!(mysql_scalar("datetime"), ScalarType::Timestamp);
        assert_eq!(mysql_scalar("timestamp"), ScalarType::Timestamp);
        assert_eq!(mysql_scalar("date"), ScalarType::Date);
        assert_eq!(mysql_scalar("time"), ScalarType::Time);
        assert_eq!(mysql_scalar("blob"), ScalarType::Bytes);
        assert_eq!(mysql_scalar("varbinary"), ScalarType::Bytes);
    }

    #[test]
    fn mysql_is_case_insensitive() {
        assert_eq!(mysql_scalar("INT"), ScalarType::Int);
        assert_eq!(mysql_scalar("VarChar"), ScalarType::Text);
        assert_eq!(mysql_scalar("JSON"), ScalarType::Json);
    }

    #[test]
    fn mysql_unknown_falls_back_to_other_verbatim() {
        assert_eq!(
            mysql_scalar("geometry"),
            ScalarType::Other("geometry".to_string())
        );
    }
}
