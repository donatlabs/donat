use std::sync::atomic::{AtomicUsize, Ordering};

use donat_server::migrate::check_consistency;
use tokio_postgres::NoTls;

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn pg_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15433/postgres".to_string())
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
    let connection = tokio::spawn(async move { connection.await });
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
    let connection = tokio::spawn(async move { connection.await });
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
    let connection = tokio::spawn(async move { connection.await });
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
    let connection = tokio::spawn(async move { connection.await });
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
    let connection = tokio::spawn(async move { connection.await });
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
