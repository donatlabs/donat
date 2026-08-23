//! Native conformance for the notification module.
//!
//! There are no upstream fixtures for this — it is a Donat-owned surface, and
//! more than that it is a *module*: the thing under test is the YAML in
//! `modules/notifications`, adopted here exactly the way an application adopts
//! it. Nothing in these tests declares a permission, a command or a process of
//! its own, so a module whose filter stops naming the session variable, or
//! whose flow stops compiling, fails here rather than in whichever project
//! shipped it.
//!
//! What the module promises: a recipient reads their own feed and nobody
//! else's; the unread count is an aggregate rather than a full fetch; marking a
//! notification read is the only thing they may change about it; a preference
//! belongs to whoever wrote it regardless of what the request said; `notify`
//! reaches the feed through a durable Process; an opted-out channel is logged
//! rather than silently dropped; and an unknown recipient is refused before
//! anything is written.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use donat_conformance::provider_stub::{self, ProviderStub, ScriptedResponse};
use donat_conformance::{Running, Suite, apply_sql_migration_dir};
use serde_json::{Value as Json, json};

/// The two paths the module's shipped mail contract posts to.
const MAIL_PATH: &str = "/v1/email/messages";
const DIGEST_PATH: &str = "/v1/email/digests";

/// Two recipients, fixed so a failure names the same row twice.
const ALICE: &str = "11111111-1111-4111-8111-111111111111";
const BOB: &str = "22222222-2222-4222-8222-222222222222";
const NOBODY: &str = "33333333-3333-4333-8333-333333333333";

fn module_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../modules/notifications")
        .canonicalize()
        .expect("the notification module is checked in")
}

fn client(db_url: &str) -> postgres::Client {
    postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db")
}

fn headers(role: &str, user: &str) -> Vec<(String, String)> {
    vec![
        ("X-Donat-Role".to_string(), role.to_string()),
        ("X-Donat-User-Id".to_string(), user.to_string()),
    ]
}

fn graphql(s: &Running, role: &str, user: &str, query: &str) -> Json {
    let (status, body) = s.post(
        "/v1/graphql",
        &json!({ "query": query }),
        &headers(role, user),
    );
    assert_eq!(status, 200, "unexpected status for {query}: {body}");
    body
}

/// A recipient's own read; must succeed.
fn read(s: &Running, user: &str, query: &str) -> Json {
    let body = graphql(s, "notification_user", user, query);
    assert!(body.get("errors").is_none(), "unexpected errors: {body}");
    body["data"].clone()
}

/// The binding an adopting application supplies: its own users table, and the
/// view the module ships the shape of. `create or replace view` is the contract
/// — a binding whose columns did not match would fail right here.
fn bind_recipients(db_url: &str) {
    client(db_url)
        .batch_execute(
            "create table public.app_user (
               id             uuid primary key,
               email          text not null,
               email_verified boolean not null default false,
               locale         text not null default 'en',
               timezone       text not null default 'UTC'
             );
             create or replace view notification.recipient as
             select u.id::text, u.email, u.email_verified, u.locale, u.timezone
             from public.app_user u;",
        )
        .expect("bind the recipient view to the application's users");
}

fn add_user(db: &mut postgres::Client, id: &str, email: &str) {
    db.execute(
        "insert into public.app_user (id, email) values ($1::text::uuid, $2)",
        &[&id, &email],
    )
    .expect("add an application user");
}

/// Seed a notification directly, for the tests that are about reading a feed
/// rather than about how it got there.
fn seed(db: &mut postgres::Client, recipient: &str, title: &str) -> String {
    let row = db
        .query_one(
            "with d as (
               insert into notification.dispatch (workflow, recipient_id, title, body)
               values ('order_shipped', $1::text::uuid, $2, 'body')
               returning id
             )
             insert into notification.inbox (dispatch_id, recipient_id, title, body)
             select d.id, $1::text::uuid, $2, 'body'
             from d
             returning id::text",
            &[&recipient, &title],
        )
        .expect("seed an inbox row");
    row.get(0)
}

/// Stand the module up against a fresh suite database: its DDL, the
/// application's recipient binding, then its metadata — with a real local HTTP
/// server standing in for the mail relay the module's contract describes.
fn stand(name: &str) -> (Running, ProviderStub) {
    let mail = provider_stub::spawn();
    mail.set_default(
        MAIL_PATH,
        ScriptedResponse::ok(json!({ "message_id": "relay-1", "status": "accepted" })),
    );
    mail.set_default(
        DIGEST_PATH,
        ScriptedResponse::ok(json!({ "message_id": "digest-1", "status": "accepted" })),
    );
    let s = Suite::new(name)
        .with_migrations()
        .env("NOTIFICATION_MAIL_BASE_URL", mail.base_url())
        .env("NOTIFICATION_MAIL_TOKEN", "Bearer conformance-mail-token")
        .start();
    apply_sql_migration_dir(s.db_url(), &module_root().join("migrations"))
        .expect("apply the module's migrations");
    bind_recipients(s.db_url());
    s.adopt_metadata_module(&module_root().join("metadata"));
    (s, mail)
}

/// Trigger a notification the way an application does: one mutation, as the
/// role allowed to send.
fn notify(s: &Running, recipient: &str, title: &str, request_id: &str) -> Json {
    graphql(
        s,
        "notification_sender",
        ALICE,
        &format!(
            "mutation {{ notify(workflow: \"order_shipped\", \
             recipient_id: \"{recipient}\", title: \"{title}\", body: \"it shipped\", \
             url: \"/orders/1\", request_id: \"{request_id}\") \
             {{ dispatch_id workflow }} }}"
        ),
    )
}

