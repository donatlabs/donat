//! Pure catalog snapshot types.
//!
//! The serde data shapes produced by introspection and consumed by the
//! planner. No I/O, no drivers — this crate compiles to `wasm32` so the
//! wasm-core can deserialize a [`Catalog`] snapshot. The introspection
//! drivers live in `donat-catalog`, which re-exports these types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    /// Keyed by "schema.table".
    pub tables: BTreeMap<String, TableInfo>,
    /// Keyed by "schema.function". Overloads are not supported.
    pub functions: BTreeMap<String, FunctionInfo>,
}

impl Catalog {
    pub fn table(&self, schema: &str, name: &str) -> Option<&TableInfo> {
        self.tables.get(&format!("{schema}.{name}"))
    }

    pub fn function(&self, schema: &str, name: &str) -> Option<&FunctionInfo> {
        self.functions.get(&format!("{schema}.{name}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionInfo {
    pub schema: String,
    pub name: String,
    pub args: Vec<FunctionArg>,
    /// If the function returns (setof) a known table's row type.
    pub returns_table: Option<(String, String)>,
    pub returns_set: bool,
    /// Scalar return type name when not returning a table row.
    pub returns_scalar: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionArg {
    pub name: Option<String>,
    /// The argument has a DEFAULT and may be omitted from calls.
    #[serde(default)]
    pub has_default: bool,
    pub pg_type: String,
    /// Set when the argument type is the row type of a known table.
    pub composite_of: Option<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Vec<String>,
    pub foreign_keys: Vec<ForeignKey>,
}

impl TableInfo {
    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.name == name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    /// Postgres type name as reported by pg_catalog (e.g. `int4`, `text`).
    pub pg_type: String,
    pub nullable: bool,
    pub has_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub constraint_name: String,
    /// Local column -> referenced column.
    pub column_mapping: BTreeMap<String, String>,
    pub referenced_schema: String,
    pub referenced_table: String,
}
