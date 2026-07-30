//! Unit tests for the metadata type model: serde behaviour that the engine
//! relies on — legacy `$op` permission spellings, `Columns` star/list,
//! serde defaults, RemoteSchema round-trips, and acceptance of a full v2
//! metadata document. Pure deserialization; no database.

use std::path::Path;

use donat_metadata::{
    ActionEntry, Columns, Command, CommandEffect, CommandIdempotencyKey, CommandResultValue,
    CommandStepOperation, CommandValue, ConnectorBaseUrl, ConnectorInstance, CronTrigger,
    DatabaseUrl, InsertPermission, Metadata, PermissionEntry, QualifiedTable, RemoteSchema,
    RestEndpoint, RulesMetadata, SelectPermission, SourceKind, TableConfiguration,
    action_visible_to_role, load_metadata_dir,
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
        err.to_string()
            .contains("expected \"*\" or a list of columns"),
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
fn empty_action_permissions_are_visible_to_any_explicit_role_without_inheritance() {
    let public: ActionEntry = serde_json::from_value(json!({
        "name": "public_action",
        "definition": { "handler": "https://example.invalid/action" },
        "permissions": []
    }))
    .expect("public action metadata deserializes");
    let restricted: ActionEntry = serde_json::from_value(json!({
        "name": "restricted_action",
        "definition": { "handler": "https://example.invalid/action" },
        "permissions": [{ "role": "owner" }]
    }))
    .expect("restricted action metadata deserializes");

    assert!(action_visible_to_role(&public, "customer"));
    assert!(action_visible_to_role(&restricted, "owner"));
    assert!(
        !action_visible_to_role(&restricted, "member"),
        "Action permissions retain their existing exact-role semantics"
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
    assert!(
        !obj.contains_key("comment"),
        "comment must be omitted when None"
    );
    assert!(
        !obj.contains_key("permissions"),
        "empty permissions omitted"
    );
    // url_from_env is None and must be skipped too.
    assert!(
        !out["definition"]
            .as_object()
            .unwrap()
            .contains_key("url_from_env")
    );
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

    let dotted: QualifiedTable = serde_yaml::from_str("app.author").unwrap();
    assert_eq!(dotted, QualifiedTable::Name("app.author".into()));
    assert_eq!(dotted.schema(), "app");
    assert_eq!(dotted.name(), "author");
    assert_eq!(dotted.to_string(), "app.author");
    assert_eq!(
        serde_json::to_value(&dotted).unwrap(),
        json!("app.author"),
        "the accepted scalar form remains scalar when metadata is serialized"
    );

    let qual: QualifiedTable = serde_yaml::from_str("{ schema: app, name: author }").unwrap();
    assert_eq!(qual.schema(), "app");
    assert_eq!(qual.to_string(), "app.author");

    let parts: QualifiedTable = serde_yaml::from_str("[app, author]").unwrap();
    assert_eq!(
        parts,
        QualifiedTable::Parts(vec!["app".into(), "author".into()])
    );
    assert_eq!(parts.schema(), "app");
    assert_eq!(parts.name(), "author");
}

#[test]
fn qualified_table_rejects_malformed_dotted_scalar_names() {
    for invalid in [".author", "app.", "app.public.author"] {
        let error = serde_yaml::from_str::<QualifiedTable>(invalid)
            .expect_err("a dotted relation must contain exactly schema and name");
        assert!(
            error.to_string().contains(&format!(
                "invalid qualified table name '{invalid}': expected 'name' or 'schema.name'"
            )),
            "{invalid}: {error}"
        );
    }
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
fn table_configuration_column_config_deserializes_and_round_trips() {
    // column_config carries per-column custom_name/comment; the comment is
    // surfaced as a field description. Unknown keys (an `extra`) must survive
    // a serialize -> deserialize cycle so exported v2 metadata is lossless.
    let yaml = "\
column_config:
  id:
    comment: The primary key
  name:
    custom_name: full_name
    comment: The person's name
    some_future_key: 42
";
    let cfg: TableConfiguration =
        serde_yaml::from_str(yaml).expect("column_config must deserialize");

    let id = &cfg.column_config["id"];
    assert_eq!(id.comment.as_deref(), Some("The primary key"));
    assert!(id.custom_name.is_none());
    assert!(id.extra.is_empty());

    let name = &cfg.column_config["name"];
    assert_eq!(name.custom_name.as_deref(), Some("full_name"));
    assert_eq!(name.comment.as_deref(), Some("The person's name"));
    assert_eq!(name.extra.get("some_future_key"), Some(&json!(42)));

    // Serialize -> deserialize must be lossless, including the unknown key.
    let out = serde_json::to_value(&cfg).unwrap();
    let back: TableConfiguration = serde_json::from_value(out).unwrap();
    let name_back = &back.column_config["name"];
    assert_eq!(name_back.custom_name.as_deref(), Some("full_name"));
    assert_eq!(name_back.comment.as_deref(), Some("The person's name"));
    assert_eq!(name_back.extra.get("some_future_key"), Some(&json!(42)));
    assert_eq!(
        back.column_config["id"].comment.as_deref(),
        Some("The primary key")
    );
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
    assert!(
        ct.include_in_metadata,
        "include_in_metadata defaults to true"
    );
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
    assert_eq!(
        (rc.num_retries, rc.interval_sec, rc.timeout_sec),
        (0, 10, 60)
    );
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
fn rest_endpoints_parse_single_and_multi_method() {
    // The shape donat-cli writes to rest_endpoints.yaml: a list of endpoints
    // referencing a saved query by collection + query name.
    let yaml = "\
- name: get_pet_by_id
  url: pet/:id
  methods:
    - GET
  definition:
    query:
      collection_name: pet_queries
      query_name: PetById
  comment: fetch one pet
- name: upsert_pet
  url: pet
  methods:
    - POST
    - PUT
  definition:
    query:
      collection_name: pet_queries
      query_name: UpsertPet
";
    let endpoints: Vec<RestEndpoint> =
        serde_yaml::from_str(yaml).expect("rest endpoints must deserialize");
    assert_eq!(endpoints.len(), 2);

    let get = &endpoints[0];
    assert_eq!(get.name, "get_pet_by_id");
    assert_eq!(get.url, "pet/:id");
    assert_eq!(get.methods, vec!["GET"]);
    assert_eq!(get.definition.query.collection_name, "pet_queries");
    assert_eq!(get.definition.query.query_name, "PetById");
    assert_eq!(get.comment.as_deref(), Some("fetch one pet"));

    let upsert = &endpoints[1];
    assert_eq!(upsert.methods, vec!["POST", "PUT"]);
    assert!(upsert.comment.is_none());
}

#[test]
fn rest_endpoints_load_from_metadata_section() {
    let yaml = "\
version: 3
sources: []
rest_endpoints:
  - name: get_pet_by_id
    url: pet/:id
    methods: [GET]
    definition:
      query:
        collection_name: pet_queries
        query_name: PetById
";
    let md: Metadata = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(md.rest_endpoints.len(), 1);
    assert_eq!(md.rest_endpoints[0].name, "get_pet_by_id");
    assert_eq!(md.rest_endpoints[0].definition.query.query_name, "PetById");
}

#[test]
fn rest_endpoint_round_trips_omitting_none_comment() {
    let yaml = "\
- name: get_pet_by_id
  url: pet/:id
  methods: [GET]
  definition:
    query:
      collection_name: pet_queries
      query_name: PetById
";
    let endpoints: Vec<RestEndpoint> = serde_yaml::from_str(yaml).unwrap();

    // Serialize -> deserialize must be lossless for the populated fields.
    let out = serde_json::to_value(&endpoints).unwrap();
    let obj = out[0].as_object().unwrap();
    assert!(!obj.contains_key("comment"), "None comment must be omitted");

    let back: Vec<RestEndpoint> = serde_json::from_value(out).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].name, endpoints[0].name);
    assert_eq!(back[0].url, endpoints[0].url);
    assert_eq!(back[0].methods, endpoints[0].methods);
    assert_eq!(
        back[0].definition.query.collection_name,
        endpoints[0].definition.query.collection_name
    );
    assert_eq!(
        back[0].definition.query.query_name,
        endpoints[0].definition.query.query_name
    );
    assert_eq!(back[0].comment, endpoints[0].comment);
}

#[test]
fn rest_endpoints_absent_from_directory_yields_empty_vec() {
    // The canonical fixture has no rest_endpoints.yaml; load_section must
    // treat the absent file as an empty section.
    let dir = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/metadata"
    ));
    let md = load_metadata_dir(dir).expect("fixture metadata should load");
    assert!(md.rest_endpoints.is_empty());
}

