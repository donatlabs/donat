//! monday.com's platform API — one GraphQL endpoint, ten checked-in documents,
//! and the batch's one published idempotency key that does not qualify.
//!
//! Ground truth is monday.com's own published documentation, read on 2026-08-10:
//!
//! * <https://developer.monday.com/api-reference/docs/authentication> — the
//!   endpoint `https://api.monday.com/v2`, `Content-Type: application/json`, and
//!   the credential form `"Authorization": "{API_TOKEN}"` — **no scheme in front
//!   of the token**, which every worked example on the site repeats.
//! * <https://developer.monday.com/api-reference/docs/api-versioning> — "We
//!   guarantee at least **three** different versions of the API in parallel and
//!   release a new version every quarter", with `2026-07` published as the
//!   *current* (stable) version and "Used as the default version when no header
//!   is passed" for it.
//! * <https://developer.monday.com/api-reference/docs/errors> — the error
//!   envelope: an `errors` array whose entries carry `message`, `path`,
//!   `extensions.code`, and `extensions.status_code`, and monday's own note that
//!   a `200` can carry one: "2xx (200 OK): Application-level errors from
//!   platform restrictions".
//! * <https://developer.monday.com/api-reference/docs/rate-limits> — the
//!   complexity budget, the per-minute and concurrency limits, the
//!   `COMPLEXITY_BUDGET_EXHAUSTED`, `maxConcurrencyExceeded`, and
//!   `IP_RATE_LIMIT_EXCEEDED` codes at `429`, and the `Retry-After` header.
//! * <https://developer.monday.com/api-reference/docs/idempotency> — quoted in
//!   full below.
//! * The reference pages for `items`, `items_page`, `columns`, `updates`, and
//!   `boards` for every field, argument, and input type these documents name.
//!
//! # A document is a constant
//!
//! Each operation carries one checked-in `.graphql` document, included at
//! compile time from `providers/monday/queries/`, and binds **only** typed
//! variables into `variables`. The `query` leaf of the request body is a
//! [`JsonTemplate::literal`], so there is no input name a caller could fill to
//! supply a query, a fragment, an alias, a directive, or one more field in a
//! selection set. The page size is declaration material for the same reason.
//! This is `linear.rs`'s shape, followed exactly.
//!
//! # A `200` is not a success
//!
//! monday answers a rejected request with a `200` and reports what happened in
//! the GraphQL `errors` array — its own error guide lists twelve codes that
//! arrive that way, from `ItemsLimitationException` to
//! `missingRequiredPermissions`. [`decode`] therefore refuses any response
//! carrying a non-empty `errors`, whatever the status and whatever `data` is
//! beside it.
//!
//! # The cursor is a variable, not a pagination plan
//!
//! `items_page.cursor` is a GraphQL field and `cursor` is a GraphQL variable, so
//! the continuation lives in the request *body* and no SDK pagination plan can
//! spend it. `item.list` therefore declares `cursor` as an input the caller
//! echoes back verbatim and publishes the next `cursor` as an output; one call
//! is one page (`knowledgebase/declarative-saas/decisions/055-*`). `board.list`
//! is monday's other regime — "page — The page number to return. Starts at 1" —
//! and is declared the same way, because the SDK's page-number plan spends its
//! page in the query string and this one is a variable too.
//!
//! # The idempotency key, and why the class is not `ExplicitKey`
//!
//! monday publishes a real one: "Send a unique `Idempotency-Key` header with any
//! mutation request", "First request: Executes normally and caches the response
//! for 30 minutes", "Retry with same key: Returns the cached response with an
//! `Idempotency-Replayed: true` header — no duplicate side effect occurs", and a
//! `409` with `IDEMPOTENCY_CONFLICT` for a concurrent duplicate.
//!
//! It is still not `ExplicitKey`, and the reason is one row of monday's own
//! rules table: "**Per-user budget** — Each user+app combination has a memory
//! budget for cached responses. **If the budget is exceeded, new responses will
//! execute but won't be cached for replay.**" `ExplicitKeyEvidence` is built
//! from a documented *minimum* retention with a clock safety margin strictly
//! under it (ADR 042), and a retention a provider publishes an unquantified
//! escape clause for has a minimum of zero: the connector cannot observe whether
//! the budget was exceeded, so a class that told the durable runtime "send it
//! again, monday will absorb it" would be a promise monday explicitly declines
//! to make. The mutations here are `AtMostOnce` instead — never twice, sometimes
//! never — and the whole mechanism is recorded in `providers/INVENTORY.md`. See
//! `knowledgebase/declarative-saas/decisions/067-*`.
//!
//! # Effect classification
//!
//! `item.create` and `update.create` are `AtMostOnce` (ADR 063) on the evidence
//! above. `item.update` is `InventoryOnly`: `change_multiple_column_values`
//! writes the column values it is given and leaves every other column alone, so
//! a repeat sets the same values to the same things. `item.delete` is
//! `InventoryOnly` too — monday publishes "Deletes an item (or subitem) and its
//! nested subitems" and nothing about a second send, and a GraphQL mutation is a
//! `POST`, which spec 010 §7 does not admit for `NaturalMethod`.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};

