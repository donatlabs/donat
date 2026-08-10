//! Mattermost connector proofs (spec 025 §4), against the SDK's local provider
//! stub.

use std::time::Duration;

use donat_connectors::providers::mattermost;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass,
    Operation, OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const CHANNEL_ID: &str = "4xp9fdt77pncbef59f4k1qe83o";
const POST_ID: &str = "ojkkw1yrxtn1umbo1n7ttfnf9a";
const TEAM_ID: &str = "7gnk6xrnhpn5tp3fbgnkn7bdcy";
const USER_ID: &str = "9uwmkw7dt3n5upfhqnp1rn5ame";

fn operation(id: &str) -> &'static Operation {
    mattermost::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the mattermost declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::bearer()
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn post() -> JsonValue {
    json!({
        "id": POST_ID,
        "channel_id": CHANNEL_ID,
        "user_id": USER_ID,
        "message": "the invoice is approved",
        "root_id": "",
        "create_at": 1_785_000_000_000_i64,
        "update_at": 1_785_000_000_000_i64,
    })
}

fn channel() -> JsonValue {
    json!({
        "id": CHANNEL_ID,
        "team_id": TEAM_ID,
        "name": "town-square",
        "display_name": "Town Square",
        "type": "O",
        "purpose": "",
        "header": "",
    })
}

/// Every operation, with an input that satisfies it and the documented success.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        (
            "post.create",
            json!({ "channel_id": CHANNEL_ID, "message": "the invoice is approved",
                    "root_id": null }),
        ),
        ("post.get", json!({ "post_id": POST_ID })),
        ("channel.get", json!({ "channel_id": CHANNEL_ID })),
        (
            "channel.posts",
            json!({ "channel_id": CHANNEL_ID, "page": 0 }),
        ),
        ("channel.list", json!({ "team_id": TEAM_ID })),
        ("channel.member_list", json!({ "channel_id": CHANNEL_ID })),
        ("user.get", json!({ "user_id": USER_ID })),
    ]
}

/// `mattermost_request_shape`: exact method, path, query, headers, and body for
/// every operation, each under the published `/api/v4` prefix.
#[tokio::test]
async fn mattermost_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("POST", "/api/v4/posts")
            .query("")
            .json_body(json!({
                "channel_id": CHANNEL_ID,
                "message": "the invoice is approved",
                "root_id": null,
            }))
            .respond_json(201, post()),
        Expectation::new("GET", &format!("/api/v4/posts/{POST_ID}"))
            .query("")
            .no_body()
            .respond_json(200, post()),
        Expectation::new("GET", &format!("/api/v4/channels/{CHANNEL_ID}"))
            .query("")
            .respond_json(200, channel()),
        Expectation::new("GET", &format!("/api/v4/channels/{CHANNEL_ID}/posts"))
            .query("page=0&per_page=200")
            .respond_json(
                200,
                json!({ "order": [POST_ID], "posts": { POST_ID: post() }, "has_next": false }),
            ),
        Expectation::new("GET", &format!("/api/v4/teams/{TEAM_ID}/channels"))
            .query("")
            .respond_json(200, json!([channel()])),
        Expectation::new("GET", &format!("/api/v4/channels/{CHANNEL_ID}/members"))
            .query("")
            .respond_json(
                200,
                json!([{ "channel_id": CHANNEL_ID, "user_id": USER_ID }]),
            ),
        Expectation::new("GET", &format!("/api/v4/users/{USER_ID}"))
            .query("")
            .respond_json(
                200,
                json!({ "id": USER_ID, "username": "donat", "email": "donat@example.test" }),
            ),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/v4/"),
            "{id} renders a published Mattermost path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `mattermost_auth_is_applied`: the personal access token reaches the wire as
/// `Authorization: Bearer …` and appears nowhere else.
#[tokio::test]
async fn mattermost_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/api/v4/channels/{CHANNEL_ID}"))
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, channel()),
        ])
        .await;

    let request = render(&stub, "channel.get", json!({ "channel_id": CHANNEL_ID }));
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        mattermost::connector().credential(),
        mattermost::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `mattermost_origin_comes_only_from_deploy_time_configuration`: the whole