/// Durable work is waited for, never slept through.
fn wait_for<T>(what: &str, mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The delivery log row for a channel, once the Process has written it.
fn await_delivery(db: &mut postgres::Client, dispatch_id: &str, channel: &str) -> String {
    wait_for(&format!("the {channel} delivery of {dispatch_id}"), || {
        db.query_opt(
            "select status from notification.delivery
             where dispatch_id = $1::text::uuid and channel = $2",
            &[&dispatch_id, &channel],
        )
        .expect("read the delivery log")
        .map(|row| row.get::<_, String>(0))
    })
}

#[test]
fn a_recipient_reads_their_own_feed_and_nobody_elses() {
    let (s, _mail) = stand("notifications_feed");
    let mut db = client(s.db_url());
    seed(&mut db, ALICE, "alice one");
    seed(&mut db, ALICE, "alice two");
    seed(&mut db, BOB, "bob one");

    let alice = read(
        &s,
        ALICE,
        "query { notification_inbox(order_by: {title: asc}) { title } }",
    );
    assert_eq!(
        alice["notification_inbox"],
        json!([{ "title": "alice one" }, { "title": "alice two" }]),
        "Alice must see her two notifications and only those"
    );

    let bob = read(&s, BOB, "query { notification_inbox { title } }");
    assert_eq!(
        bob["notification_inbox"],
        json!([{ "title": "bob one" }]),
        "Bob's feed is his own"
    );
}

#[test]
fn the_unread_count_is_an_aggregate_and_marking_read_moves_it() {
    let (s, _mail) = stand("notifications_unread");
    let mut db = client(s.db_url());
    let first = seed(&mut db, ALICE, "alice one");
    seed(&mut db, ALICE, "alice two");
    seed(&mut db, BOB, "bob one");

    let unread = "query { notification_inbox_aggregate(where: {read_at: {_is_null: true}}) \
                  { aggregate { count } } }";
    let before = read(&s, ALICE, unread);
    assert_eq!(
        before["notification_inbox_aggregate"]["aggregate"]["count"],
        json!(2),
        "the count is the recipient's own unread rows"
    );

    let marked = read(
        &s,
        ALICE,
        &format!(
            "mutation {{ update_notification_inbox(where: {{id: {{_eq: \"{first}\"}}}}, \
             _set: {{read_at: \"2026-08-20T10:00:00+00:00\"}}) {{ affected_rows }} }}"
        ),
    );
    assert_eq!(
        marked["update_notification_inbox"]["affected_rows"],
        json!(1)
    );

    let after = read(&s, ALICE, unread);
    assert_eq!(
        after["notification_inbox_aggregate"]["aggregate"]["count"],
        json!(1),
        "marking one read leaves one unread"
    );
}

#[test]
fn a_recipient_cannot_mark_someone_elses_notification_read() {
    let (s, _mail) = stand("notifications_isolation");
    let mut db = client(s.db_url());
    let bobs = seed(&mut db, BOB, "bob one");

    let attempt = read(
        &s,
        ALICE,
        &format!(
            "mutation {{ update_notification_inbox(where: {{id: {{_eq: \"{bobs}\"}}}}, \
             _set: {{read_at: \"2026-08-20T10:00:00+00:00\"}}) {{ affected_rows }} }}"
        ),
    );
    assert_eq!(
        attempt["update_notification_inbox"]["affected_rows"],
        json!(0),
        "the row filter is what refuses this, so it is a no-op rather than an error"
    );

    let row = db
        .query_one(
            "select read_at is null from notification.inbox where id = $1::text::uuid",
            &[&bobs],
        )
        .expect("read Bob's row back");
    let still_unread: bool = row.get(0);
    assert!(still_unread, "Bob's notification is untouched");
}

#[test]
fn a_preference_belongs_to_whoever_wrote_it() {
    let (s, _mail) = stand("notifications_preference");

    let inserted = read(
        &s,
        ALICE,
        "mutation { insert_notification_preference(objects: \
         [{workflow: \"order_shipped\", channel: \"email\", enabled: false}]) \
         { affected_rows } }",
    );
    assert_eq!(
        inserted["insert_notification_preference"]["affected_rows"],
        json!(1)
    );

    let mine = read(
        &s,
        ALICE,
        "query { notification_preference { workflow channel enabled } }",
    );
    assert_eq!(
        mine["notification_preference"],
        json!([{ "workflow": "order_shipped", "channel": "email", "enabled": false }])
    );

    let bobs = read(
        &s,
        BOB,
        "query { notification_preference { workflow channel enabled } }",
    );
    assert_eq!(
        bobs["notification_preference"],
        json!([]),
        "Bob has opted out of nothing, and cannot see that Alice has"
    );
}

#[test]
fn a_recipient_id_is_a_preset_and_not_an_argument() {
    let (s, _mail) = stand("notifications_preference_preset");

    // The column is not in the role's insert list, so it is not in the input
    // type at all: naming it is a validation error rather than a value the
    // preset quietly overwrites.
    let body = graphql(
        &s,
        "notification_user",
        ALICE,
        &format!(
            "mutation {{ insert_notification_preference(objects: \
             [{{recipient_id: \"{BOB}\", workflow: \"order_shipped\", \
             channel: \"email\", enabled: false}}]) {{ affected_rows }} }}"
        ),
    );
    assert!(
        body.get("errors").is_some(),
        "supplying another recipient's id must be refused: {body}"
    );
}

#[test]
fn notify_reaches_the_feed_through_the_delivery_process() {
    let (s, _mail) = stand("notifications_delivery");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let sent = notify(&s, ALICE, "your order shipped", ALICE);
    assert!(sent.get("errors").is_none(), "notify failed: {sent}");
    let dispatch_id = sent["data"]["notify"]["dispatch_id"]
        .as_str()
        .expect("notify returns the dispatch it recorded")
        .to_string();

    assert_eq!(
        await_delivery(&mut db, &dispatch_id, "in_app"),
        "sent",
        "the in-app channel records that it delivered"
    );

    let feed = read(&s, ALICE, "query { notification_inbox { title body } }");
    assert_eq!(
        feed["notification_inbox"],
        json!([{ "title": "your order shipped", "body": "it shipped" }]),
        "the recipient reads what the sender wrote"
    );
}

#[test]
fn an_opted_out_channel_is_logged_and_not_delivered() {
    let (s, _mail) = stand("notifications_opted_out");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let opted_out = read(
        &s,
        ALICE,
        "mutation { insert_notification_preference(objects: \
         [{workflow: \"order_shipped\", channel: \"in_app\", enabled: false}]) \
         { affected_rows } }",
    );
    assert_eq!(
        opted_out["insert_notification_preference"]["affected_rows"],
        json!(1)
    );

    let sent = notify(&s, ALICE, "your order shipped", ALICE);
    assert!(sent.get("errors").is_none(), "notify failed: {sent}");
    let dispatch_id = sent["data"]["notify"]["dispatch_id"]
        .as_str()
        .expect("notify returns the dispatch it recorded")
        .to_string();

    assert_eq!(
        await_delivery(&mut db, &dispatch_id, "in_app"),
        "suppressed",
        "an opt-out is a recorded outcome, not a silent drop"
    );

    let feed = read(&s, ALICE, "query { notification_inbox { title } }");
    assert_eq!(
        feed["notification_inbox"],
        json!([]),
        "nothing reaches a feed the recipient opted out of"
    );
}

#[test]
fn notifying_an_unknown_recipient_is_refused_before_anything_is_written() {
    let (s, _mail) = stand("notifications_unknown_recipient");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let sent = notify(&s, NOBODY, "into the void", ALICE);
    assert!(
        sent.get("errors").is_some(),
        "an unknown recipient must be refused: {sent}"
    );

    let dispatches: i64 = db
        .query_one("select count(*) from notification.dispatch", &[])
        .expect("count dispatches")
        .get(0);
    assert_eq!(dispatches, 0, "a refused send writes nothing");
}

#[test]
fn the_same_request_id_notifies_once() {
    let (s, _mail) = stand("notifications_dedupe");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let first = notify(&s, ALICE, "your order shipped", ALICE);
    assert!(
        first.get("errors").is_none(),
        "first notify failed: {first}"
    );
    let dispatch_id = first["data"]["notify"]["dispatch_id"]
        .as_str()
        .expect("notify returns the dispatch it recorded")
        .to_string();
    await_delivery(&mut db, &dispatch_id, "in_app");

    // The dedupe window is the command's idempotency retention: the same key
    // for the same recipient and workflow returns the first result and writes
    // nothing further.
    let second = notify(&s, ALICE, "your order shipped", ALICE);
    assert!(
        second.get("errors").is_none(),
        "a repeat is answered, not rejected: {second}"
    );
    assert_eq!(
        second["data"]["notify"]["dispatch_id"],
        json!(dispatch_id),
        "the repeat is answered with the first dispatch"
    );

    let feed = read(&s, ALICE, "query { notification_inbox { title } }");
    assert_eq!(
        feed["notification_inbox"],
        json!([{ "title": "your order shipped" }]),
        "one notification, not two"
    );
}

#[test]
fn a_notification_is_sent_to_the_mail_relay() {
    let (s, mail) = stand("notifications_email");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let sent = notify(&s, ALICE, "your order shipped", ALICE);
    assert!(sent.get("errors").is_none(), "notify failed: {sent}");
    let dispatch_id = sent["data"]["notify"]["dispatch_id"]
        .as_str()
        .expect("notify returns the dispatch it recorded")
        .to_string();

    assert_eq!(
        await_delivery(&mut db, &dispatch_id, "email"),
        "sent",
        "the email channel records that the relay accepted it"
    );

    let calls = wait_for("the relay to be called", || {
        let calls = mail.calls_for(MAIL_PATH);
        (!calls.is_empty()).then_some(calls)
    });
    assert_eq!(calls.len(), 1, "one message, not two");
    let body = &calls[0].body;
    assert_eq!(body["recipient"], json!("alice@example.test"));
    assert_eq!(body["subject"], json!("your order shipped"));
    assert_eq!(
        body["body"],
        json!("it shipped"),
        "the relay is sent the same words the feed shows"
    );
    assert!(
        calls[0].header("Idempotency-Key").is_some(),
        "the send carries the key its contract binds"
    );

    let stored: String = db
        .query_one(
            "select provider_message_id from notification.delivery
             where dispatch_id = $1::text::uuid and channel = 'email'",
            &[&dispatch_id],
        )
        .expect("read the email delivery row")
        .get(0);
    assert_eq!(
        stored, "relay-1",
        "the provider's own id is what makes a later question answerable"
    );
}

#[test]
fn an_opted_out_email_is_logged_and_never_sent() {
    let (s, mail) = stand("notifications_email_opted_out");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let opted_out = read(
        &s,
        ALICE,
        "mutation { insert_notification_preference(objects: \
         [{workflow: \"order_shipped\", channel: \"email\", enabled: false}]) \
         { affected_rows } }",
    );
    assert_eq!(
        opted_out["insert_notification_preference"]["affected_rows"],
        json!(1)
    );

    let sent = notify(&s, ALICE, "your order shipped", ALICE);
    assert!(sent.get("errors").is_none(), "notify failed: {sent}");
    let dispatch_id = sent["data"]["notify"]["dispatch_id"]
        .as_str()
        .expect("notify returns the dispatch it recorded")
        .to_string();

    assert_eq!(
        await_delivery(&mut db, &dispatch_id, "email"),
        "suppressed",
        "an opt-out is a recorded outcome, not a silent drop"
    );
    // The in-app channel was not opted out of, so the bell still rang.
    assert_eq!(await_delivery(&mut db, &dispatch_id, "in_app"), "sent");
    assert!(
        mail.calls_for(MAIL_PATH).is_empty(),
        "nothing reaches a relay the recipient opted out of"
    );
}

#[test]
fn a_refused_send_is_recorded_rather_than_retried_forever() {
    let (s, mail) = stand("notifications_email_refused");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    // 422 is mapped to `validation` by the module's error map, which is not in
    // the operation's `retry_on` set — so it is a permanent outcome and the
    // Process records it instead of hammering the relay.
    mail.set_default(MAIL_PATH, ScriptedResponse::status(422));

    let sent = notify(&s, ALICE, "your order shipped", ALICE);
    assert!(sent.get("errors").is_none(), "notify failed: {sent}");
    let dispatch_id = sent["data"]["notify"]["dispatch_id"]
        .as_str()
        .expect("notify returns the dispatch it recorded")
        .to_string();

    assert_eq!(
        await_delivery(&mut db, &dispatch_id, "email"),
        "failed",
        "a refused send is a recorded failure"
    );
    let code: String = db
        .query_one(
            "select error_code from notification.delivery
             where dispatch_id = $1::text::uuid and channel = 'email'",
            &[&dispatch_id],
        )
        .expect("read the email delivery row")
        .get(0);
    assert_eq!(code, "mail_send_failed");
    assert_eq!(
        mail.calls_for(MAIL_PATH).len(),
        1,
        "a class the operation does not retry on is not retried"
    );
}

#[test]
fn an_email_is_skipped_when_the_recipient_already_saw_it_in_the_app() {
    let (s, mail) = stand("notifications_email_skipped");
    // The deployment's edit, not the module's default: hold the email behind
    // the bell long enough for the recipient to act on it.
    s.tune_metadata(|md| {
        let table = md
            .rules
            .decision_tables
            .iter_mut()
            .find(|table| table.name == "notification_email_delay")
            .expect("the module ships the delay table");
        table.rows[0].output = json!({ "delay_seconds": 5 });
        // The table's own test case is what stops that edit from being silent,
        // so a deployment changing the row changes the expectation with it.
        table.test_cases[0].expect.output = json!({ "delay_seconds": 5 });
    });
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let sent = notify(&s, ALICE, "your order shipped", ALICE);
    assert!(sent.get("errors").is_none(), "notify failed: {sent}");
    let dispatch_id = sent["data"]["notify"]["dispatch_id"]
        .as_str()
        .expect("notify returns the dispatch it recorded")
        .to_string();

    // The bell rings first, and the recipient looks at it while the email is
    // still waiting.
    let message = wait_for("the feed row", || {
        let feed = read(&s, ALICE, "query { notification_inbox { id } }");
        feed["notification_inbox"]
            .as_array()
            .and_then(|rows| rows.first().cloned())
    });
    let id = message["id"].as_str().expect("the feed row has an id");
    read(
        &s,
        ALICE,
        &format!(
            "mutation {{ update_notification_inbox(where: {{id: {{_eq: \"{id}\"}}}}, \
             _set: {{seen_at: \"2026-08-20T10:00:00+00:00\"}}) {{ affected_rows }} }}"
        ),
    );

    assert_eq!(
        await_delivery(&mut db, &dispatch_id, "email"),
        "skipped",
        "an escalation the recipient pre-empted is recorded as skipped, \
         which is neither a failure nor an opt-out"
    );
    assert!(
        mail.calls_for(MAIL_PATH).is_empty(),
        "nothing was sent, because there was nothing left to say"
    );
}

/// Trigger a batched notification: the bell rings now, the mail waits.
fn notify_digested(s: &Running, recipient: &str, title: &str, request_id: &str) -> Json {
    graphql(
        s,
        "notification_sender",
        ALICE,
        &format!(
            "mutation {{ notify_digested(workflow: \"order_shipped\", \
             recipient_id: \"{recipient}\", title: \"{title}\", body: \"it shipped\", \
             request_id: \"{request_id}\") {{ dispatch_id }} }}"
        ),
    )
}

fn dispatch_id_of(response: &Json, field: &str) -> String {
    assert!(
        response.get("errors").is_none(),
        "trigger failed: {response}"
    );
    response["data"][field]["dispatch_id"]
        .as_str()
        .expect("the trigger returns the dispatch it recorded")
        .to_string()
}

/// A scheduler's own loop: read what is owed, sweep each group.
fn pending_groups(s: &Running) -> Vec<Json> {
    let body = graphql(
        s,
        "notification_scheduler",
        ALICE,
        "query { notification_pending_digest(order_by: {oldest: asc}) \
         { recipient_id workflow pending } }",
    );
    assert!(
        body.get("errors").is_none(),
        "reading the pending list failed: {body}"
    );
    body["data"]["notification_pending_digest"]
        .as_array()
        .expect("the pending list is a list")
        .clone()
}

fn sweep(s: &Running, group: &Json, request_id: &str) -> Json {
    let recipient = group["recipient_id"].as_str().expect("a recipient");
    let workflow = group["workflow"].as_str().expect("a workflow");
    graphql(
        s,
        "notification_scheduler",
        ALICE,
        &format!(
            "mutation {{ flush_notification_digests(request_id: \"{request_id}\", \
             recipient_id: \"{recipient}\", workflow: \"{workflow}\") {{ sweep_id }} }}"
        ),
    )
}

/// Wait for one delivery row to reach a status.
fn await_status(db: &mut postgres::Client, dispatch_id: &str, status: &str) {
    wait_for(&format!("{dispatch_id} to become {status}"), || {
        let actual: String = db
            .query_one(
                "select status from notification.delivery
                 where dispatch_id = $1::text::uuid and channel = 'email'",
                &[&dispatch_id],
            )
            .expect("read the delivery row")
            .get(0);
        (actual == status).then_some(())
    });
}

#[test]
fn a_digested_notification_rings_the_bell_now_and_defers_the_mail() {
    let (s, mail) = stand("notifications_digest_defer");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let dispatch_id = dispatch_id_of(
        &notify_digested(&s, ALICE, "your order shipped", ALICE),
        "notify_digested",
    );

    assert_eq!(
        await_delivery(&mut db, &dispatch_id, "in_app"),
        "sent",
        "the bell is not what a digest delays"
    );
    assert_eq!(
        await_delivery(&mut db, &dispatch_id, "email"),
        "deferred",
        "the mail waits for the sweep"
    );
    assert!(
        mail.calls_for(MAIL_PATH).is_empty(),
        "nothing was sent on its own"
    );

    let groups = pending_groups(&s);
    assert_eq!(
        groups.len(),
        1,
        "the scheduler sees one group owed a digest"
    );
    assert_eq!(groups[0]["pending"], json!(1));
}

#[test]
fn a_sweep_collapses_a_recipients_backlog_into_one_message() {
    let (s, mail) = stand("notifications_digest_sweep");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");
    add_user(&mut db, BOB, "bob@example.test");

    // Three for Alice, one for Bob. Four deferred emails, two messages.
    let mut alices = Vec::new();
    for request in ["aaaaaaa1", "aaaaaaa2", "aaaaaaa3"] {
        let id = format!("{request}-1111-4111-8111-111111111111");
        let dispatch = dispatch_id_of(
            &notify_digested(&s, ALICE, "your order shipped", &id),
            "notify_digested",
        );
        await_delivery(&mut db, &dispatch, "email");
        alices.push(dispatch);
    }
    let bobs = dispatch_id_of(
        &notify_digested(
            &s,
            BOB,
            "your order shipped",
            "bbbbbbb1-2222-4222-8222-222222222222",
        ),
        "notify_digested",
    );
    await_delivery(&mut db, &bobs, "email");

    // The scheduler's loop: read the list, sweep each row of it.
    let groups = pending_groups(&s);
    assert_eq!(groups.len(), 2, "one group per recipient: {groups:?}");
    for (index, group) in groups.iter().enumerate() {
        let request = format!("cccccccc-3333-4333-8333-33333333330{index}");
        let answer = sweep(&s, group, &request);
        assert!(answer.get("errors").is_none(), "the sweep failed: {answer}");
    }

    let calls = wait_for("both digests to be sent", || {
        let calls = mail.calls_for(DIGEST_PATH);
        (calls.len() >= 2).then_some(calls)
    });
    assert_eq!(
        calls.len(),
        2,
        "one message per recipient, not one per notification"
    );
    let hers = calls
        .iter()
        .find(|call| call.body["recipient"] == json!("alice@example.test"))
        .expect("Alice was sent a digest");
    assert_eq!(
        hers.body["pending"],
        json!(3),
        "the digest says how many notifications it stands for"
    );

    for dispatch in alices.iter().chain(std::iter::once(&bobs)) {
        await_status(&mut db, dispatch, "sent");
    }
    let stamped: i64 = db
        .query_one(
            "select count(*) from notification.delivery
             where channel = 'email' and status = 'sent' and provider_message_id = 'digest-1'",
            &[],
        )
        .expect("count sent rows")
        .get(0);
    assert_eq!(
        stamped, 4,
        "all four notifications are recorded as delivered"
    );
}

#[test]
fn a_backlog_larger_than_one_sweep_still_drains() {
    // The shape this module was rebuilt around. Enumeration happens in the
    // scheduler's paged read, not inside the Process, so no bound inside the
    // Process can be exceeded — a sweep handles one group however large the
    // backlog is, and however many groups there are.
    let (s, mail) = stand("notifications_digest_backlog");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    // 70 deferred notifications for one recipient: more than any fan-out bound
    // the module ever declared.
    for index in 0..70u32 {
        let request = format!("aaaa{index:04}-1111-4111-8111-111111111111");
        dispatch_id_of(
            &notify_digested(&s, ALICE, "your order shipped", &request),
            "notify_digested",
        );
    }
    wait_for("every notification to be deferred", || {
        let count: i64 = db
            .query_one(
                "select count(*) from notification.delivery
                 where channel = 'email' and status = 'deferred'",
                &[],
            )
            .expect("count deferred rows")
            .get(0);
        (count == 70).then_some(())
    });

    let groups = pending_groups(&s);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["pending"], json!(70));
    let answer = sweep(&s, &groups[0], "dddddddd-4444-4444-8444-444444444444");
    assert!(answer.get("errors").is_none(), "the sweep failed: {answer}");

    wait_for("the backlog to settle", || {
        let count: i64 = db
            .query_one(
                "select count(*) from notification.delivery
                 where channel = 'email' and status in ('deferred', 'sending')",
                &[],
            )
            .expect("count rows still in flight")
            .get(0);
        (count == 0).then_some(())
    });
    assert_eq!(
        mail.calls_for(DIGEST_PATH).len(),
        1,
        "seventy notifications are one message"
    );
    assert_eq!(pending_groups(&s).len(), 0, "nothing is owed any more");
}