/// The connector name a deployment selects.
pub const NAME: &str = "monday";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "the endpoint `https://api.monday.com/v2`".
const ORIGIN: &str = "https://api.monday.com";

/// The one path every operation of this connector renders.
const PATH: &str = "/v2";

/// The API version this declaration is written against.
///
/// monday publishes `2026-07` as the *current* stable version — "Only bug fixes
/// with no breaking changes", "Anything built using this won't change for at
/// least six months" — and publishes that omitting the header means the default,
/// which moves every quarter. A declaration that let the schema move under it
/// would be a declaration nobody could review, so the version is pinned here and
/// changing it is a change to this connector.
pub const API_VERSION: &str = "2026-07";

/// "limit — The number of items returned. The default is 25, and the maximum is
/// 100" (`items`), and `items_page` takes the same argument.
const PAGE_SIZE: i64 = 100;

// The checked-in documents. Each is a compile-time constant, and it is the only
// thing that ever reaches monday's `query` field.
const ITEM_GET: &str = include_str!("monday/queries/item_get.graphql");
const ITEM_LIST: &str = include_str!("monday/queries/item_list.graphql");
const ITEM_SEARCH: &str = include_str!("monday/queries/item_search.graphql");
const ITEM_CREATE: &str = include_str!("monday/queries/item_create.graphql");
const ITEM_UPDATE: &str = include_str!("monday/queries/item_update.graphql");
const ITEM_DELETE: &str = include_str!("monday/queries/item_delete.graphql");
const UPDATE_CREATE: &str = include_str!("monday/queries/update_create.graphql");
const UPDATE_LIST: &str = include_str!("monday/queries/update_list.graphql");
const BOARD_GET: &str = include_str!("monday/queries/board_get.graphql");
const BOARD_LIST: &str = include_str!("monday/queries/board_list.graphql");

/// The document one operation sends, for a reviewer and for the test that reads
/// every one of them.
pub fn document(operation_id: &str) -> Option<&'static str> {
    documents()
        .iter()
        .find(|(id, _)| *id == operation_id)
        .map(|(_, document)| *document)
}