/// origin is a value the deployment names, and nothing in input, a provider
/// body, or a continuation can move it — nor can a deployment name one this
/// connector may not send a bearer token to.
#[test]
fn mattermost_origin_comes_only_from_deploy_time_configuration() {
    let connector: &Connector = mattermost::connector();
    assert_eq!(
        connector.origin().host_variable(),
        Some(mattermost::SERVER_ORIGIN),
        "the origin names the one configuration key it reads"
    );

    let configured = ConnectorConfiguration::from_deployment([(
        mattermost::SERVER_ORIGIN,
        "https://chat.example.test",
    )]);
    let origin = connector
        .resolve_origin(&configured)
        .expect("a configured origin resolves");
    assert_eq!(origin.as_url().host_str(), Some("chat.example.test"));

    // Input cannot move it: every declared slot renders inside the path, and a
    // hostile value stays inside its own segment.
    let request = operation("channel.get")
        .plan_request(
            &origin,
            &json!({ "channel_id": "../../evil.example.test/api/v4/channels/x" }),
        )
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("chat.example.test"));
    assert!(!request.url().path().contains("evil.example.test/api"));

    // A deployment cannot name a plain-HTTP server, because the credential is a
    // bearer token on every request.
    assert!(mattermost::validate_server_origin("http://chat.example.test").is_err());
    // Nor a path: an origin is a scheme, a host, and a port.
    assert!(mattermost::validate_server_origin("https://example.test/mattermost").is_err());
    assert!(mattermost::validate_server_origin("https://chat.example.test").is_ok());
    assert!(mattermost::validate_server_origin("https://chat.example.test:8443").is_ok());
    // And an unresolved instance has no origin at all, rather than a default.
    assert!(
        connector
            .resolve_origin(&ConnectorConfiguration::default())
            .is_err()
    );
}

