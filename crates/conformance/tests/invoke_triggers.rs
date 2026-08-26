//! Native conformance for `invoke` targets on cron and table event triggers
//! (spec 010): a trigger names an existing action or command, the engine
//! builds a classic session from the triggering row, binds arguments from it
//! and runs the same resolver a GraphQL call would — in the binary, with no
//! receiver service and no request back to `/v1/graphql`.
//!
//! The recording cron stub plays the action handler (`{{CRON_WEBHOOK_BASE}}`),
//! so a test can assert the exact `{action, input, session_variables}` the
//! engine sent. Timing follows `cron_triggers.rs`: a yearly schedule keeps
//! materialization out of the window and a past-due occurrence is seeded.

use std::time::{Duration, Instant};

use donat_conformance::Suite;
use donat_conformance::cron_webhook::{CronWebhook, Received};
use donat_metadata::QualifiedTable;
use serde_json::{Value as Json, json};

fn workspace() -> QualifiedTable {
    QualifiedTable::Qualified {
        schema: "public".into(),
        name: "workspace".into(),
    }
}

/// The stand every case starts from: a `workspace` table holding a
/// write-only token per owner, an `issue` table the `then` command writes,
/// the `linear_issues` action pointed at the recording stub and the
/// `ingest_issue` command — everything granted to the classic `user` role,
/// with rows bounded by `X-Donat-User-Id`.
fn stand(name: &str, handler_path: &str) -> donat_conformance::Running {
    let s = Suite::new(name)
        .with_cron_webhook()
        .env("DONAT_EVENTS_POLL_SECONDS", "1")
        .request_header("X-Donat-Role", "user")
        .start();
    let mut c = postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect suite db");
    c.batch_execute(
        "CREATE TABLE public.workspace (\
            id bigint PRIMARY KEY,\
            owner text NOT NULL,\
            linear_token text,\
            team_ids jsonb\
         );\
         CREATE TABLE public.issue (\
            id bigserial PRIMARY KEY,\
            owner text NOT NULL,\
            identifier text NOT NULL,\
            title text,\
            UNIQUE (owner, identifier)\
         )",
    )
    .expect("create stand tables");

    s.edit_metadata(|md| {
        let source = md
            .sources
            .iter_mut()
            .find(|s| s.name == "default")
            .expect("default source");
        source.tables.push(
            serde_json::from_value(json!({
                "table": { "schema": "public", "name": "workspace" },
                "select_permissions": [{
                    "role": "user",
                    // The token is deliberately not selectable: it is written
                    // by the owner and read by nobody over GraphQL.
                    "permission": { "columns": ["id", "owner", "team_ids"],
                                    "filter": { "owner": { "_eq": "X-Donat-User-Id" } } }
                }],
                "update_permissions": [{
                    "role": "user",
                    "permission": { "columns": ["linear_token", "team_ids"],
                                    "filter": { "owner": { "_eq": "X-Donat-User-Id" } },
                                    "check": {} }
                }]
            }))
            .expect("workspace table entry"),
        );
        source.tables.push(
            serde_json::from_value(json!({
                "table": { "schema": "public", "name": "issue" },
                "select_permissions": [{
                    "role": "user",
                    "permission": { "columns": "*",
                                    "filter": { "owner": { "_eq": "X-Donat-User-Id" } } }
                }],
                "insert_permissions": [{
                    "role": "user",
                    "permission": { "columns": "*", "check": {} }
                }]
            }))
            .expect("issue table entry"),
        );
        md.custom_types = serde_json::from_value(json!({
            "objects": [{
                "name": "Issue",
                "fields": [
                    { "name": "identifier", "type": "String!" },
                    { "name": "title", "type": "String" }
                ]
            }]
        }))
        .expect("custom types");
        md.actions.push(
            serde_json::from_value(json!({
                "name": "linear_issues",
                "definition": {
                    "kind": "synchronous",
                    "arguments": [
                        { "name": "token", "type": "String!" },
                        { "name": "teamId", "type": "String" }
                    ],
                    "output_type": "[Issue]",
                    "handler": format!("{{{{CRON_WEBHOOK_BASE}}}}{handler_path}")
                },
                "permissions": [{ "role": "user" }]
            }))
            .expect("action entry"),
        );
        md.commands.push(
            serde_json::from_value(json!({
                "name": "ingest_issue",
                "source": "default",
                "permissions": [{ "role": "user" }],
                "arguments": [
                    { "name": "identifier", "type": "String!" },
                    { "name": "title", "type": "String" }
                ],
                "steps": [{
                    "name": "issue",
                    "insert": {
                        "table": { "schema": "public", "name": "issue" },
                        "object": {
                            "owner": { "session_variable": "x-donat-user-id" },
                            "identifier": { "arg": "identifier" },
                            "title": { "arg": "title" }
                        },
                        "returning": ["id"]
                    }
                }],
                "result": { "issue_id": { "step": "issue", "column": "id" } }
            }))
            .expect("command entry"),
        );
    });
    s
}

