//! The engine-served half of file attachments (spec 008): the completion route
//! and the background collector.
//!
//! It is deliberately small. Bytes never pass through the engine — the object
//! store presigns both the upload and the download — so the only call the
//! engine answers is the one reporting an upload finished, and the only file
//! I/O it performs is deleting what nothing references any more.
//!
//! Authorization here is the signature, not a session: the URL was minted by a
//! statement that had already passed the role's own permission on the owning
//! row. The route verifies that capability and nothing else, and it never
//! accepts a path, a bucket, or a key from the caller.

use std::time::Duration;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use donat_storage::{Backend, PURPOSE_COMPLETE};

use crate::state::SharedState;

/// The signature and expiry the completion URL carries.
#[derive(Debug, Deserialize)]
pub struct Capability {
    exp: i64,
    sig: String,
}

/// Mount the file routes, or nothing at all when the deployment declares no
/// attachment. A deployment without them has no `/v1/files` surface to probe.
pub fn router(state: &SharedState) -> Option<Router<SharedState>> {
    if state.storage.is_empty() {
        return None;
    }
    // Deliberately no `DefaultBodyLimit` layer. It would not do anything here:
    // the upload takes the raw `Body` so it can stream, and that extractor never
    // consults the limit — a layer would only imply a protection the surface
    // does not have. The real gate is the running byte count in `upload`, which
    // holds the body to the column's own `max_bytes` and abandons it mid-stream.
    Some(Router::new().route("/v1/files/complete/{id}", post(complete).options(preflight)))
}

/// `POST /v1/files/complete/{id}`
async fn complete(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<Uuid>,
    Query(capability): Query<Capability>,
    headers: HeaderMap,
) -> Response {
    let mut response = complete_inner(State(state.clone()), AxumPath(id), Query(capability)).await;
    apply_cors(&state, &headers, &mut response);
    log_outcome("complete", id, &response);
    response
}

/// Answer a browser's preflight. Reporting an upload finished is a cross-origin
/// POST, which no browser will send unasked.
async fn preflight(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    apply_cors(&state, &headers, &mut response);
    response
}

/// Add the cross-origin headers this deployment declared, if the request came
/// from an origin it allows. Declaring none mounts no CORS at all.
fn apply_cors(state: &SharedState, request: &HeaderMap, response: &mut Response) {
    let cors = state.storage.cors();
    if cors.is_empty() {
        return;
    }
    let Some(origin) = request
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    if !cors.allows(origin) {
        return;
    }
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, PUT, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
    if let Ok(value) = HeaderValue::from_str(&cors.max_age_seconds.to_string()) {
        headers.insert(header::ACCESS_CONTROL_MAX_AGE, value);
    }
    // The allowed origin varies by request, so a cache must not reuse one
    // origin's response for another.
    headers.insert(header::VARY, HeaderValue::from_static("Origin"));
}

/// Every file-route outcome, at one event per request.
///
/// Without this the surface is silent: a forged signature, an object that was
/// never uploaded and an unreachable store all look identical to a deployment
/// reading its logs.
fn log_outcome(route: &'static str, id: Uuid, response: &Response) {
    let status = response.status();
    if status.is_success() || status == StatusCode::NOT_MODIFIED {
        tracing::info!(target: "donat::files", route, %id, status = status.as_u16(), "file request");
    } else {
        tracing::warn!(target: "donat::files", route, %id, status = status.as_u16(), "file request refused");
    }
}

/// One upload row, as the routes need it.
struct Upload {
    id: Uuid,
    #[allow(dead_code)]
    attachment: String,
    backend: String,
    object_key: String,
    declared_bytes: u64,
    state: String,
}

/// Load one upload row and the limits of the column it belongs to.
async fn load_upload(state: &SharedState, id: Uuid) -> Option<(Upload, AttachmentLimits)> {
    let pool = storage_pool(state).await?;
    let client = pool.get().await.ok()?;
    let row = client
        .query_opt(
            "SELECT id, attachment, backend, object_key, declared_bytes, state \
             FROM donat.file_uploads WHERE id = $1 AND expires_at > now()",
            &[&id],
        )
        .await
        .ok()??;
    let upload = Upload {
        id: row.get("id"),
        attachment: row.get("attachment"),
        backend: row.get("backend"),
        object_key: row.get("object_key"),
        declared_bytes: row.get::<_, i64>("declared_bytes").max(0) as u64,
        state: row.get("state"),
    };
    let spec = state.storage.attachment(&upload.attachment)?;
    let limits = AttachmentLimits {
        max_bytes: spec.max_bytes,
    };
    Some((upload, limits))
}

/// The part of the declaration the route needs, copied out so the borrow on the
/// registry ends with the lookup.
struct AttachmentLimits {
    max_bytes: u64,
}

