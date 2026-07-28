use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_server::migrate::{check_consistency, run_migrate};
use serde_json::{Value as Json, json};
use tokio_postgres::NoTls;

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn pg_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15433/postgres".to_string())
}

fn bundled_migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

async fn fresh_migration_database(label: &str) -> (String, String, String) {
    let admin_url = pg_url();
    let database_name = format!(
        "donat_{label}_{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let (client, connection) = tokio_postgres::connect(&admin_url, NoTls)
        .await
        .expect("Postgres admin database is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS {database_name} WITH (FORCE);"
        ))
        .await
        .expect("stale isolated migration database drops");
    client
        .batch_execute(&format!("CREATE DATABASE {database_name};"))
        .await
        .expect("isolated migration database creates");
    connection.abort();

    let prefix = admin_url
        .rsplit_once('/')
        .expect("Postgres URL contains a database name")
        .0
        .to_string();
    (
        admin_url,
        database_name.clone(),
        format!("{prefix}/{database_name}"),
    )
}

async fn drop_migration_database(admin_url: &str, database_name: &str) {
    let (client, connection) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .expect("Postgres admin database is available for cleanup");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP DATABASE {database_name} WITH (FORCE);"))
        .await
        .expect("isolated migration database drops");
    connection.abort();
}

struct MetadataDir {
    path: std::path::PathBuf,
}

impl MetadataDir {
    fn new(table: &str) -> Self {
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(format!("donat-command-validate-{suffix}"));
        std::fs::create_dir_all(path.join("databases")).expect("metadata directory creates");
        std::fs::write(path.join("version.yaml"), "version: 3\n").expect("version writes");
        std::fs::write(
            path.join("databases/databases.yaml"),
            format!(
                r#"
- name: default
  kind: postgres
  configuration:
    connection_info:
      database_url: postgres://unused
  tables:
    - table:
        schema: public
        name: {table}
      select_permissions:
        - role: customer
          permission:
            columns: "*"
            filter: {{}}
      insert_permissions:
        - role: customer
          permission:
            columns: "*"
            check: {{}}
"#
            ),
        )
        .expect("database metadata writes");
        std::fs::write(
            path.join("commands.yaml"),
            format!(
                r#"
- name: create_order
  source: default
  permissions:
    - role: customer
  arguments:
    - name: id
      type: uuid!
    - name: request_id
      type: uuid!
  steps:
    - name: order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ arg: missing_argument }}
        returning: [id]
  result:
    order_id: {{ step: order, column: id }}
"#
            ),
        )
        .expect("command metadata writes");
        Self { path }
    }
}