fn seed_workspace(db_url: &str, id: i64, owner: &str, token: Option<&str>, teams: Json) {
    let mut c = postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db");
    c.execute(
        "INSERT INTO public.workspace (id, owner, linear_token, team_ids) VALUES ($1, $2, $3, $4)",
        &[&id, &owner, &token, &teams],
    )
    .expect("seed workspace");
}

fn seed_past_due(db_url: &str, trigger: &str) {
    let mut c = postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db");
    c.execute(
        "INSERT INTO donat.cron_events (trigger_name, scheduled_time) \
         VALUES ($1, now() - interval '30 seconds')",
        &[&trigger],
    )
    .expect("seed cron event");
}

fn scalar<T: for<'a> postgres::types::FromSql<'a>>(db_url: &str, sql: &str, trigger: &str) -> T {
    let mut c = postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db");
    c.query_one(sql, &[&trigger]).expect("query").get(0)
}

fn invocations(db_url: &str, trigger: &str, status: &str) -> i64 {
    let mut c = postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db");
    c.query_one(
        "SELECT count(*) FROM donat.trigger_invocations WHERE trigger_name = $1 AND status = $2",
        &[&trigger, &status],
    )
    .expect("invocation count")
    .get(0)
}

fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

fn received_for(cw: &CronWebhook, path: &str) -> Vec<Received> {
    cw.received()
        .into_iter()
        .filter(|r| r.path == path)
        .collect()
}

/// The cron trigger under test: a yearly schedule, an `invoke` target.
fn cron_invoke(name: &str, invoke: Json, retry: Json) -> donat_metadata::CronTrigger {
    serde_json::from_value(json!({
        "name": name,
        "schedule": "0 0 1 1 *",
        "retry_conf": retry,
        "invoke": invoke,
    }))
    .expect("valid cron trigger")
}

fn linear_invoke(foreach: Json, then: Option<Json>) -> Json {
    let mut invoke = json!({
        "action": "linear_issues",
        "session": {
            "role": "user",
            "vars": {
                "x-donat-user-id": { "column": "owner" },
                "x-donat-tenant-id": { "column": "owner" }
            }
        },
        "foreach": foreach,
        "arguments": {
            "token": { "column": "linear_token" },
            "teamId": { "literal": "T1" }
        }
    });
    if let Some(then) = then {
        invoke["then"] = then;
    }
    invoke
}

fn workspaces_with_a_token() -> Json {
    json!({
        "table": { "schema": "public", "name": "workspace" },
        "where": { "linear_token": { "_is_null": false } }
    })
}

fn ingest_then() -> Json {
    json!({
        "foreach": "$",
        "command": "ingest_issue",
        "arguments": {
            "identifier": { "item": "identifier" },
            "title": { "item": "title" }
        }
    })
}

