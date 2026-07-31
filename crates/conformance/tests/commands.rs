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

fn source_and_role_qualified_command_metadata() -> Metadata {
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
                "table": { "schema": "public", "name": "buyer_orders" },
                "select_permissions": [{
                    "role": "buyer",
                    "permission": { "columns": "*", "filter": {} }
                }],
                "insert_permissions": [{
                    "role": "buyer",
                    "permission": { "columns": "*", "check": {} }
                }]
            }]
        }, {
            "name": "secondary",
            "kind": "postgres",
            "configuration": {
                "connection_info": {
                    "database_url": { "from_env": "DONAT_DATABASE_URL" }
                }
            },
            "tables": [{
                "table": { "schema": "public", "name": "merchant_orders" },
                "select_permissions": [{
                    "role": "merchant",
                    "permission": { "columns": "*", "filter": {} }
                }],
                "insert_permissions": [{
                    "role": "merchant",
                    "permission": { "columns": "*", "check": {} }
                }]
            }]
        }],
        "commands": [{
            "name": "create_order",
            "source": "default",
            "permissions": [{ "role": "buyer" }],
            "arguments": [
                { "name": "id", "type": "uuid!" },
                { "name": "tenant", "type": "String!" },
                { "name": "status", "type": "String!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "steps": [{
                "name": "order",
                "insert": {
                    "table": { "schema": "public", "name": "buyer_orders" },
                    "object": {
                        "id": { "arg": "id" },
                        "tenant": { "arg": "tenant" },
                        "status": { "arg": "status" }
                    },
                    "returning": ["status"]
                }
            }],
            "result": { "status": { "step": "order", "column": "status" } },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": [{ "argument": "tenant" }],
                "retention": "1d"
            }
        }, {
            "name": "create_order",
            "source": "secondary",
            "permissions": [{ "role": "merchant" }],
            "arguments": [
                { "name": "id", "type": "uuid!" },
                { "name": "tenant", "type": "String!" },
                { "name": "status", "type": "String!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "steps": [{
                "name": "order",
                "insert": {
                    "table": { "schema": "public", "name": "merchant_orders" },
                    "object": {
                        "id": { "arg": "id" },
                        "tenant": { "arg": "tenant" },
                        "status": { "arg": "status" }
                    },
                    "returning": ["status"]
                }
            }],
            "result": { "status": { "step": "order", "column": "status" } },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": [{ "argument": "tenant" }],
                "retention": "1d"
            }
        }]
    }))
    .expect("source- and role-qualified command metadata deserializes")
}

fn create_qualified_command_tables(database_url: &str) {
    let mut client = postgres::Client::connect(database_url, NoTls)
        .expect("connect to the qualified command suite database");
    client
        .batch_execute(
            "CREATE TABLE public.buyer_orders (\
                id uuid PRIMARY KEY,\
                tenant text NOT NULL,\
                status text NOT NULL\
             );\
             CREATE TABLE public.merchant_orders (\
                id uuid PRIMARY KEY,\
                tenant text NOT NULL,\
                status text NOT NULL\
             )",
        )
        .expect("create source-local command tables");
}

fn batch_rule_command_metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "custom_types": {
            "input_objects": [{
                "name": "OrderLineInput",
                "fields": [{ "name": "quantity", "type": "Int!" }]
            }]
        },
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": {
                    "database_url": { "from_env": "DONAT_DATABASE_URL" }
                }
            },
            "tables": [{
                "table": { "schema": "public", "name": "order_lines" },
                "select_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "filter": {} }
                }],
                "insert_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "check": {} }
                }]
            }]
        }],
        "commands": [{
            "name": "create_order_lines",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [{ "name": "lines", "type": "[OrderLineInput!]!" }],
            "steps": [{
                "name": "lines",
                "insert_many": {
                    "table": { "schema": "public", "name": "order_lines" },
                    "for_each": { "arg": "lines" },
                    "object": {
                        "quantity": {
                            "rule": "double_quantity",
                            "with": { "quantity": { "item": "quantity" } }
                        }
                    },
                    "returning": ["quantity"]
                }
            }],
            "result": { "lines": { "step": "lines" } }
        }],
        "rules": {
            "rules": [{
                "name": "double_quantity",
                "parameters": { "quantity": "int!" },
                "result": "int!",
                "expression": "quantity * 2"
            }]
        }
    }))
    .expect("batch Rule command metadata deserializes")
}

