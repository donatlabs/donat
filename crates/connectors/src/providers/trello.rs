//! Trello's REST API v1 — the batch's board-and-card surface, and its one
//! two-value credential.
//!
//! Ground truth is Trello's own published documentation and its own published
//! OpenAPI description, read on 2026-08-10:
//!
//! * <https://developer.atlassian.com/cloud/trello/guides/rest-api/authorization/>
//!   — "`https://api.trello.com/1/members/me?key={{apiKey}}&token={{apiToken}}`",
//!   the one form a deployment can send without OAuth 1.0a request signing.
//! * <https://developer.atlassian.com/cloud/trello/guides/rest-api/rate-limits/>
//!   — "300 requests per 10 seconds for each API key" and "100 requests per 10
//!   second interval for each token", answered with a `429`, and the
//!   `x-rate-limit-api-key-*` / `x-rate-limit-api-token-*` headers. Trello
//!   publishes **no** `Retry-After`.
//! * The published OpenAPI
//!   (<https://developer.atlassian.com/cloud/trello/swagger.v3.json>,
//!   `servers: - url: https://api.trello.com/1`) for every path, method, and
//!   parameter named below, including "Create a new card. Query parameters may
//!   also be replaced with a JSON request body instead."
//!
//! # The credential is two secrets on the query string
//!
//! Trello's key names the application and its token names the authorization,
//! and neither authenticates alone. Both are secrets, so neither may live in the
//! declaration — which is why this connector declares
//! [`AuthPlan::api_key_query_pair`] rather than the SDK's single-value query
//! plan (`knowledgebase/declarative-saas/decisions/066-*`). The rendered URL
//! then carries two credentials, so it is marked: `RequestPlan`'s `Debug`, its
//! `redacted_url`, and every diagnostic built from either print the origin
//! instead of the query.
//!
//! # No walk at all
//!
//! Trello publishes no continuation any SDK plan can end on. Its card
//! collections take neither a cursor nor a page number: `GET /1/lists/{id}/cards`
//! declares one parameter, its own list id, and the filtered forms take `before`
//! and `since` keyed on **object ids the caller already has**. The search takes
//! `cards_page`, but Trello publishes it as a bounded page index — "The page of
//! results for cards. Maximum: 100" — with no field that says whether another
//! page exists. So every collection here is one request, its size Trello's own,
//! and this module declares no [`Pagination`](crate::sdk::Pagination) plan
//! (`knowledgebase/declarative-saas/decisions/065-*`).
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur once in
//! Trello's published OpenAPI description — 262 KB covering every endpoint and
//! parameter — nor in its authorization or rate-limit guides. `card.create` and
//! `comment.add` are therefore `AtMostOnce` (ADR 063).
//!
//! `card.update` is `InventoryOnly`: every parameter of Trello's `PUT /1/cards/{id}`
//! is optional, which is the shape of a partial update rather than a write of a
//! complete representation, and Trello publishes nothing at all about repeating
//! one. `card.delete` is `InventoryOnly` for the reason
//! `salesforce.record.delete` is: "Delete a Card" is what the first send does,
//! and Trello publishes no sentence about the second.
//!
//! One near-miss is recorded because a reviewer will find it. Trello's *other*
//! authorization form is OAuth 1.0a, whose `oauth_nonce` "is a random string,
//! uniquely generated for each request" — a signature replay guard on the
//! transport, not an application-level deduplication of a card create, and not a
//! value this SDK can produce at all.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "trello";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "`https://api.trello.com/1/members/me`" — the origin, with the `/1` version
/// prefix left to each declared path.
const ORIGIN: &str = "https://api.trello.com";

/// The query parameter naming the application, filled from the resolved
/// credential's `api_key` field.
pub const KEY_PARAM: &str = "key";

/// The query parameter naming the authorization, filled from the resolved
/// credential's `secret` field.
pub const TOKEN_PARAM: &str = "token";

/// "The maximum number of cards to return. Maximum: 1000."  A search is one
/// request, so its size is declaration material rather than a caller's.
const SEARCH_CARD_LIMIT: &str = "100";

