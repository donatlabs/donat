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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn help(args: &[&str]) -> Output {
    let mut command = donat();
    command.arg("help");
    command.args(args);
    command.output().expect("donat binary starts")
}

/// `donat help` answers with no database, no metadata directory and no
/// network, because it reads only what is compiled into the binary.
///
/// This is the property that makes it usable at all: an operator deciding
/// whether this build can talk to a provider has, by definition, not deployed
/// it yet. `donat()` strips the database and metadata variables, so a run that
/// reached for either would fail here rather than in someone's terminal.
#[test]
fn help_describes_the_surface_without_any_deployment() {
    let output = help(&[]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("connectors ("), "{text}");
    assert!(text.contains("capabilities ("), "{text}");
}

/// The index names every compiled module, and a per-deployment module says so
/// rather than reporting zero operations.
///
/// Zero would be a lie of exactly the kind this command exists to avoid: it
/// reads as "this connector does nothing" when the truth is "its declaration
/// needs configuration this command does not have".
#[test]
fn the_connector_index_distinguishes_empty_from_per_deployment() {
    let output = help(&["connectors"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("github"), "{text}");
    let twilio = text
        .lines()
        .find(|line| line.trim_start().starts_with("twilio"))
        .expect("twilio is listed");
    assert!(twilio.contains("per deployment"), "{twilio}");
}

/// One connector prints its operations, and the example it prints names that
/// connector's own credential fields.
#[test]
fn a_connector_page_carries_its_operations_and_a_usable_example() {
    let output = help(&["github"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("https://api.github.com"), "{text}");
    assert!(text.contains("issue.list"), "{text}");
    assert!(text.contains("GET /repos/{owner}/{repo}/issues"), "{text}");
    assert!(text.contains("module: github"), "{text}");
    assert!(text.contains("operation: "), "{text}");
}

/// No help output carries a credential value or the sentinel a test would use
/// for one — a plan is named, never spelled out with its material.
#[test]
fn help_prints_no_credential_material() {
    for topic in [vec![], vec!["connectors"], vec!["github"], vec!["stripe"]] {
        let text = stdout(&help(&topic));
        for forbidden in ["Bearer sk-", "donat-secret-sentinel", "Authorization: "] {
            assert!(
                !text.contains(forbidden),
                "`donat help {}` printed `{forbidden}`",
                topic.join(" ")
            );
        }
    }
}

/// An unknown topic fails, rather than printing nothing and exiting zero.
#[test]
fn an_unknown_help_topic_is_an_error() {
    let output = help(&["not-a-connector"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no help topic named"),
        "{}",
        stderr(&output)
    );
}

/// The Markdown rendering is Markdown: fenced examples, and operations as
/// headings rather than indented lines that would reflow into a paragraph.
#[test]
fn markdown_help_is_structured_as_markdown() {
    let output = help(&["github", "--format", "markdown"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("# Connector `github`"), "{text}");
    assert!(text.contains("### `issue.list`"), "{text}");
    assert!(text.contains("```yaml"), "{text}");
}
