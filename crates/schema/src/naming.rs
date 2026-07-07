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

/// GraphQL-visible column field name for a physical database column.
///
/// Hasura metadata supports both the v2 `custom_column_names` map and the
/// older `column_config.<column>.custom_name` shape. Prefer the explicit v2
/// map, then fall back to `column_config`, then to the database column name.
pub fn column_graphql_name(entry: &TableEntry, db_name: &str) -> String {
    let Some(config) = &entry.configuration else {
        return db_name.to_string();
    };
    if let Some(custom) = config.custom_column_names.get(db_name) {
        return custom.clone();
    }
    if let Some(custom) = config
        .column_config
        .get(db_name)
        .and_then(|c| c.custom_name.clone())
    {
        return custom;
    }
    db_name.to_string()
}

/// Resolve a GraphQL-visible column name back to its physical database column.
pub fn column_db_name(entry: &TableEntry, graphql_name: &str) -> String {
    let Some(config) = &entry.configuration else {
        return graphql_name.to_string();
    };
    if let Some((db, _)) = config
        .custom_column_names
        .iter()
        .find(|(_, custom)| custom.as_str() == graphql_name)
    {
        return db.clone();
    }
    if let Some((db, _)) = config.column_config.iter().find(|(_, column)| {
        column
            .custom_name
            .as_deref()
            .is_some_and(|custom| custom == graphql_name)
    }) {
        return db.clone();
    }
    graphql_name.to_string()
}

pub struct RootNames {
    pub select: String,
    pub select_by_pk: String,
    pub select_aggregate: String,
}

pub fn root_names(entry: &TableEntry) -> RootNames {
    let base = table_base_name(entry);
    let custom = entry.configuration.as_ref().map(|c| &c.custom_root_fields);

    let get = |key: &str, default: String| -> String {
        custom.and_then(|m| m.get(key).cloned()).unwrap_or(default)
    };

    RootNames {
        select: get("select", base.clone()),
        select_by_pk: get("select_by_pk", format!("{base}_by_pk")),
        select_aggregate: get("select_aggregate", format!("{base}_aggregate")),
    }
}

/// The CRUD root field names for a table — the `select` query root and the
/// `insert`/`update`/`delete` mutation roots — honoring `custom_root_fields`.
/// The type-name base (for `<base>_bool_exp` etc.) is [`table_base_name`].
pub struct CrudRoots {
    pub query: String,
    pub insert: String,
    pub update: String,
    pub delete: String,
}

pub fn crud_roots(entry: &TableEntry) -> CrudRoots {
    let base = table_base_name(entry);
    let custom = entry.configuration.as_ref().map(|c| &c.custom_root_fields);
    let get = |key: &str, default: String| -> String {
        custom.and_then(|m| m.get(key).cloned()).unwrap_or(default)
    };
    CrudRoots {
        query: get("select", base.clone()),
        insert: get("insert", format!("insert_{base}")),
        update: get("update", format!("update_{base}")),
        delete: get("delete", format!("delete_{base}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(value: serde_json::Value) -> TableEntry {
        serde_json::from_value(value).expect("table entry deserializes")
    }

    #[test]
    fn crud_roots_default_naming() {
        let e = entry(serde_json::json!({ "table": "pet" }));
        let r = crud_roots(&e);
        assert_eq!(r.query, "pet");
        assert_eq!(r.insert, "insert_pet");
        assert_eq!(r.update, "update_pet");
        assert_eq!(r.delete, "delete_pet");
    }

    #[test]
    fn crud_roots_honor_custom_root_fields_and_custom_name() {
        let e = entry(serde_json::json!({
            "table": { "schema": "public", "name": "gadget" },
            "configuration": {
                "custom_name": "widget",
                "custom_root_fields": { "select": "all_widgets", "insert": "add_widget" },
            },
        }));
        let r = crud_roots(&e);
        // Overridden roots keep the custom field name verbatim...
        assert_eq!(r.query, "all_widgets");
        assert_eq!(r.insert, "add_widget");
        // ...the rest derive from the custom table name (custom_name).
        assert_eq!(r.update, "update_widget");
        assert_eq!(r.delete, "delete_widget");
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
        assert_eq!(
            default_base_name(&QualifiedTable::Name("author".into())),
            "author"
        );
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
    fn column_names_prefer_custom_column_names() {
        let e = entry(serde_json::json!({
            "table": "author",
            "configuration": {
                "custom_column_names": { "display_name": "displayName" },
                "column_config": { "display_name": { "custom_name": "legacyDisplayName" } },
            },
        }));
        assert_eq!(column_graphql_name(&e, "display_name"), "displayName");
        assert_eq!(column_db_name(&e, "displayName"), "display_name");
    }

    #[test]
    fn column_names_fallback_to_column_config_custom_name() {
        let e = entry(serde_json::json!({
            "table": "author",
            "configuration": {
                "column_config": { "display_name": { "custom_name": "displayName" } },
            },
        }));
        assert_eq!(column_graphql_name(&e, "display_name"), "displayName");
        assert_eq!(column_db_name(&e, "displayName"), "display_name");
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