#[test]
fn one_recipient_with_two_workflows_gets_two_digests() {
    let (s, mail) = stand("notifications_digest_two_workflows");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    for (workflow, request) in [
        ("order_shipped", "aaaaaaa1-1111-4111-8111-111111111111"),
        ("invoice_ready", "aaaaaaa2-1111-4111-8111-111111111111"),
    ] {
        let body = graphql(
            &s,
            "notification_sender",
            ALICE,
            &format!(
                "mutation {{ notify_digested(workflow: \"{workflow}\", \
                 recipient_id: \"{ALICE}\", title: \"t\", body: \"b\", \
                 request_id: \"{request}\") {{ dispatch_id }} }}"
            ),
        );
        let dispatch = dispatch_id_of(&body, "notify_digested");
        await_delivery(&mut db, &dispatch, "email");
    }

    let groups = pending_groups(&s);
    assert_eq!(
        groups.len(),
        2,
        "a group is a recipient *and* a workflow: {groups:?}"
    );
    for (index, group) in groups.iter().enumerate() {
        let request = format!("cccccccc-3333-4333-8333-33333333340{index}");
        let answer = sweep(&s, group, &request);
        assert!(answer.get("errors").is_none(), "the sweep failed: {answer}");
    }
    wait_for("both digests to be sent", || {
        (mail.calls_for(DIGEST_PATH).len() >= 2).then_some(())
    });
    assert_eq!(mail.calls_for(DIGEST_PATH).len(), 2);
}

