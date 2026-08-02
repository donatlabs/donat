//! Native conformance for file attachments (spec 008).
//!
//! There are no upstream fixtures for this — it is a Donat-owned surface — so
//! these tests follow the round trip a real client makes, end to end through
//! the spawned binary: ask for a URL, upload the bytes straight to the object
//! store, report the upload finished, store the id in an ordinary insert, read
//! the column back, and follow the download URL.
//!
//! The store is a real MinIO from `docker-compose.conformance.yml`, not a
//! double. The engine has no local-disk backend, so "the upload worked" has to
//! mean an actual S3 implementation accepted our presigned URL — and "a forged
//! URL is refused" has to mean it rejected one.
//!
//! Collection windows are whole days by declaration, so the tests backdate rows
//! in the catalog instead of waiting — the same trick `cron_triggers.rs` uses
//! for a due occurrence.

use std::time::{Duration, Instant};

use donat_conformance::Suite;
use donat_conformance::object_store;
use serde_json::{Value as Json, json};

const SIGNING_SECRET: &str = "donat-conformance-file-signing-secret";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-- pretend this is an image --";

fn client(db_url: &str) -> postgres::Client {
    postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db")
}

/// The owning table: an ordinary table with an ordinary uuid column.
fn create_pet_table(db_url: &str) {
    client(db_url)
        .batch_execute(
            "CREATE TABLE public.pet (\
               id       uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
               name     text NOT NULL, \
               owner_id text NOT NULL, \
               photo    uuid)",
        )
        .expect("create pet table");
}

fn permissions(s: &donat_conformance::Running) {
    s.add_select_permission_document(
        "pet",
        "customer",
        json!({ "columns": "*", "filter": { "owner_id": { "_eq": "X-Donat-User-Id" } } }),
    );
    s.add_insert_permission_document(
        "pet",
        "customer",
        json!({
            "columns": ["name", "owner_id", "photo"],
            "check": { "owner_id": { "_eq": "X-Donat-User-Id" } }
        }),
    );
}

fn headers(user: &str) -> Vec<(String, String)> {
    vec![
        ("X-Donat-Role".to_string(), "customer".to_string()),
        ("X-Donat-User-Id".to_string(), user.to_string()),
    ]
}

fn graphql(s: &donat_conformance::Running, user: &str, query: &str) -> Json {
    let (status, body) = s.post("/v1/graphql", &json!({ "query": query }), &headers(user));
    assert_eq!(status, 200, "unexpected status for {query}: {body}");
    body
}

/// Ask for an upload URL and return the whole envelope.
fn request_upload(s: &donat_conformance::Running, user: &str, size: usize) -> Json {
    let body = graphql(
        s,
        user,
        &format!(
            "mutation {{ donat_request_file_upload(attachment: public_pet_photo, \
             file_name: \"cat.png\", media_type: \"image/png\", size: {size}) \
             {{ id url method headers {{ name value }} complete_url expires_at }} }}"
        ),
    );
    assert!(
        body.get("errors").is_none(),
        "upload request failed: {body}"
    );
    body["data"]["donat_request_file_upload"].clone()
}

