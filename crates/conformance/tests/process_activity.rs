//! Native black-box contracts for durable connector activities.
//!
//! These cases drive the real engine binary over HTTP: a caller invokes a
//! domain Command through `/v1/graphql`, the Command's committed outbox starts
//! a Process, and the Process reaches a real provider over the network. Nothing
//! here stubs the runtime — only the provider is a local HTTP server, so the
//! recorded conversation is exactly what a deployed engine would send.

use std::time::{Duration, Instant};

use donat_conformance::Suite;
use donat_conformance::provider_stub::{self, ProviderCall, ProviderStub, ScriptedResponse};
use donat_metadata::Metadata;
use postgres::NoTls;
use serde_json::{Value as Json, json};

const ORDER_ID: &str = "550e8400-e29b-41d4-a716-446655440210";
const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440211";
const AUTHORIZE_PATH: &str = "/authorizations";
const PROVIDER_TOKEN: &str = "conformance-provider-token";

fn activity_metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": {
                    "database_url": { "from_env": "DONAT_DATABASE_URL" }
                }
            },
            "tables": []
        }],
        "commands": [{
            "name": "enqueue_authorization",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "steps": [],
            "result": { "order_id": { "arg": "order_id" } },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": "command",
                "retention": "1d"
            },
            "effects": [{
                "start_process": {
                    "process": "payment_authorization",
                    "input": {
                        "order_id": { "arg": "order_id" },
                        "request_id": { "arg": "request_id" }
                    },
                    "idempotency_key": { "argument": "request_id" }
                }
            }]
        }],
        "connectors": [{
            "name": "payments",
            "module": "http",
            "config": {
                "endpoint_identity": "conformance-provider-stub-v1",
                "credential_identity": "conformance-provider-credential",
                "base_url": { "value_from_env": "DONAT_PROVIDER_STUB_BASE_URL" },
                "headers": [{
                    "name": "Authorization",
                    "value_from_env": "DONAT_PROVIDER_STUB_TOKEN"
                }]
            },
            "operations": [{
                "name": "authorize",
                "version": "1.0.0",
                "method": "POST",
                "path": AUTHORIZE_PATH,
                "input_contract": { "order_id": "uuid!" },
                "body": { "order_id": { "input": "order_id" } },
                "success_statuses": [200],
                "response": {
                    "status": {
                        "json_pointer": "/status",
                        "type": "string!",
                        "max_bytes": 64
                    },
                    "provider_reference": {
                        "json_pointer": "/provider_reference",
                        "type": "string!",
                        "max_bytes": 64
                    }
                },
                "effect": {
                    "provider_idempotent": {
                        "side_effect_steps": [{
                            "step": "request",
                            "fixed_binding": { "header": "Idempotency-Key" },
                            "scope": "conformance-authorize-v1",
                            "minimum_retention_ms": 600000,
                            "clock_safety_margin_ms": 1000,
                            "evidence": {
                                "source_record_id": "source.conformance.provider-stub.v1",
                                "fact_ids": ["fact.conformance.fixed-idempotency-key"]
                            }
                        }]
                    }
                },
                "bounds": {
                    "deadline_ms": 2000,
                    "maximum_calls": 1,
                    "maximum_pages": 1,
                    "maximum_items": 1,
                    "maximum_aggregate_request_bytes": 1024,
                    "maximum_aggregate_response_bytes": 1024,
                    "maximum_output_canonical_bytes": 1024,
                    "maximum_redirects": 0,
                    "maximum_json_depth": 4,
                    "maximum_json_nodes": 16
                },
                "error_map": {
                    "rules": [
                        { "statuses": [429], "class": "http_429", "code": "rate_limited" },
                        {
                            "statuses": [500, 502, 503, 504],
                            "class": "http_5xx",
                            "code": "provider_unavailable"
                        }
                    ],
                    "fallback": { "class": "permanent", "code": "provider_error" }
                },
                "capacity": {
                    "max_in_flight": 4,
                    "rate_limit": { "permits": 10, "per": "1s", "burst": 4 },
                    "serialize_by": { "input": "order_id" }
                },
                "timeout": "2s",
                "retry": {
                    "maximum_attempts": 3,
                    "backoff": "100ms",
                    "retry_on": ["transport", "timeout", "http_429", "http_5xx"]
                },
                "idempotency": { "header": "Idempotency-Key" },
                "redaction": { "request_headers": ["Authorization"] },
                "error_classification": { "http_5xx": [500, 502, 503, 504] }
            }]
        }],
        "processes": [{
            "name": "payment_authorization",
            "kind": "process",
            "version": 1,
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "input": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "output": [
                { "name": "status", "type": "string!" },
                { "name": "provider_reference", "type": "string!" }
            ],
            "idempotency": {
                "key": { "input": "request_id" },
                "scope": [{ "input": "order_id" }]
            },
            "start_at": "authorize",
            "states": [
                {
                    "id": "authorize",
                    "request": {
                        "connector": "payments",
                        "operation": "authorize",
                        "input": { "order_id": { "input": "order_id" } },
                        "idempotency_key": {
                            "stable": { "run": "id", "state": "authorize" }
                        },
                        "timeout": {
                            "schedule_to_start": "5s",
                            "start_to_close": "3s"
                        },
                        "retry": {
                            "retry_on": ["transport", "timeout", "http_429", "http_5xx"],
                            "max_attempts": 3,
                            "initial_interval": "100ms",
                            "max_interval": "1s",
                            "jitter": "deterministic_full"
                        },
                        "next": "authorized",
                        "on_error": {
                            "routes": [{
                                "kinds": ["retry_exhausted"],
                                "next": "provider_unavailable"
                            }],
                            "fallback": { "next": "provider_failed" }
                        }
                    }
                },
                {
                    "id": "authorized",
                    "output": {
                        "values": {
                            "status": { "state": "authorize", "field": "status" },
                            "provider_reference": {
                                "state": "authorize",
                                "field": "provider_reference"
                            }
                        }
                    }
                },
                {
                    "id": "provider_unavailable",
                    "fail": {
                        "code": "provider_retry_exhausted",
                        "message": "the provider retry budget was exhausted"
                    }
                },
                {
                    "id": "provider_failed",
                    "fail": {
                        "code": "provider_failed",
                        "message": "the provider activity failed"
                    }
                }
            ]
        }]
    }))
    .expect("durable activity conformance metadata deserializes")
}

