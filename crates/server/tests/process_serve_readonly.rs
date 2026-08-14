use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio_postgres::NoTls;

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn postgres_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("ephemeral port binds")
        .local_addr()
        .expect("ephemeral address exists")
        .port()
}

struct MetadataDir {
    path: PathBuf,
}

impl MetadataDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "donat-readonly-process-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("databases"))
            .expect("read-only metadata directory creates");
        std::fs::write(path.join("version.yaml"), "version: 3\n").expect("metadata version writes");
        std::fs::write(
            path.join("databases/databases.yaml"),
            r#"
- name: default
  kind: postgres
  configuration: {}
  tables: []
"#,
        )
        .expect("source metadata writes");
        std::fs::write(
            path.join("flows.yaml"),
            r#"
- name: checkout
  kind: process
  version: 1
  source: default
  permissions:
    - role: customer
  output:
    - name: status
      type: string!
  start_at: done
  states:
    - id: done
      output:
        values:
          status: { literal: ready }
"#,
        )
        .expect("Process metadata writes");
        Self { path }
    }
}

impl Drop for MetadataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn database_url_for(database_url: &str, username: &str, password: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("Postgres URL parses");
    url.set_username(username).expect("read-only username sets");
    url.set_password(Some(password))
        .expect("read-only password sets");
    url.to_string()
}

async fn wait_for_health(child: &mut Child, port: u16) -> Result<(), String> {
    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{port}/healthz");
    for _ in 0..100 {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("checking server status: {error}"))?
        {
            let mut stderr = String::new();
            if let Some(mut stream) = child.stderr.take() {
                let _ = stream.read_to_string(&mut stderr);
            }
            return Err(format!(
                "server exited before health check ({status}):\n{stderr}"
            ));
        }
        match client.get(&health).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    Err("server did not become healthy".to_owned())
}

#[tokio::test]
async fn serve_with_readonly_role_issues_no_ddl_or_dml() {
    let admin_url = postgres_admin_url();
    let suffix = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let database_name = format!("donat_readonly_{}_{}", std::process::id(), suffix);
    let role_name = format!("donat_readonly_{}_{}", std::process::id(), suffix);
    let password = format!("readonly-{suffix}");
    let (admin, admin_connection) = tokio_postgres::connect(&admin_url, NoTls)
        .await
        .expect("Postgres admin database is available");
    let admin_connection = tokio::spawn(admin_connection);
    admin
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS {database_name} WITH (FORCE);"
        ))
        .await
        .expect("stale read-only database drops");
    admin
        .batch_execute(&format!("DROP ROLE IF EXISTS {role_name};"))
        .await
        .expect("stale read-only role drops");
    admin
        .batch_execute(&format!("CREATE DATABASE {database_name};"))
        .await
        .expect("read-only database creates");
    let prefix = admin_url
        .rsplit_once('/')
        .expect("Postgres URL has a database segment")
        .0;
    let owner_url = format!("{prefix}/{database_name}");
    let metadata = MetadataDir::new();

    let migrate = Command::new(env!("CARGO_BIN_EXE_donat"))
        .arg("--database-url")
        .arg(&owner_url)
        .arg("migrate")
        .arg("--migrations-dir")
        .arg(migrations_dir())
        .arg("--metadata-dir")
        .arg(&metadata.path)
        .arg("--source")
        .arg("default")
        .output()
        .expect("deploy-time migrate starts");
    assert!(
        migrate.status.success(),
        "deploy-time migrate failed:\n{}",
        String::from_utf8_lossy(&migrate.stderr)
    );

    let (owner, owner_connection) = tokio_postgres::connect(&owner_url, NoTls)
        .await
        .expect("migrated database is available");
    let owner_connection = tokio::spawn(owner_connection);
    owner
        .batch_execute(&format!(
            "
            REVOKE CREATE ON DATABASE {database_name} FROM PUBLIC;
            REVOKE CREATE ON SCHEMA public FROM PUBLIC;
            REVOKE CREATE ON SCHEMA donat FROM PUBLIC;
            CREATE ROLE {role_name} LOGIN PASSWORD '{password}';
            GRANT CONNECT ON DATABASE {database_name} TO {role_name};
            GRANT USAGE ON SCHEMA public, donat TO {role_name};
            GRANT SELECT ON ALL TABLES IN SCHEMA public, donat TO {role_name};
            GRANT EXECUTE ON FUNCTION donat.check_violation(text) TO {role_name};
            "
        ))
        .await
        .expect("strict read-only serving role creates");
    owner_connection.abort();

    let readonly_url = database_url_for(&owner_url, &role_name, &password);
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_donat"))
        .arg("--database-url")
        .arg(&readonly_url)
        .arg("--metadata-dir")
        .arg(&metadata.path)
        .arg("--port")
        .arg(port.to_string())
        // This case is about which statements serving issues, not about
        // authentication — but a deployment that names none of the three ways
        // to establish a role refuses to boot, so it has to name one to reach
        // the thing under test.
        .env("DONAT_GRAPHQL_UNAUTHORIZED_ROLE", "anonymous")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("read-only serving binary starts");
    let health = wait_for_health(&mut child, port).await;
    let _ = child.kill();
    let _ = child.wait();
    health.expect("serve must initialize entirely through read-only validation");

    admin
        .batch_execute(&format!("DROP DATABASE {database_name} WITH (FORCE);"))
        .await
        .expect("read-only database drops");
    admin
        .batch_execute(&format!("DROP ROLE {role_name};"))
        .await
        .expect("read-only role drops");
    admin_connection.abort();
}