fn create_order_lines_table(database_url: &str) {
    let mut client = postgres::Client::connect(database_url, NoTls)
        .expect("connect to the batch command suite database");
    client
        .batch_execute(
            "CREATE TABLE public.order_lines (\
                id bigserial PRIMARY KEY,\
                quantity integer NOT NULL\
             )",
        )
        .expect("create order_lines table");
}

fn multi_relation_command_metadata() -> Metadata {
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
                }],
                "insert_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "check": {} }
                }]
            }, {
                "table": { "schema": "public", "name": "order_lines" },
                "select_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "filter": {} }
                }],
                "insert_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "check": {} }
                }]
            }]
        }],
        "commands": [{
            "name": "create_order_with_line",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "line_id", "type": "uuid!" },
                { "name": "customer_id", "type": "uuid!" },
                { "name": "quantity", "type": "Int!" }
            ],
            "steps": [{
                "name": "order",
                "insert": {
                    "table": { "schema": "public", "name": "orders" },
                    "object": {
                        "id": { "arg": "order_id" },
                        "customer_id": { "arg": "customer_id" },
                        "status": { "literal": "draft" },
                        "quantity": { "arg": "quantity" }
                    },
                    "returning": ["id", "status"]
                }
            }, {
                "name": "line",
                "insert": {
                    "table": { "schema": "public", "name": "order_lines" },
                    "object": {
                        "id": { "arg": "line_id" },
                        "order_id": { "step": "order", "column": "id" },
                        "quantity": { "arg": "quantity" }
                    },
                    "returning": ["id", "order_id", "quantity"]
                }
            }],
            "result": {
                "order_id": { "step": "order", "column": "id" },
                "line_id": { "step": "line", "column": "id" },
                "quantity": { "step": "line", "column": "quantity" }
            }
        }]
    }))
    .expect("multi-relation command metadata deserializes")
}

fn create_multi_relation_tables(database_url: &str) {
    let mut client = postgres::Client::connect(database_url, NoTls)
        .expect("connect to the multi-relation command suite database");
    client
        .batch_execute(
            "CREATE TABLE public.orders (\
                id uuid PRIMARY KEY,\
                customer_id uuid NOT NULL,\
                status text NOT NULL,\
                quantity integer NOT NULL\
             );\
             CREATE TABLE public.order_lines (\
                id uuid PRIMARY KEY,\
                order_id uuid NOT NULL,\
                quantity integer NOT NULL\
             )",
        )
        .expect("create multi-relation command tables");
}

fn required_row_command_metadata() -> Metadata {
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
                }],
                "update_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "filter": {}, "check": {} }
                }],
                "delete_permissions": [{
                    "role": "customer",
                    "permission": { "filter": {} }
                }]
            }, {
                "table": { "schema": "public", "name": "command_later_writes" },
                "select_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "filter": {} }
                }],
                "insert_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "check": {} }
                }]
            }]
        }],
        "commands": [{
            "name": "select_order_then_write",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "later_id", "type": "uuid!" }
            ],
            "steps": [{
                "name": "required_order",
                "select_one": {
                    "table": { "schema": "public", "name": "orders" },
                    "by": { "id": { "arg": "order_id" } },
                    "returning": ["id"]
                }
            }, {
                "name": "later_write",
                "insert": {
                    "table": { "schema": "public", "name": "command_later_writes" },
                    "object": {
                        "id": { "arg": "later_id" },
                        "kind": { "literal": "select" }
                    },
                    "returning": ["id"]
                }
            }],
            "result": { "later_id": { "step": "later_write", "column": "id" } }
        }, {
            "name": "update_order_then_write",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "later_id", "type": "uuid!" }
            ],
            "steps": [{
                "name": "required_order",
                "update": {
                    "table": { "schema": "public", "name": "orders" },
                    "where": { "id": { "arg": "order_id" } },
                    "set": { "status": { "literal": "approved" } },
                    "returning": ["id"]
                }
            }, {
                "name": "later_write",
                "insert": {
                    "table": { "schema": "public", "name": "command_later_writes" },
                    "object": {
                        "id": { "arg": "later_id" },
                        "kind": { "literal": "update" }
                    },
                    "returning": ["id"]
                }
            }],
            "result": { "later_id": { "step": "later_write", "column": "id" } }
        }, {
            "name": "delete_order_then_write",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "later_id", "type": "uuid!" }
            ],
            "steps": [{
                "name": "required_order",
                "delete": {
                    "table": { "schema": "public", "name": "orders" },
                    "where": { "id": { "arg": "order_id" } },
                    "returning": ["id"]
                }
            }, {
                "name": "later_write",
                "insert": {
                    "table": { "schema": "public", "name": "command_later_writes" },
                    "object": {
                        "id": { "arg": "later_id" },
                        "kind": { "literal": "delete" }
                    },
                    "returning": ["id"]
                }
            }],
            "result": { "later_id": { "step": "later_write", "column": "id" } }
        }]
    }))
    .expect("required-row command metadata deserializes")
}

