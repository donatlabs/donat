use std::process::{Command, Output};

fn donat() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_donat"));
    for variable in [
        "DONAT_DATABASE_URL",
        "DONAT_GRAPHQL_DATABASE_URL",
        "DONAT_METADATA_DIR",
    ] {
        command.env_remove(variable);
    }
    command
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn metadata_free_migrate_rejects_source_before_database_resolution() {
    let output = donat()
        .args([
            "migrate",
            "--source",
            "default",
            "--migrations-dir",
            "does-not-matter",
        ])
        .output()
        .expect("donat binary starts");

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--source requires --metadata-dir"),
        "unexpected stderr:\n{}",
        stderr(&output)
    );
}

#[test]
fn validate_requires_metadata_before_database_resolution() {
    let output = donat()
        .arg("validate")
        .output()
        .expect("donat binary starts");

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("validate needs --metadata-dir"),
        "unexpected stderr:\n{}",
        stderr(&output)
    );
}