#[test]
fn a_second_sweep_of_a_claimed_group_takes_nothing() {
    // Two sweeps of one group overlap only if a scheduler runs twice. The claim
    // is what makes the second one harmless: it moves rows `deferred` →
    // `sending` under its own id, and a second claim finds nothing to move.
    let (s, _mail) = stand("notifications_digest_claim");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    for request in ["aaaaaaa1", "aaaaaaa2"] {
        let id = format!("{request}-1111-4111-8111-111111111111");
        let dispatch = dispatch_id_of(&notify_digested(&s, ALICE, "one", &id), "notify_digested");
        await_delivery(&mut db, &dispatch, "email");
    }

    let claim = |claim_id: &str| {
        graphql(
            &s,
            "notification_worker",
            ALICE,
            &format!(
                "mutation {{ notification_claim_digest(recipient_id: \"{ALICE}\", \
                 workflow: \"order_shipped\", claim_id: \"{claim_id}\") \
                 {{ pending }} }}"
            ),
        )
    };

    let first = claim("11110000-0000-4000-8000-000000000001");
    assert!(
        first.get("errors").is_none(),
        "the first claim failed: {first}"
    );
    assert_eq!(
        first["data"]["notification_claim_digest"]["pending"],
        json!(2),
        "the claim takes the whole group, which is what makes it one message"
    );

    let second = claim("11110000-0000-4000-8000-000000000002");
    assert!(
        second.get("errors").is_some(),
        "a claimed group must refuse a second claim rather than send a digest \
         of zero: {second}"
    );
}