fn create_required_row_tables(database_url: &str) {
    let mut client = postgres::Client::connect(database_url, NoTls)
        .expect("connect to the required-row command suite database");
    client
        .batch_execute(
            r#"
            CREATE TABLE public.orders (
                id uuid PRIMARY KEY,
                customer_id uuid NOT NULL,
                status text NOT NULL,
                quantity integer NOT NULL
            );
            CREATE TABLE public.command_later_writes (
                id uuid PRIMARY KEY,
                kind text NOT NULL
            );
            CREATE FUNCTION public.reject_command_later_write() RETURNS trigger AS $$
            BEGIN
                RAISE EXCEPTION 'later command step must not run' USING ERRCODE = 'P0G01';
            END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER before_command_later_write
            BEFORE INSERT ON public.command_later_writes
            FOR EACH ROW EXECUTE FUNCTION public.reject_command_later_write();
            "#,
        )
        .expect("create required-row command tables and rejection trigger");
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
    let insert_permission = if has_insert_permission {
        r#"
      insert_permissions:
        - role: customer
          permission:
            columns: "*"
            check: {}
"#
    } else {
        ""
    };
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
fn command_creates_rows_in_two_relations_atomically() {
    let suite = Suite::new("command_multi_relation_success")
        .initial_metadata(multi_relation_command_metadata())
        .with_migrations()
        .start();
    create_multi_relation_tables(suite.db_url());

    suite.check_query_f(
        "commands/multi_relation_success.yaml",
        donat_conformance::Transport::Http,
    );
}

#[test]
fn command_later_relation_failure_rolls_back_the_earlier_insert() {
    let suite = Suite::new("command_multi_relation_rollback")
        .initial_metadata(multi_relation_command_metadata())
        .with_migrations()
        .start();
    create_multi_relation_tables(suite.db_url());
    let mut client = postgres::Client::connect(suite.db_url(), NoTls)
        .expect("connect to seed the later-relation failure");
    client
        .batch_execute(
            "INSERT INTO public.order_lines (id, order_id, quantity) VALUES (\
                '550e8400-e29b-41d4-a716-446655440211',\
                '550e8400-e29b-41d4-a716-446655440299',\
                7\
             )",
        )
        .expect("seed the duplicate line identifier");

    suite.check_query_f(
        "commands/multi_relation_rollback.yaml",
        donat_conformance::Transport::Http,
    );
}

#[test]
fn command_required_rows_gate_every_later_dml_step() {
    let suite = Suite::new("command_required_rows_gate")
        .initial_metadata(required_row_command_metadata())
        .with_migrations()
        .start();
    create_required_row_tables(suite.db_url());

    suite.check_query_f(
        "commands/required_rows_gate_later_steps.yaml",
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

    let rejected_request_id = "550e8400-e29b-41d4-a716-446655440011";
    {
        let mut client = postgres::Client::connect(suite.db_url(), NoTls)
            .expect("connect to inspect guard rejection rollback");
        let domain_rows: i64 = client
            .query_one("SELECT count(*) FROM public.orders", &[])
            .expect("count orders after guard rejection")
            .get(0);
        assert_eq!(domain_rows, 0, "guard rejection must leave no domain row");
        for catalog_table in [
            "donat.command_invocation_claims",
            "donat.command_invocations",
        ] {
            let persisted: i64 = client
                .query_one(
                    &format!("SELECT count(*) FROM {catalog_table} WHERE key = $1"),
                    &[&rejected_request_id],
                )
                .expect("count guard-rejected command catalog entries")
                .get(0);
            assert_eq!(
                persisted, 0,
                "guard rejection must leave no entry in {catalog_table}"
            );
        }
    }

    let headers = vec![("X-Donat-Role".to_string(), "customer".to_string())];
    let (status, retry) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ create_order(id: \"550e8400-e29b-41d4-a716-446655440010\", customer_id: \"550e8400-e29b-41d4-a716-446655440001\", status: \"draft\", quantity: 1, request_id: \"{rejected_request_id}\") {{ order_id status }} }}"
            )
        }),
        &headers,
    );
    assert_eq!(status, 200, "retry after guard rejection: {retry}");
    assert_eq!(
        retry,
        json!({
            "data": {
                "create_order": {
                    "order_id": "550e8400-e29b-41d4-a716-446655440010",
                    "status": "draft"
                }
            }
        }),
        "the rejected key remains eligible for its first committed execution"
    );
}