/// Put the bytes where the engine said, with the headers it said to send.
fn put_bytes(s: &donat_conformance::Running, upload: &Json, bytes: &[u8]) -> u16 {
    let sent: Vec<(String, String)> = upload["headers"]
        .as_array()
        .expect("headers")
        .iter()
        .map(|h| {
            (
                h["name"].as_str().unwrap().to_string(),
                h["value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = sent
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let (status, body) = s.request_url("PUT", upload["url"].as_str().unwrap(), bytes, &borrowed);
    if status >= 400 {
        // The store's own words, so a refusal is diagnosable from the test log
        // instead of being just a number.
        eprintln!(
            "store refused the PUT ({status}): {}",
            String::from_utf8_lossy(&body)
        );
    }
    status
}

/// The whole flow up to a stored, claimed file. Returns the upload id.
fn store_file(s: &donat_conformance::Running, user: &str, name: &str) -> String {
    let upload = request_upload(s, user, PNG.len());
    let id = upload["id"].as_str().unwrap().to_string();
    assert_eq!(
        put_bytes(s, &upload, PNG),
        200,
        "the store accepted the PUT"
    );
    let (status, _) = s.request_url("POST", upload["complete_url"].as_str().unwrap(), b"", &[]);
    assert_eq!(status, 204, "completion accepted");
    let inserted = graphql(
        s,
        user,
        &format!(
            "mutation {{ insert_pet_one(object: {{name: \"{name}\", owner_id: \"{user}\", \
             photo: \"{id}\"}}) {{ id }} }}"
        ),
    );
    assert!(
        inserted.get("errors").is_none(),
        "insert failed: {inserted}"
    );
    id
}

fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("timed out waiting for: {what}");
}

fn upload_row_count(db_url: &str, state: &str) -> i64 {
    client(db_url)
        .query_one(
            "SELECT count(*) FROM donat.file_uploads WHERE state = $1",
            &[&state],
        )
        .expect("count uploads")
        .get(0)
}

/// A suite whose `pet.photo` is stored in the conformance object store.
///
/// `extra_storage` merges into `storage.yaml`; `public` publishes the column,
/// which also moves it to the anonymously readable bucket — the separation a
/// deployment would make anyway.
fn suite_with(tag: &str, extra_storage: Json, public: bool) -> donat_conformance::Running {
    object_store::require_running();
    let suite = Suite::new(&format!("files_{tag}"))
        .with_migrations()
        .env("DONAT_FILE_SIGNING_SECRET", SIGNING_SECRET)
        .env("DONAT_S3_KEY", object_store::ACCESS_KEY_ID)
        .env("DONAT_S3_SECRET", object_store::SECRET_ACCESS_KEY)
        .env("DONAT_FILES_GC_INTERVAL_SECONDS", "1")
        .start();
    create_pet_table(suite.db_url());
    permissions(&suite);
    suite.add_attachment(
        "pet",
        json!({
            "column": "photo",
            "backend": "media",
            "max_bytes": 1024,
            "media_types": ["image/png"],
            "public": public
        }),
    );
    let bucket = if public {
        object_store::PUBLIC_BUCKET
    } else {
        object_store::BUCKET
    };
    let mut backend = json!({
        "name": "media",
        "kind": "s3",
        "bucket": bucket,
        "region": object_store::REGION,
        "endpoint": object_store::endpoint(),
        "path_style": true,
        "access_key_id": { "value_from_env": "DONAT_S3_KEY" },
        "secret_access_key": { "value_from_env": "DONAT_S3_SECRET" }
    });
    if public {
        backend["public_base_url"] = json!(object_store::public_base_url());
    }
    let mut storage = json!({
        "backends": [backend],
        "signing": { "secret": { "value_from_env": "DONAT_FILE_SIGNING_SECRET" } }
    });
    for (key, value) in extra_storage.as_object().cloned().unwrap_or_default() {
        storage[key] = value;
    }
    suite.set_storage(serde_json::from_value(storage).expect("storage metadata"));
    suite
}

fn suite(tag: &str) -> donat_conformance::Running {
    suite_with(tag, json!({}), false)
}

// ---------------------------------------------------------------------------
// The round trip
// ---------------------------------------------------------------------------

#[test]
fn a_file_travels_from_presigned_url_to_column_to_download() {
    let s = suite("round_trip");

    let upload = request_upload(&s, "u-1", PNG.len());
    let id = upload["id"].as_str().unwrap().to_string();
    let url = upload["url"].as_str().unwrap().to_string();
    assert_eq!(upload["method"], json!("PUT"));
    assert!(
        url.starts_with(&object_store::endpoint()) && url.contains("X-Amz-Signature="),
        "a presigned store URL was expected, got {url}"
    );
    // The URL binds the size and the type, so the store enforces them.
    assert!(
        url.contains("content-length") && url.contains("content-type"),
        "the URL must bind both headers: {url}"
    );

    assert_eq!(
        put_bytes(&s, &upload, PNG),
        200,
        "the store accepted the PUT"
    );

    // The bytes never passed through the engine, so it asks the store.
    let (status, _) = s.request_url("POST", upload["complete_url"].as_str().unwrap(), b"", &[]);
    assert_eq!(status, 204, "completion should be accepted");

    let inserted = graphql(
        &s,
        "u-1",
        &format!(
            "mutation {{ insert_pet_one(object: {{name: \"Kit\", owner_id: \"u-1\", \
             photo: \"{id}\"}}) {{ id }} }}"
        ),
    );
    assert!(
        inserted.get("errors").is_none(),
        "insert failed: {inserted}"
    );
    assert_eq!(upload_row_count(s.db_url(), "claimed"), 1);

    let read = graphql(
        &s,
        "u-1",
        "query { pet { name photo { id file_name media_type size url } } }",
    );
    let photo = &read["data"]["pet"][0]["photo"];
    assert_eq!(photo["id"], json!(id));
    assert_eq!(photo["file_name"], json!("cat.png"));
    assert_eq!(photo["media_type"], json!("image/png"));
    assert_eq!(
        photo["size"],
        json!(PNG.len()),
        "the verified size was stored"
    );

    // The download URL is signed by SQL, not by Rust. Following it against a
    // real implementation is what proves the two signers agree.
    let download = photo["url"].as_str().expect("download url");
    let (status, body) = s.request_url("GET", download, b"", &[]);
    assert_eq!(status, 200, "the SQL-signed download URL was accepted");
    assert_eq!(body, PNG, "the downloaded bytes are the uploaded bytes");
}

#[test]
fn a_forged_or_stripped_download_url_is_refused_by_the_store() {
    let s = suite("forged_url");
    store_file(&s, "u-1", "Kit");
    let read = graphql(&s, "u-1", "query { pet { photo { url } } }");
    let download = read["data"]["pet"][0]["photo"]["url"].as_str().unwrap();

    let tampered = download.replace("X-Amz-Signature=", "X-Amz-Signature=0");
    let (status, _) = s.request_url("GET", &tampered, b"", &[]);
    assert_eq!(status, 403, "a tampered signature must be refused");

    // And without a signature at all: the bucket is not anonymously readable.
    let unsigned = download.split('?').next().unwrap().to_string();
    let (status, _) = s.request_url("GET", &unsigned, b"", &[]);
    assert!(
        status == 403 || status == 404,
        "an unsigned read must not succeed, got {status}"
    );
}

#[test]
fn a_body_of_another_length_is_refused_by_the_store() {
    // The signature binds content-length, so the size the caller declared is
    // enforced by the store rather than by a promise the engine records.
    let s = suite("wrong_length");
    let upload = request_upload(&s, "u-1", PNG.len());
    let status = put_bytes(&s, &upload, b"short");
    assert_eq!(status, 403, "a body of another length must be refused");
}

#[test]
fn a_declared_size_over_the_limit_is_refused_before_any_url_exists() {
    let s = suite("declared_too_big");
    let body = graphql(
        &s,
        "u-1",
        "mutation { donat_request_file_upload(attachment: public_pet_photo, \
         file_name: \"cat.png\", media_type: \"image/png\", size: 99999) { id url } }",
    );
    assert_eq!(
        body["errors"][0]["extensions"]["code"],
        json!("validation-failed"),
        "{body}"
    );
    assert_eq!(upload_row_count(s.db_url(), "pending"), 0);
}

#[test]
fn a_media_type_outside_the_allow_list_is_refused() {
    let s = suite("media_type");
    let body = graphql(
        &s,
        "u-1",
        "mutation { donat_request_file_upload(attachment: public_pet_photo, \
         file_name: \"notes.pdf\", media_type: \"application/pdf\", size: 10) { id } }",
    );
    assert_eq!(
        body["errors"][0]["extensions"]["code"],
        json!("validation-failed"),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// Claiming
// ---------------------------------------------------------------------------

#[test]
fn an_upload_cannot_be_claimed_twice_or_by_another_session() {
    let s = suite("claim");
    let upload = request_upload(&s, "u-1", PNG.len());
    let id = upload["id"].as_str().unwrap().to_string();
    assert_eq!(put_bytes(&s, &upload, PNG), 200);
    s.request_url("POST", upload["complete_url"].as_str().unwrap(), b"", &[]);

    let stolen = graphql(
        &s,
        "u-2",
        &format!(
            "mutation {{ insert_pet_one(object: {{name: \"Thief\", owner_id: \"u-2\", \
             photo: \"{id}\"}}) {{ id }} }}"
        ),
    );
    assert_eq!(
        stolen["errors"][0]["extensions"]["code"],
        json!("validation-failed"),
        "another session must not claim it: {stolen}"
    );
    assert_eq!(upload_row_count(s.db_url(), "claimed"), 0);

    let first = graphql(
        &s,
        "u-1",
        &format!(
            "mutation {{ insert_pet_one(object: {{name: \"Kit\", owner_id: \"u-1\", \
             photo: \"{id}\"}}) {{ id }} }}"
        ),
    );
    assert!(first.get("errors").is_none(), "{first}");

    let second = graphql(
        &s,
        "u-1",
        &format!(
            "mutation {{ insert_pet_one(object: {{name: \"Kit 2\", owner_id: \"u-1\", \
             photo: \"{id}\"}}) {{ id }} }}"
        ),
    );
    assert_eq!(
        second["errors"][0]["extensions"]["code"],
        json!("validation-failed"),
        "an upload backs exactly one column value: {second}"
    );
    let pets: i64 = client(s.db_url())
        .query_one("SELECT count(*) FROM public.pet", &[])
        .unwrap()
        .get(0);
    assert_eq!(pets, 1, "the refused insert must not have landed");
}

#[test]
fn an_upload_nobody_stored_bytes_for_cannot_be_completed_or_claimed() {
    let s = suite("missing_bytes");
    let upload = request_upload(&s, "u-1", PNG.len());
    let id = upload["id"].as_str().unwrap().to_string();

    let (status, _) = s.request_url("POST", upload["complete_url"].as_str().unwrap(), b"", &[]);
    assert_eq!(status, 404, "the store has no such object");

    let body = graphql(
        &s,
        "u-1",
        &format!(
            "mutation {{ insert_pet_one(object: {{name: \"Kit\", owner_id: \"u-1\", \
             photo: \"{id}\"}}) {{ id }} }}"
        ),
    );
    assert_eq!(
        body["errors"][0]["extensions"]["code"],
        json!("validation-failed"),
        "a column must never point at bytes that were never stored: {body}"
    );
}

#[test]
fn a_column_written_around_the_claim_gate_reads_as_empty() {
    // A migration or a repair script can put any uuid in the column. None of
    // them may produce a signed URL: the projection only joins an upload that
    // was claimed for this very column.
    let s = suite("forged_column");
    let upload = request_upload(&s, "u-1", PNG.len());
    let id = upload["id"].as_str().unwrap().to_string();
    assert_eq!(put_bytes(&s, &upload, PNG), 200);
    s.request_url("POST", upload["complete_url"].as_str().unwrap(), b"", &[]);

    client(s.db_url())
        .execute(
            &format!(
                "INSERT INTO public.pet (name, owner_id, photo) \
                 VALUES ('Forged', 'u-1', '{id}'::uuid)"
            ),
            &[],
        )
        .expect("write the column directly");

    let read = graphql(&s, "u-1", "query { pet { name photo { id url } } }");
    assert_eq!(
        read["data"]["pet"][0]["photo"],
        Json::Null,
        "an unclaimed upload must not be reachable: {read}"
    );
}

#[test]
fn bytes_are_moved_out_from_under_the_upload_url() {
    // A presigned PUT cannot be revoked and stays valid for its whole lifetime.
    // Completion copies the verified bytes to their final key, so a later write
    // to the upload URL cannot replace what the claim certified.
    let s = suite("finalize");
    let upload = request_upload(&s, "u-1", PNG.len());
    let id = upload["id"].as_str().unwrap().to_string();
    assert_eq!(put_bytes(&s, &upload, PNG), 200);
    s.request_url("POST", upload["complete_url"].as_str().unwrap(), b"", &[]);
    graphql(
        &s,
        "u-1",
        &format!(
            "mutation {{ insert_pet_one(object: {{name: \"Kit\", owner_id: \"u-1\", \
             photo: \"{id}\"}}) {{ id }} }}"
        ),
    );

    // The upload URL is still signed and still accepted by the store — and now
    // writes somewhere nothing points at.
    let poison = vec![b'X'; PNG.len()];
    assert_eq!(
        put_bytes(&s, &upload, &poison),
        200,
        "the store still honours the presigned URL"
    );

    let read = graphql(&s, "u-1", "query { pet { photo { url } } }");
    let download = read["data"]["pet"][0]["photo"]["url"].as_str().unwrap();
    let (status, body) = s.request_url("GET", download, b"", &[]);
    assert_eq!(status, 200);
    assert_eq!(body, PNG, "the claimed bytes must be the ones served");
}

// ---------------------------------------------------------------------------
// Budgets, stability, public files, CORS
// ---------------------------------------------------------------------------

#[test]
fn a_session_cannot_hold_more_unclaimed_uploads_than_its_budget() {
    let s = suite_with(
        "quota",
        json!({ "limits": { "pending_uploads_per_session": 2 } }),
        false,
    );
    request_upload(&s, "u-1", 10);
    request_upload(&s, "u-1", 10);

    let refused = graphql(
        &s,
        "u-1",
        "mutation { donat_request_file_upload(attachment: public_pet_photo, \
         file_name: \"cat.png\", media_type: \"image/png\", size: 10) { id } }",
    );
    assert_eq!(
        refused["errors"][0]["extensions"]["code"],
        json!("validation-failed"),
        "a third pending upload must be refused: {refused}"
    );
    assert_eq!(upload_row_count(s.db_url(), "pending"), 2);

    let other = request_upload(&s, "u-2", 10);
    assert!(
        other["id"].is_string(),
        "another session has its own budget"
    );
}

#[test]
fn the_same_query_returns_the_same_url_so_a_subscription_stays_quiet() {
    // A subscription re-runs its query on a timer and pushes whenever the
    // response differs. A URL that carried the current second would make every
    // poll look like a change.
    let s = suite("stable_url");
    store_file(&s, "u-1", "Kit");

    let query = "query { pet { photo { url } } }";
    let first = graphql(&s, "u-1", query);
    std::thread::sleep(Duration::from_millis(1200));
    let second = graphql(&s, "u-1", query);
    assert_eq!(
        first["data"], second["data"],
        "the response must not change while the data does not"
    );
}

#[test]
fn a_public_file_is_served_from_a_stable_unsigned_url() {
    // The bytes never change and nothing about the URL is a capability, so it
    // can be cached forever and fronted by a CDN.
    let s = suite_with("public", json!({}), true);
    let id = store_file(&s, "u-1", "Kit");

    let read = graphql(&s, "u-1", "query { pet { photo { url } } }");
    let url = read["data"]["pet"][0]["photo"]["url"].as_str().unwrap();
    assert_eq!(
        url,
        format!("{}/public.pet.photo/{id}", object_store::public_base_url()),
        "a public URL carries no signature and no expiry"
    );

    let (status, body) = s.request_url("GET", url, b"", &[]);
    assert_eq!(
        status, 200,
        "a public object is readable without a signature"
    );
    assert_eq!(body, PNG);
}

#[test]
fn a_browser_can_preflight_the_completion_call_from_a_declared_origin() {
    let s = suite_with(
        "cors",
        json!({ "cors": { "allow_origins": ["https://app.example.com"] } }),
        false,
    );
    let upload = request_upload(&s, "u-1", PNG.len());
    let complete = upload["complete_url"].as_str().unwrap().to_string();

    let (status, headers) = s.request_url_with_headers(
        "OPTIONS",
        &complete,
        b"",
        &[
            ("Origin", "https://app.example.com"),
            ("Access-Control-Request-Method", "POST"),
        ],
    );
    assert_eq!(status, 204);
    let allow = headers
        .iter()
        .find(|(k, _)| k == "access-control-allow-origin")
        .map(|(_, v)| v.clone());
    assert_eq!(allow.as_deref(), Some("https://app.example.com"));

    let (_, headers) = s.request_url_with_headers(
        "OPTIONS",
        &complete,
        b"",
        &[("Origin", "https://evil.example.com")],
    );
    assert!(
        !headers
            .iter()
            .any(|(k, _)| k == "access-control-allow-origin"),
        "an undeclared origin must not be allowed"
    );
}

// ---------------------------------------------------------------------------
// The collector
// ---------------------------------------------------------------------------

#[test]
fn the_collector_reclaims_abandoned_and_orphaned_objects_and_spares_live_ones() {
    let s = suite("collect");

    let live = store_file(&s, "u-1", "Kit");
    let orphan = store_file(&s, "u-1", "Gone");
    client(s.db_url())
        .execute("DELETE FROM public.pet WHERE name = 'Gone'", &[])
        .expect("delete the owning row");

    // An abandoned upload: a URL nobody ever used.
    let abandoned = request_upload(&s, "u-1", PNG.len());
    let abandoned_id = abandoned["id"].as_str().unwrap().to_string();

    // Backdate past the declared windows instead of waiting a day for them.
    client(s.db_url())
        .batch_execute(
            "UPDATE donat.file_uploads SET claimed_at = now() - interval '3 days' \
                WHERE state = 'claimed'; \
             UPDATE donat.file_uploads SET expires_at = now() - interval '3 days' \
                WHERE state = 'pending'",
        )
        .expect("backdate uploads");

    wait_until(
        || {
            let gone: i64 = client(s.db_url())
                .query_one(
                    "SELECT count(*) FROM donat.file_uploads WHERE id::text = ANY($1)",
                    &[&vec![orphan.clone(), abandoned_id.clone()]],
                )
                .unwrap()
                .get(0);
            gone == 0
        },
        Duration::from_secs(20),
        "the orphan and the abandoned upload to be collected",
    );

    let remaining: i64 = client(s.db_url())
        .query_one("SELECT count(*) FROM donat.file_uploads", &[])
        .unwrap()
        .get(0);
    assert_eq!(remaining, 1, "only the referenced upload survives");

    // The live file is untouched, and its object is still in the store.
    let read = graphql(&s, "u-1", "query { pet { photo { id url } } }");
    assert_eq!(read["data"]["pet"][0]["photo"]["id"], json!(live));
    let download = read["data"]["pet"][0]["photo"]["url"].as_str().unwrap();
    let (status, body) = s.request_url("GET", download, b"", &[]);
    assert_eq!(status, 200, "the surviving object is still readable");
    assert_eq!(body, PNG);
}

#[test]
fn a_collected_object_is_gone_from_the_store() {
    let s = suite("collect_store");
    store_file(&s, "u-1", "Gone");
    let read = graphql(&s, "u-1", "query { pet { photo { url } } }");
    let download = read["data"]["pet"][0]["photo"]["url"]
        .as_str()
        .unwrap()
        .to_string();

    client(s.db_url())
        .execute("DELETE FROM public.pet", &[])
        .expect("delete the owning row");
    client(s.db_url())
        .execute(
            "UPDATE donat.file_uploads SET claimed_at = now() - interval '3 days'",
            &[],
        )
        .expect("backdate the claim");

    wait_until(
        || {
            client(s.db_url())
                .query_one("SELECT count(*) FROM donat.file_uploads", &[])
                .unwrap()
                .get::<_, i64>(0)
                == 0
        },
        Duration::from_secs(20),
        "the orphaned upload to be collected",
    );

    // The download URL was signed before collection and is still valid — the
    // object behind it is what is gone.
    let (status, _) = s.request_url("GET", &download, b"", &[]);
    assert_eq!(status, 404, "the object was deleted from the store");
}

// ---------------------------------------------------------------------------
// Absence
// ---------------------------------------------------------------------------

#[test]
fn a_deployment_without_attachments_has_no_upload_surface() {
    let s = Suite::new("files_absent").with_migrations().start();
    create_pet_table(s.db_url());
    permissions(&s);

    let body = graphql(
        &s,
        "u-1",
        "mutation { donat_request_file_upload(attachment: public_pet_photo, \
         file_name: \"cat.png\", media_type: \"image/png\", size: 10) { id } }",
    );
    assert!(
        body["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not found"),
        "the root field must not exist: {body}"
    );

    let (status, _) = s.request_url(
        "POST",
        "/v1/files/complete/00000000-0000-0000-0000-000000000000?exp=1&sig=x",
        b"",
        &[],
    );
    assert_eq!(status, 404, "no /v1/files surface without attachments");
}