#[test]
fn a_sweep_records_only_what_it_claimed() {
    // Two sweeps can hold `sending` rows for one recipient at once, because the
    // second claimed notifications that arrived after the first's claim. A
    // record scoped by status alone would stamp the other sweep's messages.
    let (s, _mail) = stand("notifications_digest_scoped_record");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let first = dispatch_id_of(
        &notify_digested(&s, ALICE, "one", "aaaaaaa1-1111-4111-8111-111111111111"),
        "notify_digested",
    );
    await_delivery(&mut db, &first, "email");

    let claim_a = "11110000-0000-4000-8000-00000000000a";
    let claim_b = "11110000-0000-4000-8000-00000000000b";
    let claim = |claim_id: &str| {
        graphql(
            &s,
            "notification_worker",
            ALICE,
            &format!(
                "mutation {{ notification_claim_digest(recipient_id: \"{ALICE}\", \
                 workflow: \"order_shipped\", claim_id: \"{claim_id}\") \
                 {{ pending }} }}"
            ),
        )
    };
    assert!(claim(claim_a).get("errors").is_none());

    // A second notification arrives and a second sweep claims it.
    let late = dispatch_id_of(
        &notify_digested(&s, ALICE, "two", "aaaaaaa2-1111-4111-8111-111111111111"),
        "notify_digested",
    );
    await_delivery(&mut db, &late, "email");
    assert!(claim(claim_b).get("errors").is_none());

    // The first sweep records. Only its own row may move.
    let recorded = graphql(
        &s,
        "notification_worker",
        ALICE,
        &format!(
            "mutation {{ notification_record_digest_sent(claim_id: \"{claim_a}\", \
             provider_message_id: \"message-from-sweep-a\") {{ claim_id }} }}"
        ),
    );
    assert!(
        recorded.get("errors").is_none(),
        "recording failed: {recorded}"
    );

    let late_status: String = db
        .query_one(
            "select status from notification.delivery
             where dispatch_id = $1::text::uuid and channel = 'email'",
            &[&late],
        )
        .expect("read the second sweep's row")
        .get(0);
    assert_eq!(
        late_status, "sending",
        "the other sweep's row is untouched, and carries no message it was not in"
    );
    let first_status: String = db
        .query_one(
            "select status from notification.delivery
             where dispatch_id = $1::text::uuid and channel = 'email'",
            &[&first],
        )
        .expect("read the first sweep's row")
        .get(0);
    assert_eq!(first_status, "sent");
}

