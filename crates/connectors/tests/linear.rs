//! Linear connector proofs (spec 016 §3), against the SDK's local provider stub.
//!
//! Two of them are this connector's reason to exist:
//! `linear_graphql_errors_are_failures` — a `200` carrying `errors` is never a
//! success — and `linear_query_is_static`, which holds the document shut against
//! every input a caller could send.

use std::collections::BTreeSet;
use std::time::Duration;

use donat_connectors::providers::linear;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

const ISSUE_ID: &str = "9dc0d3a0-6b1e-4f6c-9a4b-4c1a1e0c0a11";
const ISSUE_URL: &str = "https://linear.app/acme/issue/ACM-42/first";

fn operation(id: &str) -> &'static Operation {
    linear::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the linear declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::authorization_credential()
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

/// The body one operation renders for one input, as JSON.
fn body(id: &str, input: JsonValue) -> JsonValue {
    let origin = donat_connectors::sdk::Origin::parse("https://api.linear.app")
        .expect("the published origin is valid");
    let request = operation(id)
        .plan_request(&origin, &input)
        .expect("the declared request renders");
    serde_json::from_slice(request.body()).expect("a GraphQL request body is JSON")
}

/// Every operation, with an input that satisfies it.
fn inputs() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("issue.get", json!({ "id": ISSUE_ID })),
        (
            "issue.list",
            json!({ "filter": { "state": { "type": { "eq": "started" } } }, "after": null }),
        ),
        (
            "issue.create",
            json!({ "input": { "teamId": "team_1", "title": "First" } }),
        ),
        (
            "issue.update",
            json!({ "id": ISSUE_ID, "input": { "title": "Renamed" } }),
        ),
        (
            "comment.create",
            json!({ "input": { "issueId": ISSUE_ID, "body": "hello" } }),
        ),
        ("team.list", json!({ "after": null })),
        ("user.list", json!({ "after": null })),
    ]
}

