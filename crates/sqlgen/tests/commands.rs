use donat_ir::*;
use postgres::{Client, NoTls, Transaction};
use serde_json::{Value as Json, json};
use std::{
    sync::{Mutex, MutexGuard},
    time::Duration,
};

static COMMAND_CATALOG_LOCK: Mutex<()> = Mutex::new(());

fn table(name: &str) -> Table {
    Table {
        schema: "public".to_owned(),
        name: name.to_owned(),
    }
}

fn column(name: &str, pg_type: &str) -> CommandColumn {
    CommandColumn {
        name: name.to_owned(),
        pg_type: pg_type.to_owned(),
        nullable: false,
    }
}

fn value(value: serde_json::Value, pg_type: &str) -> CommandExecutionValue {
    CommandExecutionValue::Scalar {
        value: Scalar::Json(value),
        pg_type: pg_type.to_owned(),
    }
}

fn assignment(name: &str, pg_type: &str, data: serde_json::Value) -> CommandAssignment {
    CommandAssignment {
        column: column(name, pg_type),
        value: value(data, pg_type),
    }
}

fn root(command: CommandMutation) -> MutationRoot {
    MutationRoot::Command {
        alias: "submitted".to_owned(),
        command,
    }
}

fn command_identity(name: &str) -> CommandIdentity {
    CommandIdentity {
        source: "default".to_owned(),
        name: name.to_owned(),
        role: "customer".to_owned(),
    }
}

fn order_row_result(cte: &str) -> CommandResultValue {
    CommandResultValue::StepRow {
        cte: cte.to_owned(),
        many: false,
        columns: vec![column("id", "uuid"), column("status", "text")],
    }
}

fn postgres_client() -> Client {
    let pg_url = std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15433/postgres".to_owned());
    postgres_client_at(&pg_url)
}

fn postgres_client_at(pg_url: &str) -> Client {
    Client::connect(pg_url, NoTls).expect("Postgres must be available for command SQL execution")
}

fn install_command_catalog(tx: &mut Transaction<'_>) {
    tx.batch_execute(include_str!("../../../migrations/V3__donat_commands.sql"))
        .expect("command journal and structured rejection helper install");
    tx.batch_execute(include_str!(
        "../../../migrations/V4__donat_command_claims.sql"
    ))
    .expect("command claim election catalog installs");
    let identity_columns: i64 = tx
        .query_one(
            "SELECT count(*) \
             FROM information_schema.columns \
             WHERE table_schema = 'donat' \
               AND table_name IN ('command_invocations', 'command_invocation_claims') \
               AND column_name = 'command_identity'",
            &[],
        )
        .expect("inspect command identity migration state")
        .get(0);
    match identity_columns {
        0 => tx
            .batch_execute(include_str!(
                "../../../migrations/V5__qualify_command_identity.sql"
            ))
            .expect("source/role-qualified command identity installs"),
        2 => {}
        count => panic!("partial command identity migration in test catalog: {count} columns"),
    }
}

fn install_command_identity_catalog(tx: &mut Transaction<'_>) {
    install_command_catalog(tx);
}

fn install_check_violation_helper(tx: &mut Transaction<'_>) {
    tx.batch_execute(
        r#"
        CREATE SCHEMA IF NOT EXISTS donat;
        CREATE OR REPLACE FUNCTION donat.check_violation(msg text)
        RETURNS json AS $$
        BEGIN
            RAISE EXCEPTION USING message = msg, errcode = '23514';
        END;
        $$ LANGUAGE plpgsql;
        "#,
    )
    .expect("permission-check helper installs for direct SQLgen execution");
}

fn install_command_catalog_client(client: &mut Client) {
    client
        .batch_execute(include_str!("../../../migrations/V3__donat_commands.sql"))
        .expect("command journal and structured rejection helper install");
    client
        .batch_execute(include_str!(
            "../../../migrations/V4__donat_command_claims.sql"
        ))
        .expect("command claim election catalog installs");
    let identity_columns: i64 = client
        .query_one(
            "SELECT count(*) \
             FROM information_schema.columns \
             WHERE table_schema = 'donat' \
               AND table_name IN ('command_invocations', 'command_invocation_claims') \
               AND column_name = 'command_identity'",
            &[],
        )
        .expect("inspect command identity migration state")
        .get(0);
    match identity_columns {
        0 => client
            .batch_execute(include_str!(
                "../../../migrations/V5__qualify_command_identity.sql"
            ))
            .expect("source/role-qualified command identity installs"),
        2 => {}
        count => panic!("partial command identity migration in test catalog: {count} columns"),
    }
}

fn command_catalog_test_lock() -> MutexGuard<'static, ()> {
    COMMAND_CATALOG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn idempotent_insert_root(command_name: &str, table_name: &str, status: &str) -> MutationRoot {
    idempotent_insert_root_with_id(
        command_name,
        table_name,
        "550e8400-e29b-41d4-a716-446655440000",
        status,
    )
}

fn idempotent_insert_root_with_id(
    command_name: &str,
    table_name: &str,
    id: &str,
    status: &str,
) -> MutationRoot {
    root(CommandMutation {
        identity: command_identity(command_name),
        name: command_name.to_owned(),
        steps: vec![CommandExecutionStep::Insert {
            name: "order".to_owned(),
            cte: "_cmd_step_0".to_owned(),
            table: table(table_name),
            object: vec![
                assignment("id", "uuid", json!(id)),
                assignment("status", "text", json!(status)),
            ],
            returning: vec![column("id", "uuid"), column("status", "text")],
            check: None,
            error_path: "$.selectionSet.create_order".to_owned(),
        }],
        guards: vec![],
        result: vec![CommandResultField {
            name: "status".to_owned(),
            value: CommandResultValue::StepColumn {
                cte: "_cmd_step_0".to_owned(),
                column: column("status", "text"),
            },
        }],
        idempotency: Some(CommandIdempotency {
            key: Scalar::Json(json!("request-1")),
            scope: vec![Scalar::Json(json!("tenant-1"))],
            input: Scalar::Json(json!({ "status": status })),
            retention_seconds: Some(60),
            error_path: "$.selectionSet.create_order".to_owned(),
        }),
        effects: vec![],
        selection: vec![CommandResultSelection::Scalar {
            alias: "status".to_owned(),
            field: "status".to_owned(),
        }],
    })
}