fn authorized_response() -> ScriptedResponse {
    ScriptedResponse::ok(json!({
        "status": "authorized",
        "provider_reference": "auth_conformance_1"
    }))
}

fn start_suite(stub: &ProviderStub, name: &str) -> donat_conformance::Running {
    Suite::new(name)
        .initial_metadata(activity_metadata())
        .with_migrations()
        .env("DONAT_PROVIDER_STUB_BASE_URL", stub.base_url())
        .env("DONAT_PROVIDER_STUB_TOKEN", PROVIDER_TOKEN)
        .start()
}

fn enqueue_authorization(suite: &donat_conformance::Running) -> (u16, Json) {
    suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ enqueue_authorization(order_id: \"{ORDER_ID}\", request_id: \"{REQUEST_ID}\") {{ order_id }} }}"
            )
        }),
        &[("X-Donat-Role".to_owned(), "customer".to_owned())],
    )
}

/// The durable evidence a failing case needs: how the activity ended and which
/// state the instance stopped in.
fn activity_diagnostics(client: &mut postgres::Client) -> String {
    let jobs = client
        .query(
            "
            SELECT status, attempts, coalesce(last_error_json::text, 'null')
            FROM donat.process_activity_jobs
            WHERE source_name = 'default'
            ORDER BY id
            ",
            &[],
        )
        .expect("read the durable activity journal");
    let instance = client
        .query_opt(
            "
            SELECT current_state, coalesce(state_json::text, 'null')
            FROM donat.process_instances
            WHERE source_name = 'default'
            ",
            &[],
        )
        .expect("read the durable Process instance");
    let mut report = String::new();
    if let Some(instance) = instance {
        report.push_str(&format!(
            "instance state={} state_json={}; ",
            instance.get::<_, String>(0),
            instance.get::<_, String>(1)
        ));
    }
    for job in jobs {
        report.push_str(&format!(
            "job status={} attempts={} error={}; ",
            job.get::<_, String>(0),
            job.get::<_, i32>(1),
            job.get::<_, String>(2)
        ));
    }
    report
}

