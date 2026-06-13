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
}
