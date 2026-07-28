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
