use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_server::validate::check_source_consistency;
use tokio_postgres::NoTls;

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn postgres_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

struct IsolatedDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl IsolatedDatabase {
    async fn create(label: &str) -> Self {
        let admin_url = postgres_admin_url();
        let name = format!(
            "donat_{label}_{}_{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        );
        let (client, connection) = tokio_postgres::connect(&admin_url, NoTls)
            .await
            .expect("Postgres admin database is available");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE);"))
            .await
            .expect("stale source-selection database drops");
        client
            .batch_execute(&format!("CREATE DATABASE {name};"))
            .await
            .expect("isolated source-selection database creates");
        connection.abort();
        let prefix = admin_url
            .rsplit_once('/')
            .expect("Postgres URL has a database segment")
            .0
            .to_owned();
        Self {
            admin_url,
            name: name.clone(),
            url: format!("{prefix}/{name}"),
        }
    }

    async fn drop(self) {
        let (client, connection) = tokio_postgres::connect(&self.admin_url, NoTls)
            .await
            .expect("Postgres admin database is available for cleanup");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE);", self.name))
            .await
            .expect("isolated source-selection database drops");
        connection.abort();
    }
}

struct MetadataDir {
    path: PathBuf,
}

impl MetadataDir {
    fn two_sources() -> Self {
        let path = std::env::temp_dir().join(format!(
            "donat-source-catalog-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("databases"))
            .expect("source-selection metadata directory creates");
        std::fs::write(path.join("version.yaml"), "version: 3\n").expect("metadata version writes");
        std::fs::write(
            path.join("databases/databases.yaml"),
            r#"
- name: default
  kind: postgres
  configuration:
    connection_info:
      database_url: postgres://must-not-connect.invalid/default
  tables:
    - table:
        schema: public
        name: default_only
- name: secondary
  kind: postgres
  configuration:
    connection_info:
      database_url: postgres://must-not-resolve.invalid/secondary
  tables:
    - table:
        schema: public
        name: secondary_only
"#,
        )
        .expect("source metadata writes");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MetadataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[tokio::test]
async fn validation_uses_only_the_selected_sources_real_catalog() {
    let database = IsolatedDatabase::create("selected_source").await;
    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("selected database is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute("CREATE TABLE public.secondary_only (id uuid PRIMARY KEY);")
        .await
        .expect("selected source schema creates");
    connection.abort();

    let metadata = MetadataDir::two_sources();
    let problems = check_source_consistency(&database.url, metadata.path(), "secondary")
        .await
        .expect("selected source validation completes");

    assert!(
        problems.is_empty(),
        "the selected catalog must not be reused for `default`: {problems:#?}"
    );
    database.drop().await;
}