#[test]
fn command_insert_many_rule_binds_each_graphql_item() {
    let suite = Suite::new("command_insert_many_rule_item")
        .initial_metadata(batch_rule_command_metadata())
        .with_migrations()
        .start();
    create_order_lines_table(suite.db_url());

    suite.check_query_f(
        "commands/insert_many_rule_item.yaml",
        donat_conformance::Transport::Http,
    );

    let headers = vec![("X-Donat-Role".to_string(), "customer".to_string())];
    let (status, response) = suite.post(
        "/v1/graphql",
        &json!({
            "query": "mutation { create_order_lines(lines: { quantity: 4 }) { lines { quantity } } }"
        }),
        &headers,
    );
    assert_eq!(status, 200, "singleton list coercion response: {response}");
    assert_eq!(
        response,
        json!({
            "data": {
                "create_order_lines": {
                    "lines": [{ "quantity": 8 }]
                }
            }
        }),
        "GraphQL must coerce one input object to the command list before insert_many"
    );
}

#[test]
fn command_idempotency_is_isolated_by_source_and_explicit_role() {
    let suite = Suite::new("command_source_role_identity")
        .initial_metadata(source_and_role_qualified_command_metadata())
        .with_migrations()
        .start();
    create_qualified_command_tables(suite.db_url());

    let request_id = "550e8400-e29b-41d4-a716-446655440091";
    let tenant = "tenant-identity-sentinel";
    for (role, id, expected_status) in [
        (
            "buyer",
            "550e8400-e29b-41d4-a716-446655440092",
            "buyer-created",
        ),
        (
            "merchant",
            "550e8400-e29b-41d4-a716-446655440093",
            "merchant-created",
        ),
    ] {
        let headers = vec![("X-Donat-Role".to_string(), role.to_string())];
        let (status, response) = suite.post(
            "/v1/graphql",
            &json!({
                "query": format!(
                    "mutation {{ create_order(id: \"{id}\", tenant: \"{tenant}\", status: \"{expected_status}\", request_id: \"{request_id}\") {{ status }} }}"
                )
            }),
            &headers,
        );
        assert_eq!(status, 200, "{role} command response: {response}");
        assert_eq!(
            response,
            json!({ "data": { "create_order": { "status": expected_status } } }),
            "the same command name/key must execute within the {role} identity"
        );
    }

    let mut client = postgres::Client::connect(suite.db_url(), NoTls)
        .expect("connect to inspect qualified command executions");
    for table in ["buyer_orders", "merchant_orders"] {
        let rows: i64 = client
            .query_one(&format!("SELECT count(*) FROM public.{table}"), &[])
            .unwrap_or_else(|error| panic!("count {table}: {error}"))
            .get(0);
        assert_eq!(rows, 1, "each source-local command must execute once");
    }
    let identities = client
        .query(
            "SELECT command_identity \
             FROM donat.command_invocations \
             WHERE command_name = 'create_order' AND key = $1 \
             ORDER BY command_identity",
            &[&request_id],
        )
        .expect("read source- and role-qualified invocation identities")
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    assert_eq!(identities.len(), 2);
    assert_ne!(identities[0], identities[1]);
    for identity in identities {
        assert!(
            !identity.contains(request_id) && !identity.contains(tenant),
            "qualified identity must not retain a raw key or scope value: {identity}"
        );
    }
}

