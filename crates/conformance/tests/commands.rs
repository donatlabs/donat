//! Native GraphQL and deploy-time conformance for declarative commands.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use donat_conformance::{Suite, engine_binary, pg_admin_url};
use donat_metadata::Metadata;
use postgres::NoTls;
use serde_json::json;

static NEXT_VALIDATION_FIXTURE: AtomicU32 = AtomicU32::new(0);

fn command_metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": {
                    "database_url": { "from_env": "DONAT_DATABASE_URL" }
                }
            },
            "tables": [{
                "table": { "schema": "public", "name": "orders" },
                "select_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "filter": {} }
                }, {
                    "role": "viewer",
                    "permission": { "columns": "*", "filter": {} }
                }],
                "insert_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "check": {} }
                }, {
                    "role": "viewer",
                    "permission": { "columns": "*", "check": {} }
                }],
                "update_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "filter": {}, "check": {} }
                }],
                "delete_permissions": [{
                    "role": "customer",
                    "permission": { "filter": {} }
                }]
            }]
        }],
        "commands": [{
            "name": "create_order",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "id", "type": "uuid!" },
                { "name": "customer_id", "type": "uuid!" },
                { "name": "status", "type": "String!" },
                { "name": "quantity", "type": "Int!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "guards": [{
                "rule": "positive_quantity",
                "with": { "quantity": { "arg": "quantity" } },
                "message": "order quantity must be positive"
            }],
            "steps": [{
                "name": "order",
                "insert": {
                    "table": { "schema": "public", "name": "orders" },
                    "object": {
                        "id": { "arg": "id" },
                        "customer_id": { "arg": "customer_id" },
                        "status": { "arg": "status" },
                        "quantity": { "arg": "quantity" }
                    },
                    "returning": ["id", "status"]
                }
            }],
            "result": {
                "order_id": { "step": "order", "column": "id" },
                "status": { "step": "order", "column": "status" }
            },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": [{ "argument": "customer_id" }],
                "retention": "1d"
            }
        }],
        "rules": {
            "rules": [{
                "name": "positive_quantity",
                "parameters": { "quantity": "int!" },
                "result": "bool!",
                "expression": "quantity > 0"
            }]
        }
    }))
    .expect("command metadata deserializes")
}

fn create_orders_table(database_url: &str) {
    let mut client = postgres::Client::connect(database_url, NoTls)
        .expect("connect to the command suite database");
    client
        .batch_execute(
            "CREATE TABLE public.orders (\
                id uuid PRIMARY KEY,\
                customer_id uuid NOT NULL,\
                status text NOT NULL,\
                quantity integer NOT NULL\
             )",
        )
        .expect("create orders table");
}

fn validation_database(label: &str) -> (String, String) {
    let admin_url = pg_admin_url();
    let suffix = NEXT_VALIDATION_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let name = format!("conf_commands_{label}_{}_{}", std::process::id(), suffix);
    let mut client = postgres::Client::connect(&admin_url, NoTls)
        .expect("connect to the Postgres admin database");
    client
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .expect("drop a stale command validation database");
    client
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .expect("create command validation database");
    let (prefix, _) = admin_url
        .rsplit_once('/')
        .expect("PG_URL contains a database path");
    let database_url = format!("{prefix}/{name}");
    create_orders_table(&database_url);
    (database_url, name)
}

fn drop_validation_database(name: &str) {
    let admin_url = pg_admin_url();
    let mut client = postgres::Client::connect(&admin_url, NoTls)
        .expect("connect to the Postgres admin database for cleanup");
    client
        .batch_execute(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .expect("drop command validation database");
}

fn validation_metadata_dir(case: &str, commands: &str, has_insert_permission: bool) -> PathBuf {
    let suffix = NEXT_VALIDATION_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "donat-command-conformance-{case}-{}-{suffix}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("databases")).expect("create metadata directory");
    std::fs::write(dir.join("version.yaml"), "version: 3\n").expect("write metadata version");
    let insert_permission = has_insert_permission
        .then_some(
            r#"
      insert_permissions:
        - role: customer
          permission:
            columns: "*"
            check: {}
"#,
        )
        .unwrap_or("");
    std::fs::write(
        dir.join("databases/databases.yaml"),
        format!(
            r#"- name: default
  kind: postgres
  configuration:
    connection_info:
      database_url:
        from_env: DONAT_DATABASE_URL
  tables:
    - table:
        schema: public
        name: orders
      select_permissions:
        - role: customer
          permission:
            columns: "*"
            filter: {{}}
      update_permissions:
        - role: customer
          permission:
            columns: "*"
            filter: {{}}
            check: {{}}
      delete_permissions:
        - role: customer
          permission:
            filter: {{}}
{insert_permission}"#
        ),
    )
    .expect("write table metadata");
    std::fs::write(dir.join("commands.yaml"), commands).expect("write command metadata");
    dir
}

fn validate(database_url: &str, metadata_dir: &Path) -> (bool, String) {
    let output = Command::new(engine_binary())
        .args(["validate", "--metadata-dir"])
        .arg(metadata_dir)
        .env("DONAT_DATABASE_URL", database_url)
        .output()
        .expect("run donat validate");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), combined)
}