#[test]
fn source_kind_sqlite_and_postgres_deserialize_from_string() {
    // `kind` is a lowercase string discriminant; both backends parse.
    let sqlite: SourceKind = serde_yaml::from_str("sqlite").unwrap();
    assert_eq!(sqlite, SourceKind::Sqlite);
    let postgres: SourceKind = serde_yaml::from_str("postgres").unwrap();
    assert_eq!(postgres, SourceKind::Postgres);
    let mysql: SourceKind = serde_yaml::from_str("mysql").unwrap();
    assert_eq!(mysql, SourceKind::Mysql);

    // And through a Source document's `kind` field.
    let yaml = "\
name: db
kind: sqlite
configuration:
  connection_info:
    database_url: file:local.db
tables: []
";
    let src: donat_metadata::Source = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(src.kind, SourceKind::Sqlite);
}

#[test]
fn existing_fixture_directory_still_loads() {
    // Guard: the canonical on-disk fixture (string-spelled includes, the
    // donat-cli layout) keeps loading through the public entry point.
    let dir = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/metadata"
    ));
    let md = load_metadata_dir(dir).expect("fixture metadata should load");
    assert_eq!(md.sources.len(), 1);
    assert_eq!(md.sources[0].tables.len(), 2);
}

#[test]
fn rules_wrapper_round_trips_declared_types_and_expression_source() {
    // A declarative rule keeps its source text and declared types in metadata;
    // parsing and source locations belong to the future donat-rules crate.
    let yaml = r#"
rules:
  - name: order_request_is_well_formed
    parameters:
      lines: "[CreateOrderLine!]!"
    result: bool!
    expression: "size(lines) > 0"
decision_tables:
  - name: invoice_approval
    inputs:
      amount: decimal!
    output:
      route: string!
    hit_policy: first
    rows:
      - id: default
        when: { amount: "true" }
        output: { route: manual_approval }
    test_cases:
      - name: default route
        input: { amount: 100 }
        expect:
          output: { route: manual_approval }
          matched_row_id: default
"#;

    let rules: RulesMetadata = serde_yaml::from_str(yaml).expect("rules wrapper must deserialize");
    assert_eq!(rules.rules.len(), 1);
    assert_eq!(rules.rules[0].parameters["lines"], "[CreateOrderLine!]!");
    assert_eq!(rules.rules[0].result, "bool!");
    assert_eq!(rules.rules[0].expression, "size(lines) > 0");
    assert_eq!(rules.decision_tables.len(), 1);
    assert_eq!(rules.decision_tables[0].rows[0].when["amount"], "true");
    assert_eq!(
        rules.decision_tables[0].test_cases[0].expect.matched_row_id,
        "default"
    );

    let round_trip: RulesMetadata =
        serde_json::from_value(serde_json::to_value(&rules).expect("rules wrapper must serialize"))
            .expect("serialized rules wrapper must deserialize");
    assert_eq!(round_trip.rules[0].expression, "size(lines) > 0");
    assert_eq!(
        round_trip.decision_tables[0].test_cases[0].expect.output["route"],
        json!("manual_approval")
    );
}

