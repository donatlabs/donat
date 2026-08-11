//! What the four Microsoft 365 connector test files share (spec 015).
//!
//! No test reaches Microsoft, and no test carries a real credential: the stored
//! access token is [`SECRET_SENTINEL`], which doubles as the value every
//! redaction assertion looks for.

#![allow(dead_code)]

use std::time::Duration;

use donat_connectors::providers::microsoft_graph;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AccessToken, AuthPlan, Connector, ConnectorErrorClass, Credential, Effect, EffectClass,
    Operation, OperationRejection, Pagination, PaginationBudget, RequestPlan, ValueSource,
};
use serde_json::{Value as JsonValue, json};

/// The complete `Authorization` value one attempt's credential seam produces.
pub fn applied_credential() -> AccessToken {
    AccessToken::new(format!("Bearer {SECRET_SENTINEL}"))
}

/// Render one operation the way a deployment would: the request from the
/// declaration, the credential from the source-local store.
pub fn render(stub: &ProviderStub, operation: &Operation, input: JsonValue) -> RequestPlan {
    let mut request = operation
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::oauth2_authorization_code()
        .apply(
            &Credential::from_fields([]),
            &mut request,
            Some(&applied_credential()),
        )
        .expect("the declared plan applies the stored credential");
    request
}

/// Microsoft Graph's canonical error object, as *Error responses* prints it.
///
/// The `message` carries the sort of thing Graph really says — Microsoft's own
/// warning is "they often contain dynamic information specific to the failed
/// request" — so a redaction assertion has something to find.
pub fn graph_error(code: &str) -> JsonValue {
    json!({
        "error": {
            "code": code,
            "message": format!(
                "the request failed on shard db-7.internal with token {SECRET_SENTINEL}"
            ),
            "innerError": {
                "code": "innerDetailNeverForwarded",
                "request-id": "94fb3b52-452a-4535-a601-69e0a90e3aa2",
                "date": "2026-08-10T12:51:51",
            },
        }
    })
}

/// The statuses and codes Microsoft documents for Graph, and the one class each
/// reaches.
///
/// The codes are the fifteen the *Error responses* page publishes as the
/// complete set a client must handle, plus `TooManyRequests` from the
/// throttling guidance and `badRequest` from the error-envelope example.
pub fn documented_failures() -> Vec<(u16, &'static str, ConnectorErrorClass)> {
    vec![
        (400, "badRequest", ConnectorErrorClass::Validation),
        (400, "invalidRequest", ConnectorErrorClass::Validation),
        (401, "unauthenticated", ConnectorErrorClass::Authentication),
        (403, "accessDenied", ConnectorErrorClass::Authentication),
        // "activityLimitReached — The app or user has been throttled." It is a
        // throttling answer whatever status carries it.
        (403, "activityLimitReached", ConnectorErrorClass::Http429),
        (404, "itemNotFound", ConnectorErrorClass::Permanent),
        (409, "nameAlreadyExists", ConnectorErrorClass::Permanent),
        (410, "resyncRequired", ConnectorErrorClass::Permanent),
        (412, "resourceModified", ConnectorErrorClass::Permanent),
        (413, "generalException", ConnectorErrorClass::Permanent),
        (416, "invalidRange", ConnectorErrorClass::Validation),
        (423, "notAllowed", ConnectorErrorClass::Permanent),
        (429, "TooManyRequests", ConnectorErrorClass::Http429),
        (500, "generalException", ConnectorErrorClass::Http5xx),
        (501, "notSupported", ConnectorErrorClass::Permanent),
        (503, "serviceNotAvailable", ConnectorErrorClass::Http5xx),
        (504, "generalException", ConnectorErrorClass::Http5xx),
        (507, "quotaLimitReached", ConnectorErrorClass::Permanent),
        (509, "activityLimitReached", ConnectorErrorClass::Http429),
        // Nothing Microsoft documents: the declared fallback answers.
        (418, "teapot", ConnectorErrorClass::Permanent),
    ]
}