/// The credential plan this connector declares, exposed so a test and the
/// serving module apply exactly the declaration's own form.
pub fn auth_plan() -> AuthPlan {
    AuthPlan::api_key_query_pair(KEY_PARAM, TOKEN_PARAM)
        .expect("Trello's published credential form is valid")
}

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Trello's published origin is valid"))
            .credential(CredentialSpec::for_plan(auth_plan()))
            .operations(operations().expect("the Trello declarations are valid"))
            .build()
            .expect("the Trello declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// It reads no body pointer, and that is a finding rather than an omission:
/// Trello answers a rejected request with a bare string body — `invalid id`,
/// `unauthorized card permission requested` — with no JSON envelope and no
/// machine-readable code anywhere in its published description. The status is
/// the whole contract.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 409, 410], ConnectorErrorClass::Permanent)
            // "you will receive a 429". Trello publishes no `Retry-After`, so a
            // Process backs off on the engine's own schedule.
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Trello error map is a valid declaration")
    });
    &MAP
}

/// Decode one Trello response: the declared success statuses, then the declared
/// contract.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    operation.decode_response(status, body)
}

/// No operation of this connector declares a continuation plan, and the module
/// header says why.
///
/// It is named explicitly rather than left out, because a module that declares
/// no lookup at all acquires or loses a walk by omission
/// (`knowledgebase/declarative-saas/decisions/058-*`).
pub const fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .static_header("Accept", "application/json")
        .success_statuses([StatusCode::OK])
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur once in Trello's published OpenAPI \
                      description — 262 KB covering every endpoint, parameter, and schema — nor in \
                      its authorization or rate-limit guides: no request header, no query \
                      parameter, and no body attribute carries a client-supplied request \
                      identifier or a deduplication behaviour. Trello's OAuth 1.0a `oauth_nonce` \
                      is a per-request signature replay guard on its other authorization form, not \
                      an application-level deduplication of a create";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The published card attributes a Process reads.