fn gql(s: &donat_conformance::Running, user: &str, query: &str) -> Json {
    let (_, body) = s.post(
        "/v1/graphql",
        &json!({ "query": query }),
        &[("X-Donat-User-Id".to_string(), user.to_string())],
    );
    body
}

#[test]
fn cron_invoke_action_basic() {
    let s = stand("invoke_basic", "/list");
    seed_workspace(s.db_url(), 1, "alice", Some("tok-alice"), Json::Null);
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), None),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "pull");

    let cw = s.cron_webhook().clone();
    wait_until(
        || cw.count_for("/list") >= 1,
        Duration::from_secs(15),
        "the action handler to be called",
    );
    let got = received_for(&cw, "/list");
    assert_eq!(got.len(), 1, "one work item, one call");
    let body = &got[0].body;
    assert_eq!(body["action"]["name"], json!("linear_issues"));
    assert_eq!(
        body["input"],
        json!({ "token": "tok-alice", "teamId": "T1" }),
        "arguments bound from the row and the literal"
    );
    let vars = &body["session_variables"];
    assert_eq!(vars["x-donat-role"], json!("user"));
    assert_eq!(vars["x-hasura-role"], json!("user"));
    assert_eq!(vars["x-donat-user-id"], json!("alice"));
    assert_eq!(vars["x-donat-tenant-id"], json!("alice"));

    wait_until(
        || invocations(s.db_url(), "pull", "delivered") == 1,
        Duration::from_secs(5),
        "the invocation marked delivered",
    );
    assert_eq!(
        scalar::<i64>(
            s.db_url(),
            "SELECT count(*) FROM donat.cron_events WHERE trigger_name = $1 AND status = 'delivered'",
            "pull"
        ),
        1,
        "the occurrence itself is delivered once expanded"
    );
}

#[test]
fn cron_invoke_foreach_two_rows() {
    let s = stand("invoke_foreach", "/list");
    seed_workspace(s.db_url(), 1, "alice", Some("tok-alice"), Json::Null);
    seed_workspace(s.db_url(), 2, "bob", Some("tok-bob"), Json::Null);
    // No token: not a work item.
    seed_workspace(s.db_url(), 3, "carol", None, Json::Null);
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), None),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "pull");

    let cw = s.cron_webhook().clone();
    wait_until(
        || invocations(s.db_url(), "pull", "delivered") == 2,
        Duration::from_secs(15),
        "two invocations delivered",
    );
    let got = received_for(&cw, "/list");
    assert_eq!(got.len(), 2);
    let mut tenants: Vec<String> = got
        .iter()
        .map(|r| {
            r.body["session_variables"]["x-donat-tenant-id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    tenants.sort();
    assert_eq!(tenants, vec!["alice", "bob"]);
    let mut tokens: Vec<String> = got
        .iter()
        .map(|r| r.body["input"]["token"].as_str().unwrap().to_string())
        .collect();
    tokens.sort();
    assert_eq!(tokens, vec!["tok-alice", "tok-bob"]);
    assert_eq!(
        scalar::<i64>(
            s.db_url(),
            "SELECT count(*) FROM donat.trigger_invocations WHERE trigger_name = $1",
            "pull"
        ),
        2,
        "the row without a token was never a work item"
    );
}

#[test]
fn cron_invoke_unbindable_token() {
    let s = stand("invoke_write_only", "/list");
    seed_workspace(s.db_url(), 1, "alice", Some("tok-alice"), Json::Null);
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), None),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();

    // Over GraphQL the owner cannot read the token back.
    let body = gql(&s, "alice", "query { workspace { id linear_token } }");
    assert!(
        body["errors"][0]["message"]
            .as_str()
            .unwrap_or("")
            .contains("linear_token"),
        "the token has no select field; got {body}"
    );
    assert_eq!(
        body["errors"][0]["extensions"]["code"],
        json!("validation-failed")
    );

    seed_past_due(s.db_url(), "pull");
    let cw = s.cron_webhook().clone();
    wait_until(
        || cw.count_for("/list") >= 1,
        Duration::from_secs(15),
        "the action handler to be called",
    );
    // The background bind does not go through select permissions.
    assert_eq!(
        received_for(&cw, "/list")[0].body["input"]["token"],
        json!("tok-alice")
    );
}