/// `<name>_odata_error_is_typed`, for the half every connector shares.
///
/// Microsoft is explicit about which half of the envelope a client may depend
/// on — "The **code** property contains a machine-readable value that you can
/// take a dependency on in your code", and of `message`: "Don't take any
/// dependency on the content of this value in your code." So the class comes
/// from the code, the same code means the same class whatever status carries
/// it, and the human message never crosses the boundary.
pub fn assert_odata_error_is_typed_by_code() {
    // One code, two statuses, one class.
    for (code, class) in [
        ("activityLimitReached", ConnectorErrorClass::Http429),
        ("serviceNotAvailable", ConnectorErrorClass::Http5xx),
        ("itemNotFound", ConnectorErrorClass::Permanent),
    ] {
        for status in [400, 500] {
            let failure = microsoft_graph::error_map().classify(
                status,
                &reqwest::header::HeaderMap::new(),
                &serde_json::to_vec(&graph_error(code)).expect("a fixture body serializes"),
            );
            assert_eq!(failure.class(), class, "{status} {code}");
            let surface = format!(
                "{} {} {} {failure:?}",
                failure.code(),
                failure.safe_message(),
                failure.diagnostic()
            );
            for leaked in [
                SECRET_SENTINEL,
                "db-7.internal",
                "innerDetailNeverForwarded",
            ] {
                assert!(
                    !surface.contains(leaked),
                    "{code} leaked {leaked}: {surface}"
                );
            }
        }
    }

    // A body with no code at all falls back to the status rule rather than to
    // anything read out of the message.
    assert_eq!(
        microsoft_graph::error_map()
            .classify(
                503,
                &reqwest::header::HeaderMap::new(),
                br#"{"error":{"message":"Service is temporarily unavailable"}}"#,
            )
            .class(),
        ConnectorErrorClass::Http5xx
    );
}

/// The one throttling proof: the documented response reaches `http_429`, its
/// `Retry-After` hint is honoured, and an absurd hint is clamped rather than
/// obeyed.
///
/// *Throttling guidance*: Graph "Returns HTTP status code **429 Too Many
/// Requests**", "Returns a suggested wait time in the response header of the
/// failed request", and "Wait the number of seconds specified in the
/// `Retry-After` header."
pub async fn assert_throttling_is_classified(
    method: &str,
    path: &str,
    operation: &'static Operation,
    input: JsonValue,
) {
    let body = graph_error("TooManyRequests");
    let stub = ProviderStub::start([
        Expectation::new(method, path)
            .respond_header("retry-after", "10")
            .respond_json(429, body.clone()),
        Expectation::new(method, path)
            .respond_header("retry-after", "999999999")
            .respond_json(429, body.clone()),
        Expectation::new(method, path).respond_json(429, body.clone()),
        // "509 Bandwidth Limit Exceeded: … Your app can retry the request again
        // after more time has elapsed."
        Expectation::new(method, path).respond_json(509, graph_error("activityLimitReached")),
    ])
    .await;

    let mut hints = Vec::new();
    for _ in 0..4 {
        let response = stub
            .send(render(&stub, operation, input.clone()))
            .await
            .expect("the stub answers");
        let failure = microsoft_graph::error_map().classify_response(&response);
        assert_eq!(
            failure.class(),
            ConnectorErrorClass::Http429,
            "a documented throttling answer is `http_429`"
        );
        assert!(
            !format!("{failure:?} {}", failure.diagnostic()).contains(SECRET_SENTINEL),
            "the human message never crosses the boundary"
        );
        hints.push(failure.retry_after());
    }
    assert_eq!(
        hints,
        vec![
            Some(Duration::from_secs(10)),
            // 999999999 seconds is thirty-one years; the SDK's ceiling answers.
            Some(Duration::from_secs(
                donat_connectors::sdk::MAX_RETRY_AFTER_SECONDS
            )),
            None,
            None,
        ]
    );
    stub.assert_satisfied();
}