#[test]
fn types_only_rules_wrapper_is_retained_without_empty_rule_sections() {
    let rules: RulesMetadata = serde_yaml::from_str(
        r#"
types:
  - name: OrderStatus
    enum: [draft, submitted]
  - name: CreateOrderLine
    object:
      sku: string!
      status: OrderStatus!
"#,
    )
    .expect("a types-only rules wrapper must deserialize");

    assert_eq!(rules.types.len(), 2);
    assert_eq!(rules.types[0].name, "OrderStatus");
    assert_eq!(
        rules.types[0].enum_values.as_deref(),
        Some(&["draft".to_owned(), "submitted".to_owned()][..])
    );
    assert_eq!(
        rules.types[1].object.as_ref().expect("object declaration")["status"],
        "OrderStatus!"
    );
    assert!(rules.rules.is_empty());
    assert!(rules.decision_tables.is_empty());
    assert!(
        !rules.is_empty(),
        "declared types keep the one wrapper present"
    );

    let serialized = serde_json::to_value(&rules).expect("rules wrapper serializes");
    assert!(
        serialized.get("types").is_some(),
        "types must remain in the wrapper"
    );
    assert!(serialized.get("rules").is_none(), "empty rules are omitted");
    assert!(
        serialized.get("decision_tables").is_none(),
        "empty decision tables are omitted"
    );
    let round_trip: RulesMetadata = serde_json::from_value(serialized)
        .expect("types-only wrapper must round-trip through serialization");
    assert_eq!(round_trip.types.len(), 2);
}

