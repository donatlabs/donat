//! monday.com connector proofs (spec 024 §3, which adopts spec 023 §4), against
//! the SDK's local provider stub.
//!
//! monday is the batch's GraphQL connector, so two proofs are its own: every
//! checked-in document parses and names only its declared variables, and a `200`
//! carrying a GraphQL error never reads as a success.

use std::collections::BTreeSet;
use std::time::Duration;

use donat_connectors::providers::monday;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

const BOARD_ID: &str = "1234567890";
const ITEM_ID: &str = "9876543210";

fn operation(id: &str) -> &'static Operation {
    monday::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the monday declaration publishes {id}"))
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

/// The rendered body of one operation, as JSON.
fn body(id: &str, input: JsonValue) -> JsonValue {
    let request = operation(id)
        .plan_request(
            &donat_connectors::sdk::Origin::parse("https://api.monday.com")
                .expect("a valid origin"),
            &input,
        )
        .expect("the declared request renders");
    serde_json::from_slice(request.body()).expect("a rendered GraphQL body is JSON")
}

/// Every operation, with an input that satisfies it.
fn inputs() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("item.get", json!({ "ids": [ITEM_ID] })),
        (
            "item.list",
            json!({ "board_id": BOARD_ID, "cursor": JsonValue::Null }),
        ),
        (
            "item.search",
            json!({
                "board_id": BOARD_ID,
                "query_params": { "rules": [{ "column_id": "status", "compare_value": [1] }] },
            }),
        ),
        (
            "item.create",
            json!({
                "board_id": BOARD_ID, "group_id": "group_one", "item_name": "new item",
                "column_values": "{\"date\":\"2026-08-20\"}",
            }),
        ),
        (
            "item.update",
            json!({
                "board_id": BOARD_ID, "item_id": ITEM_ID,
                "column_values": "{\"status\":{\"index\":1}}",
            }),
        ),
        ("item.delete", json!({ "item_id": ITEM_ID })),
        (
            "update.create",
            json!({ "item_id": ITEM_ID, "body": "Looks right to me." }),
        ),
        ("update.list", json!({ "ids": [ITEM_ID] })),
        ("board.get", json!({ "ids": [BOARD_ID] })),
        ("board.list", json!({ "page": 1 })),
    ]
}

/// The documented success body of one operation.
fn success(id: &str) -> JsonValue {
    match id {
        "item.get" | "update.list" => json!({ "data": { "items": [{ "id": ITEM_ID }] } }),
        "item.list" | "item.search" => json!({
            "data": { "boards": [{ "id": BOARD_ID, "items_page": {
                "cursor": null, "items": [{ "id": ITEM_ID, "name": "new item" }] } }] }
        }),
        "item.create" => json!({
            "data": { "create_item": { "id": ITEM_ID, "name": "new item",
                                       "url": "https://example.monday.com/boards/1/pulses/2" } }
        }),
        "item.update" => json!({ "data": { "change_multiple_column_values": { "id": ITEM_ID } } }),
        "item.delete" => json!({ "data": { "delete_item": { "id": ITEM_ID } } }),
        "update.create" => json!({
            "data": { "create_update": { "id": "111", "body": "Looks right to me.",
                                         "created_at": "2026-08-02T11:56:51Z" } }
        }),
        "board.get" | "board.list" => json!({ "data": { "boards": [{ "id": BOARD_ID }] } }),
        other => panic!("no documented success fixture for {other}"),
    }
}