#[test]
fn a_failed_digest_send_gives_its_rows_back() {
    let (s, mail) = stand("notifications_digest_requeue");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");
    mail.set_default(DIGEST_PATH, ScriptedResponse::status(422));

    let dispatch = dispatch_id_of(&notify_digested(&s, ALICE, "one", ALICE), "notify_digested");
    await_delivery(&mut db, &dispatch, "email");

    let groups = pending_groups(&s);
    let answer = sweep(&s, &groups[0], "dddddddd-4444-4444-8444-444444444444");
    assert!(answer.get("errors").is_none(), "the sweep failed: {answer}");

    // Back to `deferred`, so the next sweep retries it. A refused send must not
    // strand mail in `sending`.
    await_status(&mut db, &dispatch, "deferred");
    assert_eq!(
        pending_groups(&s).len(),
        1,
        "the group is owed a digest again"
    );
}

#[test]
fn a_digest_for_a_recipient_who_lost_their_address_is_recorded() {
    let (s, mail) = stand("notifications_digest_unreachable");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");
    let dispatch = dispatch_id_of(&notify_digested(&s, ALICE, "one", ALICE), "notify_digested");
    await_delivery(&mut db, &dispatch, "email");

    // The binding stops giving this recipient an address between the deferral
    // and the sweep.
    db.batch_execute(
        "create or replace view notification.recipient as
         select u.id::text, nullif(u.email, '') as email, u.email_verified, u.locale, u.timezone
         from public.app_user u;
         update public.app_user set email = '' where id = '11111111-1111-4111-8111-111111111111';",
    )
    .expect("clear the address");

    let groups = pending_groups(&s);
    let answer = sweep(&s, &groups[0], "dddddddd-4444-4444-8444-444444444444");
    assert!(answer.get("errors").is_none(), "the sweep failed: {answer}");

    await_status(&mut db, &dispatch, "failed");
    let code: String = db
        .query_one(
            "select error_code from notification.delivery
             where dispatch_id = $1::text::uuid and channel = 'email'",
            &[&dispatch],
        )
        .expect("read the delivery row")
        .get(0);
    assert_eq!(code, "mail_no_address");
    assert!(
        mail.calls_for(DIGEST_PATH).is_empty(),
        "nothing was posted with a null recipient"
    );
}