#[test]
fn opaque_json_rule_type_retains_exact_closed_bounds() {
    let rules: RulesMetadata = serde_yaml::from_str(
        r#"
types:
  - name: BoundedProviderEvidence
    opaque_json:
      maximum_bytes: 4096
      maximum_depth: 8
      maximum_nodes: 128
"#,
    )
    .expect("a bounded opaque JSON declaration must deserialize");

    let serialized = serde_json::to_value(&rules).expect("rules wrapper serializes");
    assert_eq!(
        serialized["types"][0]["opaque_json"],
        json!({
            "maximum_bytes": 4096,
            "maximum_depth": 8,
            "maximum_nodes": 128
        })
    );

    for yaml in [
        r#"
types:
  - name: Evidence
    opaque_json:
      maximum_bytes: 64
      maximum_depth: 4
      maximum_nodes: 16
      expression: request.body
"#,
        r#"
types:
  - name: Evidence
    script: request.body
"#,
    ] {
        serde_yaml::from_str::<RulesMetadata>(yaml)
            .expect_err("opaque JSON declarations must reject executable or unknown fields");
    }
}

#[test]
fn commands_deserialize_all_step_and_value_forms() {
    // This exercises parsing only. Cross-step references, table targets, and
    // rule names are deliberately validated later by catalog compilation.
    let commands: Vec<Command> = serde_yaml::from_str(
        r#"
- name: complete_order
  source: default
  permissions:
    - role: customer
  arguments:
    - name: order_id
      type: uuid!
    - name: lines
      type: "[CreateOrderLine!]!"
  guards:
    - rule: order_request_is_well_formed
      with:
        lines: { arg: lines }
      message: order request is not valid
  steps:
    - name: existing_order
      select_one:
        table: public.orders
        by:
          id: { arg: order_id }
        returning: [id, customer_id]
    - name: order
      insert:
        table: public.orders
        object:
          customer_id: { step: existing_order, column: customer_id }
          status: { literal: draft }
        returning: [id, customer_id, status]
    - name: lines
      insert_many:
        table: public.order_lines
        for_each: { arg: lines }
        object:
          order_id: { step: order, column: id }
          sku: { item: sku }
          quantity: { item: quantity }
        returning: [id, sku, quantity]
    - name: approved_order
      update:
        table: public.orders
        where:
          id: { step: order, column: id }
        set:
          status:
            rule: next_order_status
            with:
              current: { literal: draft }
        returning: [id, status]
    - name: obsolete_order
      delete:
        table: public.obsolete_orders
        where:
          id: { arg: order_id }
        returning: [id]
    - name: order_is_approved
      assert:
        rule: order_is_approved
        with:
          status: { step: approved_order, column: status }
        message: order must be approved
  result:
    order_id: { step: order, column: id }
    line_items: { step: lines }
"#,
    )
    .expect("the complete command surface must deserialize");

    let command = &commands[0];
    assert_eq!(command.arguments[0].name, "order_id");
    assert_eq!(command.guards[0].rule, "order_request_is_well_formed");
    assert!(matches!(
        &command.steps[0].operation,
        CommandStepOperation::SelectOne { .. }
    ));
    assert!(matches!(
        &command.steps[1].operation,
        CommandStepOperation::Insert { .. }
    ));
    assert!(matches!(
        &command.steps[2].operation,
        CommandStepOperation::InsertMany { .. }
    ));
    assert!(matches!(
        &command.steps[3].operation,
        CommandStepOperation::Update { .. }
    ));
    assert!(matches!(
        &command.steps[4].operation,
        CommandStepOperation::Delete { .. }
    ));
    assert!(matches!(
        &command.steps[5].operation,
        CommandStepOperation::Assert { .. }
    ));
    assert!(matches!(
        &command.steps[2].operation,
        CommandStepOperation::InsertMany { insert_many }
            if matches!(&insert_many.object["sku"], CommandValue::Item { .. })
    ));
    assert!(matches!(
        &command.steps[1].operation,
        CommandStepOperation::Insert { insert }
            if matches!(&insert.object["status"], CommandValue::Literal { .. })
    ));
    assert!(matches!(
        &command.steps[3].operation,
        CommandStepOperation::Update { update }
            if matches!(&update.set["status"], CommandValue::Rule { .. })
    ));
    assert!(matches!(
        command.result.get("order_id"),
        Some(CommandResultValue::Step { .. })
    ));
    assert_eq!(
        command
            .result
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["order_id", "line_items"],
        "command result fields must retain declaration order"
    );

    let serialized = serde_yaml::to_string(&commands)
        .expect("command metadata must serialize for an order-preserving round trip");
    let reloaded: Vec<Command> =
        serde_yaml::from_str(&serialized).expect("serialized command metadata must deserialize");
    assert_eq!(
        reloaded[0]
            .result
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["order_id", "line_items"],
        "a command result round trip must retain declaration order"
    );
}