/// Every operation this connector publishes, with the document it sends.
pub fn documents() -> &'static [(&'static str, &'static str)] {
    &[
        ("item.get", ITEM_GET),
        ("item.list", ITEM_LIST),
        ("item.search", ITEM_SEARCH),
        ("item.create", ITEM_CREATE),
        ("item.update", ITEM_UPDATE),
        ("item.delete", ITEM_DELETE),
        ("update.create", UPDATE_CREATE),
        ("update.list", UPDATE_LIST),
        ("board.get", BOARD_GET),
        ("board.list", BOARD_LIST),
    ]
}

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("monday's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::authorization_credential(),
            ))
            .operations(operations().expect("the monday declarations are valid"))
            .build()
            .expect("the monday declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// The code pointer is `extensions.code` of the first error, which is the field
/// monday publishes as machine-readable — "`extensions.code`: error code
/// identifier" — and whose values its error guide lists per status. The status
/// rules underneath answer a response that never reached GraphQL at all.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/errors/0/extensions/code")
            // The three `429` codes monday publishes, plus the temporary block
            // it answers inside a `200`.
            .on_code("COMPLEXITY_BUDGET_EXHAUSTED", ConnectorErrorClass::Http429)
            .on_code("maxConcurrencyExceeded", ConnectorErrorClass::Http429)
            .on_code("IP_RATE_LIMIT_EXCEEDED", ConnectorErrorClass::Http429)
            .on_code("API_TEMPORARILY_BLOCKED", ConnectorErrorClass::Http429)
            // "A request with this idempotency key is currently being
            // processed", answered with a `Retry-After`. It is the one `409`
            // that a later attempt resolves.
            .on_code("IDEMPOTENCY_CONFLICT", ConnectorErrorClass::Http429)
            .on_code("Unauthorized", ConnectorErrorClass::Authentication)
            .on_code(
                "UserUnauthorizedException",
                ConnectorErrorClass::Authentication,
            )
            .on_code("USER_ACCESS_DENIED", ConnectorErrorClass::Authentication)
            .on_code(
                "missingRequiredPermissions",
                ConnectorErrorClass::Authentication,
            )
            // Everything that needs a different request rather than the same
            // one again.
            .on_code("InvalidArgumentException", ConnectorErrorClass::Validation)
            .on_code("InvalidBoardIdException", ConnectorErrorClass::Validation)
            .on_code("InvalidColumnIdException", ConnectorErrorClass::Validation)
            .on_code("InvalidUserIdException", ConnectorErrorClass::Validation)
            .on_code("ColumnValueException", ConnectorErrorClass::Validation)
            .on_code("ItemNameTooLongException", ConnectorErrorClass::Validation)
            .on_code("RecordInvalidException", ConnectorErrorClass::Validation)
            // A plan ceiling and a missing record are both permanent for this
            // request.
            .on_code("ItemsLimitationException", ConnectorErrorClass::Permanent)
            .on_code("ResourceNotFoundException", ConnectorErrorClass::Permanent)
            // The statuses, for a response that never reached GraphQL at all.
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            .on_statuses([404, 409], ConnectorErrorClass::Permanent)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the monday error map is a valid declaration")
    });
    &MAP
}

/// Decode one monday response: the status, then the GraphQL envelope, then the
/// declared contract.
///
/// A non-empty `errors` array is a failure whatever the status and whatever
/// `data` carries beside it, so a partial answer never reaches the declared
/// output pointers.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    let Ok(value) = serde_json::from_slice::<JsonValue>(body) else {
        return Err(ConnectorFailure::validation(
            "connector provider returned malformed JSON",
        ));
    };
    let reported_errors = value
        .get("errors")
        .and_then(JsonValue::as_array)
        .is_some_and(|errors| !errors.is_empty());
    if reported_errors {
        return Err(error_map().classify(status, headers, body));
    }
    if !value.get("data").is_some_and(JsonValue::is_object) {
        return Err(ConnectorFailure::invariant(
            "connector provider answered outside its declared contract",
        ));
    }
    operation.extract_output(&value)
}

/// No operation of this connector declares a continuation plan, and the module
/// header says why: monday's continuation is a GraphQL variable in the request
/// body, which no plan in the SDK's closed set can spend.
pub const fn pagination(
    _operation_id: &str,
) -> Option<&'static crate::sdk::pagination::Pagination> {
    None
}

/// One operation's request body: the checked-in document, and the declared
/// variables beside it.
fn request(document: &'static str, variables: JsonTemplate) -> JsonTemplate {
    JsonTemplate::object([
        ("query", JsonTemplate::literal(json!(document))),
        ("variables", variables),
    ])
}

fn common(id: &str) -> OperationBuilder {
    Operation::post(id, PATH)
        .version(VERSION)
        // "API-Version: 2026-07" — the pinned stable version, so the schema
        // this declaration was written against is the one it talks to.
        .static_header("API-Version", API_VERSION)
        .success_statuses([StatusCode::OK])
}