#[test]
fn cron_invoke_redacts_token() {
    let s = stand("invoke_redact", "/list");
    let sentinel = "SENTINEL-8f1c2a9d-token";
    seed_workspace(s.db_url(), 1, "alice", Some(sentinel), Json::Null);
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), Some(ingest_then())),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "pull");
    wait_until(
        || invocations(s.db_url(), "pull", "delivered") == 1,
        Duration::from_secs(15),
        "the invocation delivered",
    );
    let mut c = postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect suite db");
    for table in [
        "donat.trigger_invocations",
        "donat.cron_event_invocation_logs",
        "donat.cron_events",
    ] {
        let leaked: i64 = c
            .query_one(
                &format!(
                    "SELECT count(*) FROM {table} t WHERE to_jsonb(t)::text LIKE '%SENTINEL%'"
                ),
                &[],
            )
            .expect("leak query")
            .get(0);
        assert_eq!(
            leaked, 0,
            "the bound token must not be persisted in {table}"
        );
    }
    // The bound argument is journaled, redacted: the operator can see *that*
    // a token was sent, never which.
    let input: Json = c
        .query_one(
            "SELECT input FROM donat.trigger_invocations WHERE trigger_name = 'pull'",
            &[],
        )
        .expect("input")
        .get(0);
    assert_eq!(input, json!({ "token": "***", "teamId": "T1" }));
}

#[test]
fn cron_invoke_redacts_token_echoed_by_the_handler() {
    // The provider rejects the credential and, as providers do, repeats it
    // in the message. What is journaled still does not carry it.
    let s = stand("invoke_redact_echo", "/echo-fail");
    let sentinel = "SENTINEL-3b7e1c-echoed";
    seed_workspace(s.db_url(), 1, "alice", Some(sentinel), Json::Null);
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), None),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "pull");
    wait_until(
        || invocations(s.db_url(), "pull", "error") == 1,
        Duration::from_secs(15),
        "the invocation to fail",
    );
    let mut c = postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect suite db");
    let leaked: i64 = c
        .query_one(
            "SELECT count(*) FROM donat.trigger_invocations t WHERE to_jsonb(t)::text LIKE '%SENTINEL%'",
            &[],
        )
        .expect("leak query")
        .get(0);
    assert_eq!(leaked, 0, "the echoed token must not be journaled");
    let error: Json = c
        .query_one(
            "SELECT error FROM donat.trigger_invocations WHERE trigger_name = 'pull'",
            &[],
        )
        .expect("error")
        .get(0);
    assert_eq!(error["message"], json!("invalid token ***"), "got {error}");
}

