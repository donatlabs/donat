//! Compile orchestration (filled in Tasks 2.4–2.7).

/// Deserialized engine state held per wasm instance.
pub struct CoreState {
    pub metadata: donat_metadata::Metadata,
    pub catalog: donat_catalog_types::Catalog,
}