#[test]
fn sweeping_a_group_with_nothing_pending_is_harmless() {
    let (s, mail) = stand("notifications_digest_empty");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let answer = graphql(
        &s,
        "notification_scheduler",
        ALICE,
        &format!(
            "mutation {{ flush_notification_digests(\
             request_id: \"dddddddd-4444-4444-8444-444444444444\", \
             recipient_id: \"{ALICE}\", workflow: \"order_shipped\") \
             {{ sweep_id }} }}"
        ),
    );
    assert!(answer.get("errors").is_none(), "the sweep failed: {answer}");

    wait_for("the sweep to finish", || {
        let done: i64 = db
            .query_one(
                "select count(*) from donat.process_instances
                 where process_name = 'notification_digest_sweep' and status = 'terminal'",
                &[],
            )
            .expect("count finished sweeps")
            .get(0);
        (done == 1).then_some(())
    });
    assert!(
        mail.calls_for(DIGEST_PATH).is_empty(),
        "a group with nothing pending sends nothing"
    );
}

#[test]
fn the_scheduler_reaches_the_sweep_and_nothing_else() {
    // The scheduler's token is the one that lives in a cron runner's
    // environment, so what it can reach matters. `notification_worker` commands
    // take a recipient id as a plain argument — a token holding that role could
    // read anyone's address and title through `notification_resolve_channel`.
    let (s, _mail) = stand("notifications_scheduler_role");

    let sweep_mutation = format!(
        "mutation {{ flush_notification_digests(\
         request_id: \"dddddddd-4444-4444-8444-444444444444\", \
         recipient_id: \"{ALICE}\", workflow: \"order_shipped\") {{ sweep_id }} }}"
    );
    let allowed = graphql(&s, "notification_scheduler", ALICE, &sweep_mutation);
    assert!(
        allowed.get("errors").is_none(),
        "the scheduler may sweep: {allowed}"
    );

    for attempt in [
        format!(
            "mutation {{ notification_resolve_channel(dispatch_id: \"{ALICE}\", \
             channel: \"email\") {{ email }} }}"
        ),
        format!(
            "mutation {{ notification_claim_digest(recipient_id: \"{ALICE}\", \
             workflow: \"w\", claim_id: \"{ALICE}\") {{ pending }} }}"
        ),
    ] {
        let refused = graphql(&s, "notification_scheduler", ALICE, &attempt);
        assert!(
            refused.get("errors").is_some(),
            "the scheduler must not reach a worker command: {refused}"
        );
    }

    let refused = graphql(&s, "notification_worker", ALICE, &sweep_mutation);
    assert!(
        refused.get("errors").is_some(),
        "and the worker must not reach the scheduler's entry point: {refused}"
    );
}

