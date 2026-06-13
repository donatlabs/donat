//! Per-backend feature descriptor.
//!
//! Declares which optional SQL features a backend supports so higher layers
//! can branch on capability rather than backend identity.

/// JSON operator support level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonOps {
    /// No JSON column/operator support.
    None,
    /// `json` only (text-backed, no operator class).
    Json,
    /// `jsonb` (binary, indexable operators).
    Jsonb,
}

/// Upsert (`INSERT ... ON CONFLICT`) support level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertKind {
    /// No upsert support.
    None,
    /// Conflict rows can be ignored (do nothing).
    Ignore,
    /// Conflict rows can be ignored or updated.
    Update,
}

/// Feature descriptor for a single backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub json_ops: JsonOps,
    pub geo: bool,
    pub upsert: UpsertKind,
    pub returning: bool,
    pub distinct_on: bool,
    pub lateral: bool,
    pub aggregates: bool,
    pub nested_inserts: bool,
}

/// Capabilities of the Postgres backend.
pub fn postgres() -> Capabilities {
    Capabilities {
        json_ops: JsonOps::Jsonb,
        geo: true,
        upsert: UpsertKind::Update,
        returning: true,
        distinct_on: true,
        lateral: true,
        aggregates: true,
        nested_inserts: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_descriptor_is_correct() {
        let caps = postgres();
        assert_eq!(caps.json_ops, JsonOps::Jsonb);
        assert!(caps.geo);
        assert_eq!(caps.upsert, UpsertKind::Update);
        assert!(caps.returning);
        assert!(caps.distinct_on);
        assert!(caps.lateral);
        assert!(caps.aggregates);
        assert!(caps.nested_inserts);
    }

    #[test]
    fn enums_have_expected_variants() {
        // Spelled out so the variant set is part of the test contract.
        let _ = (JsonOps::None, JsonOps::Json, JsonOps::Jsonb);
        let _ = (UpsertKind::None, UpsertKind::Ignore, UpsertKind::Update);
    }
}