/// `mattermost_error_map`: every documented status reaches exactly one closed
/// class, and none of Mattermost's prose crosses the boundary.
#[tokio::test]
async fn mattermost_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Permanent),
        (404, ConnectorErrorClass::Permanent),
        (501, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        // An undocumented status still lands in the closed set.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v4/channels/{CHANNEL_ID}"),
        )
        .respond_header("x-request-id", "bk3uzm335jr9tnoh4mcsybmmjr")
        .respond_json(
            status,
            json!({
                "id": "api.context.permissions.app_error",
                "message": format!("acme-chat rejected token {SECRET_SENTINEL}"),
                "request_id": "bk3uzm335jr9tnoh4mcsybmmjr",
                "status_code": status,
                "is_oauth": false,
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "channel.get",
                json!({ "channel_id": CHANNEL_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = mattermost::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        assert_eq!(
            failure
                .correlation_ids()
                .get("request_id")
                .map(String::as_str),
            Some("bk3uzm335jr9tnoh4mcsybmmjr")
        );
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme-chat", "api.context.permissions"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `mattermost_rate_limit_is_classified`: the documented `429` is retryable, and
/// Mattermost publishes no `Retry-After` — so the hint is absent unless the
/// response carried one, and a hostile one is clamped.
#[tokio::test]
async fn mattermost_rate_limit_is_classified() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v4/channels/{CHANNEL_ID}"))
            .respond_header("x-ratelimit-reset", "1")
            .respond_bytes(429, b"limit exceeded".to_vec()),
        Expectation::new("GET", &format!("/api/v4/channels/{CHANNEL_ID}"))
            .respond_header("retry-after", "604800")
            .respond_bytes(429, b"limit exceeded".to_vec()),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "channel.get",
                json!({ "channel_id": CHANNEL_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(mattermost::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        None,
        "Mattermost publishes X-RateLimit-Reset rather than Retry-After, so the connector invents \
         no hint"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `mattermost_pagination_is_bounded`: the two bare-array collections walk the
/// published `page`/`per_page` regime and stop on a short page; the channel's
/// posts declare no plan, because their payload is a map a walk cannot merge.
#[tokio::test]
async fn mattermost_pagination_is_bounded() {
    let plan = mattermost::pagination("channel.list").expect("the listing declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let full_page: Vec<JsonValue> = (0..200).map(|index| json!({ "id": index })).collect();
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v4/teams/{TEAM_ID}/channels"))
            .query("page=0&per_page=200")
            .respond_json(200, JsonValue::Array(full_page)),
        Expectation::new("GET", &format!("/api/v4/teams/{TEAM_ID}/channels"))
            .query("page=1&per_page=200")
            .respond_json(200, json!([{ "id": 200 }])),
    ])
    .await;

    let channels = plan
        .collect(
            render(&stub, "channel.list", json!({ "team_id": TEAM_ID })),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk advances one page and stops on the short one");
    assert_eq!(channels.len(), 201);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();

    // An endless provider exhausts each ceiling rather than looping.
    for budget in [
        PaginationBudget::new(2, 8, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 100_000, 1, Duration::from_secs(5)),
    ] {
        let page: Vec<JsonValue> = (0..200).map(|index| json!({ "id": index })).collect();
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &format!("/api/v4/teams/{TEAM_ID}/channels"))
                .respond_json(200, JsonValue::Array(page.clone()))
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "channel.list", json!({ "team_id": TEAM_ID })),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    assert!(
        mattermost::pagination("channel.member_list").is_some(),
        "a bare-array member page is walked"
    );
    for id in [
        "post.create",
        "post.get",
        "channel.get",
        "channel.posts",
        "user.get",
    ] {
        assert!(
            mattermost::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `mattermost_effects_are_classified`: every operation carries a class, and the
/// one write is at-most-once with both halves of ADR 063's evidence.
#[test]
fn mattermost_effects_are_classified() {
    let connector = mattermost::connector();
    let expected = [
        ("post.create", EffectClass::AtMostOnce),
        ("post.get", EffectClass::ReadOnly),
        ("channel.get", EffectClass::ReadOnly),
        ("channel.posts", EffectClass::ReadOnly),
        ("channel.list", EffectClass::ReadOnly),
        ("channel.member_list", EffectClass::ReadOnly),
        ("user.get", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert!(
        connector.admit_operation("not.an.operation").is_err(),
        "a name this binary was not built with is refused"
    );
    assert!(
        !matches!(
            connector.admit_operation("post.create"),
            Err(OperationRejection::InventoryOnly)
        ),
        "an at-most-once operation is admitted by the module and gated at process compilation"
    );

    let evidence = operation("post.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.searched_documentation().contains("OpenAPI"));
    assert!(evidence.repeat_produces().contains("a second post"));
}

/// `mattermost_output_contract`: the declared pointers read Mattermost's own
/// objects, a bare collection is published whole, and the post list's map is
/// published beside its order rather than merged into it.
#[test]
fn mattermost_output_contract() {
    let get = operation("post.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&post()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": POST_ID,
            "channel_id": CHANNEL_ID,
            "user_id": USER_ID,
            "message": "the invoice is approved",
            "root_id": "",
            "create_at": 1_785_000_000_000_i64,
            "update_at": 1_785_000_000_000_i64,
        })
    );
    // Mattermost publishes its timestamps as milliseconds since the epoch; a
    // string there is a contract violation rather than a coercion.
    assert_eq!(
        get.decode_response(200, br#"{"id":"a","create_at":"2026-08-02"}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"message":"hi"}"#)
            .expect_err("an answer with no id is not a post")
            .class(),
        ConnectorErrorClass::Validation
    );

    let posts = operation("channel.posts");
    assert_eq!(
        posts
            .decode_response(
                200,
                br#"{"order":["p1"],"posts":{"p1":{"id":"p1"}},"has_next":true}"#,
            )
            .expect("the declared contract is satisfied"),
        json!({
            "order": ["p1"],
            "posts": { "p1": { "id": "p1" } },
            "has_next": true,
        })
    );

    let channels = operation("channel.list");
    assert_eq!(
        channels
            .decode_response(200, br#"[{"id":"c1"}]"#)
            .expect("a bare collection is the whole document"),
        json!([{ "id": "c1" }])
    );
}