#[test]
fn command_legacy_unqualified_retry_fails_closed_before_domain_write() {
    let suite = Suite::new("command_legacy_identity_gate")
        .initial_metadata(command_metadata())
        .with_migrations()
        .start();
    create_orders_table(suite.db_url());

    let request_id = "550e8400-e29b-41d4-a716-446655440095";
    let customer_id = "550e8400-e29b-41d4-a716-446655440001";
    let headers = vec![("X-Donat-Role".to_string(), "customer".to_string())];
    let (bootstrap_status, bootstrap) = suite.post(
        "/v1/graphql",
        &json!({ "query": "query { __typename }" }),
        &headers,
    );
    assert_eq!(
        bootstrap_status, 200,
        "start the migrated command engine before seeding legacy state: {bootstrap}"
    );
    let mut client = postgres::Client::connect(suite.db_url(), NoTls)
        .expect("connect to seed a preserved pre-V5 command");
    client
        .execute(
            "INSERT INTO public.orders (id, customer_id, status, quantity) \
             VALUES ('550e8400-e29b-41d4-a716-446655440094', $1::text::uuid, 'legacy', 1)",
            &[&customer_id],
        )
        .expect("seed the domain effect completed before V5");
    client
        .execute(
            "INSERT INTO donat.command_invocations \
                (command_identity, command_name, scope_hash, key, invocation_id, input_fingerprint, result, expires_at) \
             VALUES (\
                'legacy-unqualified:6372656174655f6f72646572',\
                'create_order',\
                decode(md5((jsonb_build_array(to_jsonb($1::text)))::text), 'hex'),\
                $2,\
                gen_random_uuid(),\
                decode('01', 'hex'),\
                '{\"order_id\":\"550e8400-e29b-41d4-a716-446655440094\",\"status\":\"legacy\"}'::jsonb,\
                statement_timestamp() + interval '1 day'\
             )",
            &[&customer_id, &request_id],
        )
        .expect("seed the unattributable completed invocation");

    let (status, response) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ create_order(id: \"550e8400-e29b-41d4-a716-446655440096\", customer_id: \"{customer_id}\", status: \"new\", quantity: 1, request_id: \"{request_id}\") {{ order_id }} }}"
            )
        }),
        &headers,
    );
    assert_eq!(status, 200, "legacy-key rejection response: {response}");
    assert_eq!(
        response,
        json!({
            "errors": [{
                "extensions": {
                    "path": "$.selectionSet.create_order",
                    "code": "validation-failed"
                },
                "message": "legacy idempotency key cannot be replayed safely after command identity migration"
            }]
        }),
        "the structured P0D01 envelope must survive GraphQL decoding"
    );
    let rendered = response.to_string();
    assert!(!rendered.contains(request_id));
    assert!(!rendered.contains(customer_id));

    let domain_rows: i64 = client
        .query_one("SELECT count(*) FROM public.orders", &[])
        .expect("count domain rows after legacy retry")
        .get(0);
    assert_eq!(domain_rows, 1, "legacy retry must not duplicate the write");
    let claims: i64 = client
        .query_one(
            "SELECT count(*) FROM donat.command_invocation_claims WHERE key = $1",
            &[&request_id],
        )
        .expect("count claims after legacy retry")
        .get(0);
    assert_eq!(claims, 0, "legacy rejection must precede claim election");
}