#[test]
fn event_invoke_action_on_update() {
    let s = stand("invoke_event", "/list");
    seed_workspace(s.db_url(), 1, "alice", Some("tok-old"), Json::Null);
    s.add_event_trigger(
        &workspace(),
        serde_json::from_value(json!({
            "name": "on_linear_token",
            "definition": { "enable_manual": false,
                            "update": { "columns": ["linear_token"] } },
            "retry_conf": { "num_retries": 0 },
            "invoke": {
                "action": "linear_issues",
                "session": {
                    "role": "user",
                    "vars": { "x-donat-user-id": { "column": "owner" } }
                },
                "arguments": {
                    "token": { "column": "linear_token" },
                    "teamId": { "literal": "T1" }
                }
            }
        }))
        .expect("event trigger"),
    );
    let _ = s.base_url();
    let mut c = postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect suite db");
    c.execute(
        "UPDATE public.workspace SET linear_token = 'tok-new' WHERE id = 1",
        &[],
    )
    .expect("rotate token");

    let cw = s.cron_webhook().clone();
    wait_until(
        || cw.count_for("/list") >= 1,
        Duration::from_secs(15),
        "the action handler to be called",
    );
    let got = received_for(&cw, "/list");
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].body["input"]["token"],
        json!("tok-new"),
        "NEW values"
    );
    assert_eq!(
        got[0].body["session_variables"]["x-donat-user-id"],
        json!("alice")
    );
    wait_until(
        || invocations(s.db_url(), "on_linear_token", "delivered") == 1,
        Duration::from_secs(5),
        "the invocation delivered",
    );
    let parent: i64 = c
        .query_one(
            "SELECT count(*) FROM donat.event_log WHERE trigger_name = 'on_linear_token' \
             AND status = 'delivered'",
            &[],
        )
        .expect("event status")
        .get(0);
    assert_eq!(parent, 1);
}

#[test]
fn cron_invoke_then_command() {
    let s = stand("invoke_then", "/list");
    seed_workspace(s.db_url(), 1, "alice", Some("tok-alice"), Json::Null);
    seed_workspace(s.db_url(), 2, "bob", None, Json::Null);
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), Some(ingest_then())),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "pull");
    wait_until(
        || invocations(s.db_url(), "pull", "delivered") == 1,
        Duration::from_secs(15),
        "the invocation delivered",
    );
    // The command ran as alice: her rows, and only hers, exist.
    let alice = gql(
        &s,
        "alice",
        "query { issue(order_by: {identifier: asc}) { identifier title owner } }",
    );
    assert_eq!(
        alice["data"]["issue"],
        json!([
            { "identifier": "A-1", "title": "one", "owner": "alice" },
            { "identifier": "A-2", "title": "two", "owner": "alice" }
        ]),
        "got {alice}"
    );
    let bob = gql(&s, "bob", "query { issue { identifier } }");
    assert_eq!(bob["data"]["issue"], json!([]), "got {bob}");
}

#[test]
fn cron_invoke_then_retry() {
    let s = stand("invoke_retry", "/fail-then-list");
    seed_workspace(s.db_url(), 1, "alice", Some("tok-alice"), Json::Null);
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), Some(ingest_then())),
        json!({ "num_retries": 2, "retry_interval_seconds": 1 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "pull");
    let cw = s.cron_webhook().clone();
    wait_until(
        || invocations(s.db_url(), "pull", "delivered") == 1,
        Duration::from_secs(20),
        "the invocation delivered after a retry",
    );
    assert_eq!(cw.count_for("/fail-then-list"), 2, "500, then 200");
    assert_eq!(
        scalar::<i32>(
            s.db_url(),
            "SELECT tries FROM donat.trigger_invocations WHERE trigger_name = $1",
            "pull"
        ),
        2
    );
    let rows = gql(&s, "alice", "query { issue { identifier } }");
    assert_eq!(
        rows["data"]["issue"].as_array().map(Vec::len),
        Some(2),
        "one business row per item; got {rows}"
    );
    assert_eq!(
        scalar::<i64>(
            s.db_url(),
            "SELECT count(*) FROM donat.cron_events WHERE trigger_name = $1 \
             AND status = 'delivered'",
            "pull"
        ),
        1,
        "the parent occurrence is not retried; its child is"
    );
}

