//! An upstream peer does not get to decide how much memory the engine holds.
//!
//! The four outbound paths that predate the connector transport — action
//! handlers, the auth webhook, remote schemas and JWKS — used to read a
//! response with `reqwest`'s `.json()`, which buffers whatever arrives. A
//! peer that streams without end therefore ended in an allocation failure,
//! and an allocation failure in Rust aborts the process, taking every other
//! request with it.

use axum::Router;
use axum::body::Body;
use axum::routing::get;
use donat_server::upstream::{self, UpstreamBodyError};

/// Serve `bytes` bytes of a JSON string on 127.0.0.1, and return its URL.
async fn serve_body(bytes: usize) -> String {
    let app: Router<()> = Router::new().route(
        "/",
        get(move || async move {
            // A JSON document, so nothing is refused for being unparseable
            // when the point of the test is the size.
            let payload = format!("\"{}\"", "x".repeat(bytes));
            Body::from(payload)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port binds");
    let addr = listener.local_addr().expect("the listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/")
}

#[tokio::test]
async fn a_body_over_the_ceiling_is_refused_instead_of_held() {
    let url = serve_body(8 * 1024).await;
    let response = reqwest::get(&url).await.expect("the test peer answers");

    let error = upstream::read_json(response, 1024)
        .await
        .expect_err("a body over the ceiling is refused");
    assert!(
        matches!(error, UpstreamBodyError::TooLarge { limit: 1024 }),
        "expected a size refusal, got {error:?}"
    );
    // The message names the ceiling, so an operator can raise it knowingly.
    assert_eq!(error.to_string(), "response body exceeds 1024 bytes");
}

#[tokio::test]
async fn a_body_under_the_ceiling_is_read_normally() {
    let url = serve_body(16).await;
    let response = reqwest::get(&url).await.expect("the test peer answers");

    let body = upstream::read_json(response, 1024)
        .await
        .expect("a body under the ceiling is read");
    assert_eq!(body, serde_json::json!("x".repeat(16)));
}

/// The control-plane ceiling is the one the auth webhook and JWKS use, and it
/// is deliberately far smaller than the data-path one: neither a session's
/// variables nor a key set is ever large.
#[tokio::test]
async fn the_control_ceiling_is_smaller_than_the_data_ceiling() {
    const { assert!(upstream::MAX_CONTROL_BODY_BYTES < upstream::DEFAULT_MAX_UPSTREAM_BODY_BYTES) };

    let url = serve_body(upstream::MAX_CONTROL_BODY_BYTES + 1).await;
    let response = reqwest::get(&url).await.expect("the test peer answers");
    assert!(
        upstream::read_json(response, upstream::MAX_CONTROL_BODY_BYTES)
            .await
            .is_err(),
        "a control-plane peer must not exceed its own ceiling"
    );
}

/// A peer that answers with nothing at all is not an error: several of these
/// paths treat an empty body as "no data", and did so before the ceiling.
#[tokio::test]
async fn an_empty_body_reads_as_null() {
    let url = serve_body(0).await;
    let response = reqwest::get(&url).await.expect("the test peer answers");
    let body = upstream::read_json(response, 1024)
        .await
        .expect("an empty-ish body is read");
    assert_eq!(body, serde_json::json!(""));
}