#[test]
fn command_renderer_lowers_guard_and_session_scoped_idempotency() {
    let root = MutationRoot::Command {
        alias: "submitted".to_owned(),
        command: CommandMutation {
            identity: command_identity("create_order"),
            name: "create_order".to_owned(),
            steps: vec![],
            guards: vec![CommandRule {
                sql: "TRUE".to_owned(),
                pg_type: "bool".to_owned(),
                error_path: "$.selectionSet.create_order".to_owned(),
                message: "customer is not allowed to order".to_owned(),
            }],
            result: vec![],
            idempotency: Some(CommandIdempotency {
                key: Scalar::Json(json!("550e8400-e29b-41d4-a716-446655440002")),
                scope: vec![Scalar::Json(json!("customer-7"))],
                input: Scalar::Json(json!({
                    "request_id": "550e8400-e29b-41d4-a716-446655440002"
                })),
                retention_seconds: None,
                error_path: "$.selectionSet.create_order".to_owned(),
            }),
            effects: vec![],
            selection: vec![],
        },
    };

    let sql = donat_sqlgen::mutation_to_sql(&root);

    assert!(sql.contains("donat.raise_graphql_error"), "{sql}");
    assert!(sql.contains("\"donat\".\"command_invocations\""));
    assert!(sql.contains("idempotency key was reused with different input"));
    insta::assert_snapshot!(sql);
}

#[test]
#[should_panic(expected = "command effects must be rejected before SQL generation")]
fn command_renderer_defensively_refuses_effect_bearing_ir() {
    let _ = donat_sqlgen::mutation_to_sql(&root(CommandMutation {
        identity: command_identity("create_order"),
        name: "create_order".to_owned(),
        steps: vec![],
        guards: vec![],
        result: vec![],
        idempotency: None,
        effects: vec![CommandEffectKind::StartProcess],
        selection: vec![],
    }));
}

#[test]
fn command_renderer_fails_closed_for_non_list_insert_many_ir() {
    let malformed = root(CommandMutation {
        identity: command_identity("malformed_batch"),
        name: "malformed_batch".to_owned(),
        steps: vec![CommandExecutionStep::InsertMany {
            name: "lines".to_owned(),
            cte: "_cmd_step_0".to_owned(),
            table: table("orders"),
            items: Scalar::Json(json!({ "quantity": 2 })),
            item_fields: vec![column("quantity", "int4")],
            object: vec![assignment("quantity", "int4", json!(2))],
            returning: vec![column("quantity", "int4")],
            allow_empty: false,
            check: None,
            error_path: "$.selectionSet.malformed_batch".to_owned(),
        }],
        guards: vec![],
        result: vec![],
        idempotency: None,
        effects: vec![],
        selection: vec![],
    });

    let rendered = std::panic::catch_unwind(|| donat_sqlgen::mutation_to_sql(&malformed))
        .expect("malformed closed IR must not panic the serving process");
    assert!(
        rendered.contains("raise_graphql_error")
            && rendered.contains("insert_many items must be a list of objects"),
        "malformed IR must render one fail-closed structured rejection: {rendered}"
    );
    assert!(
        !rendered.contains("INSERT INTO \"public\".\"orders\""),
        "fail-closed rendering must contain no domain write: {rendered}"
    );
}

#[test]
fn command_idempotency_executes_once_replays_and_rejects_changed_input() {
    let _catalog_lock = command_catalog_test_lock();
    let table_name = format!("command_sqlgen_{}", std::process::id());
    let command_name = format!("sqlgen_runtime_{}", std::process::id());
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated command renderer transaction");
    install_command_catalog(&mut tx);
    tx.batch_execute(&format!(
        "CREATE TABLE \"public\".\"{table_name}\" (id uuid PRIMARY KEY, status text NOT NULL)"
    ))
    .expect("create the command target table");

    let first =
        donat_sqlgen::mutation_to_sql(&idempotent_insert_root(&command_name, &table_name, "draft"));
    assert!(
        first.contains("statement_timestamp() + 60 * interval '1 second'"),
        "retention starts when this command statement elects its claim: {first}"
    );
    tx.execute(&first, &[])
        .expect("first command execution inserts and stores its result");
    let rows: i64 = tx
        .query_one(
            &format!("SELECT count(*) FROM \"public\".\"{table_name}\""),
            &[],
        )
        .expect("count the target rows")
        .get(0);
    assert_eq!(rows, 1);

    tx.execute(&first, &[])
        .expect("an exact idempotent replay returns the stored result");
    let replay_rows: i64 = tx
        .query_one(
            &format!("SELECT count(*) FROM \"public\".\"{table_name}\""),
            &[],
        )
        .expect("count rows after replay")
        .get(0);
    assert_eq!(replay_rows, 1, "replay must not run the insert CTE again");

    let changed_input = donat_sqlgen::mutation_to_sql(&idempotent_insert_root(
        &command_name,
        &table_name,
        "approved",
    ));
    tx.batch_execute("SAVEPOINT changed_input")
        .expect("savepoint isolates the expected business rejection");
    let error = tx
        .execute(&changed_input, &[])
        .expect_err("the same key with a changed canonical input must reject");
    assert_eq!(
        error.code().map(|code| code.code()),
        Some("P0D01"),
        "changed input must use the structured command rejection: {error:?}"
    );
    tx.batch_execute("ROLLBACK TO SAVEPOINT changed_input")
        .expect("the rejected command statement rolls back atomically");
    let rows_after_conflict: i64 = tx
        .query_one(
            &format!("SELECT count(*) FROM \"public\".\"{table_name}\""),
            &[],
        )
        .expect("count rows after rejected reuse")
        .get(0);
    assert_eq!(rows_after_conflict, 1);

    tx.rollback()
        .expect("remove the isolated command target and journal rows");
}

#[test]
fn command_idempotency_identity_separates_source_and_explicit_role() {
    let _catalog_lock = command_catalog_test_lock();
    let suffix = std::process::id();
    let first_table = format!("command_identity_first_{suffix}");
    let second_table = format!("command_identity_second_{suffix}");
    let command_name = format!("shared_create_order_{suffix}");
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start source/role identity transaction");
    install_command_identity_catalog(&mut tx);
    tx.batch_execute(&format!(
        "CREATE TABLE \"public\".\"{first_table}\" (id uuid PRIMARY KEY, status text NOT NULL); \
         CREATE TABLE \"public\".\"{second_table}\" (id uuid PRIMARY KEY, status text NOT NULL)"
    ))
    .expect("create identity-isolated command targets");

    let mut first = idempotent_insert_root_with_id(
        &command_name,
        &first_table,
        "550e8400-e29b-41d4-a716-446655440071",
        "buyer",
    );
    let MutationRoot::Command { command, .. } = &mut first else {
        unreachable!()
    };
    command.identity.source = "default".to_owned();
    command.identity.role = "buyer".to_owned();

    let mut second = idempotent_insert_root_with_id(
        &command_name,
        &second_table,
        "550e8400-e29b-41d4-a716-446655440072",
        "merchant",
    );
    let MutationRoot::Command { command, .. } = &mut second else {
        unreachable!()
    };
    command.identity.source = "secondary".to_owned();
    command.identity.role = "merchant".to_owned();

    tx.execute(&donat_sqlgen::mutation_to_sql(&first), &[])
        .expect("first source/role execution succeeds");
    tx.execute(&donat_sqlgen::mutation_to_sql(&second), &[])
        .expect("same command name/key in another source/role executes independently");

    for table in [&first_table, &second_table] {
        let count: i64 = tx
            .query_one(&format!("SELECT count(*) FROM \"public\".\"{table}\""), &[])
            .expect("count source-local command rows")
            .get(0);
        assert_eq!(count, 1, "each source-local command must execute once");
    }
    let identities: i64 = tx
        .query_one(
            "SELECT count(DISTINCT command_identity) FROM donat.command_invocations WHERE command_name = $1",
            &[&command_name],
        )
        .expect("count qualified journal identities")
        .get(0);
    assert_eq!(
        identities, 2,
        "source and role must qualify the journal key"
    );
    tx.rollback().expect("remove identity-isolation fixtures");
}