#[test]
fn cron_invoke_action_get_transform() {
    // The handler is the API's base; the transform names the path, the
    // method and the (absent) body — the way an action fronts an API that
    // was never written for this engine.
    let s = stand("invoke_get", "");
    s.edit_metadata(|md| {
        let action = md
            .actions
            .iter_mut()
            .find(|a| a.name == "linear_issues")
            .unwrap();
        action.definition.request_transform = Some(
            serde_json::from_value(json!({
                "version": 2,
                "method": "GET",
                "url": "{{$base_url}}/get-list",
                "body": { "action": "remove" }
            }))
            .expect("request transform"),
        );
    });
    seed_workspace(s.db_url(), 1, "alice", Some("tok-alice"), Json::Null);
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), None),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "pull");
    let cw = s.cron_webhook().clone();
    wait_until(
        || cw.count_for("/get-list") >= 1,
        Duration::from_secs(15),
        "the transformed request",
    );
    let got = received_for(&cw, "/get-list");
    assert_eq!(got[0].method, "GET");
    assert_eq!(got[0].body, Json::Null, "body removed");
    wait_until(
        || invocations(s.db_url(), "pull", "delivered") == 1,
        Duration::from_secs(5),
        "the invocation delivered",
    );
}

#[test]
fn cron_invoke_expand_limit() {
    let s = Suite::new("invoke_expand_limit")
        .with_cron_webhook()
        .env("DONAT_CRON_INVOKE_EXPAND_LIMIT", "2")
        .request_header("X-Donat-Role", "user")
        .start();
    // The same stand, on a suite that carries the cap.
    let mut c = postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect suite db");
    c.batch_execute(
        "CREATE TABLE public.workspace (id bigint PRIMARY KEY, owner text NOT NULL, \
         linear_token text, team_ids jsonb)",
    )
    .expect("create table");
    s.edit_metadata(|md| {
        let source = md
            .sources
            .iter_mut()
            .find(|s| s.name == "default")
            .unwrap();
        source.tables.push(
            serde_json::from_value(
                json!({ "table": { "schema": "public", "name": "workspace" } }),
            )
            .unwrap(),
        );
        md.custom_types = serde_json::from_value(json!({
            "objects": [{ "name": "Issue", "fields": [{ "name": "identifier", "type": "String!" }] }]
        }))
        .unwrap();
        md.actions.push(
            serde_json::from_value(json!({
                "name": "linear_issues",
                "definition": {
                    "arguments": [
                        { "name": "token", "type": "String!" },
                        { "name": "teamId", "type": "String" }
                    ],
                    "output_type": "[Issue]",
                    "handler": "{{CRON_WEBHOOK_BASE}}/list"
                },
                "permissions": [{ "role": "user" }]
            }))
            .unwrap(),
        );
    });
    for (id, owner) in [(1, "a"), (2, "b"), (3, "c")] {
        seed_workspace(s.db_url(), id, owner, Some("tok"), Json::Null);
    }
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), None),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "pull");

    // One tick delivers at most two; the third waits for the next poll and
    // needs no second occurrence.
    wait_until(
        || {
            invocations(s.db_url(), "pull", "delivered") == 2
                && invocations(s.db_url(), "pull", "scheduled") == 1
        },
        Duration::from_secs(15),
        "two delivered and one still scheduled",
    );
    wait_until(
        || invocations(s.db_url(), "pull", "delivered") == 3,
        Duration::from_secs(15),
        "the third delivered on a later poll",
    );
    assert_eq!(
        scalar::<i64>(
            s.db_url(),
            "SELECT count(*) FROM donat.cron_events WHERE trigger_name = $1 \
             AND status = 'delivered'",
            "pull"
        ),
        1,
        "one occurrence carried all three"
    );
    assert_eq!(s.cron_webhook().count_for("/list"), 3);
}