/// Wait for the instance to leave `running`, then return its terminal output.
fn await_terminal(client: &mut postgres::Client) -> Json {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let row = client
            .query_opt(
                "
                SELECT status, terminal_output_json::text
                FROM donat.process_instances
                WHERE source_name = 'default'
                  AND process_name = 'payment_authorization'
                ",
                &[],
            )
            .expect("poll the durable Process instance");
        if let Some(row) = row {
            let status: String = row.get(0);
            if status != "running" {
                assert_eq!(
                    status,
                    "terminal",
                    "the Process did not complete: {}",
                    activity_diagnostics(client)
                );
                let output: String = row
                    .get::<_, Option<String>>(1)
                    .expect("a terminal instance has its declared output");
                return serde_json::from_str(&output).expect("terminal output is valid JSON");
            }
        }
        assert!(
            Instant::now() < deadline,
            "the Process did not reach a terminal state before the timeout"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn idempotency_key(call: &ProviderCall) -> &str {
    call.header("idempotency-key")
        .expect("a provider-idempotent step sends its fixed idempotency header")
}

#[test]
fn command_started_process_authorizes_over_real_http_and_returns_provider_data() {
    let stub = provider_stub::spawn();
    stub.set_default(AUTHORIZE_PATH, authorized_response());
    let suite = start_suite(&stub, "process_activity_http");

    let (status, body) = enqueue_authorization(&suite);
    assert_eq!(status, 200, "command mutation status: {body}");
    assert_eq!(
        body.pointer("/data/enqueue_authorization/order_id")
            .and_then(Json::as_str),
        Some(ORDER_ID),
        "the command answers its own result, never Process internals: {body}"
    );

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the source database");
    let output = await_terminal(&mut client);

    assert_eq!(
        output,
        json!({
            "status": "authorized",
            "provider_reference": "auth_conformance_1"
        }),
        "the terminal output carries the provider's own normalized data"
    );

    let calls = stub.calls_for(AUTHORIZE_PATH);
    assert_eq!(calls.len(), 1, "one committed activity performs one call");
    assert_eq!(
        calls[0].body,
        json!({ "order_id": ORDER_ID }),
        "the provider receives the compiled activity input"
    );
    assert_eq!(
        calls[0].header("authorization"),
        Some(PROVIDER_TOKEN),
        "the fixed-origin request carries its resolved credential"
    );
    assert!(
        !idempotency_key(&calls[0]).is_empty(),
        "the provider-idempotent step sends a non-empty key"
    );
}

#[test]
fn retried_activity_reuses_one_provider_idempotency_key_and_commits_once() {
    let stub = provider_stub::spawn();
    // The first attempt fails with a retryable status; the durable retry must
    // reach the same provider resource rather than authorize a second time.
    stub.script(
        AUTHORIZE_PATH,
        vec![ScriptedResponse::status(500), authorized_response()],
    );
    stub.set_default(AUTHORIZE_PATH, authorized_response());
    let suite = start_suite(&stub, "process_activity_http_retry");

    let (status, body) = enqueue_authorization(&suite);
    assert_eq!(status, 200, "command mutation status: {body}");

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the source database");
    let output = await_terminal(&mut client);
    assert_eq!(
        output.pointer("/status"),
        Some(&json!("authorized")),
        "a retryable provider failure still completes the Process"
    );

    let calls = stub.calls_for(AUTHORIZE_PATH);
    assert_eq!(calls.len(), 2, "exactly one retry follows the 500");
    assert_eq!(
        idempotency_key(&calls[0]),
        idempotency_key(&calls[1]),
        "the retry reuses the committed provider idempotency key"
    );

    let jobs = client
        .query_one(
            "
            SELECT count(*), max(attempts)
            FROM donat.process_activity_jobs
            WHERE source_name = 'default'
            ",
            &[],
        )
        .expect("read the durable activity journal");
    assert_eq!(
        jobs.get::<_, i64>(0),
        1,
        "a retry stays inside one durable activity job"
    );
    assert_eq!(
        jobs.get::<_, i32>(1),
        2,
        "the durable job records both attempts"
    );
}

/// The same deployment declared the way the catalog fields intend it: the
/// idempotency header comes from the operation's `effect`, and the retryable
/// statuses from its `error_map`. The legacy `idempotency` and
/// `error_classification` fields are absent, exactly as in a metadata
/// directory written against the current contract.
fn catalog_declared_metadata() -> Metadata {
    let mut document =
        serde_json::to_value(activity_metadata()).expect("activity metadata serializes");
    let operation = document["connectors"][0]["operations"][0]
        .as_object_mut()
        .expect("the connector declares one operation");
    operation.remove("idempotency");
    operation.remove("error_classification");
    serde_json::from_value(document).expect("catalog-declared metadata parses")
}

fn start_catalog_declared_suite(stub: &ProviderStub, name: &str) -> donat_conformance::Running {
    Suite::new(name)
        .initial_metadata(catalog_declared_metadata())
        .with_migrations()
        .env("DONAT_PROVIDER_STUB_BASE_URL", stub.base_url())
        .env("DONAT_PROVIDER_STUB_TOKEN", PROVIDER_TOKEN)
        .start()
}

/// A provider-idempotent step binds its header from `effect`, not from the
/// legacy field. Without the header the provider cannot deduplicate a replay,
/// which is the entire reason the effect is declared.
#[test]
fn a_catalog_declared_idempotent_step_sends_its_fixed_header() {
    let stub = provider_stub::spawn();
    stub.set_default(AUTHORIZE_PATH, authorized_response());
    let suite = start_catalog_declared_suite(&stub, "process_activity_catalog_idempotency");

    let (status, body) = enqueue_authorization(&suite);
    assert_eq!(status, 200, "command mutation status: {body}");

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the source database");
    await_terminal(&mut client);

    let calls = stub.calls_for(AUTHORIZE_PATH);
    assert_eq!(calls.len(), 1, "one authorization");
    assert!(
        calls[0].header("idempotency-key").is_some(),
        "the effect's fixed binding puts the stable key on the wire: {:?}",
        calls[0].headers
    );
}

/// `error_map` is what the operation says about provider failures. A status it
/// maps to `http_5xx` has to reach the retry policy as one.
#[test]
fn a_catalog_declared_retryable_status_is_retried() {
    let stub = provider_stub::spawn();
    stub.script(
        AUTHORIZE_PATH,
        vec![ScriptedResponse::status(500), authorized_response()],
    );
    stub.set_default(AUTHORIZE_PATH, authorized_response());
    let suite = start_catalog_declared_suite(&stub, "process_activity_catalog_retry");

    let (status, body) = enqueue_authorization(&suite);
    assert_eq!(status, 200, "command mutation status: {body}");

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the source database");
    let output = await_terminal(&mut client);
    assert_eq!(
        output.pointer("/status"),
        Some(&json!("authorized")),
        "the declared retry carries the Process through a mapped 5xx"
    );

    let calls = stub.calls_for(AUTHORIZE_PATH);
    assert_eq!(
        calls.len(),
        2,
        "the mapped 500 is retried once, not treated as permanent"
    );
}

/// A provider that names resources by string is handed a numeric identifier
/// through the declared cast `as: string`. The compiler types the binding as a
/// string on the strength of that declaration, so the value on the wire has to
/// be one — a number would be refused as breaking the operation's own input
/// contract, and the activity could never run.
fn scalar_cast_metadata() -> Metadata {
    let mut document =
        serde_json::to_value(activity_metadata()).expect("activity metadata serializes");
    let command = document["commands"][0]
        .as_object_mut()
        .expect("the fixture declares one command");
    command["arguments"]
        .as_array_mut()
        .expect("command arguments")
        .push(json!({ "name": "sequence", "type": "Int!" }));
    command["effects"][0]["start_process"]["input"]["sequence"] = json!({ "arg": "sequence" });

    let operation = document["connectors"][0]["operations"][0]
        .as_object_mut()
        .expect("the connector declares one operation");
    operation["input_contract"]["reference"] = json!("string!");
    operation["body"]["reference"] = json!({ "input": "reference" });

    let process = document["processes"][0]
        .as_object_mut()
        .expect("the fixture declares one process");
    process["input"]
        .as_array_mut()
        .expect("process input")
        .push(json!({ "name": "sequence", "type": "Int!" }));
    process["states"][0]["request"]["input"]["reference"] =
        json!({ "input": "sequence", "as": "string" });

    serde_json::from_value(document).expect("scalar-cast metadata parses")
}

#[test]
fn a_declared_scalar_cast_reaches_the_provider_as_a_string() {
    let stub = provider_stub::spawn();
    stub.set_default(AUTHORIZE_PATH, authorized_response());
    let suite = Suite::new("process_activity_scalar_cast")
        .initial_metadata(scalar_cast_metadata())
        .with_migrations()
        .env("DONAT_PROVIDER_STUB_BASE_URL", stub.base_url())
        .env("DONAT_PROVIDER_STUB_TOKEN", PROVIDER_TOKEN)
        .start();

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ enqueue_authorization(order_id: \"{ORDER_ID}\", \
                 request_id: \"{REQUEST_ID}\", sequence: 7) {{ order_id }} }}"
            )
        }),
        &[("X-Donat-Role".to_owned(), "customer".to_owned())],
    );
    assert_eq!(status, 200, "command mutation status: {body}");

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the source database");
    await_terminal(&mut client);

    let calls = stub.calls_for(AUTHORIZE_PATH);
    assert_eq!(calls.len(), 1, "one authorization");
    assert_eq!(
        calls[0].body.pointer("/reference"),
        Some(&json!("7")),
        "the declared cast is what the provider receives: {}",
        calls[0].body
    );
}
