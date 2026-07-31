use donat_server::commands::decode_command_execution_result;
use tokio_postgres::NoTls;

fn pg_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

#[tokio::test]
async fn command_execution_decoder_exposes_internal_generation_without_changing_result_json() {
    let (client, connection) = tokio_postgres::connect(&pg_url(), NoTls)
        .await
        .expect("Postgres is available");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "SELECT '{\"status\":\"ok\"}'::jsonb AS root, \
                    gen_random_uuid() AS invocation_id, \
                    TRUE AS replayed",
            &[],
        )
        .await
        .expect("internal command result row is available");

    let decoded =
        decode_command_execution_result(&row).expect("typed command generation result decodes");
    assert_eq!(decoded.result_json, serde_json::json!({ "status": "ok" }));
    assert!(decoded.invocation.replayed);
    assert!(!decoded.invocation.invocation_id.is_nil());

    connection.abort();
}