impl Drop for MetadataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[tokio::test]
async fn command_invocations_migration_creates_journal_and_graphql_error_helper() {
    let (admin_url, database_name, url) = fresh_migration_database("command_journal").await;
    run_migrate(&url, &bundled_migrations_dir())
        .await
        .expect("bundled migrations apply");

    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("isolated Postgres is available");
    let connection = tokio::spawn(connection);

    let columns = client
        .query(
            "
            SELECT attribute.attname,
                   format_type(attribute.atttypid, attribute.atttypmod),
                   attribute.attnotnull,
                   COALESCE(pg_get_expr(default_value.adbin, default_value.adrelid), '')
            FROM pg_attribute attribute
            JOIN pg_class relation ON relation.oid = attribute.attrelid
            JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
            LEFT JOIN pg_attrdef default_value
              ON default_value.adrelid = attribute.attrelid
             AND default_value.adnum = attribute.attnum
            WHERE namespace.nspname = 'donat'
              AND relation.relname = 'command_invocations'
              AND attribute.attnum > 0
              AND NOT attribute.attisdropped
            ORDER BY attribute.attnum
            ",
            &[],
        )
        .await
        .expect("command journal columns query succeeds")
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, bool>(2),
                row.get::<_, String>(3),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        columns,
        vec![
            ("command_name".into(), "text".into(), true, "".into()),
            ("scope_hash".into(), "bytea".into(), true, "".into()),
            ("key".into(), "text".into(), true, "".into()),
            ("input_fingerprint".into(), "bytea".into(), true, "".into()),
            ("result".into(), "jsonb".into(), true, "".into()),
            (
                "status".into(),
                "text".into(),
                true,
                "'succeeded'::text".into(),
            ),
            (
                "expires_at".into(),
                "timestamp with time zone".into(),
                true,
                "".into(),
            ),
            (
                "created_at".into(),
                "timestamp with time zone".into(),
                true,
                "now()".into(),
            ),
        ],
        "journal columns, PostgreSQL types, nullability, and defaults",
    );

    let primary_key_columns = client
        .query(
            "
            SELECT attribute.attname
            FROM pg_constraint constraint_row
            CROSS JOIN LATERAL unnest(constraint_row.conkey) WITH ORDINALITY
                AS key_column(attnum, position)
            JOIN pg_attribute attribute
              ON attribute.attrelid = constraint_row.conrelid
             AND attribute.attnum = key_column.attnum
            WHERE constraint_row.conrelid = 'donat.command_invocations'::regclass
              AND constraint_row.contype = 'p'
            ORDER BY key_column.position
            ",
            &[],
        )
        .await
        .expect("command journal primary-key query succeeds")
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    assert_eq!(
        primary_key_columns,
        ["command_name", "scope_hash", "key"],
        "the idempotency scope is exactly the primary key"
    );

    let redundant_unique_constraints: i64 = client
        .query_one(
            "
            SELECT count(*)
            FROM pg_constraint
            WHERE conrelid = 'donat.command_invocations'::regclass
              AND contype = 'u'
            ",
            &[],
        )
        .await
        .expect("command journal unique-constraint query succeeds")
        .get(0);
    assert_eq!(
        redundant_unique_constraints, 0,
        "the primary key is the sole idempotency uniqueness contract"
    );

    let expiry_index: String = client
        .query_one(
            "SELECT pg_get_indexdef('donat.command_invocations_expires_at_idx'::regclass)",
            &[],
        )
        .await
        .expect("command journal expiry index exists")
        .get(0);
    assert!(
        expiry_index.contains("ON donat.command_invocations")
            && expiry_index.contains("(expires_at)"),
        "retention index must target expires_at: {expiry_index}"
    );

    let function_error = client
        .query_one(
            "SELECT donat.raise_graphql_error($1, $2, $3)",
            &[
                &"validation-failed",
                &"$.selectionSet.create_order",
                &"customer is not allowed to order",
            ],
        )
        .await
        .expect_err("structured GraphQL helper always rejects");
    let db = function_error
        .as_db_error()
        .expect("structured GraphQL helper returns a database error");
    assert_eq!(db.code().code(), "P0D01", "dedicated SQLSTATE");
    let payload: Json = serde_json::from_str(db.message()).expect("JSON envelope");
    assert_eq!(
        payload,
        json!({
            "kind": "donat.graphql-error.v1",
            "code": "validation-failed",
            "path": "$.selectionSet.create_order",
            "message": "customer is not allowed to order",
        })
    );

    let function_definition: String = client
        .query_one(
            "SELECT pg_get_functiondef('donat.raise_graphql_error(text, text, text)'::regprocedure)",
            &[],
        )
        .await
        .expect("structured GraphQL helper definition query succeeds")
        .get(0);
    let function_definition_upper = function_definition.to_ascii_uppercase();
    assert!(
        !function_definition_upper.contains("EXECUTE"),
        "the helper must not construct or execute SQL: {function_definition}"
    );
    assert!(
        !function_definition_upper.contains("SECURITY DEFINER"),
        "the helper must not change role semantics: {function_definition}"
    );

    connection.abort();
    drop_migration_database(&admin_url, &database_name).await;
}

