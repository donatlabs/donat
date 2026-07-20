//! Legacy graphql-ws (Apollo subscriptions-transport-ws) support.
//! Queries/mutations answer with one `data` + `complete`; subscriptions
//! are live: re-executed once a second, a `data` frame is sent whenever
//! the result changes, until the client stops or the connection dies.
//! When JWT auth is on, the connection is closed at token expiry.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::Response;
use futures_util::SinkExt;
use futures_util::stream::{SplitSink, StreamExt};
use serde_json::{Value as Json, json};
use tokio::sync::Mutex;

use crate::gql;
use crate::state::SharedState;

type Sender = Arc<Mutex<SplitSink<WebSocket, Message>>>;
const MAX_SUBSCRIPTIONS_PER_CONNECTION: usize = 100;
const SUBSCRIPTION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
static NEXT_SUBSCRIPTION_POLL_PHASE: AtomicU64 = AtomicU64::new(0);

struct ActiveSubscription {
    handle: tokio::task::JoinHandle<()>,
}

fn subscription_poll_phase(sequence: u64) -> std::time::Duration {
    // The multiplier is coprime with 1,000, so consecutive subscriptions are
    // distributed over the entire one-second polling interval rather than
    // waking in a synchronized burst.
    std::time::Duration::from_millis(sequence.wrapping_mul(618_033) % 1_000)
}

async fn acquire_subscription_poll_permit(
    permits: Arc<tokio::sync::Semaphore>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    permits.acquire_owned().await.ok()
}

pub async fn upgrade(State(state): State<SharedState>, ws: WebSocketUpgrade) -> Response {
    ws.protocols(["graphql-ws"])
        .on_upgrade(move |socket| serve(state, socket, false))
}

pub async fn upgrade_relay(State(state): State<SharedState>, ws: WebSocketUpgrade) -> Response {
    ws.protocols(["graphql-ws"])
        .on_upgrade(move |socket| serve(state, socket, true))
}

async fn send(sender: &Sender, frame: Json) -> Result<(), axum::Error> {
    sender
        .lock()
        .await
        .send(Message::Text(frame.to_string().into()))
        .await
}

async fn serve(state: SharedState, socket: WebSocket, relay: bool) {
    let (sink, mut stream) = socket.split();
    let sender: Sender = Arc::new(Mutex::new(sink));
    let mut session_headers = HeaderMap::new();
    let mut subscriptions: HashMap<String, ActiveSubscription> = HashMap::new();
    let mut expiry_task: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(Ok(message)) = stream.next().await {
        subscriptions.retain(|_, subscription| !subscription.handle.is_finished());
        let text = match message {
            Message::Text(text) => text.to_string(),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => break,
            Message::Binary(_) => continue,
        };
        let Ok(frame) = serde_json::from_str::<Json>(&text) else {
            continue;
        };
        match frame.get("type").and_then(Json::as_str) {
            Some("connection_init") => {
                if let Some(headers) = frame.pointer("/payload/headers").and_then(Json::as_object) {
                    for (key, value) in headers {
                        let (Ok(name), Some(value)) = (
                            HeaderName::try_from(key.to_ascii_lowercase()),
                            value.as_str().and_then(|v| HeaderValue::try_from(v).ok()),
                        ) else {
                            continue;
                        };
                        session_headers.insert(name, value);
                    }
                }
                if send(&sender, json!({ "type": "connection_ack" }))
                    .await
                    .is_err()
                {
                    break;
                }
                let _ = send(&sender, json!({ "type": "ka" })).await;

                // JWT mode: close the connection when the token expires.
                if let Some(jwt) = &state.jwt {
                    let token = session_headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "))
                        .map(str::to_string);
                    if let Some(exp) = token.and_then(|t| jwt.token_expiry(&t)) {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let wait = exp.saturating_sub(now);
                        let sender = sender.clone();
                        if let Some(task) = expiry_task.take() {
                            task.abort();
                        }
                        expiry_task = Some(tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                            let mut sink = sender.lock().await;
                            let _ = sink.send(Message::Close(None)).await;
                            let _ = sink.close().await;
                        }));
                    }
                }
            }
            Some("start") => {
                let id = frame.get("id").cloned().unwrap_or(Json::Null);
                let payload = frame.get("payload").cloned().unwrap_or(Json::Null);
                let session = match gql::resolve_session(&state, &session_headers).await {
                    Ok(session) => session,
                    Err((_, errors)) => {
                        let _ = send(
                            &sender,
                            json!({ "type": "data", "id": id, "payload": errors }),
                        )
                        .await;
                        let _ = send(&sender, json!({ "type": "complete", "id": id })).await;
                        continue;
                    }
                };

                if let Some(subscription_doc) = subscription_document(&payload) {
                    // Live query: poll and push on change.
                    let id_key = id.as_str().unwrap_or_default().to_string();
                    if let Some(old) = subscriptions.remove(&id_key) {
                        old.handle.abort();
                        let _ = old.handle.await;
                    }
                    if subscriptions.len() >= MAX_SUBSCRIPTIONS_PER_CONNECTION {
                        let _ = send(
                            &sender,
                            json!({
                                "type": "data",
                                "id": id,
                                "payload": {
                                    "errors": [{
                                        "extensions": { "path": "$", "code": "unexpected" },
                                        "message": "subscription limit exceeded"
                                    }]
                                }
                            }),
                        )
                        .await;
                        let _ = send(&sender, json!({ "type": "complete", "id": id })).await;
                        continue;
                    }
                    let permit = match state.subscription_permits.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            let _ = send(
                                &sender,
                                json!({
                                    "type": "data",
                                    "id": id,
                                    "payload": {
                                        "errors": [{
                                            "extensions": { "path": "$", "code": "unexpected" },
                                            "message": "server subscription capacity exhausted"
                                        }]
                                    }
                                }),
                            )
                            .await;
                            let _ = send(&sender, json!({ "type": "complete", "id": id })).await;
                            continue;
                        }
                    };
                    let state = state.clone();
                    let poll_permits = state.subscription_poll_permits.clone();
                    let initial_poll_delay = subscription_poll_phase(
                        NEXT_SUBSCRIPTION_POLL_PHASE.fetch_add(1, Ordering::Relaxed),
                    );
                    let sender_task = sender.clone();
                    let id_task = id.clone();
                    let handle = tokio::spawn(async move {
                        // Keep the process-wide slot with the task itself so
                        // it is released immediately on normal exit or abort.
                        let _permit = permit;
                        let mut last: Option<Json> = None;
                        tokio::time::sleep(initial_poll_delay).await;
                        loop {
                            // Only an individual poll holds this permit. A
                            // long-lived subscription therefore cannot occupy
                            // a backend execution slot while it sleeps.
                            let Some(poll_permit) =
                                acquire_subscription_poll_permit(poll_permits.clone()).await
                            else {
                                break;
                            };
                            let response = gql::execute_preparsed_full(
                                &state,
                                &session,
                                &payload,
                                relay,
                                &HeaderMap::new(),
                                &subscription_doc,
                            )
                            .await
                            .1;
                            drop(poll_permit);
                            if last.as_ref() != Some(&response) {
                                last = Some(response.clone());
                                if send(
                                    &sender_task,
                                    json!({ "type": "data", "id": id_task, "payload": response }),
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            tokio::time::sleep(SUBSCRIPTION_POLL_INTERVAL).await;
                        }
                    });
                    subscriptions.insert(id_key, ActiveSubscription { handle });
                } else {
                    let response = gql::execute_with(&state, &session, &payload, relay).await.1;
                    if send(
                        &sender,
                        json!({ "type": "data", "id": id, "payload": response }),
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                    let _ = send(&sender, json!({ "type": "complete", "id": id })).await;
                }
            }
            Some("stop") => match frame.get("id").and_then(Json::as_str) {
                Some(id) => {
                    if let Some(task) = subscriptions.remove(id) {
                        task.handle.abort();
                    }
                }
                None => {
                    let _ = send(
                        &sender,
                        json!({
                            "type": "connection_error",
                            "payload": "Message missing 'id' field",
                        }),
                    )
                    .await;
                }
            },
            Some("connection_terminate") => break,
            // Unknown client message types are protocol errors.
            other => {
                let _ = send(
                    &sender,
                    json!({
                        "type": "connection_error",
                        "payload": format!(
                            "unexpected message type: {}",
                            other.unwrap_or("<none>")
                        ),
                    }),
                )
                .await;
            }
        }
    }

    for (_, task) in subscriptions {
        task.handle.abort();
    }
    if let Some(task) = expiry_task {
        task.abort();
    }
}