/// The reason every mutation in this module carries.
const NO_QUALIFYING_KEY: &str = "monday publishes an `Idempotency-Key` header — \"First request: Executes normally and caches \
     the response for 30 minutes\", \"Retry with same key: Returns the cached response with an \
     `Idempotency-Replayed: true` header\" — and publishes, in the same rules table, that the cache \
     is best effort: \"Per-user budget — Each user+app combination has a memory budget for cached \
     responses. If the budget is exceeded, new responses will execute but won't be cached for \
     replay.\" ADR 042 admits ExplicitKey on a documented *minimum* retention with a clock safety \
     margin strictly under it, and a retention with an unquantified escape clause the connector \
     cannot observe has a minimum of zero";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_QUALIFYING_KEY,
        repeat_produces,
    )?))
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // `items(ids: [ID!])` — "Maximum of 100 IDs at one time."
    let item_get = common("item.get")
        .body(request(
            ITEM_GET,
            JsonTemplate::object([("ids", JsonTemplate::input("ids"))]),
        ))
        .declared_input("ids", ValueScalar::Json, Required::Yes)
        .output_pointer("items", "/data/items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only_documented(
            "monday's `items` query is a read — \"Returns an array containing metadata about one or \
             a collection of specific items\" — and reaches the API as a POST only because GraphQL \
             has one endpoint and one method",
        )?)
        .build()?;

    let item_list = common("item.list")
        .body(request(
            ITEM_LIST,
            JsonTemplate::object([
                ("board_id", JsonTemplate::input("board_id")),
                ("limit", JsonTemplate::literal(json!(PAGE_SIZE))),
                ("cursor", JsonTemplate::input("cursor")),
            ]),
        ))
        .declared_input("board_id", ValueScalar::String, Required::Yes)
        // monday types `cursor` as a nullable `String`, and the first page of a
        // walk has no cursor yet, so the slot is published as JSON: an explicit
        // `null` is a value it must admit.
        .declared_input("cursor", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "items",
            "/data/boards/0/items_page/items",
            ValueScalar::Json,
            Required::Yes,
        )
        .output_pointer(
            "cursor",
            "/data/boards/0/items_page/cursor",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only_documented(
            "monday's `items_page` query is a read — \"Returns an object containing metadata about \
             a collection of items filtered by the specified criteria\"; the POST is GraphQL's one \
             method",
        )?)
        .build()?;

    let item_search = common("item.search")
        .body(request(
            ITEM_SEARCH,
            JsonTemplate::object([
                ("board_id", JsonTemplate::input("board_id")),
                ("limit", JsonTemplate::literal(json!(PAGE_SIZE))),
                ("query_params", JsonTemplate::input("query_params")),
            ]),
        ))
        .declared_input("board_id", ValueScalar::String, Required::Yes)
        // `ItemsQuery` is monday's own input type; the caller supplies a value
        // monday validates against its own schema, exactly as Linear's `filter`
        // is validated against its.
        .declared_input("query_params", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "items",
            "/data/boards/0/items_page/items",
            ValueScalar::Json,
            Required::Yes,
        )
        .output_pointer(
            "cursor",
            "/data/boards/0/items_page/cursor",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only_documented(
            "monday's `items_page` filter query is a read; the POST is GraphQL's one method",
        )?)
        .build()?;

    let item_create = common("item.create")
        .body(request(
            ITEM_CREATE,
            JsonTemplate::object([
                ("board_id", JsonTemplate::input("board_id")),
                ("group_id", JsonTemplate::input("group_id")),
                ("item_name", JsonTemplate::input("item_name")),
                ("column_values", JsonTemplate::input("column_values")),
            ]),
        ))
        .declared_input("board_id", ValueScalar::String, Required::Yes)
        .declared_input("group_id", ValueScalar::Json, Required::Yes)
        .declared_input("item_name", ValueScalar::String, Required::Yes)
        // "When sending data to a particular column, use a JSON-formatted
        // string", so the value monday takes is a *string* carrying JSON.
        .declared_input("column_values", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "id",
            "/data/create_item/id",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "name",
            "/data/create_item/name",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "url",
            "/data/create_item/url",
            ValueScalar::String,
            Required::No,
        )
        .effect(at_most_once(
            "a second item on the same board with a new id — and one more row against the board's \
             own item ceiling, which monday answers with `ItemsLimitationException` once it is \
             reached",
        )?)
        .build()?;

    let item_update = common("item.update")
        .body(request(
            ITEM_UPDATE,
            JsonTemplate::object([
                ("board_id", JsonTemplate::input("board_id")),
                ("item_id", JsonTemplate::input("item_id")),
                ("column_values", JsonTemplate::input("column_values")),
            ]),
        ))
        .declared_input("board_id", ValueScalar::String, Required::Yes)
        .declared_input("item_id", ValueScalar::String, Required::Yes)
        .declared_input("column_values", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "id",
            "/data/change_multiple_column_values/id",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
        .build()?;

    let item_delete = common("item.delete")
        .body(request(
            ITEM_DELETE,
            JsonTemplate::object([("item_id", JsonTemplate::input("item_id"))]),
        ))
        .declared_input("item_id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "id",
            "/data/delete_item/id",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::inventory_only(SILENT_DELETE)?)
        .build()?;

    let update_create = common("update.create")
        .body(request(
            UPDATE_CREATE,
            JsonTemplate::object([
                ("item_id", JsonTemplate::input("item_id")),
                ("body", JsonTemplate::input("body")),
            ]),
        ))
        .declared_input("item_id", ValueScalar::String, Required::Yes)
        .declared_input("body", ValueScalar::String, Required::Yes)
        .output_pointer(
            "id",
            "/data/create_update/id",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "created_at",
            "/data/create_update/created_at",
            ValueScalar::String,
            Required::No,
        )
        .effect(at_most_once(
            "a second update on the same item, with a new id, and a second notification to every \
             subscriber of the board",
        )?)
        .build()?;

    let update_list = common("update.list")
        .body(request(
            UPDATE_LIST,
            JsonTemplate::object([
                ("ids", JsonTemplate::input("ids")),
                ("limit", JsonTemplate::literal(json!(PAGE_SIZE))),
            ]),
        ))
        .declared_input("ids", ValueScalar::Json, Required::Yes)
        .output_pointer("items", "/data/items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only_documented(
            "monday's `updates` field on an item is a read — \"The item's updates\"; the POST is \
             GraphQL's one method",
        )?)
        .build()?;

    let board_get = common("board.get")
        .body(request(
            BOARD_GET,
            JsonTemplate::object([("ids", JsonTemplate::input("ids"))]),
        ))
        .declared_input("ids", ValueScalar::Json, Required::Yes)
        .output_pointer("boards", "/data/boards", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only_documented(
            "monday's `boards` query is a read — \"Returns an array containing metadata about one \
             or a collection of boards\"; the POST is GraphQL's one method",
        )?)
        .build()?;

    let board_list = common("board.list")
        .body(request(
            BOARD_LIST,
            JsonTemplate::object([
                ("limit", JsonTemplate::literal(json!(PAGE_SIZE))),
                ("page", JsonTemplate::input("page")),
            ]),
        ))
        // "page — The page number to return. Starts at 1." It is a declared
        // input because the SDK's page-number plan spends its page in the query
        // string, and monday's is a GraphQL variable.
        .declared_input("page", ValueScalar::Int64, Required::Yes)
        .output_pointer("boards", "/data/boards", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only_documented(
            "monday's `boards` collection query is a read; the POST is GraphQL's one method",
        )?)
        .build()?;

    Ok(vec![
        item_get,
        item_list,
        item_search,
        item_create,
        item_update,
        item_delete,
        update_create,
        update_list,
        board_get,
        board_list,
    ])
}

/// The reason the update carries.
const PARTIAL_UPDATE: &str = "monday's `change_multiple_column_values` writes the column values it \
                              is given and leaves every other column alone — \"This mutation allows \
                              you to update multiple column values of a specific Item (row)\" — so \
                              a second identical send leaves exactly the state the first one did. \
                              There is no consequence ADR 063 admits a class on, and a GraphQL \
                              mutation is a POST, which spec 010 §7 does not admit for \
                              NaturalMethod";

/// The reason the delete carries.
const SILENT_DELETE: &str = "monday publishes \"Deletes an item (or subitem) and its nested \
                             subitems\", which is what the first send does, and nothing at all \
                             about a second one. A GraphQL mutation is a POST in any case, which \
                             spec 010 §7 does not admit for NaturalMethod";