#[test]
fn relational_batch_deserializes_closed_step_and_value_forms() {
    // This proves the metadata boundary accepts only the declarative batch
    // shape. Relation kinds, primary keys, permissions, and step ordering are
    // deliberately catalog-validation concerns.
    let command: Command = serde_yaml::from_str(
        r#"
name: reserve_cart_stock
source: default
steps:
  - name: priced_lines
    select_many:
      table: { schema: public, name: cart_pricing }
      by:
        cart_id: { step: cart, column: id }
      order_by: [variant_id]
      returning: [variant_id, quantity, line_total_minor, currency]
      require_non_empty: true
  - name: totals
    aggregate:
      from: { step: priced_lines }
      values:
        line_count: { count: {} }
        subtotal_minor: { sum: { column: line_total_minor } }
        currency_count: { count_distinct: { column: currency } }
        currency: { min: { column: currency } }
  - name: reserve_stock
    update_many:
      table: { schema: public, name: inventory_stock }
      for_each: { step: priced_lines }
      by:
        variant_id: { item: variant_id }
      set:
        reserved:
          rule: add_int
          with:
            left: { current_column: reserved }
            right: { item: quantity }
      check:
        rule: can_reserve
        with:
          on_hand: { current_column: on_hand }
          reserved: { current_column: reserved }
          requested: { item: quantity }
      returning: [variant_id, reserved]
      require_each: true
"#,
    )
    .expect("the relational batch metadata surface must deserialize");

    assert!(matches!(
        &command.steps[0].operation,
        CommandStepOperation::SelectMany { select_many }
            if select_many.order_by == ["variant_id"] && select_many.require_non_empty
    ));
    assert!(matches!(
        &command.steps[1].operation,
        CommandStepOperation::Aggregate { aggregate }
            if matches!(&aggregate.values["line_count"], donat_metadata::CommandAggregate::Count { .. })
                && matches!(&aggregate.values["subtotal_minor"], donat_metadata::CommandAggregate::Sum { .. })
                && matches!(&aggregate.values["currency_count"], donat_metadata::CommandAggregate::CountDistinct { .. })
    ));
    assert!(matches!(
        &command.steps[2].operation,
        CommandStepOperation::UpdateMany { update_many }
            if update_many.require_each
                && matches!(
                    &update_many.set["reserved"],
                    CommandValue::Rule { bindings, .. }
                        if matches!(bindings["left"], CommandValue::CurrentColumn { .. })
                )
    ));
}