#[cfg(test)]
fn is_subscription(payload: &Json) -> bool {
    subscription_document(payload).is_some()
}

fn subscription_document(
    payload: &Json,
) -> Option<graphql_parser::query::Document<'static, String>> {
    let Some(query) = payload.get("query").and_then(Json::as_str) else {
        return None;
    };
    // Don't parse a too-deep query here (would overflow); execute_with will
    // reject it with the depth error.
    if gql::query_too_deep(query) {
        return None;
    }
    let doc = graphql_parser::parse_query::<String>(query)
        .ok()?
        .into_static();
    doc.definitions
        .iter()
        .any(|d| {
            matches!(
                d,
                graphql_parser::query::Definition::Operation(
                    graphql_parser::query::OperationDefinition::Subscription(_)
                )
            )
        })
        .then_some(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_operations_detected() {
        assert!(is_subscription(
            &json!({ "query": "subscription { user { id } }" })
        ));
        assert!(is_subscription(&json!({
            "query": "query Q { a } subscription S { b }"
        })));
    }

    #[test]
    fn queries_and_mutations_are_not_subscriptions() {
        assert!(!is_subscription(
            &json!({ "query": "query { user { id } }" })
        ));
        assert!(!is_subscription(&json!({ "query": "{ user { id } }" })));
        assert!(!is_subscription(&json!({
            "query": "mutation { delete_user { affected_rows } }"
        })));
    }

    #[test]
    fn malformed_payloads_are_not_subscriptions() {
        assert!(!is_subscription(&json!({})));
        assert!(!is_subscription(&json!({ "query": 5 })));
        assert!(!is_subscription(&json!({ "query": "not graphql {" })));
    }

    #[test]
    fn subscription_poll_phases_cover_the_polling_interval() {
        let mut phases: Vec<_> = (0..1_000)
            .map(subscription_poll_phase)
            .map(|phase| phase.as_millis())
            .collect();
        phases.sort_unstable();
        phases.dedup();
        assert_eq!(phases.len(), 1_000);
    }

    #[tokio::test]
    async fn subscription_poll_permit_bounds_only_poll_execution() {
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let held = acquire_subscription_poll_permit(permits.clone())
            .await
            .expect("first poll acquires the permit");
        let waiter = tokio::spawn(acquire_subscription_poll_permit(permits.clone()));

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), waiter)
                .await
                .is_err(),
            "another poll must wait without consuming an active subscription slot"
        );
        drop(held);
        let permit = acquire_subscription_poll_permit(permits)
            .await
            .expect("next poll acquires after execution finishes");
        drop(permit);
    }
}