#[test]
fn command_legacy_unqualified_key_fails_closed_without_a_domain_write() {
    let _catalog_lock = command_catalog_test_lock();
    let suffix = std::process::id();
    let table_name = format!("command_legacy_gate_{suffix}");
    let command_name = format!("legacy_create_order_{suffix}");
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start legacy identity transaction");
    install_command_identity_catalog(&mut tx);
    tx.batch_execute(&format!(
        "CREATE TABLE \"public\".\"{table_name}\" (id uuid PRIMARY KEY, status text NOT NULL); \
         INSERT INTO \"public\".\"{table_name}\" VALUES ('550e8400-e29b-41d4-a716-446655440080', 'legacy')"
    ))
    .expect("seed the pre-upgrade domain write");
    tx.execute(
        "INSERT INTO donat.command_invocations \
         (command_identity, command_name, scope_hash, key, input_fingerprint, result, expires_at) \
         VALUES ('legacy-unqualified:' || encode(convert_to($1, 'UTF8'), 'hex'), $1, \
                 decode(md5((jsonb_build_array('\"tenant-1\"'::jsonb))::text), 'hex'), \
                 'request-1', decode('01', 'hex'), '{\"status\":\"legacy\"}'::jsonb, \
                 statement_timestamp() + interval '1 day')",
        &[&command_name],
    )
    .expect("seed a preserved pre-V5 completed invocation");

    let invocation = idempotent_insert_root_with_id(
        &command_name,
        &table_name,
        "550e8400-e29b-41d4-a716-446655440081",
        "new",
    );
    tx.batch_execute("SAVEPOINT legacy_key")
        .expect("isolate the expected legacy-key rejection");
    let error = tx
        .execute(&donat_sqlgen::mutation_to_sql(&invocation), &[])
        .expect_err("an unattributable pre-V5 key must fail closed");
    assert_eq!(error.code().map(|code| code.code()), Some("P0D01"));
    let payload: Json = serde_json::from_str(
        error
            .as_db_error()
            .expect("legacy rejection is a database error")
            .message(),
    )
    .expect("structured legacy error");
    assert_eq!(
        payload["message"],
        "legacy idempotency key cannot be replayed safely after command identity migration"
    );
    tx.batch_execute("ROLLBACK TO SAVEPOINT legacy_key")
        .expect("rollback expected legacy-key rejection");
    let rows: i64 = tx
        .query_one(
            &format!("SELECT count(*) FROM \"public\".\"{table_name}\""),
            &[],
        )
        .expect("count domain rows after legacy rejection")
        .get(0);
    assert_eq!(
        rows, 1,
        "upgrade safety must prevent a duplicate domain write"
    );
    tx.rollback().expect("remove legacy gate fixtures");
}

#[test]
fn command_business_rejection_rolls_back_claim_and_journal_before_retry() {
    let _catalog_lock = command_catalog_test_lock();
    let table_name = format!("command_sqlgen_rollback_{}", std::process::id());
    let command_name = format!("sqlgen_rollback_{}", std::process::id());
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated command renderer transaction");
    install_command_catalog(&mut tx);
    tx.batch_execute(&format!(
        "CREATE TABLE \"public\".\"{table_name}\" (id uuid PRIMARY KEY, status text NOT NULL)"
    ))
    .expect("create the command target table");

    let mut rejected = idempotent_insert_root(&command_name, &table_name, "draft");
    let MutationRoot::Command { command, .. } = &mut rejected else {
        panic!("command helper must build a command root");
    };
    command.guards.push(CommandRule {
        sql: "FALSE".to_owned(),
        pg_type: "bool".to_owned(),
        error_path: "$.selectionSet.create_order".to_owned(),
        message: "customer is not allowed to order".to_owned(),
    });
    let rejected_sql = donat_sqlgen::mutation_to_sql(&rejected);

    tx.batch_execute("SAVEPOINT rejected_command")
        .expect("savepoint isolates expected business rejection");
    let error = tx
        .execute(&rejected_sql, &[])
        .expect_err("a guard denial rejects the first command execution");
    assert_eq!(error.code().map(|code| code.code()), Some("P0D01"));
    tx.batch_execute("ROLLBACK TO SAVEPOINT rejected_command")
        .expect("business rejection rolls back its whole command statement");

    let target_rows: i64 = tx
        .query_one(
            &format!("SELECT count(*) FROM \"public\".\"{table_name}\""),
            &[],
        )
        .expect("target row count query succeeds")
        .get(0);
    let claim_rows: i64 = tx
        .query_one(
            "SELECT count(*) FROM donat.command_invocation_claims WHERE command_name = $1",
            &[&command_name],
        )
        .expect("claim row count query succeeds")
        .get(0);
    let journal_rows: i64 = tx
        .query_one(
            "SELECT count(*) FROM donat.command_invocations WHERE command_name = $1",
            &[&command_name],
        )
        .expect("journal row count query succeeds")
        .get(0);
    assert_eq!(target_rows, 0, "guard denial rolls back the domain write");
    assert_eq!(claim_rows, 0, "guard denial rolls back the V4 claim");
    assert_eq!(
        journal_rows, 0,
        "guard denial rolls back the V3 result journal"
    );

    let retry =
        donat_sqlgen::mutation_to_sql(&idempotent_insert_root(&command_name, &table_name, "draft"));
    tx.execute(&retry, &[])
        .expect("the same key can execute after the rejected statement rolled back");
    let retry_rows: i64 = tx
        .query_one(
            &format!("SELECT count(*) FROM \"public\".\"{table_name}\""),
            &[],
        )
        .expect("target row count after retry succeeds")
        .get(0);
    assert_eq!(retry_rows, 1, "retry becomes the first committed executor");

    tx.rollback()
        .expect("remove the isolated command target and journal rows");
}

