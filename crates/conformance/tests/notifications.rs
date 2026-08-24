//! The notification module's own tests: `*_test.yaml` files beside the
//! declarations they exercise, run through `donat-testkit` — the runner behind
//! `donat test`.
//!
//! The module is a metadata directory in its own right, so it is its own
//! application here: `modules/notifications/donat.test.yaml` stands it up with
//! the one thing it cannot ship, a recipient binding. The second app dir is a
//! *deployment* that adopts it — its own sender, and the email escalation
//! turned on — because those two seams are the module's promise to whoever
//! adopts it, and a promise nothing exercises is a comment.
//!
//! Nothing in this file asserts behaviour. It is the cargo entry, so that
//! `make conformance` covers what `make app-test` covers; one `#[test]` per
//! file keeps cargo's filtering and parallelism, and the last test refuses a
//! file nobody listed, so a new `_test.yaml` cannot silently go unrun.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use donat_conformance::{engine_binary, pg_admin_url};
use donat_testkit::AppTestConfig;
use donat_testkit::runner::{self, RunConfig, TEST_FILE_SUFFIX};

/// The module itself, and the deployment example that adopts it.
const MODULE: &str = "modules/notifications";
const DEPLOYMENT: &str = "modules/notifications/examples/deployment";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn app(app_dir: &str) -> AppTestConfig {
    let root = workspace_root().join(app_dir);
    AppTestConfig::load(&root).unwrap_or_else(|error| panic!("{app_dir}/donat.test.yaml: {error}"))
}

fn run(app_dir: &str, rel: &str) {
    let app = app(app_dir);
    let run = RunConfig {
        engine_binary: engine_binary(),
        engine_migrations_dir: workspace_root().join("migrations"),
        admin_database_url: pg_admin_url(),
        log_dir: workspace_root().join("target/app-test-logs"),
        filter: None,
        // Cargo already runs one test binary per file in parallel; within a
        // file, two stands at a time keeps the machine's share bounded.
        jobs: Some(2),
    };
    let file = app.metadata.join(rel);
    let report = runner::run_file(&app, &run, &file).expect("test file runs");
    let mut out = Vec::new();
    report.write(&mut out, &app.metadata).unwrap();
    let text = String::from_utf8_lossy(&out);
    eprintln!("{text}");
    assert!(!report.cases.is_empty(), "{rel} holds no test cases");
    assert_eq!(report.failed(), 0, "{rel} failed:\n{text}");
}

/// Every `_test.yaml` under an app's metadata, relative to it.
fn discovered(app_dir: &str) -> BTreeSet<String> {
    let app = app(app_dir);
    runner::discover(&app.metadata)
        .unwrap()
        .into_iter()
        .map(|path| {
            path.strip_prefix(&app.metadata)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

macro_rules! yaml_files {
    ($($name:ident => $app:ident / $rel:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                run($app, $rel);
            }
        )*

        const LISTED: &[(&str, &str)] = &[$(($app, $rel)),*];
    };
}

yaml_files! {
    inbox => MODULE / "databases/default/tables/notification_inbox_test.yaml",
    preference => MODULE / "databases/default/tables/notification_preference_test.yaml",
    pending_digest => MODULE / "databases/default/tables/notification_pending_digest_test.yaml",
    notify => MODULE / "commands/notify_test.yaml",
    flush_digests => MODULE / "commands/flush-digests_test.yaml",
    claim_digest => MODULE / "commands/claim-digest_test.yaml",
    record_digest_sent => MODULE / "commands/record-digest-sent_test.yaml",
    delivery => MODULE / "flows/notification-delivery_test.yaml",
    digest_sweep => MODULE / "flows/notification-digest-sweep_test.yaml",
    own_sender => DEPLOYMENT / "connectors/own-mail_test.yaml",
    email_delay => DEPLOYMENT / "rules/email-delay_test.yaml",
}

#[test]
fn every_test_file_has_a_cargo_entry() {
    for app_dir in [MODULE, DEPLOYMENT] {
        let listed = LISTED
            .iter()
            .filter(|(app, _)| *app == app_dir)
            .map(|(_, rel)| (*rel).to_string())
            .collect::<BTreeSet<_>>();
        let found = discovered(app_dir);
        assert_eq!(
            found,
            listed,
            "every `{TEST_FILE_SUFFIX}` under {app_dir}/metadata must be listed in \
             `yaml_files!` here (found − listed = {:?}, listed − found = {:?})",
            found.difference(&listed).collect::<Vec<_>>(),
            listed.difference(&found).collect::<Vec<_>>()
        );
    }
}
