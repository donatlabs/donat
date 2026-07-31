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
        logical_type: pg_type.to_owned(),
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

fn item_assignment(target: &str, field: &str, pg_type: &str) -> CommandAssignment {
    CommandAssignment {
        column: column(target, pg_type),
        value: CommandExecutionValue::Item {
            field: field.to_owned(),
            pg_type: pg_type.to_owned(),
        },
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

fn decision(
    name: &str,
    hit_policy: CommandDecisionHitPolicy,
    rows: &[(&str, &str, i64)],
) -> CommandDecision {
    CommandDecision {
        name: name.to_owned(),
        revision: format!("{name}-v1"),
        hit_policy,
        rows: rows
            .iter()
            .map(|(id, condition_sql, rank)| CommandDecisionRow {
                id: (*id).to_owned(),
                condition_sql: (*condition_sql).to_owned(),
                output: vec![CommandDecisionOutput {
                    name: "rank".to_owned(),
                    sql: format!("{rank}::numeric"),
                    column: column("rank", "numeric"),
                }],
            })
            .collect(),
    }
}

fn decision_root(
    command_name: &str,
    hit_policy: CommandDecisionHitPolicy,
    rows: &[(&str, &str, i64)],
) -> MutationRoot {
    root(CommandMutation {
        identity: command_identity(command_name),
        name: command_name.to_owned(),
        steps: vec![CommandExecutionStep::Decision {
            name: "route".to_owned(),
            cte: "_cmd_step_0".to_owned(),
            decision: decision("route_table", hit_policy, rows),
            input: vec![CommandNamedValue {
                name: "amount".to_owned(),
                column: column("amount", "numeric"),
                value: value(json!(50), "numeric"),
            }],
            returning: vec![column("rank", "numeric")],
            error_path: format!("$.selectionSet.{command_name}"),
        }],
        guards: vec![],
        result: vec![CommandResultField {
            name: "rank".to_owned(),
            value: CommandResultValue::StepColumn {
                cte: "_cmd_step_0".to_owned(),
                column: column("rank", "numeric"),
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::Scalar {
            alias: "rank".to_owned(),
            field: "rank".to_owned(),
        }],
    })
}

fn allocation_root(command_name: &str, maximum_rows: u32) -> MutationRoot {
    let candidate_columns = vec![
        column("order_id", "uuid"),
        column("order_line_id", "uuid"),
        column("line_sequence", "int4"),
        column("variant_id", "uuid"),
        column("location_code", "text"),
        column("inventory_level_id", "uuid"),
        column("requested_quantity", "int4"),
        column("available_quantity", "int4"),
        column("unit_price_minor", "int8"),
        column("currency", "text"),
        column("allocation_rank", "int4"),
    ];
    let allocation_id = column("allocation_id", "uuid");
    let allocated = column("allocated_quantity", "int4");
    let backordered = column("backordered_quantity", "int4");
    let group_columns = vec![
        allocation_id.clone(),
        column("order_id", "uuid"),
        column("first_line_sequence", "int4"),
        column("allocation_rank", "int4"),
        column("location_code", "text"),
        column("currency", "text"),
        column("items", "jsonb"),
    ];
    let line_columns = vec![
        allocation_id.clone(),
        column("order_id", "uuid"),
        column("order_line_id", "uuid"),
        column("line_sequence", "int4"),
        column("variant_id", "uuid"),
        column("location_code", "text"),
        column("inventory_level_id", "uuid"),
        column("requested_quantity", "int4"),
        allocated.clone(),
        column("unit_price_minor", "int8"),
        column("currency", "text"),
    ];
    let backorder_columns = vec![
        column("order_id", "uuid"),
        column("order_line_id", "uuid"),
        column("requested_quantity", "int4"),
        backordered.clone(),
    ];
    let order_id = "00000000-0000-0000-0000-000000000010";
    let line_1 = "00000000-0000-0000-0000-000000000011";
    let line_2 = "00000000-0000-0000-0000-000000000012";
    let variant_1 = "00000000-0000-0000-0000-000000000021";
    let variant_2 = "00000000-0000-0000-0000-000000000022";
    let inventory_a_1 = "00000000-0000-0000-0000-000000000031";
    let inventory_b_1 = "00000000-0000-0000-0000-000000000032";
    let inventory_a_2 = "00000000-0000-0000-0000-000000000033";
    let inventory_b_2 = "00000000-0000-0000-0000-000000000034";
    let candidate = |line: &str,
                     sequence: i32,
                     variant: &str,
                     location: &str,
                     inventory: &str,
                     requested: i32,
                     available: i32,
                     rank: i32| {
        vec![
            value(json!(order_id), "uuid"),
            value(json!(line), "uuid"),
            value(json!(sequence), "int4"),
            value(json!(variant), "uuid"),
            value(json!(location), "text"),
            value(json!(inventory), "uuid"),
            value(json!(requested), "int4"),
            value(json!(available), "int4"),
            value(json!(100), "int8"),
            value(json!("USD"), "text"),
            value(json!(rank), "int4"),
        ]
    };
    let project = |columns: &[CommandColumn]| {
        columns
            .iter()
            .cloned()
            .map(|source| CommandResultProjection {
                name: source.name.clone(),
                source,
            })
            .collect()
    };
    root(CommandMutation {
        identity: command_identity(command_name),
        name: command_name.to_owned(),
        steps: vec![
            CommandExecutionStep::FixedRows {
                name: "candidates".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                maximum_rows: 4,
                columns: candidate_columns,
                rows: vec![
                    candidate(line_1, 1, variant_1, "A", inventory_a_1, 5, 3, 1),
                    candidate(line_1, 1, variant_1, "B", inventory_b_1, 5, 4, 2),
                    candidate(line_2, 2, variant_2, "A", inventory_a_2, 2, 0, 1),
                    candidate(line_2, 2, variant_2, "B", inventory_b_2, 2, 1, 2),
                ],
                error_path: format!("$.selectionSet.{command_name}"),
            },
            CommandExecutionStep::AllocateMany {
                name: "allocation".to_owned(),
                cte: "_cmd_step_1".to_owned(),
                input_cte: "_cmd_step_0".to_owned(),
                request_id: value(json!("00000000-0000-0000-0000-000000000099"), "uuid"),
                group_key: vec![column("location_code", "text")],
                requested: column("requested_quantity", "int4"),
                available: column("available_quantity", "int4"),
                allocated: allocated.clone(),
                backordered,
                groups: group_columns.clone(),
                lines: line_columns.clone(),
                backorders: backorder_columns.clone(),
                group_order_by: vec![
                    column("first_line_sequence", "int4"),
                    column("allocation_rank", "int4"),
                    column("location_code", "text"),
                    allocation_id.clone(),
                ],
                line_order_by: vec![
                    column("line_sequence", "int4"),
                    column("location_code", "text"),
                    allocation_id,
                ],
                maximum_rows,
                error_path: format!("$.selectionSet.{command_name}"),
            },
        ],
        guards: vec![],
        result: vec![
            CommandResultField {
                name: "groups".to_owned(),
                value: CommandResultValue::ProjectedRows {
                    cte: "_cmd_step_1_groups".to_owned(),
                    many: true,
                    columns: project(&group_columns),
                    maximum_items: 4,
                },
            },
            CommandResultField {
                name: "lines".to_owned(),
                value: CommandResultValue::ProjectedRows {
                    cte: "_cmd_step_1_lines".to_owned(),
                    many: true,
                    columns: project(&line_columns),
                    maximum_items: 4,
                },
            },
            CommandResultField {
                name: "backorders".to_owned(),
                value: CommandResultValue::ProjectedRows {
                    cte: "_cmd_step_1_backorders".to_owned(),
                    many: true,
                    columns: project(&backorder_columns),
                    maximum_items: 4,
                },
            },
        ],
        idempotency: Some(CommandIdempotency {
            key: Scalar::Json(json!("allocation-request")),
            scope: vec![value(json!(order_id), "uuid")],
            input: Scalar::Json(json!({"order_id": order_id})),
            retention_seconds: Some(60),
            error_path: format!("$.selectionSet.{command_name}"),
        }),
        effects: vec![],
        selection: vec![
            CommandResultSelection::List {
                alias: "groups".to_owned(),
                field: "groups".to_owned(),
                selections: group_columns
                    .iter()
                    .map(|column| CommandResultSelection::Scalar {
                        alias: column.name.clone(),
                        field: column.name.clone(),
                    })
                    .collect(),
            },
            CommandResultSelection::List {
                alias: "lines".to_owned(),
                field: "lines".to_owned(),
                selections: line_columns
                    .iter()
                    .map(|column| CommandResultSelection::Scalar {
                        alias: column.name.clone(),
                        field: column.name.clone(),
                    })
                    .collect(),
            },
            CommandResultSelection::List {
                alias: "backorders".to_owned(),
                field: "backorders".to_owned(),
                selections: backorder_columns
                    .iter()
                    .map(|column| CommandResultSelection::Scalar {
                        alias: column.name.clone(),
                        field: column.name.clone(),
                    })
                    .collect(),
            },
        ],
    })
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
    tx.query_one("SELECT pg_advisory_xact_lock(604630061)", &[])
        .expect("serialize command catalog migration in tests");
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
    let invocation_columns: i64 = tx
        .query_one(
            "SELECT count(*) \
             FROM information_schema.columns \
             WHERE table_schema = 'donat' \
               AND table_name = 'command_invocations' \
               AND column_name = 'invocation_id'",
            &[],
        )
        .expect("inspect command invocation generation migration state")
        .get(0);
    match invocation_columns {
        0 => tx
            .batch_execute(include_str!("../../../migrations/V6__donat_processes.sql"))
            .expect("command generation and process journal catalog installs"),
        1 => {}
        count => panic!("invalid invocation generation migration state: {count} columns"),
    }
    let caller_context_columns: i64 = tx
        .query_one(
            "SELECT count(*) \
             FROM information_schema.columns \
             WHERE table_schema = 'donat' \
               AND table_name = 'process_start_requests' \
               AND column_name = 'caller_role'",
            &[],
        )
        .expect("inspect Process caller-context migration state")
        .get(0);
    match caller_context_columns {
        0 => tx
            .batch_execute(include_str!(
                "../../../migrations/V7__process_execution_context.sql"
            ))
            .expect("Process caller context and deterministic events install"),
        1 => {}
        count => panic!("invalid Process caller-context migration state: {count} columns"),
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

fn install_check_violation_helper_client(client: &mut Client) {
    client
        .batch_execute(
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
        .query_one("SELECT pg_advisory_lock(604630061)", &[])
        .expect("serialize command catalog migration in tests");
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
    let invocation_columns: i64 = client
        .query_one(
            "SELECT count(*) \
             FROM information_schema.columns \
             WHERE table_schema = 'donat' \
               AND table_name = 'command_invocations' \
               AND column_name = 'invocation_id'",
            &[],
        )
        .expect("inspect command invocation generation migration state")
        .get(0);
    match invocation_columns {
        0 => client
            .batch_execute(include_str!("../../../migrations/V6__donat_processes.sql"))
            .expect("command generation and process journal catalog installs"),
        1 => {}
        count => panic!("invalid invocation generation migration state: {count} columns"),
    }
    let caller_context_columns: i64 = client
        .query_one(
            "SELECT count(*) \
             FROM information_schema.columns \
             WHERE table_schema = 'donat' \
               AND table_name = 'process_start_requests' \
               AND column_name = 'caller_role'",
            &[],
        )
        .expect("inspect Process caller-context migration state")
        .get(0);
    match caller_context_columns {
        0 => client
            .batch_execute(include_str!(
                "../../../migrations/V7__process_execution_context.sql"
            ))
            .expect("Process caller context and deterministic events install"),
        1 => {}
        count => panic!("invalid Process caller-context migration state: {count} columns"),
    }
    client
        .query_one("SELECT pg_advisory_unlock(604630061)", &[])
        .expect("release command catalog migration lock");
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
            scope: vec![value(json!("tenant-1"), "text")],
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

fn relational_batch_root(
    command_name: &str,
    pricing_table: &str,
    stock_table: &str,
    cart_id: i32,
    order_by: &[&str],
    idempotency: bool,
) -> MutationRoot {
    let selected_columns = vec![
        column("line_id", "int4"),
        column("variant_id", "int4"),
        column("quantity", "int4"),
        column("unit_price_minor", "int8"),
        column("currency", "text"),
    ];
    let updated_columns = vec![column("variant_id", "int4"), column("reserved", "int4")];
    root(CommandMutation {
        identity: command_identity(command_name),
        name: command_name.to_owned(),
        steps: vec![
            CommandExecutionStep::SelectMany {
                name: "priced_lines".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                table: table(pricing_table),
                equality: vec![assignment("cart_id", "int4", json!(cart_id))],
                order_by: order_by
                    .iter()
                    .map(|name| column(name, "int4"))
                    .collect(),
                returning: selected_columns.clone(),
                require_non_empty: true,
                filter: Some(BoolExp::Compare {
                    column: "customer_id".to_owned(),
                    pg_type: "int4".to_owned(),
                    op: CompareOp::Eq(Scalar::Json(json!(7))),
                }),
                error_path: format!("$.selectionSet.{command_name}"),
            },
            CommandExecutionStep::Aggregate {
                name: "totals".to_owned(),
                cte: "_cmd_step_1".to_owned(),
                input_cte: "_cmd_step_0".to_owned(),
                values: vec![
                    CommandAggregateIr::Count {
                        output: column("line_count", "int8"),
                    },
                    CommandAggregateIr::Sum {
                        output: column("subtotal_minor", "int8"),
                        input: column("unit_price_minor", "int8"),
                    },
                    CommandAggregateIr::Min {
                        output: CommandColumn {
                            nullable: true,
                            ..column("first_price", "int8")
                        },
                        input: column("unit_price_minor", "int8"),
                    },
                    CommandAggregateIr::Max {
                        output: CommandColumn {
                            nullable: true,
                            ..column("last_price", "int8")
                        },
                        input: column("unit_price_minor", "int8"),
                    },
                    CommandAggregateIr::CountDistinct {
                        output: column("currency_count", "int8"),
                        input: column("currency", "text"),
                    },
                ],
                error_path: format!("$.selectionSet.{command_name}"),
            },
            CommandExecutionStep::UpdateMany {
                name: "reserve_stock".to_owned(),
                cte: "_cmd_step_2".to_owned(),
                table: table(stock_table),
                input_cte: "_cmd_step_0".to_owned(),
                primary_key: vec![item_assignment("variant_id", "variant_id", "int4")],
                guards: vec![],
                assignments: vec![CommandAssignment {
                    column: column("reserved", "int4"),
                    value: CommandExecutionValue::Rule {
                        sql: "\"_cmd_target\".\"reserved\" + \"_cmd_input\".\"quantity\""
                            .to_owned(),
                        pg_type: "int4".to_owned(),
                    },
                }],
                check: Some(CommandRule {
                    sql: "\"_cmd_target\".\"on_hand\" - \"_cmd_target\".\"reserved\" >= \"_cmd_input\".\"quantity\"".to_owned(),
                    pg_type: "bool".to_owned(),
                    error_path: format!("$.selectionSet.{command_name}"),
                    message: "command update_many check rejected".to_owned(),
                }),
                returning: updated_columns.clone(),
                require_each: true,
                filter: Some(BoolExp::Compare {
                    column: "tenant_id".to_owned(),
                    pg_type: "int4".to_owned(),
                    op: CompareOp::Eq(Scalar::Json(json!(7))),
                }),
                permission_check: Some(BoolExp::Compare {
                    column: "reserved".to_owned(),
                    pg_type: "int4".to_owned(),
                    op: CompareOp::Gte(Scalar::Json(json!(0))),
                }),
                error_path: format!("$.selectionSet.{command_name}"),
            },
        ],
        guards: vec![],
        result: vec![
            CommandResultField {
                name: "priced_lines".to_owned(),
                value: CommandResultValue::StepRow {
                    cte: "_cmd_step_0".to_owned(),
                    many: true,
                    columns: selected_columns,
                },
            },
            CommandResultField {
                name: "totals".to_owned(),
                value: CommandResultValue::StepRow {
                    cte: "_cmd_step_1".to_owned(),
                    many: false,
                    columns: vec![
                        column("line_count", "int8"),
                        column("subtotal_minor", "int8"),
                        column("first_price", "int8"),
                        column("last_price", "int8"),
                        column("currency_count", "int8"),
                    ],
                },
            },
            CommandResultField {
                name: "reserved".to_owned(),
                value: CommandResultValue::StepRow {
                    cte: "_cmd_step_2".to_owned(),
                    many: true,
                    columns: updated_columns,
                },
            },
        ],
        idempotency: idempotency.then(|| CommandIdempotency {
            key: Scalar::Json(json!(format!("request-{cart_id}"))),
            scope: vec![value(json!("tenant-7"), "text")],
            input: Scalar::Json(json!({ "cart_id": cart_id })),
            retention_seconds: Some(60),
            error_path: format!("$.selectionSet.{command_name}"),
        }),
        effects: vec![],
        selection: vec![
            CommandResultSelection::List {
                alias: "priced_lines".to_owned(),
                field: "priced_lines".to_owned(),
                selections: vec![
                    CommandResultSelection::Scalar {
                        alias: "variant_id".to_owned(),
                        field: "variant_id".to_owned(),
                    },
                    CommandResultSelection::Scalar {
                        alias: "quantity".to_owned(),
                        field: "quantity".to_owned(),
                    },
                ],
            },
            CommandResultSelection::Object {
                alias: "totals".to_owned(),
                field: "totals".to_owned(),
                selections: vec![
                    CommandResultSelection::Scalar {
                        alias: "line_count".to_owned(),
                        field: "line_count".to_owned(),
                    },
                    CommandResultSelection::Scalar {
                        alias: "subtotal_minor".to_owned(),
                        field: "subtotal_minor".to_owned(),
                    },
                ],
            },
            CommandResultSelection::List {
                alias: "reserved".to_owned(),
                field: "reserved".to_owned(),
                selections: vec![
                    CommandResultSelection::Scalar {
                        alias: "variant_id".to_owned(),
                        field: "variant_id".to_owned(),
                    },
                    CommandResultSelection::Scalar {
                        alias: "reserved".to_owned(),
                        field: "reserved".to_owned(),
                    },
                ],
            },
        ],
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
                scope: vec![value(json!("customer-7"), "text")],
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
fn command_v6_writer_generates_and_replays_one_durable_invocation_uuid() {
    let sql =
        donat_sqlgen::mutation_to_sql(&idempotent_insert_root("create_order", "orders", "draft"));

    assert!(
        sql.starts_with("WITH ") && !sql.contains(';'),
        "the command writer remains one top-level statement: {sql}"
    );
    assert!(
        sql.contains("\"invocation_id\"")
            && sql.contains("gen_random_uuid()")
            && sql.contains("\"_cmd_store_first\"")
            && sql.contains("\"_cmd_store_replay\""),
        "first/expired execution must generate and replay must retain the V6 UUID: {sql}"
    );
}

fn effectful_command_root(start_policy: ProcessStartPolicy) -> MutationRoot {
    root(CommandMutation {
        identity: command_identity("create_order"),
        name: "create_order".to_owned(),
        steps: vec![],
        guards: vec![],
        result: vec![CommandResultField {
            name: "order_id".to_owned(),
            value: CommandResultValue::Scalar {
                value: Scalar::Json(json!("550e8400-e29b-41d4-a716-446655440010")),
                pg_type: "uuid".to_owned(),
            },
        }],
        idempotency: Some(CommandIdempotency {
            key: Scalar::Json(json!("request-1")),
            scope: vec![value(json!("tenant-1"), "text")],
            input: Scalar::Json(json!({ "request_id": "request-1" })),
            retention_seconds: Some(60),
            error_path: "$.selectionSet.create_order".to_owned(),
        }),
        effects: vec![
            ResolvedCommandEffect::StartProcess(ResolvedStartProcessEffect {
                source: "default".to_owned(),
                process_name: "checkout".to_owned(),
                process_revision: "checkout-r1".to_owned(),
                start_policy,
                input: std::collections::BTreeMap::from([(
                    "order_id".to_owned(),
                    value(json!("550e8400-e29b-41d4-a716-446655440010"), "uuid"),
                )]),
                semantic_idempotency_key: value(json!("request-1"), "text"),
                caller_role: None,
                caller_session_variables: std::collections::BTreeMap::new(),
                command_invocation_id: CommandInvocationIdSource::CurrentExecution,
                effect_position: 0,
            }),
            ResolvedCommandEffect::SignalProcess(ResolvedSignalProcessEffect {
                source: "default".to_owned(),
                process_name: "approval".to_owned(),
                process_revision: "approval-r2".to_owned(),
                signal_name: "approval_decision".to_owned(),
                correlation: std::collections::BTreeMap::from([(
                    "order_id".to_owned(),
                    value(json!("550e8400-e29b-41d4-a716-446655440010"), "uuid"),
                )]),
                payload: std::collections::BTreeMap::from([(
                    "decision".to_owned(),
                    value(json!("approved"), "text"),
                )]),
                semantic_idempotency_key: value(json!("request-1"), "text"),
                command_invocation_id: CommandInvocationIdSource::CurrentExecution,
                effect_position: 1,
            }),
        ],
        selection: vec![],
    })
}

fn caller_effectful_command_root() -> MutationRoot {
    let mut root = effectful_command_root(ProcessStartPolicy::Enabled);
    let MutationRoot::Command { command, .. } = &mut root else {
        unreachable!("effectful root is a Command");
    };
    let ResolvedCommandEffect::StartProcess(start) = &mut command.effects[0] else {
        unreachable!("first effect is a Process start");
    };
    start.caller_role = Some("customer".to_owned());
    start.caller_session_variables = std::collections::BTreeMap::from([(
        "x-donat-user-id".to_owned(),
        value(json!("550e8400-e29b-41d4-a716-446655440099"), "text"),
    )]);
    root
}

#[test]
fn command_effect_positions_share_generation_in_one_statement() {
    let sql = donat_sqlgen::mutation_to_sql(&effectful_command_root(ProcessStartPolicy::Enabled));

    assert!(sql.starts_with("WITH ") && !sql.contains(';'), "{sql}");
    assert!(
        sql.contains("\"donat\".\"process_start_requests\"")
            && sql.contains("\"donat\".\"process_signal_requests\"")
            && sql
                .matches("\"_cmd_store_first\".\"invocation_id\"")
                .count()
                >= 2,
        "both outboxes must copy the one current execution generation: {sql}"
    );
    assert!(
        sql.contains("'checkout-r1'")
            && sql.contains("'approval-r2'")
            && sql.contains("\"effect_position\""),
        "outboxes must pin their finalized revision and canonical position: {sql}"
    );
}

#[test]
fn command_retired_start_gate_precedes_claim_and_domain_work() {
    let sql =
        donat_sqlgen::mutation_to_sql(&effectful_command_root(ProcessStartPolicy::RejectRetired));
    let retired_gate = sql
        .find("_cmd_effect_policy_gate_0")
        .expect("retired start has a materialized gate");
    let claim = sql.find("_cmd_claim").expect("idempotency claim exists");
    let result = sql.find("_cmd_result").expect("command result exists");

    assert!(retired_gate < claim && retired_gate < result, "{sql}");
    assert!(
        sql.contains("default.checkout")
            && sql.contains("does not accept new starts")
            && sql.contains("$.selectionSet.create_order"),
        "retired rejection keeps the exact command error envelope: {sql}"
    );
}

fn insert_process_revision(tx: &mut Transaction<'_>, process: &str, revision: &str) {
    tx.execute(
        "INSERT INTO donat.process_definition_versions \
         (source_name, process_name, revision, canonical_definition, \
          dependency_descriptors, runtime_abi, status) \
         VALUES ('default', $1, $2, '{}'::jsonb, '{}'::jsonb, 1, 'active')",
        &[&process, &revision],
    )
    .expect("effect target revision is deployed");
}

#[test]
fn command_effects_commit_atomically_and_exact_replay_writes_no_second_outbox() {
    let _guard = command_catalog_test_lock();
    let mut client = postgres_client();
    let mut tx = client.transaction().expect("effect transaction starts");
    install_command_catalog(&mut tx);
    insert_process_revision(&mut tx, "checkout", "checkout-r1");
    insert_process_revision(&mut tx, "approval", "approval-r2");
    let sql = donat_sqlgen::mutation_to_sql(&effectful_command_root(ProcessStartPolicy::Enabled));

    let first = tx
        .query_one(&sql, &[])
        .expect("first command generation and both effects commit");
    assert!(!first.get::<_, bool>("replayed"));
    let first_generation: uuid::Uuid = first.get("invocation_id");

    let start = tx
        .query_one(
            "SELECT revision, input_json, command_invocation_id, effect_position, \
                    idempotency_key, status \
             FROM donat.process_start_requests \
             WHERE source_name = 'default' AND process_name = 'checkout'",
            &[],
        )
        .expect("one start outbox row is durable");
    assert_eq!(start.get::<_, String>("revision"), "checkout-r1");
    assert_eq!(
        start.get::<_, Json>("input_json"),
        json!({ "order_id": "550e8400-e29b-41d4-a716-446655440010" })
    );
    assert_eq!(
        start.get::<_, uuid::Uuid>("command_invocation_id"),
        first_generation
    );
    assert_eq!(start.get::<_, i32>("effect_position"), 0);
    assert_eq!(start.get::<_, String>("idempotency_key"), "request-1");
    assert_eq!(start.get::<_, String>("status"), "pending");

    let signal = tx
        .query_one(
            "SELECT process_revision, signal_name, correlation_json, payload_json, \
                    command_invocation_id, effect_position, idempotency_key, status \
             FROM donat.process_signal_requests \
             WHERE source_name = 'default' AND process_name = 'approval'",
            &[],
        )
        .expect("one signal outbox row is durable");
    assert_eq!(signal.get::<_, String>("process_revision"), "approval-r2");
    assert_eq!(signal.get::<_, String>("signal_name"), "approval_decision");
    assert_eq!(
        signal.get::<_, Json>("correlation_json"),
        json!({ "order_id": "550e8400-e29b-41d4-a716-446655440010" })
    );
    assert_eq!(
        signal.get::<_, Json>("payload_json"),
        json!({ "decision": "approved" })
    );
    assert_eq!(
        signal.get::<_, uuid::Uuid>("command_invocation_id"),
        first_generation
    );
    assert_eq!(signal.get::<_, i32>("effect_position"), 1);
    assert_eq!(signal.get::<_, String>("idempotency_key"), "request-1");
    assert_eq!(signal.get::<_, String>("status"), "pending");

    let replay = tx
        .query_one(&sql, &[])
        .expect("exact command replay returns its canonical result");
    assert!(replay.get::<_, bool>("replayed"));
    assert_eq!(
        replay.get::<_, uuid::Uuid>("invocation_id"),
        first_generation,
        "exact replay must retain the execution generation"
    );
    let counts = tx
        .query_one(
            "SELECT \
                (SELECT count(*) FROM donat.process_start_requests \
                 WHERE source_name = 'default' AND process_name = 'checkout'), \
                (SELECT count(*) FROM donat.process_signal_requests \
                 WHERE source_name = 'default' AND process_name = 'approval')",
            &[],
        )
        .expect("effect counts remain inspectable");
    assert_eq!(counts.get::<_, i64>(0), 1);
    assert_eq!(counts.get::<_, i64>(1), 1);

    tx.rollback().expect("effect test state rolls back");
}

#[test]
fn command_start_effect_writes_the_exact_closed_caller_context() {
    let _guard = command_catalog_test_lock();
    let mut client = postgres_client();
    let mut tx = client.transaction().expect("effect transaction starts");
    install_command_catalog(&mut tx);
    insert_process_revision(&mut tx, "checkout", "checkout-r1");
    insert_process_revision(&mut tx, "approval", "approval-r2");
    let sql = donat_sqlgen::mutation_to_sql(&caller_effectful_command_root());

    tx.query_one(&sql, &[])
        .expect("caller-qualified Process start effect commits");
    let start = tx
        .query_one(
            "SELECT caller_role, caller_session_json \
             FROM donat.process_start_requests \
             WHERE source_name = 'default' AND process_name = 'checkout'",
            &[],
        )
        .expect("caller Process start context is durable");
    assert_eq!(start.get::<_, String>("caller_role"), "customer");
    assert_eq!(
        start.get::<_, Json>("caller_session_json"),
        json!({
            "x-donat-user-id": "550e8400-e29b-41d4-a716-446655440099"
        })
    );

    tx.rollback().expect("caller effect test state rolls back");
}

#[test]
fn command_effect_failure_and_retired_policy_leave_no_partial_command_state() {
    let _guard = command_catalog_test_lock();
    let mut client = postgres_client();
    let mut tx = client.transaction().expect("effect transaction starts");
    install_command_catalog(&mut tx);
    insert_process_revision(&mut tx, "checkout", "checkout-r1");
    let enabled_sql =
        donat_sqlgen::mutation_to_sql(&effectful_command_root(ProcessStartPolicy::Enabled));

    tx.batch_execute("SAVEPOINT missing_signal_revision")
        .expect("effect failure savepoint creates");
    let error = tx
        .query_one(&enabled_sql, &[])
        .expect_err("a missing pinned signal revision must abort the whole statement");
    assert_eq!(
        error
            .as_db_error()
            .expect("foreign-key failure is structured")
            .code()
            .code(),
        "23503"
    );
    tx.batch_execute("ROLLBACK TO SAVEPOINT missing_signal_revision")
        .expect("effect failure rolls back to an inspectable transaction");
    assert_no_effect_command_state(&mut tx);

    let retired_sql =
        donat_sqlgen::mutation_to_sql(&effectful_command_root(ProcessStartPolicy::RejectRetired));
    tx.batch_execute("SAVEPOINT retired_start")
        .expect("retired start savepoint creates");
    let error = tx
        .query_one(&retired_sql, &[])
        .expect_err("retired Process rejects before command execution");
    let db = error
        .as_db_error()
        .expect("retired Process rejection is structured");
    assert_eq!(db.code().code(), "P0D01");
    let payload: Json = serde_json::from_str(db.message()).expect("GraphQL envelope is JSON");
    assert_eq!(
        payload,
        json!({
            "kind": "donat.graphql-error.v1",
            "code": "validation-failed",
            "path": "$.selectionSet.create_order",
            "message": "process 'default.checkout' does not accept new starts"
        })
    );
    tx.batch_execute("ROLLBACK TO SAVEPOINT retired_start")
        .expect("retired rejection rolls back to an inspectable transaction");
    assert_no_effect_command_state(&mut tx);

    tx.rollback().expect("effect failure test state rolls back");
}

fn assert_no_effect_command_state(tx: &mut Transaction<'_>) {
    let counts = tx
        .query_one(
            "SELECT \
                (SELECT count(*) FROM donat.command_invocation_claims \
                 WHERE command_name = 'create_order' AND key = 'request-1'), \
                (SELECT count(*) FROM donat.command_invocations \
                 WHERE command_name = 'create_order' AND key = 'request-1'), \
                (SELECT count(*) FROM donat.process_start_requests \
                 WHERE source_name = 'default' AND process_name = 'checkout'), \
                (SELECT count(*) FROM donat.process_signal_requests \
                 WHERE source_name = 'default' AND process_name = 'approval')",
            &[],
        )
        .expect("command and effect state remains inspectable");
    assert_eq!(counts.get::<_, i64>(0), 0, "claim must roll back");
    assert_eq!(counts.get::<_, i64>(1), 0, "journal must roll back");
    assert_eq!(counts.get::<_, i64>(2), 0, "start outbox must roll back");
    assert_eq!(counts.get::<_, i64>(3), 0, "signal outbox must roll back");
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
fn bounded_argument_rows_render_one_typed_postgres_cte() {
    let _catalog_lock = command_catalog_test_lock();
    let input = "_cmd_step_0_input";
    let root = root(CommandMutation {
        identity: command_identity("sum_lines"),
        name: "sum_lines".to_owned(),
        steps: vec![
            CommandExecutionStep::ArgumentRows {
                name: "totals_input".to_owned(),
                cte: input.to_owned(),
                items: Scalar::Json(json!([
                    { "sku": "first", "quantity": 2 },
                    { "sku": "second", "quantity": 3 }
                ])),
                columns: vec![column("sku", "text"), column("quantity", "int4")],
                minimum_items: 1,
                maximum_items: 2,
                error_path: "$.selectionSet.sum_lines".to_owned(),
            },
            CommandExecutionStep::Aggregate {
                name: "totals".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                input_cte: input.to_owned(),
                values: vec![
                    CommandAggregateIr::Count {
                        output: column("item_count", "int8"),
                    },
                    CommandAggregateIr::Sum {
                        output: column("quantity_sum", "int8"),
                        input: column("quantity", "int4"),
                    },
                ],
                error_path: "$.selectionSet.sum_lines".to_owned(),
            },
        ],
        guards: vec![],
        result: vec![CommandResultField {
            name: "item_count".to_owned(),
            value: CommandResultValue::StepColumn {
                cte: "_cmd_step_0".to_owned(),
                column: column("item_count", "int8"),
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::Scalar {
            alias: "item_count".to_owned(),
            field: "item_count".to_owned(),
        }],
    });

    let sql = donat_sqlgen::mutation_to_sql(&root);
    assert!(
        sql.contains("jsonb_to_recordset")
            && sql.contains("\"sku\" \"text\"")
            && sql.contains("\"quantity\" \"int4\"")
            && sql.contains("WITH ORDINALITY"),
        "argument items become one typed relational source: {sql}"
    );
    assert!(
        sql.contains("jsonb_array_length")
            && sql.contains("minimum_items 1")
            && sql.contains("maximum_items 2"),
        "the renderer retains both declared structural bounds: {sql}"
    );
    assert!(
        !sql.contains("_item_0") && !sql.contains("_item_1"),
        "argument rows are decoded as one set, not one Rust-expanded CTE per row: {sql}"
    );

    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start isolated argument-row execution");
    install_command_catalog(&mut tx);
    let result: Json = tx
        .query_one(&sql, &[])
        .expect("the typed argument-row CTE executes in Postgres")
        .get(0);
    assert_eq!(result, json!({ "item_count": 2 }));
    tx.rollback()
        .expect("roll back isolated argument-row execution");
}

#[test]
fn bounded_argument_rows_keep_update_many_exact_key_gates() {
    let input = "_cmd_step_0_input";
    let root = root(CommandMutation {
        identity: command_identity("update_lines"),
        name: "update_lines".to_owned(),
        steps: vec![
            CommandExecutionStep::ArgumentRows {
                name: "updated_input".to_owned(),
                cte: input.to_owned(),
                items: Scalar::Json(json!([
                    { "status": "first", "quantity": 2 },
                    { "status": "second", "quantity": 3 }
                ])),
                columns: vec![column("status", "text"), column("quantity", "int4")],
                minimum_items: 0,
                maximum_items: 2,
                error_path: "$.selectionSet.update_lines".to_owned(),
            },
            CommandExecutionStep::UpdateMany {
                name: "updated".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                table: table("orders"),
                input_cte: input.to_owned(),
                primary_key: vec![item_assignment("status", "status", "text")],
                guards: vec![assignment("tenant_id", "int4", json!(7))],
                assignments: vec![item_assignment("quantity", "quantity", "int4")],
                check: None,
                returning: vec![column("status", "text"), column("quantity", "int4")],
                require_each: true,
                filter: None,
                permission_check: None,
                error_path: "$.selectionSet.update_lines".to_owned(),
            },
        ],
        guards: vec![],
        result: vec![],
        idempotency: None,
        effects: vec![],
        selection: vec![],
    });

    let sql = donat_sqlgen::mutation_to_sql(&root);
    assert!(
        sql.contains("FROM \"_cmd_step_0_input\" AS \"_cmd_input\"")
            && sql.contains("duplicate input primary keys")
            && sql.contains("did not affect every input row")
            && sql.contains("\"_cmd_target\".\"tenant_id\" = (7)::\"int4\""),
        "argument-backed update_many retains exact-key gates and command-scoped predicates: {sql}"
    );
}

#[test]
fn projected_scalar_steps_render_as_bounded_single_item_lists() {
    let _catalog_lock = command_catalog_test_lock();
    let root = root(CommandMutation {
        identity: command_identity("project_one"),
        name: "project_one".to_owned(),
        steps: vec![CommandExecutionStep::Project {
            name: "candidate".to_owned(),
            cte: "_cmd_step_0".to_owned(),
            values: vec![CommandNamedValue {
                name: "status".to_owned(),
                column: column("status", "text"),
                value: value(json!("ready"), "text"),
            }],
            error_path: "$.selectionSet.project_one".to_owned(),
        }],
        guards: vec![],
        result: vec![CommandResultField {
            name: "items".to_owned(),
            value: CommandResultValue::ProjectedRows {
                cte: "_cmd_step_0".to_owned(),
                many: false,
                columns: vec![CommandResultProjection {
                    name: "status".to_owned(),
                    source: column("status", "text"),
                }],
                maximum_items: 1,
            },
        }],
        idempotency: None,
        effects: vec![],
        selection: vec![CommandResultSelection::List {
            alias: "items".to_owned(),
            field: "items".to_owned(),
            selections: vec![CommandResultSelection::Scalar {
                alias: "status".to_owned(),
                field: "status".to_owned(),
            }],
        }],
    });

    let sql = donat_sqlgen::mutation_to_sql(&root);
    assert!(
        sql.contains("jsonb_agg") && !sql.contains("\"_cmd_step_0\".\"_cmd_ordinal\""),
        "a scalar source has no ordinal but its projected result is still an array: {sql}"
    );

    let mut client = postgres_client();
    let mut tx = client
        .transaction()
        .expect("start isolated scalar-projection execution");
    install_command_catalog(&mut tx);
    let result: Json = tx
        .query_one(&sql, &[])
        .expect("the scalar projection executes in Postgres")
        .get(0);
    assert_eq!(result, json!({ "items": [{ "status": "ready" }] }));
    tx.rollback()
        .expect("roll back isolated scalar-projection execution");
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
    let first_invocation_id: String = tx
        .query_one(
            "SELECT invocation_id::text FROM donat.command_invocations WHERE command_name = $1",
            &[&command_name],
        )
        .expect("first command generation UUID is stored")
        .get(0);
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
    let replay_invocation_id: String = tx
        .query_one(
            "SELECT invocation_id::text FROM donat.command_invocations WHERE command_name = $1",
            &[&command_name],
        )
        .expect("replayed command generation UUID is stored")
        .get(0);
    assert_eq!(
        replay_invocation_id, first_invocation_id,
        "exact replay must retain the command execution generation"
    );
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
         (command_identity, command_name, scope_hash, key, invocation_id, input_fingerprint, result, expires_at) \
         VALUES ('legacy-unqualified:' || encode(convert_to($1, 'UTF8'), 'hex'), $1, \
                 decode(md5((jsonb_build_array('\"tenant-1\"'::jsonb))::text), 'hex'), \
                 'request-1', gen_random_uuid(), decode('01', 'hex'), '{\"status\":\"legacy\"}'::jsonb, \
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
    let first_invocation_id: String = tx
        .query_one(
            "SELECT invocation_id::text FROM donat.command_invocations WHERE command_name = $1",
            &[&command_name],
        )
        .expect("first command generation UUID is stored")
        .get(0);
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
    let reclaimed_invocation_id: String = tx
        .query_one(
            "SELECT invocation_id::text FROM donat.command_invocations WHERE command_name = $1",
            &[&command_name],
        )
        .expect("reclaimed command generation UUID is stored")
        .get(0);
    assert_eq!(
        target_rows, 2,
        "expired key permits the newly elected write"
    );
    assert_eq!(
        stored_status, "approved",
        "reclaim replaces the expired canonical result instead of replaying it"
    );
    assert_ne!(
        reclaimed_invocation_id, first_invocation_id,
        "expired-key execution must allocate a new command generation"
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

#[test]
fn relational_batch_renderer_snapshots_ordered_aggregate_guarded_update_and_replay() {
    let sql = donat_sqlgen::mutation_to_sql(&relational_batch_root(
        "reserve_cart",
        "cart_pricing",
        "inventory_stock",
        42,
        &["line_id"],
        true,
    ));

    assert!(
        sql.starts_with("WITH ") && !sql.contains(';'),
        "a relational batch remains one top-level Postgres statement: {sql}"
    );
    assert!(
        sql.contains("\"_cmd_step_0\" AS MATERIALIZED")
            && sql.contains("row_number() OVER (ORDER BY")
            && sql.contains("\"_cmd_ordinal\""),
        "select_many must materialize and preserve its declared total order: {sql}"
    );
    for aggregate in ["count(*)", "sum(", "min(", "max(", "count(DISTINCT"] {
        assert!(
            sql.contains(aggregate),
            "the closed aggregate renderer must include {aggregate}: {sql}"
        );
    }
    assert!(
        sql.contains("UPDATE \"public\".\"inventory_stock\" AS \"_cmd_target\"")
            && sql.contains("FROM \"_cmd_step_0\" AS \"_cmd_input\"")
            && sql.contains("\"_cmd_target\".\"reserved\" + \"_cmd_input\".\"quantity\""),
        "update_many must use the fixed typed current/input aliases: {sql}"
    );
    assert!(
        sql.contains("\"customer_id\"")
            && sql.contains("\"tenant_id\"")
            && sql.contains("donat.check_violation"),
        "ordinary select/update permission predicates and checks remain explicit: {sql}"
    );
    assert!(
        sql.contains("duplicate order keys")
            && sql.contains("requires at least one row")
            && sql.contains("duplicate input primary keys")
            && sql.contains("did not affect every input row"),
        "all relational cardinality gates must be present: {sql}"
    );
    assert!(
        sql.contains("\"donat\".\"command_invocations\"")
            && sql.contains("\"_cmd_store_replay\"")
            && sql.contains("jsonb_agg"),
        "idempotent replay stores and returns the canonical ordered row-set JSON: {sql}"
    );
    insta::assert_snapshot!(sql);
}

#[test]
fn relational_batch_renderer_lowers_closed_step_rows_and_current_columns() {
    let batch = relational_batch_root(
        "capture_cart",
        "cart_pricing",
        "inventory_stock",
        42,
        &["line_id"],
        false,
    );
    let MutationRoot::Command { mut command, .. } = batch else {
        panic!("the relational helper builds a command mutation");
    };
    let CommandExecutionStep::UpdateMany { assignments, .. } = &mut command.steps[2] else {
        panic!("the relational helper builds update_many as its third step");
    };
    assignments[0].value = CommandExecutionValue::CurrentColumn {
        column: column("reserved", "int4"),
    };
    command.steps.push(CommandExecutionStep::Insert {
        name: "audit".to_owned(),
        cte: "_cmd_step_3".to_owned(),
        table: table("reservation_audit"),
        object: vec![CommandAssignment {
            column: column("payload", "jsonb"),
            value: CommandExecutionValue::StepRows {
                cte: "_cmd_step_0".to_owned(),
                columns: vec![column("variant_id", "int4"), column("quantity", "int4")],
            },
        }],
        returning: vec![],
        check: None,
        error_path: "$.selectionSet.capture_cart".to_owned(),
    });

    let sql = donat_sqlgen::mutation_to_sql(&root(command));

    assert!(
        sql.contains("SET \"reserved\" = \"_cmd_target\".\"reserved\""),
        "current_column remains a fixed, quoted reference to the update target: {sql}"
    );
    assert!(
        sql.contains("INSERT INTO \"public\".\"reservation_audit\"")
            && sql.contains(
                "json_build_object('variant_id', \"_cmd_step_0\".\"variant_id\", 'quantity', \"_cmd_step_0\".\"quantity\") ORDER BY \"_cmd_step_0\".\"_cmd_ordinal\""
            ),
        "StepRows is assembled as ordered JSON inside the same statement: {sql}"
    );
    assert!(
        sql.contains("EXISTS (SELECT 1 FROM \"_cmd_affected_each_gate_2\") RETURNING *"),
        "the later audit write retains the earlier relational materialized-gate dependency: {sql}"
    );
}

#[test]
fn relational_batch_executes_once_replays_row_sets_and_rolls_back_all_cardinality_failures() {
    let _catalog_lock = command_catalog_test_lock();
    let suffix = std::process::id();
    let pricing_table = format!("command_relational_pricing_{suffix}");
    let stock_table = format!("command_relational_stock_{suffix}");
    let command_name = format!("command_relational_runtime_{suffix}");
    let mut client = postgres_client();
    install_command_catalog_client(&mut client);
    install_check_violation_helper_client(&mut client);
    client
        .batch_execute(&format!(
            "CREATE TABLE \"public\".\"{pricing_table}\" (\
                cart_id int4 NOT NULL, \
                line_id int4 NOT NULL, \
                variant_id int4 NOT NULL, \
                quantity int4 NOT NULL, \
                unit_price_minor int8, \
                currency text NOT NULL, \
                customer_id int4 NOT NULL\
             ); \
             CREATE TABLE \"public\".\"{stock_table}\" (\
                variant_id int4 PRIMARY KEY, \
                reserved int4 NOT NULL, \
                on_hand int4 NOT NULL, \
                tenant_id int4 NOT NULL\
             ); \
             INSERT INTO \"public\".\"{pricing_table}\" VALUES \
                (1, 1, 2, 3, 100, 'USD', 7), \
                (1, 2, 1, 2, 200, 'USD', 7), \
                (2, 3, 1, 1, 100, 'USD', 7), \
                (2, 4, 1, 1, 100, 'USD', 7), \
                (3, 5, 2, 1, 100, 'USD', 7), \
                (3, 6, 999, 1, 100, 'USD', 7), \
                (5, 7, 1, 1, 100, 'USD', 7), \
                (5, 7, 2, 1, 100, 'USD', 7), \
                (6, 8, 1, 0, 9223372036854775807, 'USD', 7), \
                (6, 9, 2, 0, 1, 'USD', 7), \
                (7, 10, 1, 0, NULL, 'USD', 7), \
                (7, 11, 2, 0, NULL, 'USD', 7); \
             INSERT INTO \"public\".\"{stock_table}\" VALUES \
                (1, 0, 10, 7), \
                (2, 0, 10, 7)"
        ))
        .expect("create relational command execution fixture");

    let mut idempotent_root = relational_batch_root(
        &command_name,
        &pricing_table,
        &stock_table,
        1,
        &["line_id"],
        true,
    );
    let MutationRoot::Command { command, .. } = &mut idempotent_root else {
        unreachable!("relational helper returns a command");
    };
    command.steps.insert(
        1,
        CommandExecutionStep::ProjectMany {
            name: "projected_prices".to_owned(),
            cte: "_cmd_projected_prices".to_owned(),
            input_cte: "_cmd_step_0".to_owned(),
            maximum_rows: 256,
            values: vec![
                CommandNamedValue {
                    name: "unit_price_minor".to_owned(),
                    column: column("unit_price_minor", "int8"),
                    value: CommandExecutionValue::CurrentColumn {
                        column: column("unit_price_minor", "int8"),
                    },
                },
                CommandNamedValue {
                    name: "currency".to_owned(),
                    column: column("currency", "text"),
                    value: CommandExecutionValue::CurrentColumn {
                        column: column("currency", "text"),
                    },
                },
            ],
            error_path: format!("$.selectionSet.{command_name}"),
        },
    );
    let CommandExecutionStep::Aggregate { input_cte, .. } = &mut command.steps[2] else {
        unreachable!("project_many precedes the aggregate");
    };
    *input_cte = "_cmd_projected_prices".to_owned();
    let idempotent = donat_sqlgen::mutation_to_sql(&idempotent_root);
    let first: Json = client
        .query_one(&idempotent, &[])
        .expect("the first relational batch executes")
        .get(0);
    assert_eq!(
        first,
        json!({
            "priced_lines": [
                { "variant_id": 2, "quantity": 3 },
                { "variant_id": 1, "quantity": 2 }
            ],
            "totals": { "line_count": 2, "subtotal_minor": 300 },
            "reserved": [
                { "variant_id": 2, "reserved": 3 },
                { "variant_id": 1, "reserved": 2 }
            ]
        }),
        "all row-set results preserve the selected input order"
    );

    client
        .execute(
            &format!("UPDATE \"public\".\"{stock_table}\" SET reserved = 9"),
            &[],
        )
        .expect("make a repeated domain write observable");
    let replay: Json = client
        .query_one(&idempotent, &[])
        .expect("an exact retry replays the canonical relational result")
        .get(0);
    assert_eq!(
        replay, first,
        "replay returns the first canonical row sets and aggregate"
    );
    let replay_reserved: Vec<i32> = client
        .query(
            &format!("SELECT reserved FROM \"public\".\"{stock_table}\" ORDER BY variant_id"),
            &[],
        )
        .expect("inspect stock after replay")
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        replay_reserved,
        vec![9, 9],
        "replay does not execute update_many again"
    );

    client
        .execute(
            &format!("UPDATE \"public\".\"{stock_table}\" SET reserved = 0"),
            &[],
        )
        .expect("reset stock for rejection cases");

    for cart_id in [4, 7] {
        let mut root = relational_batch_root(
            &format!("{command_name}_nullable_{cart_id}"),
            &pricing_table,
            &stock_table,
            cart_id,
            &["line_id"],
            false,
        );
        let MutationRoot::Command { command, .. } = &mut root else {
            unreachable!("relational helper returns a command");
        };
        let CommandExecutionStep::SelectMany {
            require_non_empty, ..
        } = &mut command.steps[0]
        else {
            unreachable!("first step selects the bounded input");
        };
        *require_non_empty = false;
        let CommandExecutionStep::Aggregate { values, .. } = &mut command.steps[1] else {
            unreachable!("select_many feeds the nullable aggregate");
        };
        for aggregate in values {
            match aggregate {
                CommandAggregateIr::Sum { output, .. }
                | CommandAggregateIr::Min { output, .. }
                | CommandAggregateIr::Max { output, .. } => output.nullable = true,
                CommandAggregateIr::Count { .. } | CommandAggregateIr::CountDistinct { .. } => {}
            }
        }
        let totals = command
            .result
            .iter_mut()
            .find(|field| field.name == "totals")
            .expect("totals result");
        let CommandResultValue::StepRow { columns, .. } = &mut totals.value else {
            unreachable!("totals is one aggregate row");
        };
        for column in columns {
            if matches!(
                column.name.as_str(),
                "subtotal_minor" | "first_price" | "last_price"
            ) {
                column.nullable = true;
            }
        }
        let value: Json = client
            .query_one(&donat_sqlgen::mutation_to_sql(&root), &[])
            .expect("empty and all-null bounded sums remain nullable")
            .get(0);
        assert_eq!(value["totals"]["subtotal_minor"], Json::Null);
    }

    let overflow = donat_sqlgen::mutation_to_sql(&relational_batch_root(
        &format!("{command_name}_overflow"),
        &pricing_table,
        &stock_table,
        6,
        &["line_id"],
        false,
    ));
    let overflow_error = client
        .query_one(&overflow, &[])
        .expect_err("checked bigint accumulation must fail instead of widening");
    assert_eq!(
        overflow_error
            .as_db_error()
            .expect("overflow is a database error")
            .code()
            .code(),
        "22003"
    );
    let reserved_after_overflow: Vec<i32> = client
        .query(
            &format!("SELECT reserved FROM \"public\".\"{stock_table}\" ORDER BY variant_id"),
            &[],
        )
        .expect("overflow rolls the statement back and keeps the connection usable")
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(reserved_after_overflow, vec![0, 0]);

    for (cart_id, order_by, expected_message) in [
        (2, &["line_id"][..], "duplicate input primary keys"),
        (3, &["line_id"][..], "did not affect every input row"),
        (4, &["line_id"][..], "requires at least one row"),
        (5, &["line_id"][..], "duplicate order keys"),
    ] {
        let sql = donat_sqlgen::mutation_to_sql(&relational_batch_root(
            &format!("{command_name}_{cart_id}"),
            &pricing_table,
            &stock_table,
            cart_id,
            order_by,
            false,
        ));
        let error = client
            .query_one(&sql, &[])
            .expect_err("the relational cardinality gate must reject the statement");
        let database_error = error
            .as_db_error()
            .expect("structured command rejection is a database error");
        assert_eq!(database_error.code().code(), "P0D01");
        assert!(
            database_error.message().contains(expected_message),
            "unexpected rejection for cart {cart_id}: {database_error:?}"
        );
        let reserved: Vec<i32> = client
            .query(
                &format!("SELECT reserved FROM \"public\".\"{stock_table}\" ORDER BY variant_id"),
                &[],
            )
            .expect("the connection and fixture remain usable after statement rollback")
            .into_iter()
            .map(|row| row.get(0))
            .collect();
        assert_eq!(
            reserved,
            vec![0, 0],
            "a rejected relational statement rolls back every partial update"
        );
    }

    client
        .batch_execute(&format!(
            "DROP TABLE \"public\".\"{pricing_table}\", \"public\".\"{stock_table}\""
        ))
        .expect("remove relational command execution fixture");
}

#[test]
fn command_decisions_execute_first_and_unique_policies_and_reject_bad_cardinality() {
    let _catalog_lock = command_catalog_test_lock();
    let mut client = postgres_client();
    install_command_catalog_client(&mut client);

    let first_sql = donat_sqlgen::mutation_to_sql(&decision_root(
        "decision_first",
        CommandDecisionHitPolicy::First,
        &[("first", "TRUE", 1), ("second", "TRUE", 2)],
    ));
    let first: Json = client
        .query_one(&first_sql, &[])
        .expect("first policy selects the first declared matching row")
        .get(0);
    assert_eq!(first, json!({"rank": 1}));

    let unique_sql = donat_sqlgen::mutation_to_sql(&decision_root(
        "decision_unique",
        CommandDecisionHitPolicy::Unique,
        &[("only", "TRUE", 7), ("miss", "FALSE", 9)],
    ));
    let unique: Json = client
        .query_one(&unique_sql, &[])
        .expect("unique policy selects its sole matching row")
        .get(0);
    assert_eq!(unique, json!({"rank": 7}));

    for (name, rows, expected_message) in [
        (
            "decision_no_match",
            vec![("miss", "FALSE", 1)],
            "had no matching row",
        ),
        (
            "decision_multiple",
            vec![("one", "TRUE", 1), ("two", "TRUE", 2)],
            "matched multiple rows",
        ),
    ] {
        let sql = donat_sqlgen::mutation_to_sql(&decision_root(
            name,
            CommandDecisionHitPolicy::Unique,
            &rows,
        ));
        let error = client
            .query_one(&sql, &[])
            .expect_err("invalid decision cardinality must reject the command");
        let database_error = error
            .as_db_error()
            .expect("decision rejection is structured");
        assert_eq!(database_error.code().code(), "P0D01");
        assert!(
            database_error.message().contains(expected_message),
            "unexpected decision rejection: {database_error:?}"
        );
    }
}

#[test]
fn command_renderer_executes_pure_forms_typed_results_and_false_conditional_gates() {
    let _catalog_lock = command_catalog_test_lock();
    let suffix = std::process::id();
    let table_name = format!("command_conditional_{suffix}");
    let mut client = postgres_client();
    install_command_catalog_client(&mut client);
    client
        .batch_execute(&format!(
            "CREATE TABLE \"public\".\"{table_name}\" (id int4 PRIMARY KEY, created_at timestamptz NOT NULL)"
        ))
        .expect("create conditional command fixture");

    let condition = CommandCondition::ArgumentEquals {
        argument: Scalar::Json(json!(false)),
        expected: Scalar::Json(json!(true)),
        pg_type: "boolean".to_owned(),
    };
    let sql = donat_sqlgen::mutation_to_sql(&root(CommandMutation {
        identity: command_identity("pure_and_conditional"),
        name: "pure_and_conditional".to_owned(),
        steps: vec![
            CommandExecutionStep::FixedRows {
                name: "fixed".to_owned(),
                cte: "_cmd_step_0".to_owned(),
                maximum_rows: 2,
                columns: vec![column("id", "int4"), column("quantity", "int4")],
                rows: vec![
                    vec![value(json!(1), "int4"), value(json!(2), "int4")],
                    vec![value(json!(2), "int4"), value(json!(3), "int4")],
                ],
                error_path: "$.selectionSet.pure_and_conditional".to_owned(),
            },
            CommandExecutionStep::ProjectMany {
                name: "projected".to_owned(),
                cte: "_cmd_step_1".to_owned(),
                input_cte: "_cmd_step_0".to_owned(),
                maximum_rows: 2,
                values: vec![
                    CommandNamedValue {
                        name: "line_id".to_owned(),
                        column: column("line_id", "int4"),
                        value: CommandExecutionValue::Item {
                            field: "id".to_owned(),
                            pg_type: "int4".to_owned(),
                        },
                    },
                    CommandNamedValue {
                        name: "quantity".to_owned(),
                        column: column("quantity", "int4"),
                        value: CommandExecutionValue::Item {
                            field: "quantity".to_owned(),
                            pg_type: "int4".to_owned(),
                        },
                    },
                ],
                error_path: "$.selectionSet.pure_and_conditional".to_owned(),
            },
            CommandExecutionStep::AssertWhen {
                name: "skipped_assert".to_owned(),
                condition: condition.clone(),
                rule: CommandRule {
                    sql: "FALSE".to_owned(),
                    pg_type: "bool".to_owned(),
                    error_path: "$.selectionSet.pure_and_conditional".to_owned(),
                    message: "false conditional assertion ran".to_owned(),
                },
            },
            CommandExecutionStep::InsertWhen {
                name: "skipped_insert".to_owned(),
                cte: "_cmd_step_3".to_owned(),
                condition,
                table: table(&table_name),
                object: vec![
                    assignment("id", "int4", json!(1)),
                    CommandAssignment {
                        column: column("created_at", "timestamptz"),
                        value: CommandExecutionValue::DatabaseTime {
                            function: CommandDatabaseTime::Now,
                            pg_type: "timestamptz".to_owned(),
                        },
                    },
                ],
                returning: vec![column("id", "int4")],
                check: None,
                error_path: "$.selectionSet.pure_and_conditional".to_owned(),
            },
        ],
        guards: vec![],
        result: vec![
            CommandResultField {
                name: "items".to_owned(),
                value: CommandResultValue::ProjectedRows {
                    cte: "_cmd_step_1".to_owned(),
                    many: true,
                    columns: vec![
                        CommandResultProjection {
                            name: "id".to_owned(),
                            source: column("line_id", "int4"),
                        },
                        CommandResultProjection {
                            name: "count".to_owned(),
                            source: column("quantity", "int4"),
                        },
                    ],
                    maximum_items: 2,
                },
            },
            CommandResultField {
                name: "rule_value".to_owned(),
                value: CommandResultValue::Rule {
                    sql: "'typed'::text".to_owned(),
                    pg_type: "text".to_owned(),
                },
            },
            CommandResultField {
                name: "literal_array".to_owned(),
                value: CommandResultValue::Array {
                    value: Scalar::Json(json!(["a", "b"])),
                    maximum_items: 2,
                },
            },
        ],
        idempotency: None,
        effects: vec![],
        selection: vec![
            CommandResultSelection::List {
                alias: "items".to_owned(),
                field: "items".to_owned(),
                selections: vec![
                    CommandResultSelection::Scalar {
                        alias: "id".to_owned(),
                        field: "id".to_owned(),
                    },
                    CommandResultSelection::Scalar {
                        alias: "count".to_owned(),
                        field: "count".to_owned(),
                    },
                ],
            },
            CommandResultSelection::Scalar {
                alias: "rule_value".to_owned(),
                field: "rule_value".to_owned(),
            },
            CommandResultSelection::Scalar {
                alias: "literal_array".to_owned(),
                field: "literal_array".to_owned(),
            },
        ],
    }));

    assert!(
        sql.contains("_cmd_condition_gate_3")
            && sql.contains("AS MATERIALIZED")
            && sql.contains("statement_timestamp()"),
        "conditional writes and database time must remain inside materialized SQL gates: {sql}"
    );
    let result: Json = client
        .query_one(&sql, &[])
        .expect("pure forms execute while false conditional operations are skipped")
        .get(0);
    assert_eq!(
        result,
        json!({
            "items": [{"id": 1, "count": 2}, {"id": 2, "count": 3}],
            "rule_value": "typed",
            "literal_array": ["a", "b"]
        })
    );
    let rows: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM \"public\".\"{table_name}\""),
            &[],
        )
        .expect("inspect skipped conditional insert")
        .get(0);
    assert_eq!(rows, 0, "a false conditional gate must prevent the write");

    client
        .batch_execute(&format!("DROP TABLE \"public\".\"{table_name}\""))
        .expect("remove conditional command fixture");
}

#[test]
fn allocation_renderer_conserves_quantities_orders_outputs_and_replays_row_sets() {
    let _catalog_lock = command_catalog_test_lock();
    let mut client = postgres_client();
    install_command_catalog_client(&mut client);
    let command_name = format!("allocation_runtime_{}", std::process::id());
    let root = allocation_root(&command_name, 4);
    let sql = donat_sqlgen::mutation_to_sql(&root);

    assert!(
        sql.starts_with("WITH ") && !sql.contains(';'),
        "allocation must remain one Postgres statement: {sql}"
    );
    for required in [
        "_cmd_step_1_ranked",
        "_cmd_step_1_groups",
        "_cmd_step_1_lines",
        "_cmd_step_1_backorders",
        "duplicate candidates",
        "duplicate allocation ids",
        "quantity conservation",
        "row_number() OVER",
    ] {
        assert!(
            sql.contains(required),
            "allocation SQL is missing {required}: {sql}"
        );
    }

    let first: Json = client
        .query_one(&sql, &[])
        .expect("bounded allocation executes")
        .get(0);
    let replay: Json = client
        .query_one(&sql, &[])
        .expect("exact allocation retry replays its canonical row sets")
        .get(0);
    assert_eq!(replay, first, "idempotent allocation replay must be exact");

    let lines = first["lines"]
        .as_array()
        .expect("allocation lines are a JSON list");
    assert_eq!(
        lines
            .iter()
            .map(|line| line["allocated_quantity"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [3, 2, 1],
        "candidate order determines the exact split"
    );
    let groups = first["groups"]
        .as_array()
        .expect("allocation groups are a JSON list");
    assert_eq!(groups.len(), 2);
    assert_ne!(
        groups[0]["allocation_id"], groups[1]["allocation_id"],
        "each typed group key receives one stable deterministic allocation id"
    );
    let backorders = first["backorders"]
        .as_array()
        .expect("backorders are explicit for every requested line");
    assert_eq!(
        backorders
            .iter()
            .map(|row| row["backordered_quantity"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1]
    );
    for backorder in backorders {
        let line_id = &backorder["order_line_id"];
        let requested = backorder["requested_quantity"].as_i64().unwrap();
        let allocated: i64 = lines
            .iter()
            .filter(|line| &line["order_line_id"] == line_id)
            .map(|line| line["allocated_quantity"].as_i64().unwrap())
            .sum();
        assert_eq!(
            allocated + backorder["backordered_quantity"].as_i64().unwrap(),
            requested,
            "every line must conserve requested quantity"
        );
    }

    let overflow =
        donat_sqlgen::mutation_to_sql(&allocation_root(&format!("{command_name}_overflow"), 3));
    let error = client
        .query_one(&overflow, &[])
        .expect_err("allocation input beyond its fixed bound must reject");
    let database_error = error.as_db_error().expect("bound rejection is structured");
    assert_eq!(database_error.code().code(), "P0D01");
    assert!(database_error.message().contains("row bound"));

    let MutationRoot::Command { mut command, alias } =
        allocation_root(&format!("{command_name}_duplicate"), 4)
    else {
        unreachable!("allocation helper returns a command")
    };
    let CommandExecutionStep::FixedRows { rows, .. } = &mut command.steps[0] else {
        unreachable!("allocation helper starts with fixed candidates")
    };
    rows[1] = rows[0].clone();
    let duplicate_sql = donat_sqlgen::mutation_to_sql(&MutationRoot::Command { alias, command });
    let error = client
        .query_one(&duplicate_sql, &[])
        .expect_err("duplicate allocation candidates must reject");
    let database_error = error
        .as_db_error()
        .expect("duplicate rejection is structured");
    assert_eq!(database_error.code().code(), "P0D01");
    assert!(database_error.message().contains("duplicate candidates"));
}