#[test]
fn command_idempotency_replays_a_wider_projection_from_the_complete_canonical_result() {
    let suite = Suite::new("command_idempotency_replay")
        .initial_metadata(command_metadata())
        .with_migrations()
        .start();
    create_orders_table(suite.db_url());

    suite.check_query_f(
        "commands/idempotency_first_narrow.yaml",
        donat_conformance::Transport::Http,
    );

    let mut client = postgres::Client::connect(suite.db_url(), NoTls)
        .expect("connect to inspect the canonical command result");
    let stored_result: String = client
        .query_one(
            "SELECT result::text \
             FROM donat.command_invocations \
             WHERE command_name = $1 AND key = $2",
            &[&"create_order", &"550e8400-e29b-41d4-a716-446655440021"],
        )
        .expect("load the persisted canonical command result")
        .get(0);
    let stored_result: serde_json::Value =
        serde_json::from_str(&stored_result).expect("canonical result is valid JSON");
    assert_eq!(
        stored_result,
        json!({
            "order_id": "550e8400-e29b-41d4-a716-446655440020",
            "status": "draft"
        }),
        "V3 retains every declared result field independently of the first GraphQL selection"
    );

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
fn command_database_failures_redact_postgres_details_and_roll_back_claims() {
    let suite = Suite::new("command_database_error_redaction")
        .initial_metadata(command_metadata())
        .with_migrations()
        .start();
    create_orders_table(suite.db_url());

    let headers = vec![("X-Donat-Role".to_string(), "customer".to_string())];
    let order_id = "550e8400-e29b-41d4-a716-446655440050";
    let first_request_id = "550e8400-e29b-41d4-a716-446655440051";
    let rejected_request_id = "550e8400-e29b-41d4-a716-446655440052";

    let (status, first) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ create_order(id: \"{order_id}\", customer_id: \"550e8400-e29b-41d4-a716-446655440001\", status: \"first\", quantity: 1, request_id: \"{first_request_id}\") {{ order_id }} }}"
            )
        }),
        &headers,
    );
    assert_eq!(status, 200, "first command response: {first}");
    assert_eq!(
        first,
        json!({ "data": { "create_order": { "order_id": order_id } } }),
        "first command response"
    );

    let (status, rejected) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ create_order(id: \"{order_id}\", customer_id: \"550e8400-e29b-41d4-a716-446655440001\", status: \"private-command-input-sentinel\", quantity: 1, request_id: \"{rejected_request_id}\") {{ order_id }} }}"
            )
        }),
        &headers,
    );
    assert_eq!(status, 200, "database rejection response: {rejected}");
    assert_eq!(
        rejected,
        json!({ "errors": [{
            "extensions": { "path": "$", "code": "data-exception" },
            "message": "command database error"
        }]}),
        "command database errors must use the stable generic body"
    );
    let rendered = rejected.to_string();
    for sensitive_detail in [
        order_id,
        rejected_request_id,
        "private-command-input-sentinel",
        "orders",
        "orders_pkey",
        "duplicate key",
    ] {
        assert!(
            !rendered.contains(sensitive_detail),
            "command database error leaked {sensitive_detail:?}: {rendered}"
        );
    }

    let mut client = postgres::Client::connect(suite.db_url(), NoTls)
        .expect("connect to inspect command rollback");
    let domain_rows: i64 = client
        .query_one("SELECT count(*) FROM public.orders", &[])
        .expect("count orders")
        .get(0);
    assert_eq!(domain_rows, 1, "failed command must not add a domain row");
    for catalog_table in [
        "donat.command_invocation_claims",
        "donat.command_invocations",
    ] {
        let persisted: i64 = client
            .query_one(
                &format!("SELECT count(*) FROM {catalog_table} WHERE key = $1"),
                &[&rejected_request_id],
            )
            .expect("count failed command catalog entries")
            .get(0);
        assert_eq!(
            persisted, 0,
            "failed command must roll back its entry in {catalog_table}"
        );
    }
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