#[test]
fn command_insert_many_rule_binding_uses_each_current_item_value() {
    let table_name = format!("command_sqlgen_item_rule_{}", std::process::id());
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated command renderer transaction");
    tx.batch_execute(&format!(
        "CREATE TABLE \"public\".\"{table_name}\" (id bigserial PRIMARY KEY, quantity int4 NOT NULL)"
    ))
    .expect("create the command target table");

    let command = root(CommandMutation {
        identity: command_identity("item_rule_batch"),
        name: "item_rule_batch".to_owned(),
        steps: vec![CommandExecutionStep::InsertMany {
            name: "lines".to_owned(),
            cte: "_cmd_step_0".to_owned(),
            table: table(&table_name),
            items: Scalar::Json(json!([
                { "quantity": 2 },
                { "quantity": 3 }
            ])),
            item_fields: vec![column("quantity", "int4")],
            object: vec![CommandAssignment {
                column: column("quantity", "int4"),
                value: CommandExecutionValue::Rule {
                    sql: "((\"_cmd_item\".\"quantity\")::numeric * 2)".to_owned(),
                    pg_type: "int4".to_owned(),
                },
            }],
            returning: vec![column("quantity", "int4")],
            allow_empty: false,
            check: None,
            error_path: "$.selectionSet.item_rule_batch".to_owned(),
        }],
        guards: vec![],
        result: vec![CommandResultField {
            name: "lines".to_owned(),
            value: CommandResultValue::StepRow {
                cte: "_cmd_step_0".to_owned(),
                many: true,
                columns: vec![column("quantity", "int4")],
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::List {
            alias: "lines".to_owned(),
            field: "lines".to_owned(),
            selections: vec![CommandResultSelection::Scalar {
                alias: "quantity".to_owned(),
                field: "quantity".to_owned(),
            }],
        }],
    });

    let sql = donat_sqlgen::mutation_to_sql(&command);
    assert!(
        sql.contains("AS \"_cmd_item\""),
        "compiled Rules must receive a concrete per-item alias: {sql}"
    );
    tx.execute(&sql, &[])
        .expect("the pre-lowered Rule sees each typed item binding");
    let quantities = tx
        .query(
            &format!("SELECT quantity FROM \"public\".\"{table_name}\" ORDER BY id"),
            &[],
        )
        .expect("query inserted rows")
        .into_iter()
        .map(|row| row.get::<_, i32>(0))
        .collect::<Vec<_>>();
    assert_eq!(quantities, vec![4, 6]);

    tx.rollback()
        .expect("remove the isolated command target table");
}

#[test]
fn command_guard_rejection_does_not_reach_a_before_insert_trigger() {
    let _catalog_lock = command_catalog_test_lock();
    let table_name = format!("command_sqlgen_guard_trigger_{}", std::process::id());
    let function_name = format!("command_sqlgen_guard_trigger_fn_{}", std::process::id());
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated command renderer transaction");
    install_command_catalog(&mut tx);
    tx.batch_execute(&format!(
        r#"
        CREATE TABLE "public"."{table_name}" (id uuid PRIMARY KEY, status text NOT NULL);
        CREATE FUNCTION "public"."{function_name}"() RETURNS trigger AS $$
        BEGIN
            RAISE EXCEPTION 'guard trigger must not run' USING ERRCODE = 'P0G01';
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER "before_insert" BEFORE INSERT ON "public"."{table_name}"
        FOR EACH ROW EXECUTE FUNCTION "public"."{function_name}"();
        "#
    ))
    .expect("create trigger-sensitive command target");

    let mut root = idempotent_insert_root("guard_trigger_command", &table_name, "draft");
    let MutationRoot::Command { command, .. } = &mut root else {
        panic!("helper must construct a command root");
    };
    command.idempotency = None;
    command.guards.push(CommandRule {
        sql: "FALSE".to_owned(),
        pg_type: "bool".to_owned(),
        error_path: "$.selectionSet.create_order".to_owned(),
        message: "guard denied".to_owned(),
    });

    let sql = donat_sqlgen::mutation_to_sql(&root);
    assert!(
        sql.contains("\"_cmd_guard_gate\""),
        "the guard must be an explicit dependency of every command DML CTE: {sql}"
    );
    let error = tx
        .execute(&sql, &[])
        .expect_err("a false guard must reject before the DML trigger runs");
    assert_eq!(
        error.code().map(|code| code.code()),
        Some("P0D01"),
        "a trigger error proves the insert was reached: {error:?}"
    );

    tx.rollback()
        .expect("remove the isolated trigger-sensitive target");
}

#[test]
fn command_assertion_rejection_does_not_reach_later_dml_trigger() {
    let _catalog_lock = command_catalog_test_lock();
    let table_name = format!("command_sqlgen_assert_trigger_{}", std::process::id());
    let function_name = format!("command_sqlgen_assert_trigger_fn_{}", std::process::id());
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated command renderer transaction");
    install_command_catalog(&mut tx);
    tx.batch_execute(&format!(
        r#"
        CREATE TABLE "public"."{table_name}" (id uuid PRIMARY KEY, status text NOT NULL);
        CREATE FUNCTION "public"."{function_name}"() RETURNS trigger AS $$
        BEGIN
            RAISE EXCEPTION 'assertion trigger must not run' USING ERRCODE = 'P0G01';
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER "before_insert" BEFORE INSERT ON "public"."{table_name}"
        FOR EACH ROW EXECUTE FUNCTION "public"."{function_name}"();
        "#
    ))
    .expect("create trigger-sensitive command target");

    let command = root(CommandMutation {
        identity: command_identity("assert_trigger_command"),
        name: "assert_trigger_command".to_owned(),
        steps: vec![
            CommandExecutionStep::Assert {
                name: "must_be_allowed".to_owned(),
                rule: CommandRule {
                    sql: "FALSE".to_owned(),
                    pg_type: "bool".to_owned(),
                    error_path: "$.selectionSet.create_order".to_owned(),
                    message: "assertion denied".to_owned(),
                },
            },
            CommandExecutionStep::Insert {
                name: "order".to_owned(),
                cte: "_cmd_step_1".to_owned(),
                table: table(&table_name),
                object: vec![
                    assignment("id", "uuid", json!("550e8400-e29b-41d4-a716-446655440000")),
                    assignment("status", "text", json!("draft")),
                ],
                returning: vec![column("id", "uuid"), column("status", "text")],
                check: None,
                error_path: "$.selectionSet.create_order".to_owned(),
            },
        ],
        guards: vec![],
        result: vec![CommandResultField {
            name: "status".to_owned(),
            value: CommandResultValue::StepColumn {
                cte: "_cmd_step_1".to_owned(),
                column: column("status", "text"),
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::Scalar {
            alias: "status".to_owned(),
            field: "status".to_owned(),
        }],
    });

    let sql = donat_sqlgen::mutation_to_sql(&command);
    assert!(
        sql.contains("\"_cmd_assert_gate_0\""),
        "an assertion must materialize a gate CTE for later DML: {sql}"
    );
    let error = tx
        .execute(&sql, &[])
        .expect_err("a false assertion must reject before later DML runs");
    assert_eq!(
        error.code().map(|code| code.code()),
        Some("P0D01"),
        "a trigger error proves the later insert was reached: {error:?}"
    );

    tx.rollback()
        .expect("remove the isolated trigger-sensitive target");
}

#[test]
fn command_required_update_rejection_does_not_reach_later_dml_trigger() {
    let _catalog_lock = command_catalog_test_lock();
    let table_name = format!(
        "command_sqlgen_required_update_trigger_{}",
        std::process::id()
    );
    let function_name = format!(
        "command_sqlgen_required_update_trigger_fn_{}",
        std::process::id()
    );
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated command renderer transaction");
    install_command_catalog(&mut tx);
    tx.batch_execute(&format!(
        r#"
        CREATE TABLE "public"."{table_name}" (id uuid PRIMARY KEY, status text NOT NULL);
        CREATE FUNCTION "public"."{function_name}"() RETURNS trigger AS $$
        BEGIN
            RAISE EXCEPTION 'required update trigger must not run' USING ERRCODE = 'P0G01';
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER "before_insert" BEFORE INSERT ON "public"."{table_name}"
        FOR EACH ROW EXECUTE FUNCTION "public"."{function_name}"();
        "#
    ))
    .expect("create required-update trigger-sensitive command target");

    let command = root(CommandMutation {
        identity: command_identity("required_update_trigger_command"),
        name: "required_update_trigger_command".to_owned(),
        steps: vec![
            CommandExecutionStep::Update {
                name: "missing_order".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                table: table(&table_name),
                predicate: vec![assignment(
                    "id",
                    "uuid",
                    json!("550e8400-e29b-41d4-a716-446655440000"),
                )],
                set: vec![assignment("status", "text", json!("approved"))],
                returning: vec![column("id", "uuid"), column("status", "text")],
                require_affected: true,
                filter: None,
                check: None,
                error_path: "$.selectionSet.advance_order".to_owned(),
            },
            CommandExecutionStep::Insert {
                name: "later_order".to_owned(),
                cte: "_cmd_step_1".to_owned(),
                table: table(&table_name),
                object: vec![
                    assignment("id", "uuid", json!("550e8400-e29b-41d4-a716-446655440001")),
                    assignment("status", "text", json!("draft")),
                ],
                returning: vec![column("id", "uuid"), column("status", "text")],
                check: None,
                error_path: "$.selectionSet.advance_order".to_owned(),
            },
        ],
        guards: vec![],
        result: vec![CommandResultField {
            name: "status".to_owned(),
            value: CommandResultValue::StepColumn {
                cte: "_cmd_step_1".to_owned(),
                column: column("status", "text"),
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::Scalar {
            alias: "status".to_owned(),
            field: "status".to_owned(),
        }],
    });

    let sql = donat_sqlgen::mutation_to_sql(&command);
    assert!(
        sql.contains("\"_cmd_required_gate_0\" AS MATERIALIZED"),
        "a required update must materialize its success gate: {sql}"
    );
    let later_step_sql = sql
        .split("\"_cmd_step_1\" AS")
        .nth(1)
        .and_then(|tail| tail.split(" RETURNING *").next())
        .expect("rendered SQL contains the later insert CTE");
    assert!(
        later_step_sql.contains("\"_cmd_required_gate_0\""),
        "later DML must explicitly depend on the required update gate: {sql}"
    );
    let error = tx
        .execute(&sql, &[])
        .expect_err("a missing required update row must reject before later DML runs");
    assert_eq!(
        error.code().map(|code| code.code()),
        Some("P0D01"),
        "a trigger error proves the later insert was reached: {error:?}"
    );

    tx.rollback()
        .expect("remove the isolated required-update trigger-sensitive target");
}

#[test]
fn command_required_select_rejection_does_not_reach_later_dml_trigger() {
    let _catalog_lock = command_catalog_test_lock();
    let table_name = format!(
        "command_sqlgen_required_select_trigger_{}",
        std::process::id()
    );
    let function_name = format!(
        "command_sqlgen_required_select_trigger_fn_{}",
        std::process::id()
    );
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated command renderer transaction");
    install_command_catalog(&mut tx);
    tx.batch_execute(&format!(
        r#"
        CREATE TABLE "public"."{table_name}" (id uuid PRIMARY KEY, status text NOT NULL);
        CREATE FUNCTION "public"."{function_name}"() RETURNS trigger AS $$
        BEGIN
            RAISE EXCEPTION 'required select trigger must not run' USING ERRCODE = 'P0G01';
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER "before_insert" BEFORE INSERT ON "public"."{table_name}"
        FOR EACH ROW EXECUTE FUNCTION "public"."{function_name}"();
        "#
    ))
    .expect("create required-select trigger-sensitive command target");

    let command = root(CommandMutation {
        identity: command_identity("required_select_trigger_command"),
        name: "required_select_trigger_command".to_owned(),
        steps: vec![
            CommandExecutionStep::SelectOne {
                name: "missing_order".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                table: table(&table_name),
                by: vec![assignment(
                    "id",
                    "uuid",
                    json!("550e8400-e29b-41d4-a716-446655440000"),
                )],
                returning: vec![column("id", "uuid"), column("status", "text")],
                require_found: true,
                filter: None,
                error_path: "$.selectionSet.advance_order".to_owned(),
            },
            CommandExecutionStep::Insert {
                name: "later_order".to_owned(),
                cte: "_cmd_step_1".to_owned(),
                table: table(&table_name),
                object: vec![
                    assignment("id", "uuid", json!("550e8400-e29b-41d4-a716-446655440001")),
                    assignment("status", "text", json!("draft")),
                ],
                returning: vec![column("id", "uuid"), column("status", "text")],
                check: None,
                error_path: "$.selectionSet.advance_order".to_owned(),
            },
        ],
        guards: vec![],
        result: vec![CommandResultField {
            name: "status".to_owned(),
            value: CommandResultValue::StepColumn {
                cte: "_cmd_step_1".to_owned(),
                column: column("status", "text"),
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::Scalar {
            alias: "status".to_owned(),
            field: "status".to_owned(),
        }],
    });

    let sql = donat_sqlgen::mutation_to_sql(&command);
    assert!(
        sql.contains("\"_cmd_required_gate_0\" AS MATERIALIZED"),
        "a required select must materialize its success gate: {sql}"
    );
    let later_step_sql = sql
        .split("\"_cmd_step_1\" AS")
        .nth(1)
        .and_then(|tail| tail.split(" RETURNING *").next())
        .expect("rendered SQL contains the later insert CTE");
    assert!(
        later_step_sql.contains("\"_cmd_required_gate_0\""),
        "later DML must explicitly depend on the required select gate: {sql}"
    );
    let error = tx
        .execute(&sql, &[])
        .expect_err("a missing required select row must reject before later DML runs");
    assert_eq!(
        error.code().map(|code| code.code()),
        Some("P0D01"),
        "a trigger error proves the later insert was reached: {error:?}"
    );

    tx.rollback()
        .expect("remove the isolated required-select trigger-sensitive target");
}

#[test]
fn command_required_delete_rejection_does_not_reach_later_dml_trigger() {
    let _catalog_lock = command_catalog_test_lock();
    let table_name = format!(
        "command_sqlgen_required_delete_trigger_{}",
        std::process::id()
    );
    let function_name = format!(
        "command_sqlgen_required_delete_trigger_fn_{}",
        std::process::id()
    );
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated command renderer transaction");
    install_command_catalog(&mut tx);
    tx.batch_execute(&format!(
        r#"
        CREATE TABLE "public"."{table_name}" (id uuid PRIMARY KEY, status text NOT NULL);
        CREATE FUNCTION "public"."{function_name}"() RETURNS trigger AS $$
        BEGIN
            RAISE EXCEPTION 'required delete trigger must not run' USING ERRCODE = 'P0G01';
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER "before_insert" BEFORE INSERT ON "public"."{table_name}"
        FOR EACH ROW EXECUTE FUNCTION "public"."{function_name}"();
        "#
    ))
    .expect("create required-delete trigger-sensitive command target");

    let command = root(CommandMutation {
        identity: command_identity("required_delete_trigger_command"),
        name: "required_delete_trigger_command".to_owned(),
        steps: vec![
            CommandExecutionStep::Delete {
                name: "missing_order".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                table: table(&table_name),
                predicate: vec![assignment(
                    "id",
                    "uuid",
                    json!("550e8400-e29b-41d4-a716-446655440000"),
                )],
                returning: vec![column("id", "uuid"), column("status", "text")],
                require_affected: true,
                filter: None,
                error_path: "$.selectionSet.advance_order".to_owned(),
            },
            CommandExecutionStep::Insert {
                name: "later_order".to_owned(),
                cte: "_cmd_step_1".to_owned(),
                table: table(&table_name),
                object: vec![
                    assignment("id", "uuid", json!("550e8400-e29b-41d4-a716-446655440001")),
                    assignment("status", "text", json!("draft")),
                ],
                returning: vec![column("id", "uuid"), column("status", "text")],
                check: None,
                error_path: "$.selectionSet.advance_order".to_owned(),
            },
        ],
        guards: vec![],
        result: vec![CommandResultField {
            name: "status".to_owned(),
            value: CommandResultValue::StepColumn {
                cte: "_cmd_step_1".to_owned(),
                column: column("status", "text"),
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::Scalar {
            alias: "status".to_owned(),
            field: "status".to_owned(),
        }],
    });

    let sql = donat_sqlgen::mutation_to_sql(&command);
    assert!(
        sql.contains("\"_cmd_required_gate_0\" AS MATERIALIZED"),
        "a required delete must materialize its success gate: {sql}"
    );
    let later_step_sql = sql
        .split("\"_cmd_step_1\" AS")
        .nth(1)
        .and_then(|tail| tail.split(" RETURNING *").next())
        .expect("rendered SQL contains the later insert CTE");
    assert!(
        later_step_sql.contains("\"_cmd_required_gate_0\""),
        "later DML must explicitly depend on the required delete gate: {sql}"
    );
    let error = tx
        .execute(&sql, &[])
        .expect_err("a missing required delete row must reject before later DML runs");
    assert_eq!(
        error.code().map(|code| code.code()),
        Some("P0D01"),
        "a trigger error proves the later insert was reached: {error:?}"
    );

    tx.rollback()
        .expect("remove the isolated required-delete trigger-sensitive target");
}

#[test]
fn command_reclaims_expired_claim_and_replaces_expired_canonical_result() {
    let _catalog_lock = command_catalog_test_lock();
    let table_name = format!("command_sqlgen_expiry_{}", std::process::id());
    let command_name = format!("sqlgen_expiry_{}", std::process::id());
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated command renderer transaction");
    install_command_catalog(&mut tx);
    tx.batch_execute(&format!(
        "CREATE TABLE \"public\".\"{table_name}\" (id uuid PRIMARY KEY, status text NOT NULL)"
    ))
    .expect("create the command target table");

    let first = donat_sqlgen::mutation_to_sql(&idempotent_insert_root_with_id(
        &command_name,
        &table_name,
        "550e8400-e29b-41d4-a716-446655440000",
        "draft",
    ));
    tx.execute(&first, &[])
        .expect("first command execution stores an expirable invocation");
    tx.execute(
        "UPDATE donat.command_invocation_claims SET expires_at = statement_timestamp() - interval '1 second' WHERE command_name = $1",
        &[&command_name],
    )
    .expect("expire the election row without relying on a cleanup worker");
    tx.execute(
        "UPDATE donat.command_invocations SET expires_at = statement_timestamp() - interval '1 second' WHERE command_name = $1",
        &[&command_name],
    )
    .expect("expire the matching canonical journal row");

    let reclaimed = donat_sqlgen::mutation_to_sql(&idempotent_insert_root_with_id(
        &command_name,
        &table_name,
        "550e8400-e29b-41d4-a716-446655440001",
        "approved",
    ));
    tx.execute(&reclaimed, &[])
        .expect("an expired claim elects a new executor and replaces V3 data");
    let target_rows: i64 = tx
        .query_one(
            &format!("SELECT count(*) FROM \"public\".\"{table_name}\""),
            &[],
        )
        .expect("target row count query succeeds")
        .get(0);
    let stored_status: String = tx
        .query_one(
            "SELECT result->>'status' FROM donat.command_invocations WHERE command_name = $1",
            &[&command_name],
        )
        .expect("reclaimed invocation result query succeeds")
        .get(0);
    assert_eq!(
        target_rows, 2,
        "expired key permits the newly elected write"
    );
    assert_eq!(
        stored_status, "approved",
        "reclaim replaces the expired canonical result instead of replaying it"
    );

    tx.rollback()
        .expect("remove the isolated command target and journal rows");
}

#[test]
fn command_insert_many_executes_postgres_typed_items_in_declared_input_order() {
    let table_name = format!("command_sqlgen_items_{}", std::process::id());
    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start an isolated insert_many transaction");
    tx.batch_execute(&format!(
        "CREATE TABLE \"public\".\"{table_name}\" (sku text NOT NULL, quantity int4 NOT NULL, guard_value int4 NOT NULL)"
    ))
    .expect("create typed insert_many target table");
    install_check_violation_helper(&mut tx);

    let sql = donat_sqlgen::mutation_to_sql(&root(CommandMutation {
        identity: command_identity("create_lines"),
        name: "create_lines".to_owned(),
        steps: vec![CommandExecutionStep::InsertMany {
            name: "lines".to_owned(),
            cte: "_cmd_step_0".to_owned(),
            table: table(&table_name),
            items: Scalar::Json(json!([
                { "sku": "sku-1", "quantity": 2 },
                { "sku": "sku-2", "quantity": 1 }
            ])),
            item_fields: vec![column("sku", "text"), column("quantity", "int4")],
            object: vec![
                CommandAssignment {
                    column: column("sku", "text"),
                    value: CommandExecutionValue::Item {
                        field: "sku".to_owned(),
                        pg_type: "text".to_owned(),
                    },
                },
                CommandAssignment {
                    column: column("quantity", "int4"),
                    value: CommandExecutionValue::Item {
                        field: "quantity".to_owned(),
                        pg_type: "int4".to_owned(),
                    },
                },
                assignment("guard_value", "int4", json!(1)),
            ],
            returning: vec![column("sku", "text"), column("quantity", "int4")],
            allow_empty: false,
            check: Some(BoolExp::Compare {
                column: "guard_value".to_owned(),
                pg_type: "int4".to_owned(),
                op: CompareOp::Gte(Scalar::Json(json!(1))),
            }),
            error_path: "$.selectionSet.create_lines".to_owned(),
        }],
        guards: vec![],
        result: vec![CommandResultField {
            name: "lines".to_owned(),
            value: CommandResultValue::StepRow {
                cte: "_cmd_step_0".to_owned(),
                many: true,
                columns: vec![column("sku", "text"), column("quantity", "int4")],
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::List {
            alias: "lines".to_owned(),
            field: "lines".to_owned(),
            selections: vec![
                CommandResultSelection::Scalar {
                    alias: "sku".to_owned(),
                    field: "sku".to_owned(),
                },
                CommandResultSelection::Scalar {
                    alias: "quantity".to_owned(),
                    field: "quantity".to_owned(),
                },
            ],
        }],
    }));

    let response: Json = tx
        .query_one(&sql, &[])
        .expect("one command statement executes typed insert_many values")
        .get(0);
    assert_eq!(
        response,
        json!({
            "lines": [
                { "sku": "sku-1", "quantity": 2 },
                { "sku": "sku-2", "quantity": 1 }
            ]
        }),
        "the response preserves the declared array order rather than RETURNING's implicit order"
    );
    let target_rows: i64 = tx
        .query_one(
            &format!("SELECT count(*) FROM \"public\".\"{table_name}\""),
            &[],
        )
        .expect("typed insert_many target row count query succeeds")
        .get(0);
    assert_eq!(target_rows, 2);

    tx.rollback()
        .expect("remove the isolated insert_many target table");
}

#[test]
fn command_concurrent_retry_waits_for_claim_and_replays_one_canonical_result() {
    let _catalog_lock = command_catalog_test_lock();
    let suffix = std::process::id();
    let schema_name = format!("command_sqlgen_concurrent_{suffix}");
    let table_name = "orders";
    let command_name = format!("sqlgen_concurrent_{suffix}");
    let advisory_lock = 7_100_000_000_i64 + i64::from(suffix);
    let pg_url = std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15433/postgres".to_owned());
    let mut setup = postgres_client_at(&pg_url);
    install_command_catalog_client(&mut setup);
    setup
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS \"{schema_name}\" CASCADE; \
             CREATE SCHEMA \"{schema_name}\"; \
             CREATE TABLE \"{schema_name}\".\"{table_name}\" (id uuid PRIMARY KEY, status text NOT NULL); \
             CREATE FUNCTION \"{schema_name}\".\"hold_first_insert\"() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN \
               PERFORM pg_advisory_xact_lock({advisory_lock}); \
               PERFORM pg_sleep(0.5); \
               RETURN NEW; \
             END; \
             $$; \
             CREATE TRIGGER \"hold_first_insert\" BEFORE INSERT ON \"{schema_name}\".\"{table_name}\" \
             FOR EACH ROW EXECUTE FUNCTION \"{schema_name}\".\"hold_first_insert\"();"
        ))
        .expect("create deterministic concurrent command fixture");

    let command_root = idempotent_insert_root(&command_name, table_name, "draft");
    let MutationRoot::Command { command, .. } = &command_root else {
        panic!("command helper must build a command root");
    };
    let mut command = command.clone();
    let CommandExecutionStep::Insert { table, .. } = &mut command.steps[0] else {
        panic!("command helper must create an insert step");
    };
    table.schema = schema_name.clone();
    let sql = donat_sqlgen::mutation_to_sql(&root(command));

    let first_url = pg_url.clone();
    let first_sql = sql.clone();
    let first = std::thread::spawn(move || {
        let mut client = postgres_client_at(&first_url);
        let mut tx = client
            .transaction()
            .expect("first concurrent command transaction starts");
        tx.execute(&first_sql, &[])
            .expect("first concurrent command executes");
        tx.commit().expect("first concurrent command commits");
    });

    let mut probe = postgres_client_at(&pg_url);
    let mut first_has_claim = false;
    for _ in 0..100 {
        let acquired: bool = probe
            .query_one("SELECT pg_try_advisory_lock($1)", &[&advisory_lock])
            .expect("probe advisory lock query succeeds")
            .get(0);
        if acquired {
            probe
                .execute("SELECT pg_advisory_unlock($1)", &[&advisory_lock])
                .expect("release unsuccessful probe lock");
            std::thread::sleep(Duration::from_millis(10));
        } else {
            first_has_claim = true;
            break;
        }
    }
    assert!(
        first_has_claim,
        "the target trigger proves the first statement passed the V4 claim before retry starts"
    );

    let mut second_client = postgres_client_at(&pg_url);
    let mut second_tx = second_client
        .transaction()
        .expect("second concurrent command transaction starts");
    second_tx
        .execute(&sql, &[])
        .expect("later retry waits for the claim then replays the stored result");
    second_tx
        .commit()
        .expect("second concurrent command transaction commits");
    first.join().expect("first command thread succeeds");

    let target_rows: i64 = setup
        .query_one(
            &format!("SELECT count(*) FROM \"{schema_name}\".\"{table_name}\""),
            &[],
        )
        .expect("concurrent target row count query succeeds")
        .get(0);
    let journal_rows: i64 = setup
        .query_one(
            "SELECT count(*) FROM donat.command_invocations WHERE command_name = $1",
            &[&command_name],
        )
        .expect("concurrent journal row count query succeeds")
        .get(0);
    let stored_status: String = setup
        .query_one(
            "SELECT result->>'status' FROM donat.command_invocations WHERE command_name = $1",
            &[&command_name],
        )
        .expect("concurrent canonical result query succeeds")
        .get(0);
    assert_eq!(
        target_rows, 1,
        "only the elected statement writes the domain row"
    );
    assert_eq!(
        journal_rows, 1,
        "both statements share one V3 invocation row"
    );
    assert_eq!(
        stored_status, "draft",
        "retry returns the first canonical result"
    );

    setup
        .batch_execute(&format!("DROP SCHEMA \"{schema_name}\" CASCADE"))
        .expect("remove deterministic concurrent command fixture");
}

#[test]
fn command_renderer_snapshots_guarded_insert_and_declared_projection() {
    let sql = donat_sqlgen::mutation_to_sql(&root(CommandMutation {
        identity: command_identity("create_order"),
        name: "create_order".to_owned(),
        steps: vec![CommandExecutionStep::Insert {
            name: "order".to_owned(),
            cte: "_cmd_step_0".to_owned(),
            table: table("orders"),
            object: vec![
                assignment("id", "uuid", json!("550e8400-e29b-41d4-a716-446655440000")),
                assignment("status", "text", json!("draft")),
            ],
            returning: vec![column("id", "uuid"), column("status", "text")],
            check: Some(BoolExp::Compare {
                column: "status".to_owned(),
                pg_type: "text".to_owned(),
                op: CompareOp::Neq(Scalar::Json(json!("blocked"))),
            }),
            error_path: "$.selectionSet.create_order".to_owned(),
        }],
        guards: vec![CommandRule {
            sql: "TRUE".to_owned(),
            pg_type: "bool".to_owned(),
            error_path: "$.selectionSet.create_order".to_owned(),
            message: "customer is not allowed to order".to_owned(),
        }],
        result: vec![CommandResultField {
            name: "order".to_owned(),
            value: order_row_result("_cmd_step_0"),
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::Object {
            alias: "order".to_owned(),
            field: "order".to_owned(),
            selections: vec![
                CommandResultSelection::Scalar {
                    alias: "id".to_owned(),
                    field: "id".to_owned(),
                },
                CommandResultSelection::Scalar {
                    alias: "status".to_owned(),
                    field: "status".to_owned(),
                },
            ],
        }],
    }));

    assert!(sql.contains("donat.check_violation"), "{sql}");
    assert!(sql.contains("donat.raise_graphql_error"), "{sql}");
    insta::assert_snapshot!(sql);
}

#[test]
fn command_renderer_snapshots_select_one_then_update() {
    let order_id = column("id", "uuid");
    let sql = donat_sqlgen::mutation_to_sql(&root(CommandMutation {
        identity: command_identity("approve_order"),
        name: "approve_order".to_owned(),
        steps: vec![
            CommandExecutionStep::SelectOne {
                name: "order".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                table: table("orders"),
                by: vec![assignment(
                    "id",
                    "uuid",
                    json!("550e8400-e29b-41d4-a716-446655440000"),
                )],
                returning: vec![order_id.clone(), column("status", "text")],
                require_found: true,
                filter: Some(BoolExp::Compare {
                    column: "customer_id".to_owned(),
                    pg_type: "uuid".to_owned(),
                    op: CompareOp::Eq(Scalar::Json(json!("550e8400-e29b-41d4-a716-446655440001"))),
                }),
                error_path: "$.selectionSet.approve_order".to_owned(),
            },
            CommandExecutionStep::Update {
                name: "approved".to_owned(),
                cte: "_cmd_step_1".to_owned(),
                table: table("orders"),
                predicate: vec![CommandAssignment {
                    column: order_id.clone(),
                    value: CommandExecutionValue::StepColumn {
                        cte: "_cmd_step_0".to_owned(),
                        column: order_id,
                    },
                }],
                set: vec![assignment("status", "text", json!("approved"))],
                returning: vec![column("id", "uuid"), column("status", "text")],
                require_affected: true,
                filter: None,
                check: None,
                error_path: "$.selectionSet.approve_order".to_owned(),
            },
        ],
        guards: vec![],
        result: vec![CommandResultField {
            name: "order".to_owned(),
            value: order_row_result("_cmd_step_1"),
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::Object {
            alias: "order".to_owned(),
            field: "order".to_owned(),
            selections: vec![CommandResultSelection::Scalar {
                alias: "status".to_owned(),
                field: "status".to_owned(),
            }],
        }],
    }));

    assert!(sql.contains("UPDATE \"public\".\"orders\""), "{sql}");
    assert!(sql.contains("did not find a row"), "{sql}");
    assert!(sql.contains("did not affect a row"), "{sql}");
    insta::assert_snapshot!(sql);
}

#[test]
fn command_renderer_snapshots_insert_many_and_assert() {
    let order_id = column("id", "uuid");
    let sql = donat_sqlgen::mutation_to_sql(&root(CommandMutation {
        identity: command_identity("create_order_lines"),
        name: "create_order_lines".to_owned(),
        steps: vec![
            CommandExecutionStep::Insert {
                name: "order".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                table: table("orders"),
                object: vec![assignment(
                    "id",
                    "uuid",
                    json!("550e8400-e29b-41d4-a716-446655440000"),
                )],
                returning: vec![order_id.clone()],
                check: None,
                error_path: "$.selectionSet.create_order_lines".to_owned(),
            },
            CommandExecutionStep::InsertMany {
                name: "lines".to_owned(),
                cte: "_cmd_step_1".to_owned(),
                table: table("order_lines"),
                items: Scalar::Json(json!([
                    { "sku": "sku-1", "quantity": 2 },
                    { "sku": "sku-2", "quantity": 1 }
                ])),
                item_fields: vec![column("sku", "text"), column("quantity", "int4")],
                object: vec![
                    CommandAssignment {
                        column: column("order_id", "uuid"),
                        value: CommandExecutionValue::StepColumn {
                            cte: "_cmd_step_0".to_owned(),
                            column: order_id,
                        },
                    },
                    CommandAssignment {
                        column: column("sku", "text"),
                        value: CommandExecutionValue::Item {
                            field: "sku".to_owned(),
                            pg_type: "text".to_owned(),
                        },
                    },
                    CommandAssignment {
                        column: column("quantity", "int4"),
                        value: CommandExecutionValue::Item {
                            field: "quantity".to_owned(),
                            pg_type: "int4".to_owned(),
                        },
                    },
                ],
                returning: vec![column("id", "uuid"), column("sku", "text")],
                allow_empty: false,
                check: None,
                error_path: "$.selectionSet.create_order_lines".to_owned(),
            },
            CommandExecutionStep::Assert {
                name: "line_policy".to_owned(),
                rule: CommandRule {
                    sql: "FALSE".to_owned(),
                    pg_type: "bool".to_owned(),
                    error_path: "$.selectionSet.create_order_lines".to_owned(),
                    message: "line policy rejected".to_owned(),
                },
            },
        ],
        guards: vec![],
        result: vec![CommandResultField {
            name: "lines".to_owned(),
            value: CommandResultValue::StepRow {
                cte: "_cmd_step_1".to_owned(),
                many: true,
                columns: vec![column("id", "uuid"), column("sku", "text")],
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::List {
            alias: "lines".to_owned(),
            field: "lines".to_owned(),
            selections: vec![CommandResultSelection::Scalar {
                alias: "sku".to_owned(),
                field: "sku".to_owned(),
            }],
        }],
    }));

    assert!(sql.contains("\"_cmd_step_1_item_0\""), "{sql}");
    assert!(sql.contains("\"_cmd_ordinal\""), "{sql}");
    assert!(sql.contains("line policy rejected"), "{sql}");
    insta::assert_snapshot!(sql);
}
