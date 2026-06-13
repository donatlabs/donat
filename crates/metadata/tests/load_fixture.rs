use std::path::Path;

use donat_metadata::{Columns, DatabaseUrl, QualifiedTable, SourceKind, load_metadata_dir};

fn fixture_dir() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/metadata"
    ))
}

#[test]
fn loads_v2_metadata_directory() {
    let md = load_metadata_dir(fixture_dir()).expect("metadata should load");

    assert_eq!(md.version, 3);
    assert_eq!(md.sources.len(), 1);

    let source = &md.sources[0];
    assert_eq!(source.name, "default");
    assert_eq!(source.kind, SourceKind::Postgres);
    match &source.configuration.connection_info.database_url {
        DatabaseUrl::FromEnv { from_env } => {
            assert_eq!(from_env, "HASURA_GRAPHQL_DATABASE_URL")
        }
        other => panic!("expected from_env database url, got {other:?}"),
    }

    assert_eq!(source.tables.len(), 2);

    let author = &source.tables[0];
    assert_eq!(author.table.to_string(), "public.author");
    assert_eq!(author.array_relationships.len(), 1);
    let articles_rel = &author.array_relationships[0];
    assert_eq!(articles_rel.name, "articles");
    let fk = articles_rel
        .using
        .foreign_key_constraint_on
        .as_ref()
        .expect("fk-based relationship");
    assert_eq!(fk.column.as_deref(), Some("author_id"));
    assert_eq!(
        fk.table,
        QualifiedTable::Qualified {
            schema: "public".into(),
            name: "article".into()
        }
    );

    let author_select = &author.select_permissions[0];
    assert_eq!(author_select.role, "user");
    assert_eq!(
        author_select.permission.columns,
        Columns::List(vec!["id".into(), "name".into()])
    );
    assert_eq!(
        author_select.permission.filter["id"]["_eq"],
        serde_json::json!("X-Hasura-User-Id")
    );

    let article = &source.tables[1];
    assert_eq!(article.table.to_string(), "public.article");
    assert_eq!(article.object_relationships.len(), 1);

    let article_select = &article.select_permissions[0];
    assert_eq!(article_select.permission.columns, Columns::Star);
    assert_eq!(article_select.permission.limit, Some(100));
    assert!(article_select.permission.allow_aggregations);
    assert!(article_select.permission.filter["_or"].is_array());
}

#[test]
fn loads_actions_and_custom_types() {
    let md = load_metadata_dir(fixture_dir()).expect("metadata should load");

    assert_eq!(md.actions.len(), 2);
    let mirror = &md.actions[0];
    assert_eq!(mirror.name, "mirror");
    assert_eq!(mirror.definition.output_type, "OutObject");
    assert_eq!(mirror.definition.arguments.len(), 1);
    assert_eq!(mirror.definition.arguments[0].type_, "InObject!");
    assert_eq!(mirror.permissions[0].role, "user");

    // `arguments:` written as an explicit null parses as "no arguments".
    let null_response = &md.actions[1];
    assert!(null_response.definition.arguments.is_empty());

    assert_eq!(md.custom_types.input_objects.len(), 1);
    assert_eq!(md.custom_types.objects.len(), 1);
    assert_eq!(md.custom_types.objects[0].name, "OutObject");
    assert_eq!(md.custom_types.scalars[0].name, "myCustomScalar");
}