#[test]
fn relational_batch_rejects_invalid_local_yaml_shapes() {
    // These failures catch a future widening of the closed batch grammar.
    // They do not cover catalog-dependent source/order/scope semantics.
    let invalid_documents = [
        (
            "unknown select_many key",
            r#"
name: reserve
source: default
steps:
  - name: rows
    select_many:
      table: public.cart_pricing
      by: { cart_id: { arg: cart_id } }
      order_by: [variant_id]
      arbitrary_sql: SELECT 1
"#,
        ),
        (
            "empty select_many by",
            r#"
name: reserve
source: default
steps:
  - name: rows
    select_many:
      table: public.cart_pricing
      by: {}
      order_by: [variant_id]
"#,
        ),
        (
            "missing select_many order_by",
            r#"
name: reserve
source: default
steps:
  - name: rows
    select_many:
      table: public.cart_pricing
      by: { cart_id: { arg: cart_id } }
"#,
        ),
        (
            "duplicate select_many order_by column",
            r#"
name: reserve
source: default
steps:
  - name: rows
    select_many:
      table: public.cart_pricing
      by: { cart_id: { arg: cart_id } }
      order_by: [variant_id, variant_id]
"#,
        ),
        (
            "unsupported aggregate",
            r#"
name: reserve
source: default
steps:
  - name: totals
    aggregate:
      from: { step: priced_lines }
      values:
        average: { avg: { column: line_total_minor } }
"#,
        ),
    ];

    for (kind, document) in invalid_documents {
        let error = serde_yaml::from_str::<Command>(document)
            .expect_err(&format!("{kind} must be rejected"));
        let rendered = error.to_string();
        assert!(
            rendered.contains("unknown field")
                || rendered.contains("did not match")
                || rendered.contains("non-empty")
                || rendered.contains("duplicate"),
            "{kind} must fail as a closed local shape, got: {rendered}"
        );
    }
}

#[test]
fn relational_batch_catalog_semantic_cases_remain_loadable_for_task_five() {
    // Aggregate row-set sources, current_column scope, and reference order
    // require a resolved command graph and catalog facts. Task 5 owns those
    // semantic rejections; serde must retain the declarations intact.
    let command: Command = serde_yaml::from_str(
        r#"
name: deferred_semantics
source: default
steps:
  - name: totals
    aggregate:
      from: { step: scalar_step }
      values: { total: { count: {} } }
  - name: scalar_step
    select_one:
      table: public.carts
      by: { id: { step: later, column: id } }
      returning: [id]
  - name: invalid_current_column_scope
    insert:
      table: public.carts
      object: { reserved: { current_column: reserved } }
"#,
    )
    .expect("catalog-validation cases must remain representable in metadata");

    assert_eq!(command.steps.len(), 3);
    assert!(matches!(
        &command.steps[0].operation,
        CommandStepOperation::Aggregate { .. }
    ));
    assert!(matches!(
        &command.steps[2].operation,
        CommandStepOperation::Insert { insert }
            if matches!(&insert.object["reserved"], CommandValue::CurrentColumn { .. })
    ));
}

#[test]
fn commands_retain_unvalidated_duplicate_names_and_effect_references() {
    // Name uniqueness and process contracts are catalog-validation concerns;
    // loading metadata must retain these declarations for that later phase.
    let commands: Vec<Command> = serde_yaml::from_str(
        r#"
- name: duplicate_command
  source: default
  effects:
    - start_process:
        process: checkout_order
        input:
          order_id: { arg: order_id }
- name: duplicate_command
  source: default
  effects:
    - signal_process:
        process: checkout_order
        signal: approval_recorded
        correlate:
          unknown_correlation: { arg: undeclared_correlation }
        payload:
          unknown_payload: { arg: undeclared_payload }
          actor: { session_variable: x-donat-user-id }
        idempotency_key: { argument: undeclared_idempotency }
"#,
    )
    .expect("unvalidated command declarations must deserialize");

    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].name, commands[1].name);
    assert!(commands[0].idempotency.is_none());
    match &commands[0].effects[0] {
        CommandEffect::StartProcess {
            start_process: effect,
        } => {
            assert!(effect.idempotency_key.is_none());
            assert!(matches!(
                &effect.input["order_id"],
                CommandValue::Argument { .. }
            ));
        }
        other => panic!("expected start_process effect, got {other:?}"),
    }
    match &commands[1].effects[0] {
        CommandEffect::SignalProcess {
            signal_process: effect,
        } => {
            assert!(matches!(
                &effect.correlate["unknown_correlation"],
                CommandValue::Argument { .. }
            ));
            assert!(matches!(
                &effect.payload["unknown_payload"],
                CommandValue::Argument { .. }
            ));
            assert!(matches!(
                &effect.payload["actor"],
                CommandValue::SessionVariable { .. }
            ));
            assert!(matches!(
                &effect.idempotency_key,
                Some(CommandIdempotencyKey::Argument { .. })
            ));
        }
        other => panic!("expected signal_process effect, got {other:?}"),
    }
}