fn card_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("desc", "/desc", ValueScalar::String, Required::No)
        .output_pointer("closed", "/closed", ValueScalar::Boolean, Required::No)
        .output_pointer("id_list", "/idList", ValueScalar::String, Required::No)
        .output_pointer("id_board", "/idBoard", ValueScalar::String, Required::No)
        .output_pointer("due", "/due", ValueScalar::String, Required::No)
        .output_pointer(
            "due_complete",
            "/dueComplete",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer("url", "/url", ValueScalar::String, Required::No)
        .output_pointer(
            "date_last_activity",
            "/dateLastActivity",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Get a card by its ID."
    let card_get = card_output(
        common(Operation::get("card.get", "/1/cards/{id}")).path_param("id", ValueScalar::String),
    )
    .effect(Effect::read_only())
    .build()?;

    // "List the cards in a list." The collection is a bare JSON array at the
    // document root, so the whole document is the output.
    let card_list = common(Operation::get("card.list", "/1/lists/{id}/cards"))
        .path_param("id", ValueScalar::String)
        .declared_output("items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    // "Find what you're looking for in Trello." `modelTypes` is a literal
    // because this operation searches cards: a caller that could widen it would
    // be choosing the shape of the declared output.
    let card_search = common(Operation::get("card.search", "/1/search"))
        .query_input("query", "query")
        .query_input("idBoards", "id_boards")
        .query_static("modelTypes", "cards")
        .query_static("cards_limit", SEARCH_CARD_LIMIT)
        .output_pointer("cards", "/cards", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    // "Create a new card. Query parameters may also be replaced with a JSON
    // request body instead", which is the form declared here: a JSON body is one
    // template the SDK renders, where a query would be a dozen declared keys.
    let card_create = card_output(
        common(Operation::post("card.create", "/1/cards"))
            .body(JsonTemplate::object([
                ("idList", JsonTemplate::input("id_list")),
                ("name", JsonTemplate::input("name")),
                ("desc", JsonTemplate::input("desc")),
                ("pos", JsonTemplate::input("pos")),
                ("due", JsonTemplate::input("due")),
                ("idMembers", JsonTemplate::input("id_members")),
                ("idLabels", JsonTemplate::input("id_labels")),
            ]))
            .declared_input("id_list", ValueScalar::String, Required::Yes)
            .declared_input("id_members", ValueScalar::Json, Required::Yes)
            .declared_input("id_labels", ValueScalar::Json, Required::Yes),
    )
    .effect(at_most_once(
        "a second card at the same position in the same list, with a new id — and, for a list a \
         board's automation watches, a second run of whatever that automation does",
    )?)
    .build()?;

    let card_update = card_output(
        common(Operation::put("card.update", "/1/cards/{id}"))
            .path_param("id", ValueScalar::String)
            .body(JsonTemplate::object([
                ("name", JsonTemplate::input("name")),
                ("desc", JsonTemplate::input("desc")),
                ("closed", JsonTemplate::input("closed")),
                ("idList", JsonTemplate::input("id_list")),
                ("due", JsonTemplate::input("due")),
                ("dueComplete", JsonTemplate::input("due_complete")),
            ])),
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    let card_delete = common(Operation::delete("card.delete", "/1/cards/{id}"))
        .path_param("id", ValueScalar::String)
        .declared_output("deleted", ValueScalar::Json, Required::Yes)
        .effect(Effect::inventory_only(SILENT_DELETE)?)
        .build()?;

    // "Add a new comment to a card." `text` is a required query parameter here;
    // the JSON-body alternative Trello publishes for cards is not published for
    // this endpoint, so the declaration sends what the endpoint documents.
    let comment_add = common(Operation::post(
        "comment.add",
        "/1/cards/{id}/actions/comments",
    ))
    .path_param("id", ValueScalar::String)
    .query_input("text", "text")
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("type", "/type", ValueScalar::String, Required::No)
    .output_pointer("date", "/date", ValueScalar::String, Required::No)
    .output_pointer("data", "/data", ValueScalar::Json, Required::No)
    .effect(at_most_once(
        "a second comment on the same card, with a new action id, and a second notification to \
         every member watching it",
    )?)
    .build()?;

    // "Actions on a Card." `filter` is a literal because this operation reads
    // comments: `commentCard` is the action type Trello publishes for them.
    let comment_list = common(Operation::get("comment.list", "/1/cards/{id}/actions"))
        .path_param("id", ValueScalar::String)
        .query_static("filter", "commentCard")
        .declared_output("items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let board_get = common(Operation::get("board.get", "/1/boards/{id}"))
        .path_param("id", ValueScalar::String)
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("desc", "/desc", ValueScalar::String, Required::No)
        .output_pointer("closed", "/closed", ValueScalar::Boolean, Required::No)
        .output_pointer("url", "/url", ValueScalar::String, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // "Get the Lists on a Board", a bare JSON array at the document root.
    let list_list = common(Operation::get("list.list", "/1/boards/{id}/lists"))
        .path_param("id", ValueScalar::String)
        .query_input("filter", "filter")
        .declared_output("items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        card_get,
        card_list,
        card_search,
        card_create,
        card_update,
        card_delete,
        comment_add,
        comment_list,
        board_get,
        list_list,
    ])
}

/// The reason the update carries.
const PARTIAL_UPDATE: &str = "every parameter of Trello's `PUT /1/cards/{id}` is published as \
                              optional — \"Update a card\" with `name`, `desc`, `closed`, `idList`, \
                              `due`, and the rest each `required: false` in its own OpenAPI \
                              description — which is a partial update rather than a write of a \
                              complete representation, and Trello publishes no statement about \
                              repeating one. Neither spec 010 §7's NaturalMethod evidence nor ADR \
                              063's consequence is there to cite";

/// The reason the delete carries.
const SILENT_DELETE: &str = "Trello publishes \"Delete a Card\" and a `200`, which is what the \
                             first send does, and nothing at all about a second one. Spec 010 §7 \
                             admits NaturalMethod on the provider's own repeat statement, and the \
                             shape of the request is not that statement";