/// `<name>_next_link_stays_on_origin`.
///
/// Microsoft publishes `@odata.nextLink` as an absolute URL a client follows
/// whole — "Use the entire URL in the `@odata.nextLink` property in a GET
/// request" — which is the one place a provider-chosen value becomes a
/// destination. A link on a foreign host, a different scheme, or a different
/// port is refused with no request made.
pub async fn assert_next_link_stays_on_origin(
    plan: &Pagination,
    operation: &'static Operation,
    input: JsonValue,
    method: &str,
    path: &str,
    items: &str,
) {
    let budget = || PaginationBudget::new(16, 16, 256, 512 * 1024, Duration::from_secs(5));

    // 1. A same-origin `@odata.nextLink` — the shape Microsoft's own paging
    //    example prints — is followed, and the walk ends when the property is
    //    no longer returned.
    let stub = ProviderStub::start([
        Expectation::new(method, path).respond_json(
            200,
            json!({
                items.trim_start_matches('/'): [{ "id": "one" }],
                "@odata.nextLink": format!("{{base_url}}{path}?$skiptoken=RFNwdAIAAQAAAD8"),
            }),
        ),
        Expectation::new(method, path)
            .query("$skiptoken=RFNwdAIAAQAAAD8")
            .respond_json(
                200,
                json!({ items.trim_start_matches('/'): [{ "id": "two" }] }),
            ),
    ])
    .await;
    let first = render(&stub, operation, input.clone());
    let collected = plan
        .collect(
            first,
            &stub.origin(),
            &budget(),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("a same-origin continuation is followed");
    assert_eq!(
        collected,
        vec![json!({ "id": "one" }), json!({ "id": "two" })]
    );
    stub.assert_satisfied();

    // 2. A link that leaves the origin — another host, another scheme, another
    //    port — is refused, and the other origin is never contacted.
    let elsewhere = ProviderStub::start([Expectation::new(method, path).respond_json(
        200,
        json!({ items.trim_start_matches('/'): [{ "id": "stolen" }] }),
    )])
    .await;
    let foreign_port = elsewhere.base_url().to_owned();
    for next in [
        // A host that merely *starts* with the compiled one.
        "https://graph.microsoft.com.attacker.invalid/v1.0/me/messages".to_owned(),
        // The real Graph host, which is not this attempt's compiled origin.
        "http://graph.microsoft.com/v1.0/me/messages".to_owned(),
        // The same host on another port.
        format!("{foreign_port}{path}"),
        // A protocol-relative authority, which resolves to another host.
        "//attacker.invalid/v1.0/me/messages".to_owned(),
        // The stub's own loopback host under another scheme. The stub binds
        // `127.0.0.1` on an ephemeral port, so this differs from the compiled
        // origin in scheme and in the port that scheme implies, and in nothing
        // else.
        format!("https://127.0.0.1{path}"),
        // A credential smuggled into the authority of an otherwise plausible
        // continuation.
        format!("{foreign_port}{path}").replace("//", "//user:pass@"),
    ] {
        let stub = ProviderStub::start([Expectation::new(method, path).respond_json(
            200,
            json!({
                items.trim_start_matches('/'): [{ "id": "one" }],
                "@odata.nextLink": next,
            }),
        )])
        .await;
        let first = render(&stub, operation, input.clone());
        let failure = plan
            .collect(
                first,
                &stub.origin(),
                &budget(),
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("a continuation off the compiled origin is never followed");
        assert_eq!(
            failure.class(),
            ConnectorErrorClass::Invariant,
            "continuation {next}"
        );
        assert_eq!(failure.code(), "connector_pagination_cross_origin");
        stub.assert_satisfied();
    }
    assert_eq!(
        elsewhere.mismatches().len(),
        1,
        "the other origin was never contacted"
    );
}

/// Assert the effect class of every operation one connector declares, that an
/// inventory-only one cannot be enabled, and that none claims an idempotency
/// binding Microsoft does not publish.
pub fn assert_effects(connector: &'static Connector, expected: &[(&str, EffectClass)]) {
    assert_eq!(
        connector.operations().len(),
        expected.len(),
        "every declared operation of `{}` is classified here",
        connector.name()
    );
    for (id, class) in expected {
        let operation = connector
            .operation(id)
            .unwrap_or_else(|| panic!("the declaration publishes {id}"));
        assert_eq!(operation.effect_class(), Some(*class), "{id}");
        match class {
            EffectClass::InventoryOnly => {
                assert_eq!(
                    connector.admit_operation(id),
                    Err(OperationRejection::InventoryOnly),
                    "{id} must not be enablable by a deployment"
                );
                assert!(
                    operation
                        .effect()
                        .and_then(Effect::inventory_reason)
                        .is_some_and(|reason| !reason.is_empty()),
                    "{id} records why it is not executable"
                );
            }
            _ => {
                assert!(connector.admit_operation(id).is_ok(), "{id}");
                assert!(operation.is_executable(), "{id}");
            }
        }
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: Microsoft publishes no idempotency key with a retention window to bind"
        );
    }
}

/// `<name>_consistency_headers_are_declared`.
///
/// Spec 015 §3.5: where an operation requires a specific request header to be
/// valid, it is declared statically, never derived from input. This holds it
/// shut for a whole connector at once: every header the declaration carries is
/// a static value, no operation binds a header from an input slot, and an input
/// that spells a header name changes nothing on the wire.
pub fn assert_headers_are_static(
    connector: &'static Connector,
    expected: &[(&str, &[(&str, &str)])],
) {
    for (id, _) in expected {
        assert!(
            connector.operation(id).is_some(),
            "the header table of `{}` names `{id}`, which it does not declare",
            connector.name()
        );
    }
    for operation in connector.operations() {
        let projection = operation.project();
        let declared = expected
            .iter()
            .find(|(id, _)| *id == operation.id())
            .map(|(_, headers)| *headers)
            .unwrap_or(&[]);
        let mut rendered = projection
            .headers()
            .iter()
            .map(|header| match header.value() {
                ValueSource::Static(value) => (header.name().to_owned(), value.clone()),
                ValueSource::Input(input) => panic!(
                    "`{}.{}` binds header `{}` from input `{input}`; a header an operation needs \
                     to be valid is declaration material",
                    connector.name(),
                    operation.id(),
                    header.name()
                ),
            })
            .collect::<Vec<_>>();
        rendered.sort();
        let mut wanted = declared
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<Vec<_>>();
        wanted.sort();
        assert_eq!(
            rendered,
            wanted,
            "`{}.{}` declares exactly the headers Microsoft documents for it",
            connector.name(),
            operation.id()
        );
    }
}
