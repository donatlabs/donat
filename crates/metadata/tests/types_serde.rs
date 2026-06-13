//! Unit tests for the metadata type model: serde behaviour that the engine
//! relies on — legacy `$op` permission spellings, `Columns` star/list,
//! serde defaults, RemoteSchema round-trips, and acceptance of a full v2
//! metadata document. Pure deserialization; no database.

use std::path::Path;

use donat_metadata::{
    Columns, CronTrigger, DatabaseUrl, InsertPermission, Metadata, PermissionEntry,
    QualifiedTable, RemoteSchema, SelectPermission, SourceKind, load_metadata_dir,
};
use serde_json::json;

#[test]
fn legacy_dollar_op_filter_spellings_are_accepted_verbatim() {
    // Pre-v1 Donat wrote operators as $eq/$or/...; BoolExp stays untyped,
    // so legacy spellings must deserialize and survive unchanged.
    let yaml = "\
role: user
permission:
  columns: \"*\"
  filter:
    $or:
      - id:
          $eq: X-Donat-User-Id
      - is_public:
          $eq: true
";
    let entry: PermissionEntry<SelectPermission> =
        serde_yaml::from_str(yaml).expect("legacy $op filter must deserialize");
    assert_eq!(entry.role, "user");
    assert_eq!(
        entry.permission.filter["$or"][0]["id"]["$eq"],
        json!("X-Donat-User-Id")
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
              filter: { id: { _eq: X-Donat-User-Id } }
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
fn cron_trigger_full_parse() {
    // The shape donat-cli writes to cron_triggers.yaml.
    let yaml = "\
name: send_reminders
webhook: '{{WEBHOOK_BASE}}/cron'
schedule: '*/5 * * * *'
payload:
  kind: reminder
include_in_metadata: true
retry_conf:
  num_retries: 3
  retry_interval_seconds: 30
  timeout_seconds: 120
  tolerance_seconds: 3600
headers:
  - name: X-Api-Key
    value_from_env: API_KEY
comment: nightly reminders
";
    let ct: CronTrigger = serde_yaml::from_str(yaml).expect("cron trigger must load");
    assert_eq!(ct.name, "send_reminders");
    assert_eq!(ct.webhook, "{{WEBHOOK_BASE}}/cron");
    assert_eq!(ct.schedule, "*/5 * * * *");
    assert_eq!(ct.payload, json!({ "kind": "reminder" }));
    assert!(ct.include_in_metadata);
    let rc = ct.retry_conf.expect("retry_conf present");
    assert_eq!(rc.num_retries, 3);
    assert_eq!(rc.retry_interval_seconds, 30);
    assert_eq!(rc.timeout_seconds, 120);
    assert_eq!(rc.tolerance_seconds, 3600);
    assert_eq!(ct.headers[0].name, "X-Api-Key");
    assert_eq!(ct.headers[0].value_from_env.as_deref(), Some("API_KEY"));
    assert_eq!(ct.comment.as_deref(), Some("nightly reminders"));
}

#[test]
fn cron_trigger_defaults() {
    // Minimal form: no payload, no retry_conf, no include_in_metadata.
    let yaml = "\
name: t
webhook: http://localhost/hook
schedule: '* * * * *'
";
    let ct: CronTrigger = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(ct.payload, serde_json::Value::Null);
    assert!(ct.include_in_metadata, "include_in_metadata defaults to true");
    assert!(ct.retry_conf.is_none());
    assert!(ct.headers.is_empty());
    assert!(ct.comment.is_none());
}

#[test]
fn cron_retry_conf_field_defaults_match_donat() {
    // RetryConfST defaults: num_retries=0, interval=10, timeout=60,
    // tolerance=21600. A partial retry_conf fills the rest from defaults.
    let ct: CronTrigger = serde_yaml::from_str(
        "name: t\nwebhook: http://h\nschedule: '* * * * *'\nretry_conf: { num_retries: 2 }\n",
    )
    .unwrap();
    let rc = ct.retry_conf.unwrap();
    assert_eq!(rc.num_retries, 2);
    assert_eq!(rc.retry_interval_seconds, 10);
    assert_eq!(rc.timeout_seconds, 60);
    assert_eq!(rc.tolerance_seconds, 21600);
}

#[test]
fn cron_trigger_round_trips_omitting_empty_fields() {
    let ct: CronTrigger =
        serde_yaml::from_str("name: t\nwebhook: http://h\nschedule: '* * * * *'\n").unwrap();
    let out = serde_json::to_value(&ct).unwrap();
    let obj = out.as_object().unwrap();
    assert!(!obj.contains_key("comment"), "None comment omitted");
    assert!(!obj.contains_key("retry_conf"), "None retry_conf omitted");
    assert!(!obj.contains_key("headers"), "empty headers omitted");
}

#[test]
fn cron_triggers_load_from_metadata_section() {
    let yaml = "\
version: 3
sources: []
cron_triggers:
  - name: t
    webhook: http://localhost/hook
    schedule: '* * * * *'
";
    let md: Metadata = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(md.cron_triggers.len(), 1);
    assert_eq!(md.cron_triggers[0].name, "t");
}

#[test]
fn event_trigger_full_parse() {
    // Donat directory-format event trigger (under a table entry).
    let yaml = "\
name: t1_all
definition:
  enable_manual: false
  insert:
    columns: '*'
  update:
    columns: [c2]
  delete:
    columns: '*'
retry_conf:
  num_retries: 3
  interval_sec: 5
  timeout_sec: 30
webhook: '{{EVENT_WEBHOOK_HANDLER}}'
headers:
  - name: X-Header
    value: foo
";
    let et: donat_metadata::EventTrigger = serde_yaml::from_str(yaml).expect("event trigger loads");
    assert_eq!(et.name, "t1_all");
    assert_eq!(et.webhook.as_deref(), Some("{{EVENT_WEBHOOK_HANDLER}}"));
    assert!(et.webhook_from_env.is_none());
    assert!(!et.definition.enable_manual);
    assert_eq!(et.definition.insert.unwrap().columns, Columns::Star);
    assert_eq!(
        et.definition.update.unwrap().columns,
        Columns::List(vec!["c2".into()])
    );
    assert!(et.definition.delete.is_some());
    let rc = et.retry_conf.unwrap();
    assert_eq!(rc.num_retries, 3);
    assert_eq!(rc.interval_sec, 5);
    assert_eq!(rc.timeout_sec, 30);
    assert_eq!(et.headers[0].name, "X-Header");
}

#[test]
fn event_trigger_defaults_and_webhook_from_env() {
    // Insert-only trigger, webhook from env, no retry_conf.
    let yaml = "\
name: insert_only
definition:
  insert:
    columns: '*'
webhook_from_env: MY_HOOK
";
    let et: donat_metadata::EventTrigger = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(et.webhook_from_env.as_deref(), Some("MY_HOOK"));
    assert!(et.webhook.is_none());
    assert!(et.definition.insert.is_some());
    assert!(et.definition.update.is_none());
    assert!(et.definition.delete.is_none());
    assert!(et.retry_conf.is_none());
    // RetryConf defaults (Donat): num_retries=0, interval_sec=10, timeout_sec=60.
    let rc = donat_metadata::EventRetryConf::default();
    assert_eq!((rc.num_retries, rc.interval_sec, rc.timeout_sec), (0, 10, 60));
}

#[test]
fn event_triggers_load_under_table_entry() {
    let yaml = "\
table: { schema: hge_tests, name: test_t1 }
event_triggers:
  - name: t1_all
    definition:
      insert: { columns: '*' }
    webhook: http://localhost/hook
";
    let te: donat_metadata::TableEntry = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(te.event_triggers.len(), 1);
    assert_eq!(te.event_triggers[0].name, "t1_all");
}

#[test]
fn existing_fixture_directory_still_loads() {
    // Guard: the canonical on-disk fixture (string-spelled includes, the
    // donat-cli layout) keeps loading through the public entry point.
    let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/metadata"));
    let md = load_metadata_dir(dir).expect("fixture metadata should load");
    assert_eq!(md.sources.len(), 1);
    assert_eq!(md.sources[0].tables.len(), 2);
}