#[test]
fn command_closed_unions_reject_multiple_discriminators_and_unknown_keys() {
    let invalid_documents = [
        (
            "step operation",
            r#"
name: create_order
source: default
steps:
  - name: write
    insert:
      table: public.orders
      object: { status: { literal: draft } }
    delete:
      table: public.orders
      where: { id: { arg: id } }
"#,
        ),
        (
            "command value",
            r#"
name: create_order
source: default
steps:
  - name: write
    insert:
      table: public.orders
      object:
        status: { arg: status, literal: draft }
"#,
        ),
        (
            "idempotency key",
            r#"
name: create_order
source: default
idempotency:
  key: { argument: request_id, unknown: value }
"#,
        ),
        (
            "idempotency scope",
            r#"
name: create_order
source: default
idempotency:
  key: { argument: request_id }
  scope:
    - { argument: customer_id, session_variable: x-donat-user-id }
"#,
        ),
        (
            "command effect",
            r#"
name: create_order
source: default
effects:
  - start_process:
      process: checkout_order
    signal_process:
      process: checkout_order
      signal: approved
"#,
        ),
    ];

    for (kind, document) in invalid_documents {
        let error = serde_yaml::from_str::<Command>(document)
            .expect_err(&format!("ambiguous {kind} must be rejected"));
        let rendered = error.to_string();
        assert!(
            rendered.contains("unknown field") || rendered.contains("did not match"),
            "{kind} must fail as a closed union, got: {rendered}"
        );
    }
}

#[test]
fn command_argument_mapping_shorthand_normalizes_to_the_canonical_list() {
    let command: Command = serde_yaml::from_str(
        r#"
name: create_order
source: default
arguments:
  customer_id: uuid!
  lines: "[CreateOrderLine!]!"
"#,
    )
    .expect("argument mapping shorthand must deserialize");

    assert_eq!(command.arguments.len(), 2);
    assert_eq!(command.arguments[0].name, "customer_id");
    assert_eq!(command.arguments[0].type_, "uuid!");
    assert_eq!(command.arguments[1].name, "lines");
}

#[test]
fn commands_absent_from_directory_yield_empty_vec() {
    let dir = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/metadata"
    ));
    let md = load_metadata_dir(dir).expect("fixture metadata should load");
    assert!(md.commands.is_empty());
}

#[test]
fn connectors_deserialize_secret_references_and_named_operation_capacity() {
    // This fails if a future change turns a secret reference into a literal
    // configuration value, or loses the worker-owned capacity declaration.
    let connectors: Vec<ConnectorInstance> = serde_yaml::from_str(
        r#"
- name: logistics_api
  module: http
  config:
    endpoint_identity: logistics_prod_eu_2026_07
    credential_identity: logistics_primary
    base_url: https://logistics.example.test
    headers:
      - name: Authorization
        value_from_env: LOGISTICS_TOKEN
  operations:
    - name: create_shipment
      version: v1
      method: POST
      path: /v1/shipments/{input.order_id}
      body:
        order_id: { input: order_id }
      success_statuses: [200, 201]
      idempotency: { header: Idempotency-Key }
      capacity:
        max_in_flight: 8
        rate_limit: { permits: 20, per: 1s, burst: 8 }
        serialize_by: { input: order_id }
"#,
    )
    .expect("connector metadata deserializes");

    let connector = &connectors[0];
    assert_eq!(connector.name, "logistics_api");
    assert_eq!(connector.module, "http");
    assert_eq!(
        connector.config.endpoint_identity,
        "logistics_prod_eu_2026_07"
    );
    assert_eq!(connector.config.credential_identity, "logistics_primary");
    assert!(matches!(
        connector.config.base_url.as_ref(),
        Some(ConnectorBaseUrl::Literal(value)) if value == "https://logistics.example.test"
    ));
    assert_eq!(
        connector.config.headers[0].value_from_env,
        "LOGISTICS_TOKEN"
    );
    let capacity = connector.operations[0]
        .capacity
        .as_ref()
        .expect("operation capacity is retained");
    assert_eq!(capacity.max_in_flight, 8);
    assert_eq!(capacity.rate_limit.permits, 20);
    assert_eq!(capacity.rate_limit.per, "1s");
    assert_eq!(capacity.rate_limit.burst, 8);
    assert_eq!(
        capacity
            .serialize_by
            .as_ref()
            .expect("serialization is retained")
            .input,
        "order_id"
    );

    let literal_secret = serde_yaml::from_str::<Vec<ConnectorInstance>>(
        r#"
- name: stripe
  module: stripe
  config:
    endpoint_identity: stripe_api_2025_06_30
    credential_identity: stripe_primary
    secret_key: sk_live_literal_secret
"#,
    )
    .expect_err("secret references require a value_from_env mapping, never a literal");
    assert!(
        literal_secret.to_string().contains("secret_key"),
        "literal secrets must fail at the secret field: {literal_secret}"
    );
}

