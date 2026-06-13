//! Donat v2-compatible metadata: the typed model and the YAML directory
//! loader. This crate is the single source of truth for "what the user
//! configured"; everything downstream (schema generation, permissions,
//! sqlgen) consumes these types and never re-reads YAML.

mod loader;
mod types;

pub use loader::{LoadError, load_metadata_dir};
pub use types::*;
