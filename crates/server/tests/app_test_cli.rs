//! `donat test` as a user runs it: this binary, the petshop example, a real
//! Postgres. The cargo entry in `donat-conformance` covers the runner; this
//! covers the subcommand around it — resolving the machine's side, the exit
//! code, and the one report a green run must never print: nothing ran.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn petshop_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop")
}

fn donat_test(args: &[&str]) -> Output {
    let admin_url = std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".into());
    let mut command = Command::new(env!("CARGO_BIN_EXE_donat"));
    command
        .arg("test")
        .arg("--app-dir")
        .arg(petshop_root())
        .arg("--database-url")
        .arg(admin_url)
        .arg("--engine-migrations-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations"))
        .arg("--log-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/app-test-logs"))
        .args(args)
        // The deployment's variables must not reach the stand (see
        // `child_environment` in donat-testkit); set one and prove it.
        .env("DONAT_METADATA_DIR", "/nowhere");
    command.output().expect("donat test runs")
}

#[test]
fn a_filtered_run_reports_its_cases_and_exits_zero() {
    let junit = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/app-test-logs/app_test_cli.junit.xml");
    // One case by its full name: the file beside it keeps growing, and this
    // test is about the subcommand, not the suite's size.
    let output = donat_test(&[
        "--filter",
        "the schema refuses a non-positive quantity",
        "--junit",
        junit.to_str().unwrap(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "donat test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(
            "public_cart_line_test.yaml::the schema refuses a non-positive quantity ... ok"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed"),
        "{stdout}"
    );
}

#[test]
fn a_filter_that_matches_nothing_is_a_failure() {
    let output = donat_test(&["--filter", "no-such-case"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no test case ran"), "{stderr}");
}