#[test]
fn http_connector_operations_deserialize_only_declared_dynamic_bindings() {
    // This fails if a job can later choose a URL, method, or header name
    // instead of filling only the named value slots deployed with the operation.
    let connectors: Vec<ConnectorInstance> = serde_yaml::from_str(
        r#"
- name: logistics_api
  module: http
  config:
    endpoint_identity: logistics_prod_eu_2026_07
    credential_identity: logistics_primary
    base_url: https://logistics.example.test
  operations:
    - name: create_shipment
      version: v1
      method: POST
      path: /v1/shipments/{input.order_id}
      query:
        shipment_kind: { input: shipment_kind }
      headers:
        - name: X-Request-Source
          value: donat
      body:
        order_id: { input: order_id }
        address: { input: address }
      success_statuses: [200, 201]
      response:
        shipment_id: { json_pointer: /id, type: string! }
      idempotency: { header: Idempotency-Key }
      error_classification:
        http_5xx: [500, 503]
      capacity:
        max_in_flight: 8
        rate_limit: { permits: 20, per: 1s, burst: 8 }
        serialize_by: { input: order_id }
"#,
    )
    .expect("a deployed HTTP operation has only static request shape and named bindings");

    let http = connectors[0].operations[0]
        .http()
        .expect("the HTTP module receives a typed HTTP operation");
    assert_eq!(http.version, "v1");
    assert_eq!(http.method, "POST");
    assert_eq!(http.path, "/v1/shipments/{input.order_id}");
    assert_eq!(http.query["shipment_kind"].input, "shipment_kind");
    assert_eq!(
        http.idempotency
            .as_ref()
            .expect("declared idempotency is retained")
            .header,
        "Idempotency-Key"
    );
    assert_eq!(
        connectors[0].operations[0]
            .capacity()
            .expect("worker capacity remains deploy-time metadata")
            .serialize_by
            .as_ref()
            .expect("same-resource serialization remains declared")
            .input,
        "order_id"
    );
}

#[test]
fn http_connector_operations_reject_raw_request_transport_fields() {
    let error = serde_yaml::from_str::<Vec<ConnectorInstance>>(
        r#"
- name: logistics_api
  module: http
  config:
    endpoint_identity: logistics_prod_eu_2026_07
    credential_identity: logistics_primary
    base_url: https://logistics.example.test
  operations:
    - name: create_shipment
      version: v1
      method: POST
      path: /v1/shipments/{input.order_id}
      success_statuses: [200]
      capacity:
        max_in_flight: 1
        rate_limit: { permits: 1, per: 1s, burst: 1 }
      url: https://attacker.invalid/override
"#,
    )
    .expect_err("operation metadata cannot introduce a raw arbitrary request URL");

    assert!(
        !error.to_string().is_empty(),
        "the unsafe raw transport field is rejected during metadata loading"
    );
}

#[test]
fn connectors_absent_from_directory_yield_empty_vec() {
    // Existing metadata directories remain valid without a new top-level file.
    let dir = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/metadata"
    ));
    let metadata = load_metadata_dir(dir).expect("fixture metadata should load");
    assert!(metadata.connectors.is_empty());
}