#[test]
fn a_recipient_with_no_address_is_recorded_rather_than_queued() {
    let (s, mail) = stand("notifications_no_address");
    let mut db = client(s.db_url());
    // A user row whose email the binding reports as absent.
    db.execute(
        "insert into public.app_user (id, email) values ($1::text::uuid, '')",
        &[&ALICE],
    )
    .expect("add a user");
    db.batch_execute(
        "create or replace view notification.recipient as
         select u.id::text, nullif(u.email, '') as email, u.email_verified, u.locale, u.timezone
         from public.app_user u;",
    )
    .expect("rebind with a nullable address");

    let dispatch_id = dispatch_id_of(&notify(&s, ALICE, "your order shipped", ALICE), "notify");

    assert_eq!(
        await_delivery(&mut db, &dispatch_id, "email"),
        "failed",
        "an unreachable recipient is a recorded outcome"
    );
    let code: String = db
        .query_one(
            "select error_code from notification.delivery
             where dispatch_id = $1::text::uuid and channel = 'email'",
            &[&dispatch_id],
        )
        .expect("read the email delivery row")
        .get(0);
    assert_eq!(code, "mail_no_address");
    assert!(mail.calls_for(MAIL_PATH).is_empty());
}

#[test]
fn the_sweep_is_reachable_over_rest_for_whatever_runs_the_schedule() {
    let (s, mail) = stand("notifications_digest_rest");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");
    let dispatch_id = dispatch_id_of(
        &notify_digested(&s, ALICE, "your order shipped", ALICE),
        "notify_digested",
    );
    await_delivery(&mut db, &dispatch_id, "email");

    let (status, body) = s.post(
        "/api/rest/notifications/digests/flush",
        &json!({
            "request_id": "dddddddd-4444-4444-8444-444444444444",
            "recipient_id": ALICE,
            "workflow": "order_shipped"
        }),
        &headers("notification_scheduler", ALICE),
    );
    assert_eq!(status, 200, "the sweep route answered {status}: {body}");
    assert!(body.get("errors").is_none(), "the sweep failed: {body}");

    wait_for("the digest to be sent", || {
        (!mail.calls_for(DIGEST_PATH).is_empty()).then_some(())
    });
    await_status(&mut db, &dispatch_id, "sent");
}

#[test]
fn the_feed_carries_the_link_the_sender_gave_it() {
    // The feed advertises `url` and a client makes the message clickable with
    // it, so the entry point has to be able to set one.
    let (s, _mail) = stand("notifications_url");
    let mut db = client(s.db_url());
    add_user(&mut db, ALICE, "alice@example.test");

    let dispatch = dispatch_id_of(&notify(&s, ALICE, "your order shipped", ALICE), "notify");
    await_delivery(&mut db, &dispatch, "in_app");

    let feed = read(&s, ALICE, "query { notification_inbox { title url } }");
    assert_eq!(
        feed["notification_inbox"],
        json!([{ "title": "your order shipped", "url": "/orders/1" }]),
        "the link reaches the feed row"
    );
}

#[test]
fn opting_out_twice_is_the_documented_mutation() {
    // `(recipient_id, workflow, channel)` is the key, so the README documents
    // opt-out as an upsert. This is that exact mutation, run twice around an
    // opt-back-in — the sequence a plain insert would break on.
    let (s, _mail) = stand("notifications_preference_upsert");

    let opt = |enabled: bool| {
        graphql(
            &s,
            "notification_user",
            ALICE,
            &format!(
                "mutation {{ insert_notification_preference(\
                 objects: [{{workflow: \"order_shipped\", channel: \"email\", \
                 enabled: {enabled}}}], \
                 on_conflict: {{constraint: preference_pkey, update_columns: [enabled]}}) \
                 {{ affected_rows }} }}"
            ),
        )
    };

    for (round, enabled) in [(1, false), (2, true), (3, false)] {
        let answer = opt(enabled);
        assert!(
            answer.get("errors").is_none(),
            "round {round} of the documented opt-out failed: {answer}"
        );
    }

    let mine = read(
        &s,
        ALICE,
        "query { notification_preference { workflow channel enabled } }",
    );
    assert_eq!(
        mine["notification_preference"],
        json!([{ "workflow": "order_shipped", "channel": "email", "enabled": false }]),
        "one row, carrying the last answer"
    );
}