/// `monday_request_shape`: one endpoint, one method, one checked-in document per
/// operation, and the pinned API version on every request.
#[tokio::test]
async fn monday_request_shape() {
    let stub = ProviderStub::start(
        inputs()
            .into_iter()
            .map(|(id, _)| {
                Expectation::new("POST", "/v2")
                    .query("")
                    .header("authorization", SECRET_SENTINEL)
                    .header("api-version", monday::API_VERSION)
                    .header("content-type", "application/json")
                    .respond_json(200, success(id))
            })
            .collect::<Vec<_>>(),
    )
    .await;

    for (id, input) in inputs() {
        let request = render(&stub, id, input.clone());
        assert_eq!(request.method().as_str(), "POST", "{id}");
        assert_eq!(request.url().path(), "/v2", "{id}");

        let sent: JsonValue = serde_json::from_slice(request.body()).expect("a JSON body");
        // 1. The document is the checked-in constant, byte for byte.
        assert_eq!(
            sent["query"].as_str(),
            monday::document(id),
            "{id} sends its own checked-in document"
        );
        // 2. The body carries nothing but the document and its variables.
        assert_eq!(
            sent.as_object()
                .expect("a GraphQL body is an object")
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["query", "variables"]),
            "{id} sends only the document and its variables"
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();

    // 3. No caller value can reach the document: an input named `query` is not a
    //    slot the template reads, so the document is unchanged.
    let mut poisoned = json!({ "ids": [ITEM_ID] });
    poisoned["query"] = json!("query { account { id } }");
    assert_eq!(
        body("item.get", poisoned)["query"].as_str(),
        monday::document("item.get")
    );
}

/// `monday_documents_parse_and_name_only_declared_variables` (spec 024 §1): each
/// document is one operation, carries no literal value, and binds exactly the
/// variables the module fills.
#[test]
fn monday_documents_parse_and_name_only_declared_variables() {
    for (id, document) in monday::documents() {
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
    // Every published operation has a document, and every document a published
    // operation.
    assert_eq!(
        monday::documents().len(),
        monday::connector().operations().len()
    );
    for operation in monday::connector().operations() {
        assert!(
            monday::document(operation.id()).is_some(),
            "{} carries a checked-in document",
            operation.id()
        );
    }
}

/// `monday_auth_is_applied`: the token *is* the `Authorization` value, with no
/// scheme in front of it, and it appears nowhere else.
#[tokio::test]
async fn monday_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("POST", "/v2")
        .header("authorization", SECRET_SENTINEL)
        .respond_json(200, success("item.get"))])
    .await;

    let request = render(&stub, "item.get", json!({ "ids": [ITEM_ID] }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("the token is ASCII"),
        SECRET_SENTINEL,
        "monday publishes the bare token, and `Bearer` would authenticate as nobody"
    );
    assert!(!request.url_carries_credential());
    assert!(!format!("{request:?}").contains(SECRET_SENTINEL));
    assert!(!format!("{:?}", monday::connector().credential()).contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!("{:?}", monday::error_map().classify_response(&response));
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `monday_graphql_errors_are_failures`: a `200` carrying a provider error never
/// becomes a success, and its code maps to exactly one class.
#[test]
fn monday_graphql_errors_are_failures() {
    let headers = HeaderMap::new();
    for (id, _) in inputs() {
        let operation = operation(id);

        // The documented success still decodes.
        monday::decode(
            operation,
            200,
            &headers,
            &serde_json::to_vec(&success(id)).expect("a fixture body serializes"),
        )
        .unwrap_or_else(|error| panic!("{id} documented success decodes: {error:?}"));

        // "2xx (200 OK): Application-level errors from platform restrictions."
        let failure = monday::decode(
            operation,
            200,
            &headers,
            br#"{"errors":[{"message":"Board not found","extensions":{"code":"ResourceNotFoundException","status_code":404}}]}"#,
        )
        .expect_err("a 200 carrying GraphQL errors is never a success");
        assert_eq!(failure.class(), ConnectorErrorClass::Permanent, "{id}");
        assert_eq!(failure.provider_status(), Some(200), "{id}");

        // A partial answer — data *and* errors — is a failure rather than a
        // success with holes in it.
        let mut partial = success(id);
        partial
            .as_object_mut()
            .expect("a success fixture is an object")
            .insert(
                "errors".to_owned(),
                json!([{ "message": "column failed",
                         "extensions": { "code": "ColumnValueException" } }]),
            );
        let failure = monday::decode(
            operation,
            200,
            &headers,
            &serde_json::to_vec(&partial).expect("a fixture body serializes"),
        )
        .expect_err("a partial answer is not the declared output contract");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation, "{id}");

        // An envelope with neither `data` nor `errors` is outside the contract.
        assert_eq!(
            monday::decode(operation, 200, &headers, b"{}")
                .expect_err("an envelope-less body is not a success")
                .class(),
            ConnectorErrorClass::Invariant,
            "{id}"
        );
        // And malformed bytes are a validation failure, never a panic.
        assert_eq!(
            monday::decode(operation, 200, &headers, b"not json")
                .expect_err("malformed JSON is not a success")
                .class(),
            ConnectorErrorClass::Validation,
            "{id}"
        );
    }
}

/// `monday_error_map`: every documented code and status reaches exactly one
/// closed class, and none of monday's prose crosses the boundary.
#[test]
fn monday_error_map() {
    let headers = HeaderMap::new();
    let documented = [
        (
            200,
            "COMPLEXITY_BUDGET_EXHAUSTED",
            ConnectorErrorClass::Http429,
        ),
        (429, "maxConcurrencyExceeded", ConnectorErrorClass::Http429),
        (429, "IP_RATE_LIMIT_EXCEEDED", ConnectorErrorClass::Http429),
        (200, "API_TEMPORARILY_BLOCKED", ConnectorErrorClass::Http429),
        (409, "IDEMPOTENCY_CONFLICT", ConnectorErrorClass::Http429),
        (401, "Unauthorized", ConnectorErrorClass::Authentication),
        (
            403,
            "UserUnauthorizedException",
            ConnectorErrorClass::Authentication,
        ),
        (
            403,
            "USER_ACCESS_DENIED",
            ConnectorErrorClass::Authentication,
        ),
        (
            200,
            "missingRequiredPermissions",
            ConnectorErrorClass::Authentication,
        ),
        (200, "ColumnValueException", ConnectorErrorClass::Validation),
        (
            200,
            "InvalidBoardIdException",
            ConnectorErrorClass::Validation,
        ),
        (
            422,
            "RecordInvalidException",
            ConnectorErrorClass::Validation,
        ),
        (
            200,
            "ItemsLimitationException",
            ConnectorErrorClass::Permanent,
        ),
        (
            404,
            "ResourceNotFoundException",
            ConnectorErrorClass::Permanent,
        ),
        // A status with no code monday publishes still lands in one class.
        (500, "not_a_published_code", ConnectorErrorClass::Http5xx),
        (418, "not_a_published_code", ConnectorErrorClass::Permanent),
    ];

    for (status, code, expected) in documented {
        let body = serde_json::to_vec(&json!({
            "errors": [{
                "message": format!("account {SECRET_SENTINEL} is blocked"),
                "extensions": { "code": code, "status_code": status },
            }],
            "account_id": 1234,
        }))
        .expect("a fixture body serializes");

        let failure = monday::error_map().classify(status, &headers, &body);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "is blocked", code] {
            if leaked == "not_a_published_code" {
                continue;
            }
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
    }
}

/// `monday_rate_limit_is_classified`: the documented rate-limit response reaches
/// `http_429` with its published retry hint, clamped.
#[tokio::test]
async fn monday_rate_limit_is_classified() {
    let limited = json!({
        "errors": [{ "message": "Complexity budget exhausted",
                     "extensions": { "code": "COMPLEXITY_BUDGET_EXHAUSTED" } }]
    });
    let stub = ProviderStub::start([
        Expectation::new("POST", "/v2")
            .respond_header("retry-after", "27")
            .respond_json(429, limited.clone()),
        Expectation::new("POST", "/v2")
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "item.get", json!({ "ids": [ITEM_ID] })))
            .await
            .expect("the stub answers");
        failures.push(monday::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(27)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `monday_pagination_is_bounded`: no operation declares a continuation plan,
/// because monday's cursor is a GraphQL variable in the request body — and the
/// page size the caller cannot choose is a literal in the document.
#[test]
fn monday_pagination_is_bounded() {
    for (id, _) in inputs() {
        assert!(
            monday::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    // The page size is declaration material: a caller cannot ask one call for
    // the whole board.
    let variables = body(
        "item.list",
        json!({ "board_id": BOARD_ID, "cursor": JsonValue::Null }),
    )["variables"]
        .clone();
    assert_eq!(variables["limit"], json!(100));
    assert_eq!(variables["cursor"], JsonValue::Null);

    // And the cursor a page returns is a declared *output* the caller echoes
    // back, which is what makes the walk the Process's own.
    let page = operation("item.list")
        .extract_output(&json!({
            "data": { "boards": [{ "items_page": { "cursor": "MSwxMDA", "items": [] } }] }
        }))
        .expect("the declared contract is satisfied");
    assert_eq!(page["cursor"], json!("MSwxMDA"));
}

/// `monday_effects_are_classified`: every operation carries a class, and the
/// evidence records the key monday publishes and the escape clause that keeps it
/// out of `ExplicitKey`.
#[test]
fn monday_effects_are_classified() {
    let connector = monday::connector();
    let expected = [
        ("item.get", EffectClass::ReadOnly),
        ("item.list", EffectClass::ReadOnly),
        ("item.search", EffectClass::ReadOnly),
        ("item.create", EffectClass::AtMostOnce),
        ("item.update", EffectClass::InventoryOnly),
        ("item.delete", EffectClass::InventoryOnly),
        ("update.create", EffectClass::AtMostOnce),
        ("update.list", EffectClass::ReadOnly),
        ("board.get", EffectClass::ReadOnly),
        ("board.list", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        // The class is not `ExplicitKey`, so nothing binds monday's header and
        // no runtime writes one.
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        connector.admit_operation("item.delete"),
        Err(OperationRejection::InventoryOnly)
    );

    let evidence = operation("item.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    let searched = evidence.searched_documentation();
    assert!(searched.contains("Idempotency-Key"), "{searched}");
    assert!(searched.contains("30 minutes"), "{searched}");
    assert!(searched.contains("budget is exceeded"), "{searched}");
    assert!(evidence.repeat_produces().contains("a second item"));
}

/// `monday_output_contract`: the declared pointers read monday's own envelope,
/// and a nested page is read where the document put it.
#[test]
fn monday_output_contract() {
    let headers = HeaderMap::new();
    assert_eq!(
        monday::decode(
            operation("item.create"),
            200,
            &headers,
            &serde_json::to_vec(&success("item.create")).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": ITEM_ID, "name": "new item",
            "url": "https://example.monday.com/boards/1/pulses/2",
        })
    );
    // monday's ids are strings in this version's schema, and a number is not
    // one.
    assert_eq!(
        monday::decode(
            operation("item.create"),
            200,
            &headers,
            br#"{"data":{"create_item":{"id":9876543210}}}"#,
        )
        .expect_err("a numeric id is not the declared string")
        .class(),
        ConnectorErrorClass::Validation
    );
    // A board query that matched nothing has no page to read, which is a
    // contract failure rather than an empty success: the declaration promised
    // the page.
    assert_eq!(
        monday::decode(
            operation("item.list"),
            200,
            &headers,
            br#"{"data":{"boards":[]}}"#,
        )
        .expect_err("an absent page does not satisfy the declared contract")
        .class(),
        ConnectorErrorClass::Validation
    );
    // An undeclared status never reaches the pointers at all.
    assert_eq!(
        monday::decode(operation("item.get"), 204, &headers, b"")
            .expect_err("204 is not a declared success")
            .class(),
        ConnectorErrorClass::Permanent
    );
}

// ===========================================================================
// The document parser these proofs read with. It is deliberately strict: a
// checked-in document is a constant, so anything that would make it dynamic —
// a string literal, a fragment spread, a directive, an introspection field —
// is a parse failure rather than a warning.
// ===========================================================================

struct ParsedDocument {
    operation: String,
    name: String,
    declared_names: BTreeSet<String>,
    used: BTreeSet<String>,
}

fn parse_document(document: &str) -> Result<ParsedDocument, String> {
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

    let body_start = rest[close..]
        .find('{')
        .map(|offset| close + offset)
        .ok_or_else(|| "an operation with no selection set".to_owned())?;
    let body_end = matching(rest, body_start, '{', '}')?;
    if !rest[body_end + 1..].trim().is_empty() {
        return Err("a document declares exactly one operation".to_owned());
    }

    let mut used = BTreeSet::new();
    let selection: Vec<char> = rest[body_start..=body_end].chars().collect();
    let mut index = 0;
    while index < selection.len() {
        if selection[index] == '$' {
            let start = index + 1;
            let mut end = start;
            while end < selection.len()
                && (selection[end].is_ascii_alphanumeric() || selection[end] == '_')
            {
                end += 1;
            }
            if end == start {
                return Err("a bare `$` is not a variable".to_owned());
            }
            used.insert(selection[start..end].iter().collect::<String>());
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