#[tokio::test]
async fn command_claims_migration_elects_idempotency_executor_with_canonical_key() {
    let (admin_url, database_name, url) = fresh_migration_database("command_claims").await;
    run_migrate(&url, &bundled_migrations_dir())
        .await
        .expect("bundled migrations apply");

    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("isolated Postgres is available");
    let connection = tokio::spawn(connection);

    let relation: Option<String> = client
        .query_one(
            "SELECT to_regclass('donat.command_invocation_claims')::text",
            &[],
        )
        .await
        .expect("claim relation lookup succeeds")
        .get(0);
    assert_eq!(
        relation.as_deref(),
        Some("donat.command_invocation_claims"),
        "V4 owns only the durable first-executor claim"
    );

    let columns = client
        .query(
            "
            SELECT attribute.attname,
                   format_type(attribute.atttypid, attribute.atttypmod),
                   attribute.attnotnull,
                   COALESCE(pg_get_expr(default_value.adbin, default_value.adrelid), '')
            FROM pg_attribute attribute
            JOIN pg_class relation ON relation.oid = attribute.attrelid
            JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
            LEFT JOIN pg_attrdef default_value
              ON default_value.adrelid = attribute.attrelid
             AND default_value.adnum = attribute.attnum
            WHERE namespace.nspname = 'donat'
              AND relation.relname = 'command_invocation_claims'
              AND attribute.attnum > 0
              AND NOT attribute.attisdropped
            ORDER BY attribute.attnum
            ",
            &[],
        )
        .await
        .expect("claim table columns query succeeds")
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, bool>(2),
                row.get::<_, String>(3),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        columns,
        vec![
            ("command_name".into(), "text".into(), true, "".into()),
            ("scope_hash".into(), "bytea".into(), true, "".into()),
            ("key".into(), "text".into(), true, "".into()),
            ("claim_state".into(), "text".into(), true, "".into()),
            (
                "expires_at".into(),
                "timestamp with time zone".into(),
                true,
                "".into(),
            ),
            (
                "created_at".into(),
                "timestamp with time zone".into(),
                true,
                "now()".into(),
            ),
        ],
        "claims retain no input, result, role, or raw scope values",
    );

    let primary_key_columns = client
        .query(
            "
            SELECT attribute.attname
            FROM pg_constraint constraint_row
            CROSS JOIN LATERAL unnest(constraint_row.conkey) WITH ORDINALITY
                AS key_column(attnum, position)
            JOIN pg_attribute attribute
              ON attribute.attrelid = constraint_row.conrelid
             AND attribute.attnum = key_column.attnum
            WHERE constraint_row.conrelid = 'donat.command_invocation_claims'::regclass
              AND constraint_row.contype = 'p'
            ORDER BY key_column.position
            ",
            &[],
        )
        .await
        .expect("claim primary-key query succeeds")
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    assert_eq!(
        primary_key_columns,
        ["command_name", "scope_hash", "key"],
        "claim and canonical journal share exactly one idempotency identity"
    );

    let claim_state_constraint: String = client
        .query_one(
            "
            SELECT pg_get_constraintdef(constraint_row.oid)
            FROM pg_constraint constraint_row
            WHERE constraint_row.conrelid = 'donat.command_invocation_claims'::regclass
              AND constraint_row.contype = 'c'
            ",
            &[],
        )
        .await
        .expect("claim state check exists")
        .get(0);
    assert!(
        claim_state_constraint.contains("claim_state")
            && claim_state_constraint.contains("first")
            && claim_state_constraint.contains("replay"),
        "claim state is a bounded internal election marker: {claim_state_constraint}"
    );

    let expiry_index: String = client
        .query_one(
            "SELECT pg_get_indexdef('donat.command_invocation_claims_expires_at_idx'::regclass)",
            &[],
        )
        .await
        .expect("claim expiry index exists")
        .get(0);
    assert!(
        expiry_index.contains("ON donat.command_invocation_claims")
            && expiry_index.contains("(expires_at)"),
        "claim retention index must target expires_at: {expiry_index}"
    );

    connection.abort();
    drop_migration_database(&admin_url, &database_name).await;
}