#[test]
fn cron_invoke_row_gone() {
    let s = stand("invoke_row_gone", "/list");
    s.add_cron_trigger(cron_invoke(
        "pull",
        linear_invoke(workspaces_with_a_token(), None),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    // An occurrence already expanded into a work item whose row no longer
    // exists — what a delete between expand and deliver leaves behind.
    let mut c = postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect suite db");
    c.batch_execute(
        "WITH parent AS (\
            INSERT INTO donat.cron_events (trigger_name, scheduled_time, status) \
            VALUES ('pull', now() - interval '30 seconds', 'delivered') RETURNING id\
         ) INSERT INTO donat.trigger_invocations (kind, parent_id, trigger_name, row_key) \
           SELECT 'cron', id, 'pull', '{\"id\": 404}'::jsonb FROM parent",
    )
    .expect("seed orphan work item");
    wait_until(
        || invocations(s.db_url(), "pull", "dead") == 1,
        Duration::from_secs(15),
        "the work item marked dead",
    );
    assert_eq!(
        s.cron_webhook().count_for("/list"),
        0,
        "nothing was called out"
    );
}

#[test]
fn cron_invoke_unnest() {
    let s = stand("invoke_unnest", "/list");
    seed_workspace(
        s.db_url(),
        1,
        "alice",
        Some("tok-alice"),
        json!(["T1", "T2"]),
    );
    seed_workspace(s.db_url(), 2, "bob", Some("tok-bob"), json!([]));
    seed_workspace(s.db_url(), 3, "carol", Some("tok-carol"), Json::Null);
    s.add_cron_trigger(cron_invoke(
        "pull",
        json!({
            "action": "linear_issues",
            "session": { "role": "user", "vars": { "x-donat-user-id": { "column": "owner" } } },
            "foreach": {
                "table": { "schema": "public", "name": "workspace" },
                "where": { "linear_token": { "_is_null": false } },
                "unnest": [{ "column": "team_ids", "as": "team_id" }],
                "key": ["id", "team_id"]
            },
            "arguments": {
                "token": { "column": "linear_token" },
                "teamId": { "column": "team_id" }
            }
        }),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "pull");
    wait_until(
        || invocations(s.db_url(), "pull", "delivered") == 2,
        Duration::from_secs(15),
        "one invocation per team",
    );
    let cw = s.cron_webhook().clone();
    let mut teams: Vec<(String, String)> = received_for(&cw, "/list")
        .iter()
        .map(|r| {
            (
                r.body["input"]["token"].as_str().unwrap().to_string(),
                r.body["input"]["teamId"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    teams.sort();
    assert_eq!(
        teams,
        vec![
            ("tok-alice".into(), "T1".into()),
            ("tok-alice".into(), "T2".into())
        ],
        "an empty or null array is zero work items"
    );
}

#[test]
fn cron_invoke_command_direct() {
    let s = stand("invoke_command", "/list");
    seed_workspace(s.db_url(), 1, "alice", Some("tok-alice"), Json::Null);
    seed_workspace(s.db_url(), 2, "bob", Some("tok-bob"), Json::Null);
    // No action at all: the schedule starts the command as each owner.
    s.add_cron_trigger(cron_invoke(
        "nightly",
        json!({
            "command": "ingest_issue",
            "session": { "role": "user", "vars": { "x-donat-user-id": { "column": "owner" } } },
            "foreach": { "table": { "schema": "public", "name": "workspace" } },
            "arguments": {
                "identifier": { "literal": "NIGHTLY" },
                "title": { "column": "owner" }
            }
        }),
        json!({ "num_retries": 0 }),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "nightly");
    wait_until(
        || invocations(s.db_url(), "nightly", "delivered") == 2,
        Duration::from_secs(15),
        "both commands delivered",
    );
    let alice = gql(&s, "alice", "query { issue { identifier title owner } }");
    assert_eq!(
        alice["data"]["issue"],
        json!([{ "identifier": "NIGHTLY", "title": "alice", "owner": "alice" }]),
        "got {alice}"
    );
    let bob = gql(&s, "bob", "query { issue { identifier title owner } }");
    assert_eq!(
        bob["data"]["issue"],
        json!([{ "identifier": "NIGHTLY", "title": "bob", "owner": "bob" }]),
        "got {bob}"
    );
    assert_eq!(s.cron_webhook().count_for("/list"), 0);
}
