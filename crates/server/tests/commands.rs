use donat_server::commands::{
    CommandBusinessRejection, decode_command_business_rejection, decode_command_execution_result,
};
use tokio_postgres::NoTls;

fn pg_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

#[tokio::test]
async fn command_business_rejection_decoder_is_strict() {
    let (client, connection) = tokio_postgres::connect(&pg_url(), NoTls)
        .await
        .expect("Postgres is available");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(
            "
            CREATE OR REPLACE FUNCTION pg_temp.donat_test_raise(state text, message text)
            RETURNS void LANGUAGE plpgsql AS $$
            BEGIN
                RAISE EXCEPTION USING ERRCODE = state, MESSAGE = message;
            END;
            $$;
            ",
        )
        .await
        .expect("temporary error helper creates");

    async fn raised(
        client: &tokio_postgres::Client,
        state: &str,
        message: &str,
    ) -> tokio_postgres::Error {
        client
            .execute(
                "SELECT pg_temp.donat_test_raise($1, $2)",
                &[&state, &message],
            )
            .await
            .expect_err("temporary helper raises")
    }

    let exact = raised(
        &client,
        "P0D01",
        r#"{"kind":"donat.graphql-error.v1","code":"validation-failed","path":"$.processes.checkout","message":"declined"}"#,
    )
    .await;
    assert_eq!(
        decode_command_business_rejection(&exact),
        Some(CommandBusinessRejection {
            code: "validation-failed".to_owned(),
            path: "$.processes.checkout".to_owned(),
            message: "declined".to_owned(),
        })
    );

    for (state, message) in [
        (
            "P0D01",
            r#"{"kind":"donat.graphql-error.v1","code":"validation-failed","path":"$.processes.checkout","message":"declined","extra":"no"}"#,
        ),
        (
            "P0D01",
            r#"{"kind":"donat.graphql-error.v1","code":"","path":"$.processes.checkout","message":"declined"}"#,
        ),
        (
            "P0D01",
            r#"{"kind":"donat.graphql-error.v1","code":"validation-failed","path":"processes.checkout","message":"declined"}"#,
        ),
        (
            "P0D01",
            r#"{"kind":"other","code":"validation-failed","path":"$.processes.checkout","message":"declined"}"#,
        ),
        ("P0D01", "not-json"),
        (
            "23514",
            r#"{"kind":"donat.graphql-error.v1","code":"validation-failed","path":"$.processes.checkout","message":"declined"}"#,
        ),
        ("23505", "private constraint detail"),
    ] {
        let error = raised(&client, state, message).await;
        assert_eq!(
            decode_command_business_rejection(&error),
            None,
            "state {state} and payload {message:?} must not become a business rejection"
        );
    }
    connection.abort();
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