#[tokio::test]
async fn check_consistency_collects_static_command_diagnostics() {
    let table = format!(
        "donat_command_validate_{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let url = pg_url();
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("isolated Postgres is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "CREATE TABLE public.{table} (id uuid PRIMARY KEY);"
        ))
        .await
        .expect("validation table creates");

    let metadata = MetadataDir::new(&table);
    let problems = check_consistency(&url, &metadata.path)
        .await
        .expect("metadata validation completes");

    client
        .batch_execute(&format!("DROP TABLE public.{table};"))
        .await
        .expect("validation table drops");
    connection.abort();

    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("unknown argument 'missing_argument'")),
        "command compiler diagnostics were not collected: {problems:#?}"
    );
}

#[tokio::test]
async fn check_consistency_rejects_same_role_command_root_across_sources() {
    let table = format!(
        "donat_command_root_collision_{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let url = pg_url();
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("isolated Postgres is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "CREATE TABLE public.{table} (id uuid PRIMARY KEY, status text NOT NULL);"
        ))
        .await
        .expect("validation table creates");

    let metadata = MetadataDir::new(&table);
    std::fs::write(
        metadata.path.join("databases/databases.yaml"),
        format!(
            r#"
- name: default
  kind: postgres
  configuration:
    connection_info:
      database_url: postgres://unused
  tables:
    - table:
        schema: public
        name: {table}
      select_permissions:
        - role: customer
          permission:
            columns: "*"
            filter: {{}}
      insert_permissions:
        - role: customer
          permission:
            columns: "*"
            check: {{}}
- name: secondary
  kind: postgres
  configuration:
    connection_info:
      database_url: postgres://unused
  tables:
    - table:
        schema: public
        name: {table}
      configuration:
        custom_name: secondary_order
      select_permissions:
        - role: customer
          permission:
            columns: "*"
            filter: {{}}
      insert_permissions:
        - role: customer
          permission:
            columns: "*"
            check: {{}}
"#
        ),
    )
    .expect("two-source database metadata writes");
    std::fs::write(
        metadata.path.join("commands.yaml"),
        format!(
            r#"
- name: create_order
  source: default
  permissions:
    - role: customer
  steps:
    - name: order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "00000000-0000-0000-0000-000000000001" }}
          status: {{ literal: new }}
        returning: [id]
  result:
    id: {{ step: order, column: id }}
- name: create_order
  source: secondary
  permissions:
    - role: customer
  steps:
    - name: order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "00000000-0000-0000-0000-000000000002" }}
          status: {{ literal: new }}
        returning: [id]
  result:
    id: {{ step: order, column: id }}
"#
        ),
    )
    .expect("collision command metadata writes");

    let problems = check_consistency(&url, &metadata.path)
        .await
        .expect("metadata validation completes");

    client
        .batch_execute(&format!("DROP TABLE public.{table};"))
        .await
        .expect("validation table drops");
    connection.abort();

    assert_eq!(
        problems,
        vec![
            "commands[1]: command root 'create_order' is visible to role 'customer' in both commands[0] (source 'default') and commands[1] (source 'secondary')",
            "commands[1]: generated command type 'CreateOrderResult' is visible to role 'customer' in both commands[0] (source 'default') and commands[1] (source 'secondary')"
        ]
    );
}

#[tokio::test]
async fn check_consistency_rejects_identical_generated_command_result_types() {
    let table = format!(
        "donat_command_type_collision_{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let url = pg_url();
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("Postgres is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "CREATE TABLE public.{table} (id uuid PRIMARY KEY);"
        ))
        .await
        .expect("validation table creates");

    let metadata = MetadataDir::new(&table);
    std::fs::write(
        metadata.path.join("commands.yaml"),
        format!(
            r#"
- name: foo_bar
  source: default
  permissions:
    - role: customer
  steps:
    - name: order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "00000000-0000-0000-0000-000000000001" }}
        returning: [id]
  result:
    id: {{ step: order, column: id }}
- name: fooBar
  source: default
  permissions:
    - role: customer
  steps:
    - name: order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "00000000-0000-0000-0000-000000000002" }}
        returning: [id]
  result:
    id: {{ step: order, column: id }}