/// A success body per operation, in the envelope Linear publishes.
fn success(id: &str) -> JsonValue {
    match id {
        "issue.get" => json!({ "data": { "issue": {
            "id": ISSUE_ID, "identifier": "ACM-42", "title": "First",
            "description": null, "url": ISSUE_URL, "priority": 2.0,
            "updatedAt": "2026-08-10T10:00:00.000Z",
            "state": { "id": "s1", "name": "In Progress", "type": "started" } } }}),
        "issue.list" => json!({ "data": { "issues": {
            "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } }}),
        "issue.create" => json!({ "data": { "issueCreate": { "success": true,
            "issue": { "id": ISSUE_ID, "identifier": "ACM-42", "title": "First", "url": ISSUE_URL } } }}),
        "issue.update" => json!({ "data": { "issueUpdate": { "success": true,
            "issue": { "id": ISSUE_ID, "identifier": "ACM-42", "title": "Renamed", "url": ISSUE_URL } } }}),
        "comment.create" => json!({ "data": { "commentCreate": { "success": true,
            "comment": { "id": "c1", "body": "hello", "url": ISSUE_URL, "createdAt": "2026-08-10T10:00:00.000Z" } } }}),
        "team.list" => json!({ "data": { "teams": {
            "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } }}),
        "user.list" => json!({ "data": { "users": {
            "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } }}),
        other => panic!("no success fixture for {other}"),
    }
}

/// `linear_request_shape`: one endpoint, one method, and the checked-in document
/// with its declared variables — for every operation.
#[tokio::test]
async fn linear_request_shape() {
    let stub = ProviderStub::start(inputs().iter().map(|(id, _)| {
        Expectation::new("POST", "/graphql")
            .query("")
            .header("authorization", SECRET_SENTINEL)
            .header("content-type", "application/json")
            .respond_json(200, success(id))
    }))
    .await;

    for (id, input) in inputs() {
        let request = render(&stub, id, input.clone());
        assert_eq!(request.url().path(), "/graphql", "{id}");
        assert_eq!(request.method().as_str(), "POST", "{id}");
        let sent: JsonValue = serde_json::from_slice(request.body()).expect("the body is JSON");
        assert_eq!(
            sent["query"].as_str(),
            linear::document(id),
            "{id} sends its own checked-in document"
        );
        let response = stub.send(request).await.expect("the stub answers");
        linear::decode(
            operation(id),
            response.status.as_u16(),
            response.headers(),
            response.body(),
        )
        .unwrap_or_else(|error| panic!("{id} decodes its documented success: {error:?}"));
    }
    stub.assert_satisfied();
}

/// `linear_query_is_static` (spec 016 §3 proof 2): a caller cannot supply,
/// extend, or alias any part of the GraphQL document; only declared variables
/// bind.
#[test]
fn linear_query_is_static() {
    // Everything a caller might hope reaches the wire: another document, an
    // operation name, a persisted-query extension, an alias, a fragment, a
    // directive, and an attempt to widen the page.
    let hostile = json!({
        "query": "mutation Evil { issueDelete(id: \"x\") { success } }",
        "operationName": "Evil",
        "extensions": { "persistedQuery": { "sha256Hash": "deadbeef" } },
        "variables": { "id": "injected" },
        "first": 10_000,
        "alias": "a: issues",
        "fragment": "fragment F on Issue { id }",
        "@include": true,
    });

    for (id, declared) in inputs() {
        let document = linear::document(id).expect("every operation carries a document");

        // 1. The document is the checked-in constant, whatever the input.
        let mut poisoned = declared.clone();
        for (key, value) in hostile.as_object().expect("the hostile input is an object") {
            poisoned
                .as_object_mut()
                .expect("the declared input is an object")
                .insert(key.clone(), value.clone());
        }
        for input in [declared.clone(), poisoned.clone()] {
            let sent = body(id, input);
            assert_eq!(
                sent["query"].as_str(),
                Some(document),
                "{id} sends its checked-in document byte for byte"
            );
            // 2. The request body has exactly two keys: no `operationName`, no
            //    `extensions`, no persisted query.
            let keys: BTreeSet<&str> = sent
                .as_object()
                .expect("the body is an object")
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                BTreeSet::from(["query", "variables"]),
                "{id} sends only the document and its variables"
            );
            // 3. The variables are exactly the declaration's own names.
            let honest = body(id, declared.clone());
            let declared_variables: BTreeSet<&str> = honest["variables"]
                .as_object()
                .expect("variables is an object")
                .keys()
                .map(String::as_str)
                .collect();
            let sent_variables: BTreeSet<&str> = sent["variables"]
                .as_object()
                .expect("variables is an object")
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                sent_variables, declared_variables,
                "{id} binds only its declared variables"
            );
        }

        // 4. The page size is declaration material: an input naming `first`
        //    changes nothing.
        if document.contains("$first") {
            assert_eq!(
                body(id, poisoned)["variables"]["first"],
                json!(50),
                "{id} keeps the declared page size"
            );
        }
    }
}

/// Spec 016 §4: every checked-in document parses and names only declared
/// variables.
#[test]
fn linear_documents_parse_and_name_only_declared_variables() {
    for (id, document) in linear::documents() {
        let parsed = parse_document(document)
            .unwrap_or_else(|error| panic!("{id} document does not parse: {error}"));

        assert!(
            matches!(parsed.operation.as_str(), "query" | "mutation"),
            "{id} declares one query or one mutation, not {}",
            parsed.operation
        );
        assert!(!parsed.name.is_empty(), "{id} names its operation");
        assert_eq!(
            parsed.used, parsed.declared_names,
            "{id} uses exactly the variables it declares"
        );

        // The variables the module actually binds are the ones the document
        // declares — the two halves are written separately and have to agree.
        let bound: BTreeSet<String> = body(
            id,
            inputs()
                .into_iter()
                .find(|(operation, _)| operation == id)
                .expect("every document belongs to an operation")
                .1,
        )["variables"]
            .as_object()
            .expect("variables is an object")
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            bound, parsed.declared_names,
            "{id} binds exactly the variables its document declares"
        );
    }
}

/// `linear_graphql_errors_are_failures` (spec 016 §3 proof 1): a `200` carrying
/// a provider error never becomes a success, and its code maps to exactly one
/// class.
#[test]
fn linear_graphql_errors_are_failures() {
    let headers = HeaderMap::new();
    for (id, _) in inputs() {
        let operation = operation(id);

        // The documented success still decodes.
        linear::decode(
            operation,
            200,
            &headers,
            &serde_json::to_vec(&success(id)).expect("a fixture body serializes"),
        )
        .unwrap_or_else(|error| panic!("{id} documented success decodes: {error:?}"));

        // A `200` with errors and no data.
        let failure = linear::decode(
            operation,
            200,
            &headers,
            br#"{"errors":[{"message":"Entity not found","extensions":{"type":"invalid input"}}]}"#,
        )
        .expect_err("a 200 carrying GraphQL errors is never a success");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation, "{id}");
        assert_eq!(failure.provider_status(), Some(200), "{id}");

        // The partial answer Linear documents: some data *and* errors. It is a
        // failure, not a success with holes in it.
        let mut partial = success(id);
        partial
            .as_object_mut()
            .expect("a success fixture is an object")
            .insert(
                "errors".to_owned(),
                json!([{ "message": "field failed", "extensions": { "type": "internal error" } }]),
            );
        let failure = linear::decode(
            operation,
            200,
            &headers,
            &serde_json::to_vec(&partial).expect("a fixture body serializes"),
        )
        .expect_err("a partial answer is not the declared output contract");
        assert_eq!(failure.class(), ConnectorErrorClass::Http5xx, "{id}");

        // An envelope with neither `data` nor `errors` is outside the contract.
        assert_eq!(
            linear::decode(operation, 200, &headers, b"{}")
                .expect_err("an envelope-less body is not a success")
                .class(),
            ConnectorErrorClass::Invariant,
            "{id}"
        );
        assert_eq!(
            linear::decode(operation, 200, &headers, b"<html>gateway</html>")
                .expect_err("a non-JSON body is not a success")
                .class(),
            ConnectorErrorClass::Validation,
            "{id}"
        );
    }

    // Every error type Linear's own client classifies on reaches one class, and
    // nothing the provider wrote crosses the boundary.
    let classified = [
        ("ratelimited", ConnectorErrorClass::Http429),
        ("RATELIMITED", ConnectorErrorClass::Http429),
        ("authentication error", ConnectorErrorClass::Authentication),
        ("forbidden", ConnectorErrorClass::Authentication),
        ("invalid input", ConnectorErrorClass::Validation),
        ("user error", ConnectorErrorClass::Validation),
        ("graphql error", ConnectorErrorClass::Validation),
        ("usage limit exceeded", ConnectorErrorClass::Permanent),
        ("feature not accessible", ConnectorErrorClass::Permanent),
        ("internal error", ConnectorErrorClass::Http5xx),
        ("lock timeout", ConnectorErrorClass::Http5xx),
        // Anything unmapped takes the declared fallback rather than "unknown".
        ("bootstrap error", ConnectorErrorClass::Permanent),
    ];
    for (kind, expected) in classified {
        let body = format!(
            r#"{{"errors":[{{"message":"tenant acme key {SECRET_SENTINEL}","extensions":{{"type":"{kind}","userPresentableMessage":"do not forward this"}}}}]}}"#
        );
        let failure = linear::decode(operation("issue.get"), 200, &headers, body.as_bytes())
            .expect_err("errors are a failure whatever the type");
        assert_eq!(failure.class(), expected, "type {kind}");
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "do not forward this", "acme"] {
            assert!(!surface.contains(leaked), "type {kind} leaked: {surface}");
        }
    }
}

/// The gate holds on the wire, through the same path a deployment uses.
#[tokio::test]
async fn linear_graphql_errors_are_failures_on_the_wire() {
    let stub = ProviderStub::start([Expectation::new("POST", "/graphql").respond_json(
        200,
        json!({ "data": { "issue": null },
                "errors": [{ "message": "Entity not found",
                             "extensions": { "type": "invalid input" } }] }),
    )])
    .await;

    let response = stub
        .send(render(&stub, "issue.get", json!({ "id": ISSUE_ID })))
        .await
        .expect("the stub answers");
    assert_eq!(response.status.as_u16(), 200, "Linear answered 200");
    assert_eq!(
        linear::decode(
            operation("issue.get"),
            response.status.as_u16(),
            response.headers(),
            response.body(),
        )
        .expect_err("a 200 carrying errors is a failure")
        .class(),
        ConnectorErrorClass::Validation
    );
    stub.assert_satisfied();
}

/// `linear_auth_is_applied`: the API key reaches the wire as the *whole*
/// `Authorization` value, which is the form Linear publishes for it, and it
/// appears nowhere else.
#[tokio::test]
async fn linear_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("POST", "/graphql")
        .header("authorization", SECRET_SENTINEL)
        .respond_json(200, success("issue.get"))])
    .await;

    let request = render(&stub, "issue.get", json!({ "id": ISSUE_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().ok(),
        Some(SECRET_SENTINEL),
        "an API key is sent with no scheme in front of it"
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!String::from_utf8_lossy(request.body()).contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        linear::connector().credential(),
        linear::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `linear_rate_limit_is_classified` (spec 016 §3 proof 5): Linear's documented
/// rate-limit response reaches `http_429` with its retry hint clamped.
#[tokio::test]
async fn linear_rate_limit_is_classified() {
    // "response http status code will be 400, but you can catch these by
    // inspecting the errors in the response body containing the `RATELIMITED`
    // error code."
    let limited = json!({ "errors": [{
        "message": "Rate limit exceeded",
        "extensions": { "type": "ratelimited", "code": "RATELIMITED" } }] });
    let stub = ProviderStub::start([
        Expectation::new("POST", "/graphql")
            .respond_header("x-ratelimit-requests-remaining", "0")
            .respond_json(400, limited.clone()),
        // Linear publishes no `Retry-After`; when a proxy in front of it sends
        // one, the SDK ceiling still applies.
        Expectation::new("POST", "/graphql")
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "issue.get", json!({ "id": ISSUE_ID })))
            .await
            .expect("the stub answers");
        failures.push(
            linear::decode(
                operation("issue.get"),
                response.status.as_u16(),
                response.headers(),
                response.body(),
            )
            .expect_err("a rate limit is a failure"),
        );
    }

    assert_eq!(
        failures[0].class(),
        ConnectorErrorClass::Http429,
        "a 400 carrying RATELIMITED is a rate limit, not a validation failure"
    );
    assert_eq!(failures[0].provider_status(), Some(400));
    assert_eq!(
        failures[0].retry_after(),
        None,
        "Linear publishes no Retry-After, so none is invented"
    );
    assert_eq!(failures[1].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[1].retry_after(), Some(Duration::from_secs(86_400)));
    stub.assert_satisfied();

    // A `400` whose only spelling of the condition is at `extensions.code`
    // classifies by its status. It is recorded rather than hidden: the class is
    // not retried either way, which is the safe direction.
    assert_eq!(
        linear::decode(
            operation("issue.get"),
            400,
            &HeaderMap::new(),
            br#"{"errors":[{"message":"x","extensions":{"code":"RATELIMITED"}}]}"#,
        )
        .expect_err("a 400 is a failure")
        .class(),
        ConnectorErrorClass::Validation
    );
}

/// `linear_cursor_is_opaque_and_bounded` (spec 016 §3 proof 4): the cursor is
/// echoed verbatim, is never parsed or constructed here, and one call is one
/// page whose size the declaration owns.
#[tokio::test]
async fn linear_cursor_is_opaque_and_bounded() {
    // A cursor whose bytes are hostile in every direction a value can be.
    const CURSOR: &str = "eyJpZCI6MX0=\", first: 10000) { nodes { id } } evil: issues(first: 999";

    let stub = ProviderStub::start([
        Expectation::new("POST", "/graphql").respond_json(
            200,
            json!({ "data": { "issues": { "nodes": [{ "id": "i1" }],
                    "pageInfo": { "hasNextPage": true, "endCursor": CURSOR } } } }),
        ),
        Expectation::new("POST", "/graphql").respond_json(
            200,
            json!({ "data": { "issues": { "nodes": [{ "id": "i2" }],
                    "pageInfo": { "hasNextPage": false, "endCursor": null } } } }),
        ),
    ])
    .await;

    let first = render(&stub, "issue.list", json!({ "filter": {}, "after": null }));
    let sent: JsonValue = serde_json::from_slice(first.body()).expect("the body is JSON");
    assert_eq!(sent["variables"]["after"], JsonValue::Null);
    assert_eq!(sent["variables"]["first"], json!(50));
    let response = stub.send(first).await.expect("the stub answers");
    let page = linear::decode(
        operation("issue.list"),
        response.status.as_u16(),
        response.headers(),
        response.body(),
    )
    .expect("the first page decodes");

    // The cursor is published verbatim and is never parsed here.
    assert_eq!(page["has_next_page"], json!(true));
    assert_eq!(page["end_cursor"], json!(CURSOR));

    // Echoed back verbatim, it is a JSON string value in `variables` and cannot
    // reach the document.
    let cursor = page["end_cursor"].as_str().expect("a cursor is a string");
    let second = render(
        &stub,
        "issue.list",
        json!({ "filter": {}, "after": cursor }),
    );
    let sent: JsonValue = serde_json::from_slice(second.body()).expect("the body is JSON");
    assert_eq!(sent["variables"]["after"], json!(CURSOR));
    assert_eq!(
        sent["query"].as_str(),
        linear::document("issue.list"),
        "a hostile cursor cannot reach the document"
    );
    assert_eq!(sent["variables"]["first"], json!(50));
    let response = stub.send(second).await.expect("the stub answers");
    let page = linear::decode(
        operation("issue.list"),
        response.status.as_u16(),
        response.headers(),
        response.body(),
    )
    .expect("the last page decodes");
    // "Indicates if there are more results when paginating forward" is what
    // stops the walk, and the last page publishes no cursor at all.
    assert_eq!(page["has_next_page"], json!(false));
    assert_eq!(page["end_cursor"], JsonValue::Null);
    stub.assert_satisfied();

    // One call is one page: every paginated document fixes its size in the
    // declaration, and a response is bounded by the SDK's own ceiling.
    for id in ["issue.list", "team.list", "user.list"] {
        let document = linear::document(id).expect("a paginated operation carries a document");
        assert!(document.contains("$first: Int!"), "{id}");
        assert!(document.contains("$after: String"), "{id}");
    }
    assert_eq!(donat_connectors::sdk::MAX_HTTP_BODY_BYTES, 1024 * 1024);
}

/// `linear_pagination_is_bounded`: one call is one page whose size the
/// declaration owns, and the response it can bring back is bounded by the SDK's
/// own ceiling. Linear declares no `Pagination` plan at all — its continuation
/// is a GraphQL variable, which no plan in the closed set can spend — so there
/// is no loop here to bound, and that is what this case holds shut.
#[tokio::test]
async fn linear_pagination_is_bounded() {
    // 1. The page size is declaration material on every paginated operation,
    //    whatever a caller sends.
    for (id, input) in inputs() {
        let document = linear::document(id).expect("every operation carries a document");
        if !document.contains("$first") {
            continue;
        }
        let mut greedy = input.clone();
        let object = greedy
            .as_object_mut()
            .expect("the declared input is an object");
        object.insert("first".to_owned(), json!(1_000_000));
        object.insert("last".to_owned(), json!(1_000_000));
        assert_eq!(
            body(id, greedy)["variables"]["first"],
            json!(50),
            "{id} keeps the declared page size"
        );
        assert!(
            !document.contains("$last"),
            "{id} declares only forward pagination"
        );
    }

    // 2. A provider that answers with more than the SDK's ceiling is refused by
    //    the transport rather than decoded, so one call cannot bring back an
    //    unbounded page.
    let oversized = "x".repeat(donat_connectors::sdk::MAX_HTTP_BODY_BYTES + 1);
    let stub = ProviderStub::start([Expectation::new("POST", "/graphql").respond_bytes(
        200,
        format!(r#"{{"data":{{"issues":{{"nodes":["{oversized}"]}}}}}}"#),
    )])
    .await;
    let failure = stub
        .send(render(
            &stub,
            "issue.list",
            json!({ "filter": {}, "after": null }),
        ))
        .await
        .expect_err("a page past the ceiling is refused");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(
        failure.safe_message(),
        "connector provider response exceeds the declared ceiling"
    );
}

/// `linear_error_map`: every HTTP status Linear can answer with reaches exactly
/// one closed class with a Donat-owned message, and provider text never leaks.
#[tokio::test]
async fn linear_error_map() {
    let documented = [
        // "response http status code will be 400" is Linear's answer for a
        // GraphQL-level failure that is not a transport one.
        (400, ConnectorErrorClass::Validation),
        (422, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];
    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("POST", "/graphql")
            .respond_header("x-ratelimit-requests-remaining", "0")
            .respond_bytes(status, "<html>workspace acme shard db-7</html>")])
        .await;
        let response = stub
            .send(render(&stub, "issue.get", json!({ "id": ISSUE_ID })))
            .await
            .expect("the stub answers");
        let failure = linear::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!("{} {}", failure.safe_message(), failure.diagnostic());
        for leaked in ["acme", "db-7"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }

    // Every status reaches exactly one of the eight closed classes.
    let headers = HeaderMap::new();
    for status in 100_u16..=599 {
        let failure = linear::error_map().classify(status, &headers, b"not json at all");
        assert_eq!(failure.provider_status(), Some(status));
        assert!(!failure.safe_message().is_empty());
    }
}

/// `linear_effects_are_classified`: every operation carries a class, the reads
/// are read-only on Linear's own words, and every mutation is inventory-only on
/// a documented exclusion.
#[test]
fn linear_effects_are_classified() {
    let connector = linear::connector();
    let expected = [
        ("issue.get", EffectClass::ReadOnly),
        ("issue.list", EffectClass::ReadOnly),
        ("issue.create", EffectClass::AtMostOnce),
        ("issue.update", EffectClass::InventoryOnly),
        ("comment.create", EffectClass::AtMostOnce),
        ("team.list", EffectClass::ReadOnly),
        ("user.list", EffectClass::ReadOnly),
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
    assert_eq!(
        connector.admit_operation("issue.update"),
        Err(OperationRejection::InventoryOnly),
        "a documented partial update is not the class ADR 063 admits"
    );
    // Spec 016 §2 asks whether the client-supplied mutation identifier is
    // documented as deduplicating. The recorded answer is that the key Linear
    // *does* publish is somewhere else entirely, and that answer is now the
    // search an at-most-once class stands on rather than a reason to refuse.
    assert!(
        operation("issue.create")
            .effect()
            .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
            .is_some_and(|evidence| evidence
                .searched_documentation()
                .contains("OAuthApplicationCreateInput.idempotencyKey")
                && evidence
                    .searched_documentation()
                    .contains("no deduplication")),
        "the create records the documented exclusion it was admitted on"
    );
}

/// `linear_output_contract`: the declared pointers read Linear's `data`
/// envelope, and a response that does not satisfy them is a validation failure
/// rather than a null.
#[test]
fn linear_output_contract() {
    let get = operation("issue.get");
    assert_eq!(
        linear::decode(
            get,
            200,
            &HeaderMap::new(),
            &serde_json::to_vec(&success("issue.get")).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": ISSUE_ID, "identifier": "ACM-42", "title": "First",
            "description": null, "url": ISSUE_URL, "priority": 2.0,
            "state": { "id": "s1", "name": "In Progress", "type": "started" },
            "updated_at": "2026-08-10T10:00:00.000Z",
        })
    );
    // `issue(id:)` is non-null in the schema, so a `data.issue` of `null` with
    // no errors beside it does not satisfy the contract.
    assert_eq!(
        linear::decode(get, 200, &HeaderMap::new(), br#"{"data":{"issue":null}}"#,)
            .expect_err("a missing required pointer is a failure")
            .class(),
        ConnectorErrorClass::Validation
    );
}

// ---------------------------------------------------------------------------
// A small structural GraphQL reader, written here rather than taken from a
// dependency: what these documents have to satisfy is narrower than the
// language, and the point of the test is that a checked-in document cannot
// quietly grow a fragment, a directive, or an undeclared variable.
// ---------------------------------------------------------------------------

struct ParsedDocument {
    operation: String,
    name: String,
    declared_names: BTreeSet<String>,
    used: BTreeSet<String>,
}

fn parse_document(document: &str) -> Result<ParsedDocument, String> {
    // Comments are the only non-structural text these documents carry, and a
    // string literal inside one would mean a value baked into a query.
    let stripped: String = document
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    if stripped.contains('"') {
        return Err("a checked-in document carries no literal value".to_owned());
    }
    for forbidden in ["...", "@", "__schema", "__type", "fragment "] {
        if stripped.contains(forbidden) {
            return Err(format!(
                "a checked-in document must not contain `{forbidden}`"
            ));
        }
    }

    let mut tokens = stripped.split_whitespace();
    let operation = tokens
        .next()
        .ok_or_else(|| "an empty document".to_owned())?
        .to_owned();
    if operation != "query" && operation != "mutation" {
        return Err(format!("unexpected operation keyword `{operation}`"));
    }
    let rest = stripped
        .trim_start()
        .strip_prefix(&operation)
        .ok_or_else(|| "the operation keyword is not first".to_owned())?
        .trim_start();
    let header_end = rest
        .find('(')
        .ok_or_else(|| "an operation with no variable definitions".to_owned())?;
    let name = rest[..header_end].trim().to_owned();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!(
            "an operation name must be a plain word, got `{name}`"
        ));
    }

    let close = matching(rest, header_end, '(', ')')?;
    let header = &rest[header_end + 1..close];
    let mut declared_names = BTreeSet::new();
    for definition in header.split(',') {
        let definition = definition.trim();
        if definition.is_empty() {
            continue;
        }
        let (variable, kind) = definition
            .split_once(':')
            .ok_or_else(|| format!("a variable definition needs a type: `{definition}`"))?;
        let variable = variable
            .trim()
            .strip_prefix('$')
            .ok_or_else(|| format!("a variable definition starts with `$`: `{definition}`"))?;
        if kind.trim().is_empty() {
            return Err(format!(
                "a variable definition needs a type: `{definition}`"
            ));
        }
        if !declared_names.insert(variable.to_owned()) {
            return Err(format!("`${variable}` is declared twice"));
        }
    }

    // The selection set: one balanced block, and nothing after it.
    let body_start = rest[close..]
        .find('{')
        .map(|offset| close + offset)
        .ok_or_else(|| "an operation with no selection set".to_owned())?;
    let body_end = matching(rest, body_start, '{', '}')?;
    if !rest[body_end + 1..].trim().is_empty() {
        return Err("a document declares exactly one operation".to_owned());
    }

    let mut used = BTreeSet::new();
    let selection = &rest[body_start..=body_end];
    let bytes: Vec<char> = selection.chars().collect();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == '$' {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == '_') {
                end += 1;
            }
            if end == start {
                return Err("a bare `$` is not a variable".to_owned());
            }
            used.insert(bytes[start..end].iter().collect::<String>());
            index = end;
        } else {
            index += 1;
        }
    }

    Ok(ParsedDocument {
        operation,
        name,
        declared_names,
        used,
    })
}

/// The index of the delimiter matching the one at `open`.
fn matching(text: &str, open: usize, opening: char, closing: char) -> Result<usize, String> {
    let mut depth = 0usize;
    for (index, character) in text.char_indices().skip_while(|(index, _)| *index < open) {
        if character == opening {
            depth += 1;
        } else if character == closing {
            depth -= 1;
            if depth == 0 {
                return Ok(index);
            }
        }
    }
    Err(format!("unbalanced `{opening}`"))
}
