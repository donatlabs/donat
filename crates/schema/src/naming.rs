//! Donat v2 GraphQL naming for tracked tables.

use donat_metadata::{QualifiedTable, TableEntry};

/// The base GraphQL name of a table: `custom_name`/custom root field if set,
/// otherwise `<name>` for the `public` schema and `<schema>_<name>` else.
pub fn table_base_name(entry: &TableEntry) -> String {
    if let Some(config) = &entry.configuration {
        if let Some(custom) = &config.custom_name {
            return custom.clone();
        }
    }
    default_base_name(&entry.table)
}

pub fn default_base_name(table: &QualifiedTable) -> String {
    match table.schema() {
        "public" => table.name().to_string(),
        schema => format!("{schema}_{}", table.name()),
    }
}

pub struct RootNames {
    pub select: String,
    pub select_by_pk: String,
    pub select_aggregate: String,
}

pub fn root_names(entry: &TableEntry) -> RootNames {
    let base = table_base_name(entry);
    let custom = entry
        .configuration
        .as_ref()
        .map(|c| &c.custom_root_fields);

    let get = |key: &str, default: String| -> String {
        custom
            .and_then(|m| m.get(key).cloned())
            .unwrap_or(default)
    };

    RootNames {
        select: get("select", base.clone()),
        select_by_pk: get("select_by_pk", format!("{base}_by_pk")),
        select_aggregate: get("select_aggregate", format!("{base}_aggregate")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(value: serde_json::Value) -> TableEntry {
        serde_json::from_value(value).expect("table entry deserializes")
    }

    #[test]
    fn default_base_name_drops_public_schema() {
        assert_eq!(
            default_base_name(&QualifiedTable::Qualified {
                schema: "public".into(),
                name: "author".into(),
            }),
            "author"
        );
        // Bare names default to the public schema.
        assert_eq!(default_base_name(&QualifiedTable::Name("author".into())), "author");
    }

    #[test]
    fn default_base_name_prefixes_other_schemas() {
        assert_eq!(
            default_base_name(&QualifiedTable::Qualified {
                schema: "sales".into(),
                name: "order".into(),
            }),
            "sales_order"
        );
    }

    #[test]
    fn table_base_name_prefers_custom_name() {
        let e = entry(serde_json::json!({
            "table": { "schema": "sales", "name": "order" },
            "configuration": { "custom_name": "purchase" },
        }));
        assert_eq!(table_base_name(&e), "purchase");
    }

    #[test]
    fn table_base_name_falls_back_to_default_when_no_custom_name() {
        let e = entry(serde_json::json!({
            "table": { "schema": "sales", "name": "order" },
            "configuration": { "custom_root_fields": { "select": "orders" } },
        }));
        assert_eq!(table_base_name(&e), "sales_order");
    }

    #[test]
    fn root_names_derive_from_base_name() {
        let e = entry(serde_json::json!({ "table": "author" }));
        let names = root_names(&e);
        assert_eq!(names.select, "author");
        assert_eq!(names.select_by_pk, "author_by_pk");
        assert_eq!(names.select_aggregate, "author_aggregate");
    }

    #[test]
    fn custom_root_fields_override_individual_roots() {
        let e = entry(serde_json::json!({
            "table": "author",
            "configuration": {
                "custom_name": "writer",
                "custom_root_fields": { "select_by_pk": "writerByPk" },
            },
        }));
        let names = root_names(&e);
        // Overridden root keeps the custom field name verbatim...
        assert_eq!(names.select_by_pk, "writerByPk");
        // ...the rest derive from the custom table name.
        assert_eq!(names.select, "writer");
        assert_eq!(names.select_aggregate, "writer_aggregate");
    }
}