"#
        ),
    )
    .expect("collision command metadata writes");

    let problems = check_consistency(&url, &metadata.path)
        .await
        .expect("metadata validation completes");

    client
        .batch_execute(&format!("DROP TABLE public.{table};"))
        .await
        .expect("validation table drops");
    connection.abort();

    assert_eq!(
        problems,
        vec![
            "commands[1]: generated command type 'FooBarResult' is visible to role 'customer' in both commands[0] (source 'default') and commands[1] (source 'default')"
        ]
    );
}

#[tokio::test]
async fn check_consistency_rejects_out_of_range_int8_command_literal_without_writes() {
    let table = format!(
        "donat_command_literal_int8_{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let url = pg_url();
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("isolated Postgres is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "CREATE TABLE public.{table} (id bigint PRIMARY KEY, note varchar(3), payload jsonb);"
        ))
        .await
        .expect("validation table creates");

    let metadata = MetadataDir::new(&table);
    std::fs::write(
        metadata.path.join("commands.yaml"),
        format!(
            r#"
- name: create_order
  source: default
  permissions:
    - role: customer
  steps:
    - name: order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "9223372036854775808" }}
        returning: [id]
  result:
    order_id: {{ step: order, column: id }}
"#
        ),
    )
    .expect("command metadata writes");

    let problems = check_consistency(&url, &metadata.path)
        .await
        .expect("metadata validation completes");
    let table_still_exists: bool = client
        .query_one(
            "SELECT to_regclass($1) IS NOT NULL",
            &[&format!("public.{table}")],
        )
        .await
        .expect("table presence query succeeds")
        .get(0);

    client
        .batch_execute(&format!("DROP TABLE public.{table};"))
        .await
        .expect("validation table drops");
    connection.abort();

    assert!(
        problems.iter().any(|problem| {
            problem.contains("commands[0].steps[0]")
                && problem.contains("int8")
                && problem.contains("out of range")
        }),
        "out-of-range int8 diagnostic was not collected: {problems:#?}"
    );
    assert!(
        table_still_exists,
        "validate must not create or remove database objects"
    );
}

#[tokio::test]
async fn check_consistency_collects_each_independent_invalid_command_diagnostic() {
    let table = format!(
        "donat_command_literal_many_{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let url = pg_url();
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("isolated Postgres is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "CREATE TABLE public.{table} (id bigint PRIMARY KEY, payload jsonb);"
        ))
        .await
        .expect("validation table creates");

    let metadata = MetadataDir::new(&table);
    std::fs::write(
        metadata.path.join("commands.yaml"),
        format!(
            r#"
- name: create_first_order
  source: default
  permissions:
    - role: customer
  steps:
    - name: first_order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "9223372036854775808" }}
        returning: [id]
  result:
    order_id: {{ step: first_order, column: id }}
- name: create_second_order
  source: default
  permissions:
    - role: customer
  steps:
    - name: second_order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "9223372036854775807" }}
          payload: {{ literal: "not-json" }}
        returning: [id]
  result:
    order_id: {{ step: second_order, column: id }}
"#
        ),
    )
    .expect("command metadata writes");

    let problems = check_consistency(&url, &metadata.path)
        .await
        .expect("metadata validation completes");

    client
        .batch_execute(&format!("DROP TABLE public.{table};"))
        .await
        .expect("validation table drops");
    connection.abort();

    assert!(
        problems.iter().any(|problem| {
            problem.contains("commands[0].steps[0]") && problem.contains("int8")
        }),
        "first command diagnostic was not collected: {problems:#?}"
    );
    assert!(
        problems.iter().any(|problem| {
            problem.contains("commands[1].steps[0]") && problem.contains("jsonb")
        }),
        "second command diagnostic was not collected: {problems:#?}"
    );
}