#[test]
fn command_create_order_returns_the_declared_result_shape() {
    let suite = Suite::new("command_create_order")
        .initial_metadata(command_metadata())
        .with_migrations()
        .start();
    create_orders_table(suite.db_url());

    suite.check_query_f(
        "commands/create_order.yaml",
        donat_conformance::Transport::Http,
    );
}

#[test]
fn command_guard_rejection_leaves_no_order_visible_to_the_explicit_role() {
    let suite = Suite::new("command_guard_denied")
        .initial_metadata(command_metadata())
        .with_migrations()
        .start();
    create_orders_table(suite.db_url());

    suite.check_query_f(
        "commands/guard_denied_no_write.yaml",
        donat_conformance::Transport::Http,
    );
}

#[test]
fn command_idempotency_replays_the_requested_projection_without_a_second_write() {
    let suite = Suite::new("command_idempotency_replay")
        .initial_metadata(command_metadata())
        .with_migrations()
        .start();
    create_orders_table(suite.db_url());

    suite.check_query_f(
        "commands/idempotency_replay.yaml",
        donat_conformance::Transport::Http,
    );
}

#[test]
fn command_idempotency_rejects_changed_input_without_another_write() {
    let suite = Suite::new("command_idempotency_conflict")
        .initial_metadata(command_metadata())
        .with_migrations()
        .start();
    create_orders_table(suite.db_url());

    suite.check_query_f(
        "commands/idempotency_conflict.yaml",
        donat_conformance::Transport::Http,
    );
}

#[test]
fn command_is_not_exposed_to_a_role_with_only_table_mutation_permission() {
    let suite = Suite::new("command_forbidden_role")
        .initial_metadata(command_metadata())
        .with_migrations()
        .start();
    create_orders_table(suite.db_url());

    suite.check_query_f(
        "commands/forbidden_role.yaml",
        donat_conformance::Transport::Http,
    );
}

#[test]
fn command_deploy_validation_rejects_missing_underlying_insert_permission() {
    let (database_url, database_name) = validation_database("missing_insert_permission");
    let metadata_dir = validation_metadata_dir(
        "missing_insert_permission",
        include_str!("../fixtures/commands/missing_insert_permission.yaml"),
        false,
    );

    let (ok, output) = validate(&database_url, &metadata_dir);
    assert!(
        !ok,
        "deploy validation accepted a command without table permission:\n{output}"
    );
    assert!(
        output.contains("commands[0].steps[0]"),
        "deploy validation must identify the invalid command step:\n{output}"
    );
    assert!(
        output.contains("role 'customer' lacks insert permission on table 'public.orders'"),
        "deploy validation must preserve the missing permission diagnostic:\n{output}"
    );

    std::fs::remove_dir_all(&metadata_dir).expect("remove command metadata directory");
    drop_validation_database(&database_name);
}

#[test]
fn command_deploy_validation_rejects_update_and_delete_without_primary_key_predicates() {
    let (database_url, database_name) = validation_database("missing_primary_key");
    let metadata_dir = validation_metadata_dir(
        "missing_primary_key",
        include_str!("../fixtures/commands/missing_primary_key_predicate.yaml"),
        true,
    );

    let (ok, output) = validate(&database_url, &metadata_dir);
    assert!(
        !ok,
        "deploy validation accepted update/delete commands without primary-key predicates:\n{output}"
    );
    for path in ["commands[0].steps[0]", "commands[1].steps[0]"] {
        assert!(
            output.contains(path),
            "deploy validation must identify {path}:\n{output}"
        );
    }
    assert_eq!(
        output
            .matches("update/delete/select_one requires every primary-key column (id)")
            .count(),
        2,
        "both update and delete must retain the primary-key guard:\n{output}"
    );

    std::fs::remove_dir_all(&metadata_dir).expect("remove command metadata directory");
    drop_validation_database(&database_name);
}
