//! The engine can speak TLS to Postgres.
//!
//! This is not a style preference. `tokio_postgres::NoTls` — what every
//! connection site used to pass — does not mean "TLS is terminated elsewhere",
//! it means the client has no TLS implementation at all, so a URL with
//! `sslmode=require` is refused before a socket is opened. That is the default
//! posture of RDS, Cloud SQL, Neon and Supabase, which made every one of them
//! unreachable.
//!
//! The test database has `ssl=off`, so `sslmode=require` still cannot succeed
//! here. What changed is *why*: the refusal now comes from the server, after a
//! negotiation the client was able to attempt.

fn pg_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15433/postgres".to_string())
}

#[tokio::test]
async fn sslmode_require_negotiates_instead_of_refusing_to_try() {
    let url = format!("{}?sslmode=require", pg_url());
    let error = match tokio_postgres::connect(&url, donat_server::pgtls::connector()).await {
        // A deployment whose Postgres does offer TLS: the negotiation
        // succeeded, which is the whole point.
        Ok(_) => return,
        Err(error) => error.to_string(),
    };
    assert!(
        !error.contains("no TLS implementation"),
        "the client must be able to attempt TLS, but reported: {error}"
    );
    assert!(
        error.to_lowercase().contains("tls"),
        "expected a TLS negotiation failure from the server, got: {error}"
    );
}

/// The default mode must keep working against a server without TLS. A
/// deployment that was connecting yesterday has to connect today.
#[tokio::test]
async fn the_default_mode_still_reaches_a_plaintext_server() {
    let (client, connection) = tokio_postgres::connect(&pg_url(), donat_server::pgtls::connector())
        .await
        .expect("the default sslmode reaches a server without TLS");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let row = client
        .query_one("SELECT 1", &[])
        .await
        .expect("the connection works");
    assert_eq!(row.get::<_, i32>(0), 1);
}

/// `sslmode=disable` is an explicit opt out, and stays one.
#[tokio::test]
async fn sslmode_disable_keeps_a_plaintext_socket() {
    let url = format!("{}?sslmode=disable", pg_url());
    let (client, connection) = tokio_postgres::connect(&url, donat_server::pgtls::connector())
        .await
        .expect("sslmode=disable connects");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let row = client
        .query_one("SELECT 1", &[])
        .await
        .expect("the connection works");
    assert_eq!(row.get::<_, i32>(0), 1);
}
