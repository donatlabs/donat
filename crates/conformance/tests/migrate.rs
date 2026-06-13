//! Integration tests for the deploy-time subcommands `donat migrate`
//! (refinery DDL) and `donat validate` (metadata consistency). These are
//! the replacement for the runtime run_sql / metadata-mutation API: schema
//! is migrated out-of-band, metadata is validated against the migrated DB.

use std::path::{Path, PathBuf};
use std::process::Command;

use donat_conformance::{engine_binary, pg_admin_url};

fn with_db(admin_url: &str, db: &str) -> String {
    let (prefix, _) = admin_url.rsplit_once('/').expect("PG_URL has a db path");
    format!("{prefix}/{db}")
}

fn fresh_db(name: &str) -> String {
    let admin = pg_admin_url();
    let mut client = postgres::Client::connect(&admin, postgres::NoTls)
        .expect("connect to PG_URL (is postgres up?)");
    client
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .unwrap();
    client
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .unwrap();
    with_db(&admin, name)
}

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dist_migrate_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Run a donat subcommand; returns (success, combined output).
fn run(db_url: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new(engine_binary())
        .args(args)
        .env("DONAT_DATABASE_URL", db_url)
        .output()
        .expect("spawn donat");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), s)
}

#[test]
fn migrate_applies_sql_and_is_idempotent() {
    let db = fresh_db("conf_migrate_apply");
    let migrations = tmpdir("apply");
    write(
        &migrations.join("V1__create_widget.sql"),
        "CREATE TABLE widget (id serial primary key, name text not null);\n\
         INSERT INTO widget (name) VALUES ('a'), ('b');\n",
    );

    let (ok, out) = run(&db, &["migrate", "--migrations-dir", migrations.to_str().unwrap()]);
    assert!(ok, "first migrate failed:\n{out}");
    assert!(out.contains("applied migration"), "expected applied log:\n{out}");

    // Idempotent: a second run applies nothing.
    let (ok, out) = run(&db, &["migrate", "--migrations-dir", migrations.to_str().unwrap()]);
    assert!(ok, "second migrate failed:\n{out}");
    assert!(out.contains("up to date"), "expected up-to-date log:\n{out}");

    // The table exists with the seeded rows, and refinery tracked the version.
    let mut client = postgres::Client::connect(&db, postgres::NoTls).unwrap();
    let n: i64 = client.query_one("SELECT count(*) FROM widget", &[]).unwrap().get(0);
    assert_eq!(n, 2, "seeded rows");
    let v: i32 = client
        .query_one("SELECT version FROM refinery_schema_history ORDER BY version DESC LIMIT 1", &[])
        .unwrap()
        .get(0);
    assert_eq!(v, 1, "tracked migration version");
}

#[test]
fn validate_passes_when_consistent_and_fails_when_not() {
    let db = fresh_db("conf_migrate_validate");
    let migrations = tmpdir("validate_mig");
    write(
        &migrations.join("V1__create_widget.sql"),
        "CREATE TABLE widget (id serial primary key, name text not null);\n",
    );
    let (ok, out) = run(&db, &["migrate", "--migrations-dir", migrations.to_str().unwrap()]);
    assert!(ok, "migrate failed:\n{out}");

    // Metadata tracking the migrated table -> consistent.
    let md = tmpdir("meta_ok");
    write(&md.join("version.yaml"), "version: 3\n");
    write(
        &md.join("databases/databases.yaml"),
        "- name: default\n  kind: postgres\n  configuration:\n    connection_info:\n      database_url:\n        from_env: DONAT_GRAPHQL_DATABASE_URL\n  tables: \"!include default/tables/tables.yaml\"\n",
    );
    write(
        &md.join("databases/default/tables/tables.yaml"),
        "- \"!include public_widget.yaml\"\n",
    );
    let widget = "table:\n  name: widget\n  schema: public\nselect_permissions:\n  - role: user\n    permission:\n      columns: \"*\"\n      filter: {}\n";
    write(&md.join("databases/default/tables/public_widget.yaml"), widget);

    let (ok, out) = run(&db, &["validate", "--metadata-dir", md.to_str().unwrap()]);
    assert!(ok, "validate should pass for consistent metadata:\n{out}");
    assert!(out.contains("consistent"), "expected consistent log:\n{out}");

    // Metadata tracking a non-existent table -> inconsistent, non-zero exit.
    write(
        &md.join("databases/default/tables/public_widget.yaml"),
        "table:\n  name: ghost\n  schema: public\n",
    );
    let (ok, out) = run(&db, &["validate", "--metadata-dir", md.to_str().unwrap()]);
    assert!(!ok, "validate should fail for a missing table:\n{out}");
    assert!(
        out.contains("does not exist in the database"),
        "expected missing-table inconsistency:\n{out}"
    );
}