/// The pool holding `donat.file_uploads`. Attachments are Postgres-only and
/// every declaration was checked against a Postgres source at load time.
async fn storage_pool(state: &SharedState) -> Option<deadpool_postgres::Pool> {
    let source = state.storage.attachments().next()?.source.clone();
    match state.source_pool(&source).await {
        Some(pool) => Some(pool),
        None => state.default_pool().await,
    }
}

/// `POST /v1/files/complete/{id}` — the S3 backend's completion call.
///
/// The bytes never pass through the engine, so the engine asks the provider
/// what it actually received. Recording a size the client merely promised
/// would let a claim succeed for an object that was never uploaded.
async fn complete_inner(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<Uuid>,
    Query(capability): Query<Capability>,
) -> Response {
    if !state
        .storage
        .verify_token(PURPOSE_COMPLETE, id, capability.exp, &capability.sig)
    {
        return (StatusCode::FORBIDDEN, "invalid or expired completion URL").into_response();
    }
    let Some((upload, spec)) = load_upload(&state, id).await else {
        return (StatusCode::NOT_FOUND, "unknown upload").into_response();
    };
    if upload.state != "pending" {
        return (StatusCode::CONFLICT, "upload is already claimed").into_response();
    }
    let Some(Backend::S3(s3)) = state.storage.backend(&upload.backend) else {
        return (StatusCode::NOT_FOUND, "unknown upload").into_response();
    };

    let url = s3.presign("HEAD", &upload.object_key, Utc::now(), 60);
    let response = match state
        .http
        .head(&url)
        .timeout(Duration::from_secs(20))
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => {
            return (StatusCode::BAD_GATEWAY, "storage did not answer").into_response();
        }
    };
    if !response.status().is_success() {
        return (StatusCode::NOT_FOUND, "the object was not uploaded").into_response();
    }
    // Read the header rather than the decoded body: a HEAD response has no
    // body, so the body's length is always zero and would fail every check.
    let Some(size) = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
    else {
        return (StatusCode::BAD_GATEWAY, "storage reported no size").into_response();
    };
    if size <= 0 || size as u64 > spec.max_bytes || size as u64 > upload.declared_bytes {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "the stored object exceeds the accepted size",
        )
            .into_response();
    }

    // Move the verified bytes out from under the presigned URL that wrote them.
    // That URL stays valid for its whole lifetime and cannot be revoked, so
    // leaving the file at the address it writes to would let a caller replace
    // content the claim had already certified. Whatever a late PUT stores at the
    // staging key afterwards is an orphan nothing references.
    let final_key = match state.storage.attachment(&upload.attachment) {
        Some(spec) => spec.object_key(upload.id),
        None => return (StatusCode::NOT_FOUND, "unknown upload").into_response(),
    };
    let staging_key = upload.object_key.clone();
    let (copy_url, copy_headers) = s3.presign_copy(&staging_key, &final_key, Utc::now(), 60);
    let mut request = state.http.put(&copy_url).timeout(Duration::from_secs(20));
    for (name, value) in &copy_headers {
        request = request.header(name, value);
    }
    match request.send().await {
        Ok(response) if response.status().is_success() => {}
        Ok(response) => {
            tracing::warn!(target: "donat::files", %id, status = %response.status(),
                "storage refused to finalize an upload");
            return (
                StatusCode::BAD_GATEWAY,
                "storage refused to finalize the upload",
            )
                .into_response();
        }
        Err(_) => {
            return (StatusCode::BAD_GATEWAY, "storage did not answer").into_response();
        }
    }
    // Point the row at the final key *before* dropping the staging object. The
    // other order loses a file to a crash in between: the row would still name
    // an object that no longer exists, and the copy nothing points at is an
    // orphan the collector cannot see. This way the worst case is one stale
    // staging object, and repeating the call still works.
    if let Err(message) = finalize_upload(&state, upload.id, &final_key, size).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, message).into_response();
    }

    let delete_url = s3.presign("DELETE", &staging_key, Utc::now(), 60);
    if let Err(error) = state
        .http
        .delete(&delete_url)
        .timeout(Duration::from_secs(20))
        .send()
        .await
    {
        tracing::warn!(target: "donat::files", %id, error = %error,
            "cannot delete a staged object");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Record the verified size and the address the bytes now live at.
async fn finalize_upload(
    state: &SharedState,
    id: Uuid,
    object_key: &str,
    size: i64,
) -> Result<(), &'static str> {
    let pool = storage_pool(state).await.ok_or("no storage source")?;
    let client = pool.get().await.map_err(|_| "no storage source")?;
    let updated = client
        .execute(
            "UPDATE donat.file_uploads SET byte_size = $2, object_key = $3 \
             WHERE id = $1 AND state = 'pending'",
            &[&id, &size, &object_key],
        )
        .await
        .map_err(|_| "cannot record the upload")?;
    if updated == 0 {
        return Err("the upload was claimed before it was finalized");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The collector
// ---------------------------------------------------------------------------

/// Start the collector, or nothing when no table declares an attachment.
pub fn spawn(state: SharedState) {
    if state.storage.is_empty() {
        return;
    }
    tokio::spawn(async move { run(state).await });
}

async fn run(state: SharedState) {
    // Days by declaration, seconds by environment: the interval is a
    // deployment's policy, but a test needs to watch one pass finish.
    let interval = match std::env::var("DONAT_FILES_GC_INTERVAL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(seconds) => Duration::from_secs(seconds.max(1)),
        None => Duration::from_secs(state.storage.gc().every_days.max(1) as u64 * 86_400),
    };
    tracing::info!(
        interval_seconds = interval.as_secs(),
        "file collector started"
    );
    loop {
        if let Err(error) = collect(&state).await {
            tracing::warn!(error = %error, "file collection failed");
        }
        tokio::time::sleep(interval).await;
    }
}

/// One collection pass. Returns how many objects it reclaimed.
pub async fn collect(state: &SharedState) -> anyhow::Result<usize> {
    let gc = state.storage.gc().clone();
    let pool = storage_pool(state)
        .await
        .ok_or_else(|| anyhow::anyhow!("no storage source"))?;
    let client = pool.get().await?;
    let mut reclaimed = 0;

    // 1. Uploads nobody claimed. Their expiry has already passed; the TTL is
    //    the extra grace on top of it.
    //
    //    Batched, but drained: a pass that stopped after one batch would delete
    //    at most 500 objects per interval, which a caller can out-create by
    //    orders of magnitude. SKIP LOCKED keeps two engine replicas from
    //    fighting over the same rows.
    loop {
        let pending = client
            .query(
                "SELECT id, backend, object_key FROM donat.file_uploads \
                 WHERE state = 'pending' AND expires_at < now() - ($1 || ' days')::interval \
                 ORDER BY expires_at LIMIT 500 FOR UPDATE SKIP LOCKED",
                &[&gc.pending_ttl_days.to_string()],
            )
            .await?;
        if pending.is_empty() {
            break;
        }
        let batch = pending.len();
        for row in pending {
            reclaimed += reclaim(state, &client, row).await? as usize;
        }
        if batch < 500 {
            break;
        }
    }

    // 2. Objects no row references any more. The declaration knows which
    //    column points at them, so this is one NOT EXISTS per declaration
    //    rather than a scan of the whole database.
    for attachment in state.storage.attachments() {
        let sql = format!(
            "SELECT f.id, f.backend, f.object_key FROM donat.file_uploads f \
             WHERE f.attachment = $1 AND f.state = 'claimed' \
               AND f.claimed_at < now() - ($2 || ' days')::interval \
               AND NOT EXISTS (SELECT 1 FROM {schema}.{table} t WHERE t.{column} = f.id) \
             ORDER BY f.claimed_at LIMIT 500 FOR UPDATE SKIP LOCKED",
            schema = quote_ident(&attachment.schema),
            table = quote_ident(&attachment.table),
            column = quote_ident(&attachment.column),
        );
        loop {
            let orphans = client
                .query(&sql, &[&attachment.key, &gc.orphan_grace_days.to_string()])
                .await?;
            if orphans.is_empty() {
                break;
            }
            let batch = orphans.len();
            for row in orphans {
                reclaimed += reclaim(state, &client, row).await? as usize;
            }
            if batch < 500 {
                break;
            }
        }
    }
    Ok(reclaimed)
}

/// Delete one object, then its row.
///
/// The order is deliberate: a storage failure leaves the row in place, so the
/// next pass retries instead of forgetting an object nobody can reach any more.
/// A missing object counts as deleted — that is the state the row was claiming.
async fn reclaim(
    state: &SharedState,
    client: &deadpool_postgres::Client,
    row: tokio_postgres::Row,
) -> anyhow::Result<bool> {
    let id: Uuid = row.get("id");
    let backend: String = row.get("backend");
    let object_key: String = row.get("object_key");

    let removed = match state.storage.backend(&backend) {
        Some(Backend::S3(s3)) => {
            let url = s3.presign("DELETE", &object_key, Utc::now(), 60);
            match state
                .http
                .delete(&url)
                .timeout(Duration::from_secs(20))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() || response.status() == 404 => true,
                Ok(response) => {
                    tracing::warn!(%id, status = %response.status(), "storage refused a delete");
                    false
                }
                Err(error) => {
                    tracing::warn!(%id, error = %error, "cannot reach storage to delete");
                    false
                }
            }
        }
        // The backend is gone from metadata: the row would otherwise be
        // retried forever, and the engine cannot reach the object either way.
        None => {
            tracing::warn!(%id, %backend, "collecting a row whose backend is no longer declared");
            true
        }
    };
    if !removed {
        return Ok(false);
    }
    client
        .execute("DELETE FROM donat.file_uploads WHERE id = $1", &[&id])
        .await?;
    Ok(true)
}

fn quote_ident(ident: &str) -> String {
    donat_sqlgen::quote_ident(ident)
}
