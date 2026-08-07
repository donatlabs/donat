//! `validate` is the gate between `migrate` and serving, so what it passes
//! must be able to start.
//!
//! It used to check only that the metadata agreed with the database. A
//! deployment could therefore pass it on perfectly consistent metadata and
//! still fail seconds later because an environment variable naming a storage
//! or connector credential was never set — the most common deploy mistake,
//! landing in the one place least able to explain it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_server::validate::check_consistency;

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn pg_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15433/postgres".to_string())
}

/// A metadata directory declaring one attachment on a storage backend whose
/// credentials come from environment variables.
struct MetadataDir {
    path: PathBuf,
}

impl MetadataDir {
    fn new(secret_var: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "donat-validate-secrets-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("databases")).expect("fixture directory creates");
        std::fs::write(path.join("version.yaml"), "version: 3\n").expect("version writes");
        // A backend nothing uses is deliberately not resolved, so the table has
        // to declare the attachment for the credential to be needed at all.
        std::fs::write(
            path.join("databases/databases.yaml"),
            "- name: default\n  kind: postgres\n  configuration:\n    connection_info:\n      database_url: postgres://unused\n  tables:\n    - table:\n        schema: public\n        name: customer\n      attachments:\n        - column: avatar\n          backend: files\n          max_bytes: 1024\n",
        )
        .expect("sources write");
        std::fs::write(
            path.join("storage.yaml"),
            format!(
                "backends:\n  - name: files\n    kind: s3\n    bucket: donat-test\n    region: us-east-1\n    endpoint: http://127.0.0.1:19000\n    access_key_id:\n      value_from_env: {secret_var}\n    secret_access_key:\n      value_from_env: {secret_var}_SECRET\nsigning:\n  secret:\n    value_from_env: {secret_var}_SIGNING\n"
            ),
        )
        .expect("storage writes");
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
async fn validate_reports_a_storage_credential_the_deployment_never_set() {
    // A variable name nothing in this environment defines.
    let secret_var = format!(
        "DONAT_TEST_ABSENT_SECRET_{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let metadata = MetadataDir::new(&secret_var);

    let problems = check_consistency(&pg_url(), metadata.path())
        .await
        .expect("validation completes");

    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("storage configuration")
                && problem.contains(&secret_var)),
        "validate must name the credential that is missing, got: {problems:#?}"
    );
}
