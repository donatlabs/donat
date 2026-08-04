use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_server::migrate::run_migrate;
use donat_server::processes::validate_check_violation_helper;
use tokio_postgres::{Client, NoTls};

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

fn postgres_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

async fn isolated_database(label: &str) -> (String, String, String) {
    let admin_url = postgres_admin_url();
    let name = format!(
        "donat_{label}_{}_{}",
        std::process::id(),
        NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
    );
    let (client, connection) = tokio_postgres::connect(&admin_url, NoTls)
        .await
        .expect("Postgres admin database is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE);"))
        .await
        .expect("stale process migration database drops");
    client
        .batch_execute(&format!("CREATE DATABASE {name};"))
        .await
        .expect("process migration database creates");
    connection.abort();

    let prefix = admin_url
        .rsplit_once('/')
        .expect("Postgres URL has a database segment")
        .0
        .to_owned();
    (admin_url, name.clone(), format!("{prefix}/{name}"))
}

async fn drop_database(admin_url: &str, name: &str) {
    let (client, connection) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .expect("Postgres admin database is available for cleanup");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP DATABASE {name} WITH (FORCE);"))
        .await
        .expect("process migration database drops");
    connection.abort();
}

fn expected_columns() -> BTreeMap<&'static str, &'static [&'static str]> {
    BTreeMap::from([
        (
            "process_definition_versions",
            &[
                "source_name",
                "process_name",
                "revision",
                "canonical_definition",
                "dependency_descriptors",
                "runtime_abi",
                "status",
                "deployed_at",
                "retired_at",
            ][..],
        ),
        (
            "process_start_requests",
            &[
                "source_name",
                "id",
                "process_name",
                "revision",
                "input_json",
                "command_invocation_id",
                "effect_position",
                "idempotency_key",
                "status",
                "instance_id",
                "created_at",
                "consumed_at",
                "caller_role",
                "caller_session_json",
            ],
        ),
        (
            "process_instances",
            &[
                "source_name",
                "id",
                "process_name",
                "revision",
                "source_request_id",
                "start_idempotency_key",
                "status",
                "current_state",
                "input_json",
                "state_json",
                "version",
                "created_at",
                "updated_at",
                "caller_role",
                "caller_session_json",
                "terminal_output_json",
                "failure_json",
            ],
        ),
        (
            "process_events",
            &[
                "source_name",
                "id",
                "instance_id",
                "process_name",
                "revision",
                "kind",
                "payload_json",
                "idempotency_key",
                "available_at",
                "status",
                "attempts",
                "created_at",
                "consumed_at",
            ],
        ),
        (
            "process_signal_requests",
            &[
                "source_name",
                "id",
                "process_name",
                "process_revision",
                "signal_name",
                "correlation_json",
                "payload_json",
                "command_invocation_id",
                "effect_position",
                "idempotency_key",
                "status",
                "created_at",
                "consumed_at",
            ],
        ),
        (
            "process_activity_jobs",
            &[
                "source_name",
                "id",
                "instance_id",
                "enqueued_from_event_id",
                "state_name",
                "logical_activity_id",
                "connector_instance",
                "operation",
                "serialization_key_hash",
                "input_json",
                "result_json",
                "request_fingerprint",
                "status",
                "attempts",
                "lease_generation",
                "available_at",
                "schedule_to_start_deadline",
                "start_to_close_deadline",
                "lease_token",
                "lease_expires_at",
                "last_error_json",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "process_activity_provider_steps",
            &[
                "source_name",
                "activity_job_id",
                "logical_activity_id",
                "compiled_step_id",
                "idempotency_key",
                "first_provider_attempt_at",
                "maximum_send_deadline_at",
                "usable_window_expires_at",
                "created_at",
            ],
        ),
        (
            "process_fanout_items",
            &[
                "source_name",
                "instance_id",
                "state_name",
                "entry_event_id",
                "ordinal",
                "item_key",
                "item_key_identity",
                "item_json",
                "status",
                "activity_job_id",
                "result_json",
                "failure_json",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "process_transition_logs",
            &[
                "source_name",
                "id",
                "instance_id",
                "event_id",
                "activity_job_id",
                "activity_attempt",
                "activity_lease_generation",
                "from_state",
                "to_state",
                "outcome",
                "definition_revision",
                "command_result_json",
                "before_state_hash",
                "after_state_hash",
                "redacted_context",
                "created_at",
            ],
        ),
        (
            "process_capacity_reservations",
            &[
                "source_name",
                "id",
                "activity_job_id",
                "connector_instance",
                "operation",
                "serialization_key_hash",
                "lease_token",
                "reserved_at",
                "expires_at",
                "released_at",
            ],
        ),
        (
            "process_capacity_buckets",
            &[
                "source_name",
                "connector_instance",
                "operation",
                "available_tokens",
                "last_refill_at",
                "policy_fingerprint",
            ],
        ),
        (
            "process_inbound_deliveries",
            &[
                "source_name",
                "id",
                "connector_instance",
                "provider_event_id",
                "payload_digest",
                "signature_status",
                "outcome",
                "instance_id",
                "process_event_id",
                "redacted_metadata",
                "received_at",
            ],
        ),
        (
            "process_inbound_events",
            &[
                "source_name",
                "id",
                "connector_instance",
                "provider_event_id",
                "first_delivery_id",
                "payload_digest",
                "verified_at",
            ],
        ),
    ])
}

async fn table_columns(client: &Client, table: &str) -> Vec<String> {
    client
        .query(
            "
            SELECT column_name
            FROM information_schema.columns
            WHERE table_schema = 'donat' AND table_name = $1
            ORDER BY ordinal_position
            ",
            &[&table],
        )
        .await
        .expect("process table columns query succeeds")
        .into_iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn process_schema_is_source_qualified_and_exact() {
    let (admin_url, database_name, database_url) = isolated_database("process_schema").await;
    run_migrate(&database_url, &migrations_dir())
        .await
        .expect("bundled migrations apply");
    let (client, connection) = tokio_postgres::connect(&database_url, NoTls)
        .await
        .expect("migrated database is available");
    let connection = tokio::spawn(connection);

    let invocation = client
        .query_one(
            "
            SELECT data_type, is_nullable
            FROM information_schema.columns
            WHERE table_schema = 'donat'
              AND table_name = 'command_invocations'
              AND column_name = 'invocation_id'
            ",
            &[],
        )
        .await
        .expect("V6 command generation column exists");
    assert_eq!(invocation.get::<_, String>(0), "uuid");
    assert_eq!(invocation.get::<_, String>(1), "NO");

    let invocation_unique: bool = client
        .query_one(
            "
            SELECT EXISTS (
              SELECT 1
              FROM pg_constraint constraint_
              JOIN pg_class relation ON relation.oid = constraint_.conrelid
              JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
              WHERE namespace.nspname = 'donat'
                AND relation.relname = 'command_invocations'
                AND constraint_.contype = 'u'
                AND pg_get_constraintdef(constraint_.oid) =
                    'UNIQUE (invocation_id)'
            )
            ",
            &[],
        )
        .await
        .expect("command invocation UUID uniqueness introspects")
        .get(0);
    assert!(invocation_unique);

    for (table, expected) in expected_columns() {
        let actual = table_columns(&client, table).await;
        assert_eq!(actual, expected, "unexpected columns for donat.{table}");
        assert_eq!(
            actual.first().map(String::as_str),
            Some("source_name"),
            "source_name must lead every process table"
        );
    }

    let unqualified_keys: i64 = client
        .query_one(
            "
            SELECT count(*)
            FROM pg_constraint constraint_
            JOIN pg_class relation ON relation.oid = constraint_.conrelid
            JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
            WHERE namespace.nspname = 'donat'
              AND relation.relname LIKE 'process_%'
              AND constraint_.contype IN ('p', 'f', 'u')
              AND (
                SELECT attribute.attname
                FROM unnest(constraint_.conkey) WITH ORDINALITY key(attnum, ordinality)
                JOIN pg_attribute attribute
                  ON attribute.attrelid = relation.oid
                 AND attribute.attnum = key.attnum
                ORDER BY key.ordinality
                LIMIT 1
              ) <> 'source_name'
            ",
            &[],
        )
        .await
        .expect("source-qualified constraints introspect")
        .get(0);
    assert_eq!(unqualified_keys, 0);

    let command_journal_foreign_keys: i64 = client
        .query_one(
            "
            SELECT count(*)
            FROM pg_constraint constraint_
            JOIN pg_class owner ON owner.oid = constraint_.conrelid
            JOIN pg_namespace namespace ON namespace.oid = owner.relnamespace
            JOIN pg_class target ON target.oid = constraint_.confrelid
            WHERE namespace.nspname = 'donat'
              AND owner.relname LIKE 'process_%'
              AND constraint_.contype = 'f'
              AND target.relname IN (
                'command_invocations',
                'command_invocation_claims'
              )
            ",
            &[],
        )
        .await
        .expect("process foreign keys introspect")
        .get(0);
    assert_eq!(command_journal_foreign_keys, 0);

    let oversized_json_without_check: i64 = client
        .query_one(
            "
            SELECT count(*)
            FROM information_schema.columns column_
            WHERE column_.table_schema = 'donat'
              AND column_.table_name LIKE 'process_%'
              AND column_.data_type = 'jsonb'
              AND NOT EXISTS (
                SELECT 1
                FROM pg_constraint constraint_
                JOIN pg_class relation ON relation.oid = constraint_.conrelid
                JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
                WHERE namespace.nspname = column_.table_schema
                  AND relation.relname = column_.table_name
                  AND constraint_.contype = 'c'
                  AND pg_get_constraintdef(constraint_.oid)
                      LIKE '%pg_column_size(' || column_.column_name || ') <= 262144%'
              )
            ",
            &[],
        )
        .await
        .expect("JSON size constraints introspect")
        .get(0);
    assert_eq!(oversized_json_without_check, 0);

    let wait_history_index: Option<String> = client
        .query_opt(
            "
            SELECT pg_get_indexdef(index_.indexrelid)
            FROM pg_index index_
            JOIN pg_class relation ON relation.oid = index_.indrelid
            JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
            JOIN pg_class index_relation ON index_relation.oid = index_.indexrelid
            WHERE namespace.nspname = 'donat'
              AND relation.relname = 'process_events'
              AND index_relation.relname = 'process_events_signal_wait_history_idx'
            ",
            &[],
        )
        .await
        .expect("signal wait history index introspects")
        .map(|row| row.get(0));
    let wait_history_index =
        wait_history_index.expect("signal wait history must have a bounded lookup index");
    assert!(wait_history_index.contains("USING gin (payload_json jsonb_path_ops)"));
    assert!(wait_history_index.contains("kind = 'timer'"));
    assert!(wait_history_index.contains("signal_name"));

    let webhook_indexes = client
        .query(
            "
            SELECT index_relation.relname, pg_get_indexdef(index_.indexrelid)
            FROM pg_index index_
            JOIN pg_class relation ON relation.oid = index_.indrelid
            JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
            JOIN pg_class index_relation ON index_relation.oid = index_.indexrelid
            WHERE namespace.nspname = 'donat'
              AND relation.relname = 'process_events'
              AND index_relation.relname = ANY($1::text[])
            ORDER BY index_relation.relname
            ",
            &[&vec![
                "process_events_wait_instance_idx",
                "process_events_webhook_wait_history_idx",
            ]],
        )
        .await
        .expect("webhook wait indexes introspect")
        .into_iter()
        .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect::<BTreeMap<_, _>>();
    let instance_index = webhook_indexes
        .get("process_events_wait_instance_idx")
        .expect("wait-marker lookup must have a source/instance index");
    assert!(instance_index.contains("(source_name, instance_id, status, created_at, id)"));
    assert!(instance_index.contains("WHERE (kind = 'timer'::text)"));
    let webhook_history_index = webhook_indexes
        .get("process_events_webhook_wait_history_idx")
        .expect("webhook history must have a bounded containment index");
    assert!(webhook_history_index.contains("USING gin (payload_json jsonb_path_ops)"));
    assert!(webhook_history_index.contains("connector_instance"));
    assert!(webhook_history_index.contains("trigger"));

    let helper = client
        .query_one(
            "
            SELECT pg_get_function_identity_arguments(procedure_.oid),
                   pg_get_function_result(procedure_.oid),
                   language.lanname
            FROM pg_proc procedure_
            JOIN pg_namespace namespace ON namespace.oid = procedure_.pronamespace
            JOIN pg_language language ON language.oid = procedure_.prolang
            WHERE namespace.nspname = 'donat'
              AND procedure_.proname = 'check_violation'
            ",
            &[],
        )
        .await
        .expect("migration-owned check helper exists");
    assert_eq!(helper.get::<_, String>(0), "msg text");
    assert_eq!(helper.get::<_, String>(1), "json");
    assert_eq!(helper.get::<_, String>(2), "plpgsql");

    client
        .batch_execute(
            "
            DROP FUNCTION donat.check_violation(text);
            CREATE FUNCTION donat.check_violation(msg text)
            RETURNS text
            LANGUAGE sql
            AS 'SELECT msg';
            ",
        )
        .await
        .expect("replace helper with an incompatible definition");
    let error = validate_check_violation_helper(&client)
        .await
        .expect_err("serving validation must reject an incompatible helper");
    assert!(
        error.to_string().contains("run `donat migrate`"),
        "unexpected compatibility error: {error:#}"
    );

    connection.abort();
    drop_database(&admin_url, &database_name).await;
}