#[tokio::test]
async fn check_consistency_retains_duplicate_command_name_diagnostics() {
    let table = format!(
        "donat_command_literal_duplicate_{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let url = pg_url();
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("isolated Postgres is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "CREATE TABLE public.{table} (id bigint PRIMARY KEY, payload jsonb);"
        ))
        .await
        .expect("validation table creates");

    let metadata = MetadataDir::new(&table);
    std::fs::write(
        metadata.path.join("commands.yaml"),
        format!(
            r#"
- name: duplicate_order
  source: default
  permissions:
    - role: customer
  steps:
    - name: first_order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "9223372036854775808" }}
        returning: [id]
  result:
    order_id: {{ step: first_order, column: id }}
- name: duplicate_order
  source: default
  permissions:
    - role: customer
  steps:
    - name: second_order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "9223372036854775807" }}
          payload: {{ literal: "not-json" }}
        returning: [id]
  result:
    order_id: {{ step: second_order, column: id }}
"#
        ),
    )
    .expect("command metadata writes");

    let problems = check_consistency(&url, &metadata.path)
        .await
        .expect("metadata validation completes");

    client
        .batch_execute(&format!("DROP TABLE public.{table};"))
        .await
        .expect("validation table drops");
    connection.abort();

    assert!(
        problems.iter().any(|problem| {
            problem.contains("commands[0].steps[0]") && problem.contains("int8")
        }),
        "first duplicate command diagnostic was not collected: {problems:#?}"
    );
    assert!(
        problems.iter().any(|problem| {
            problem.contains("commands[1]")
                && problem.contains("duplicate command name 'duplicate_order'")
        }),
        "duplicate-name diagnostic was not collected: {problems:#?}"
    );
    assert!(
        problems.iter().any(|problem| {
            problem.contains("commands[1].steps[0]") && problem.contains("jsonb")
        }),
        "second duplicate command diagnostic was not collected: {problems:#?}"
    );
}

#[tokio::test]
async fn check_consistency_accepts_nullable_varchar_literal_and_rejects_jsonb_literal() {
    let table = format!(
        "donat_command_literal_scalars_{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let url = pg_url();
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("isolated Postgres is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "CREATE TABLE public.{table} (id uuid PRIMARY KEY, note varchar(3), payload jsonb);"
        ))
        .await
        .expect("validation table creates");

    let metadata = MetadataDir::new(&table);
    std::fs::write(
        metadata.path.join("commands.yaml"),
        format!(
            r#"
- name: create_order
  source: default
  permissions:
    - role: customer
  steps:
    - name: order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "550e8400-e29b-41d4-a716-446655440000" }}
          note: {{ literal: null }}
        returning: [id]
  result:
    order_id: {{ step: order, column: id }}
"#
        ),
    )
    .expect("nullable command metadata writes");
    let accepted = check_consistency(&url, &metadata.path)
        .await
        .expect("nullable metadata validation completes");

    std::fs::write(
        metadata.path.join("commands.yaml"),
        format!(
            r#"
- name: create_order
  source: default
  permissions:
    - role: customer
  steps:
    - name: order
      insert:
        table:
          schema: public
          name: {table}
        object:
          id: {{ literal: "550e8400-e29b-41d4-a716-446655440000" }}
          payload: {{ literal: "not-json" }}
        returning: [id]
  result:
    order_id: {{ step: order, column: id }}
"#
        ),
    )
    .expect("unsupported-type command metadata writes");
    let rejected = check_consistency(&url, &metadata.path)
        .await
        .expect("unsupported-type metadata validation completes");

    client
        .batch_execute(&format!("DROP TABLE public.{table};"))
        .await
        .expect("validation table drops");
    connection.abort();

    assert!(
        accepted.is_empty(),
        "nullable varchar literal should validate: {accepted:#?}"
    );
    assert!(
        rejected.iter().any(|problem| {
            problem.contains("commands[0].steps[0]") && problem.contains("jsonb")
        }),
        "unsupported jsonb diagnostic was not collected: {rejected:#?}"
    );
}
