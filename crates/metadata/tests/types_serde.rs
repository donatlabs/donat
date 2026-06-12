//! Unit tests for the metadata type model: serde behaviour that the engine
//! relies on — legacy `$op` permission spellings, `Columns` star/list,
//! serde defaults, RemoteSchema round-trips, and acceptance of a full v2
//! metadata document. Pure deserialization; no database.

use std::path::Path;

use dist_metadata::{
    Columns, DatabaseUrl, InsertPermission, Metadata, PermissionEntry, QualifiedTable,
    RemoteSchema, SelectPermission, SourceKind, load_metadata_dir,
};
use serde_json::json;

#[test]
fn legacy_dollar_op_filter_spellings_are_accepted_verbatim() {
    // Pre-v1 Hasura wrote operators as $eq/$or/...; BoolExp stays untyped,
    // so legacy spellings must deserialize and survive unchanged.
    let yaml = "\
role: user
permission:
  columns: \"*\"
  filter:
    $or:
      - id:
          $eq: X-Hasura-User-Id
      - is_public:
          $eq: true
";
    let entry: PermissionEntry<SelectPermission> =
        serde_yaml::from_str(yaml).expect("legacy $op filter must deserialize");
    assert_eq!(entry.role, "user");
    assert_eq!(
        entry.permission.filter["$or"][0]["id"]["$eq"],
        json!("X-Hasura-User-Id")
    );
    assert_eq!(
        entry.permission.filter["$or"][1]["is_public"]["$eq"],
        json!(true)
    );
}

#[test]
fn columns_star_vs_list() {
    let star: Columns = serde_yaml::from_str("\"*\"").unwrap();
    assert_eq!(star, Columns::Star);

    let list: Columns = serde_yaml::from_str("[id, name]").unwrap();
    assert_eq!(list, Columns::List(vec!["id".into(), "name".into()]));

    let empty: Columns = serde_yaml::from_str("[]").unwrap();
    assert_eq!(empty, Columns::List(vec![]));
}

#[test]
fn columns_arbitrary_string_is_rejected() {
    let err = serde_yaml::from_str::<Columns>("\"id\"").unwrap_err();
    assert!(
        err.to_string().contains("expected \"*\" or a list of columns"),
        "unexpected error: {err}"
    );
}

#[test]
fn columns_round_trip_serialization() {
    assert_eq!(serde_json::to_value(Columns::Star).unwrap(), json!("*"));
    assert_eq!(
        serde_json::to_value(Columns::List(vec!["a".into()])).unwrap(),
        json!(["a"])
    );
}

#[test]
fn insert_permission_defaults() {
    // Older metadata omits everything but check; absent columns mean "*",
    // backend_only defaults to false, BoolExp defaults to JSON null.
    let perm: InsertPermission = serde_yaml::from_str("{}").unwrap();
    assert_eq!(perm.columns, Columns::Star);
    assert!(!perm.backend_only);
    assert!(perm.set.is_empty());
    assert_eq!(perm.check, serde_json::Value::Null);
}

#[test]
fn select_permission_defaults() {
    let perm: SelectPermission = serde_yaml::from_str("columns: \"*\"").unwrap();
    assert_eq!(perm.columns, Columns::Star);
    assert_eq!(perm.filter, serde_json::Value::Null);
    assert_eq!(perm.limit, None);
    assert!(!perm.allow_aggregations);
    assert!(perm.computed_fields.is_empty());
}

#[test]
fn remote_schema_without_comment_round_trips_with_comment_omitted() {
    let yaml = "\
name: my-remote
definition:
  url: http://localhost:5000/graphql
  forward_client_headers: true
";
    let rs: RemoteSchema = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(rs.name, "my-remote");
    assert_eq!(rs.comment, None);
    assert!(rs.definition.forward_client_headers);

    let out = serde_json::to_value(&rs).unwrap();
    let obj = out.as_object().unwrap();
    assert!(!obj.contains_key("comment"), "comment must be omitted when None");
    assert!(!obj.contains_key("permissions"), "empty permissions omitted");
    // url_from_env is None and must be skipped too.
    assert!(!out["definition"].as_object().unwrap().contains_key("url_from_env"));
}

