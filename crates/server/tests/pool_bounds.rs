//! The statement ceiling every pooled session starts with.
//!
//! Dropping an HTTP request does not cancel the statement it started, so a
//! query the planner accepted but the database cannot answer cheaply would
//! otherwise hold a backend open long after the caller has gone. The ceiling
//! is carried as a connection option, which only a live server can confirm:
//! it is applied by libpq at connection time, not by anything we can assert
//! from the configuration struct alone.

fn pg_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15433/postgres".to_string())
}

async fn show_statement_timeout(pool: &deadpool_postgres::Pool) -> String {
    let client = pool.get().await.expect("a pooled connection");
    client
        .query_one("SHOW statement_timeout", &[])
        .await
        .expect("statement_timeout is readable")
        .get(0)
}

#[tokio::test]
async fn a_pooled_session_starts_with_the_default_statement_ceiling() {
    let pool = donat_server::state::make_pool(&pg_url()).expect("the pool builds");
    assert_eq!(show_statement_timeout(&pool).await, "30s");
}

/// A URL that carries its own `options` keeps them. `deadpool` replaces the
/// whole option string rather than appending to it, so adding our ceiling
/// there would silently drop whatever the deployment had set.
#[tokio::test]
async fn a_url_with_its_own_options_keeps_them() {
    let url = format!("{}?options=-c%20statement_timeout%3D7s", pg_url());
    let pool = donat_server::state::make_pool(&url).expect("the pool builds");
    assert_eq!(show_statement_timeout(&pool).await, "7s");
}

/// A connection the server has since killed must never reach a caller.
///
/// This is the managed-Postgres failure: a proxy, a NAT gateway or the
/// provider's own idle reaper drops the socket while the connection sits in
/// the pool. `deadpool`'s default recycling only asks `is_closed()`, which
/// still answers "open" for a hard-closed socket, and the engine retries
/// nothing — so the next request would fail at the caller rather than in the
/// pool.
#[tokio::test]
async fn a_connection_killed_behind_the_pools_back_is_not_handed_out() {
    let pool = donat_server::state::make_pool(&pg_url()).expect("the pool builds");

    // Take a connection, learn its backend pid, and give it back to the pool.
    let pid: i32 = {
        let client = pool.get().await.expect("a pooled connection");
        client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .expect("the backend pid is readable")
            .get(0)
    };

    // Kill it from another connection, the way the network would.
    let url = pg_url();
    let (killer, connection) = tokio_postgres::connect(&url, donat_server::pgtls::connector())
        .await
        .expect("a second connection opens");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    killer
        .execute("SELECT pg_terminate_backend($1)", &[&pid])
        .await
        .expect("the pooled backend is terminated");

    // The pool must notice and produce a working connection anyway.
    let client = pool
        .get()
        .await
        .expect("the pool replaces a connection it can no longer use");
    let live: i32 = client
        .query_one("SELECT 1", &[])
        .await
        .expect("the connection handed out is alive")
        .get(0);
    assert_eq!(live, 1);
    let new_pid: i32 = client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .expect("the new backend pid is readable")
        .get(0);
    assert_ne!(
        new_pid, pid,
        "the dead backend must not have been handed out again"
    );
}