#[test]
fn remote_schema_with_comment_round_trips() {
    let yaml = "\
name: my-remote
definition:
  url_from_env: REMOTE_URL
comment: a remote schema
permissions:
  - role: user
    definition:
      schema: \"schema { query: Query }\"
";
    let rs: RemoteSchema = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(rs.comment.as_deref(), Some("a remote schema"));
    assert_eq!(rs.definition.url_from_env.as_deref(), Some("REMOTE_URL"));
    assert_eq!(rs.permissions.len(), 1);

    let out = serde_json::to_value(&rs).unwrap();
    assert_eq!(out["comment"], json!("a remote schema"));

    // Serialize -> deserialize must be lossless.
    let back: RemoteSchema = serde_json::from_value(out).unwrap();
    assert_eq!(back.comment.as_deref(), Some("a remote schema"));
    assert_eq!(back.permissions[0].role, "user");
}

#[test]
fn qualified_table_accepts_bare_name_and_qualified_form() {
    let bare: QualifiedTable = serde_yaml::from_str("author").unwrap();
    assert_eq!(bare, QualifiedTable::Name("author".into()));
    assert_eq!(bare.schema(), "public");
    assert_eq!(bare.name(), "author");
    assert_eq!(bare.to_string(), "public.author");

    let qual: QualifiedTable =
        serde_yaml::from_str("{ schema: app, name: author }").unwrap();
    assert_eq!(qual.schema(), "app");
    assert_eq!(qual.to_string(), "app.author");
}

#[test]
fn database_url_plain_string_and_from_env() {
    let url: DatabaseUrl = serde_yaml::from_str("postgresql://u@h/db").unwrap();
    match url {
        DatabaseUrl::Url(u) => assert_eq!(u, "postgresql://u@h/db"),
        other => panic!("expected plain url, got {other:?}"),
    }

    let env: DatabaseUrl = serde_yaml::from_str("{ from_env: PG_URL }").unwrap();
    match env {
        DatabaseUrl::FromEnv { from_env } => assert_eq!(from_env, "PG_URL"),
        other => panic!("expected from_env, got {other:?}"),
    }
}

#[test]
fn full_v2_metadata_document_is_accepted() {
    // A single-document v2 export (the /v1/metadata shape): sources with
    // inline tables plus the top-level sections.
    let yaml = "\
version: 3
sources:
  - name: default
    kind: postgres
    configuration:
      connection_info:
        database_url: postgresql://u@h/db
    tables:
      - table:
          schema: public
          name: author
        update_permissions:
          - role: user
            permission:
              columns: [name]
              filter: { id: { _eq: X-Hasura-User-Id } }
              check: { name: { _ne: \"\" } }
inherited_roles:
  - role_name: combined
    role_set: [user, editor]
query_collections:
  - name: allowed-queries
    definition:
      queries:
        - name: q1
          query: \"query { author { id } }\"
allowlist:
  - collection: allowed-queries
remote_schemas:
  - name: remote
    definition:
      url: http://localhost:5000/graphql
";
    let md: Metadata = serde_yaml::from_str(yaml).expect("full v2 document must load");
    assert_eq!(md.version, 3);
    assert_eq!(md.sources[0].kind, SourceKind::Postgres);
    let upd = &md.sources[0].tables[0].update_permissions[0];
    assert_eq!(upd.permission.columns, Columns::List(vec!["name".into()]));
    assert!(upd.permission.check.is_some());
    assert_eq!(md.inherited_roles[0].role_set, vec!["user", "editor"]);
    assert_eq!(md.query_collections[0].definition.queries[0].name, "q1");
    assert_eq!(md.allowlist[0].collection, "allowed-queries");
    assert_eq!(md.remote_schemas[0].name, "remote");
}

#[test]
fn existing_fixture_directory_still_loads() {
    // Guard: the canonical on-disk fixture (string-spelled includes, the
    // hasura-cli layout) keeps loading through the public entry point.
    let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/metadata"));
    let md = load_metadata_dir(dir).expect("fixture metadata should load");
    assert_eq!(md.sources.len(), 1);
    assert_eq!(md.sources[0].tables.len(), 2);
}
