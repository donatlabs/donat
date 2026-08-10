//! Zendesk's Support (Ticketing) API v2.
//!
//! Ground truth is Zendesk's own published documentation, read on 2026-08-10:
//!
//! * <https://developer.zendesk.com/api-reference/introduction/doc-conventions/>
//!   — "Prepend your Zendesk Support URL to the path to get the full endpoint
//!   URL", the `https://{subdomain}.zendesk.com/api/v2/…` form, and "You can
//!   optionally append `.json` to the resource name. ... Appending `.json`
//!   doesn't change the way the endpoint works in any way."
//! * <https://developer.zendesk.com/api-reference/introduction/security-and-auth/>
//!   — "Basic authentication is used for API tokens. The credentials must be
//!   sent with the request in an Authorization header. Use the following format
//!   for the credentials: `{email_address}/token:{api_token}`".
//! * <https://developer.zendesk.com/api-reference/ticketing/introduction/> —
//!   the idempotency contract quoted under "Effect classification" below.
//! * <https://developer.zendesk.com/api-reference/introduction/pagination/> —
//!   both pagination regimes and their stop conditions.
//! * <https://developer.zendesk.com/api-reference/introduction/rate-limits/> —
//!   "If the rate limit is exceeded, the API responds with a `429 Too Many
//!   Requests` status code. The response also has a `Retry-After` header that
//!   tells you how many seconds to wait before retrying API requests."
//! * <https://developer.zendesk.com/api-reference/introduction/requests/> — the
//!   status table and the error body, and "The status is 200 for successful GET
//!   and PUT requests, 201 for most POST requests, and 204 for DELETE requests."
//! * The `tickets`, `ticket_comments`, `users`, and `search` reference pages for
//!   every path, parameter, and field below.
//!
//! # The API token is a deprecated credential with a published end date
//!
//! The reference's own section heading is "API token (deprecated)", and Zendesk
//! publishes the schedule: "Starting July 28, 2026, unused tokens will
//! automatically deactivate. By April 30, 2027, all API tokens will stop
//! working. You must migrate your integrations to OAuth before the final
//! deadline." This connector declares the token credential because it is what
//! works today and what spec 023 asks for; moving it to the `Bearer` OAuth form
//! Zendesk publishes beside it is a contract version bump of this module, not a
//! configuration change, and it has to happen before that date.
//!
//! # The subdomain is deploy-time configuration, and so is the address
//!
//! Two values complete this declaration and neither can come from a request. The
//! **subdomain** is the host; the **email** is the Basic username, which
//! `AuthPlan::basic` takes where the plan is built — so this declaration is built
//! per deployment for the same reason Jira's and Twilio's are, and the username
//! carries Zendesk's own `/token` suffix rather than being the bare address.
//!
//! # Offset pagination is the walk, and the cursor is not
//!
//! Zendesk publishes two regimes. Cursor pagination is the one it recommends,
//! and its published stop condition is a *flag*: "Repeat the above steps until
//! the `meta[has_more]` property is false." No plan in the SDK's closed set ends
//! a walk on a flag — every one of them ends on an absence — so declaring the
//! cursor here would be declaring a walk that cannot end
//! (`knowledgebase/declarative-saas/decisions/055-*`).
//!
//! Offset pagination publishes exactly the absence the plan reads: "Stop paging
//! when the `next_page` attribute is null." So the walked collections declare
//! [`Pagination::next_uri_in_body`] on `/next_page`, at 100 records a page, and
//! Zendesk's own ceiling for that regime — "limited to the first 100 pages and
//! 10,000 resources" — is six times the SDK's own page budget, so a walk here
//! stops on its own budget long before Zendesk's.
//!
//! The **search** endpoint is deliberately not walked at all, on Zendesk's own
//! statement: "Offset pagination only. Offset pagination may result in duplicate
//! results when paging." An aggregate assembled from pages the provider says may
//! repeat themselves is not an aggregate, so the page is a declared input a
//! Process carries and `next_page` is published as an output.
//!
//! # Effect classification
//!
//! **Zendesk publishes an idempotency key, for exactly one operation.** "The
//! Ticketing API lets you specify an idempotency key that allows you to retry a
//! ticket creation request without the risk of creating duplicate records", "To
//! specify an idempotency key, provide a `Idempotency-Key: {unique_key}` header
//! with your request", "If you repeat the same request using the same body and
//! idempotency key, another ticket is not created. Instead, you'll get the same
//! response as before that is cached under the idempotency key", and — the
//! retention — "Keys expire after two hours. If a request with a duplicate key
//! is sent two hours after the original request, the request will create a new
//! ticket." `ticket.create` is therefore the batch's one
//! `ProviderIdempotent::ExplicitKey`.
//!
//! What Zendesk does **not** publish is the key's uniqueness scope in words. The
//! scope recorded on the evidence is the narrowest one its published behaviour
//! establishes: one Zendesk account — every request is authenticated to one
//! subdomain — and one request body, because "If you create a request using the
//! same idempotency key but with a different body" is published as a `400`
//! `IdempotentRequestError`. That gap is recorded here and in
//! `providers/INVENTORY.md` rather than papered over, and it is the one piece of
//! this class's evidence that is a reading rather than a quotation.
//!
//! Two consequences of the two-hour window are worth stating. A durable retry
//! window longer than two hours *loses* the guarantee while still holding the
//! key, which is the send-horizon problem
//! (`knowledgebase/declarative-saas/decisions/046-*`); and the key must not be
//! recomputed between attempts, which is exactly what a durable activity's
//! stable key is.
//!
//! Nothing else here reaches a `ProviderIdempotent` class. The string `idempot`
//! does not occur anywhere in Zendesk's own published OpenAPI description
//! (`https://developer.zendesk.com/zendesk/oas.yaml`, 1,701,930 bytes, 434
//! paths), which declares no header parameters at all — so the key above exists
//! in prose only and is bound here by hand.
//!
//! * `comment.add` is `AtMostOnce`. Zendesk publishes no dedicated endpoint for
//!   it — "The Tickets Comments API has no endpoint to create comments. Ticket
//!   comments are created by including a comment object in the ticket object
//!   when creating or updating the ticket" — so it is the ticket update with a
//!   comment in it, and a repeat appends a second comment.
//! * `user.create` is `AtMostOnce`: a second user with a new id.
//! * `ticket.update` and `user.update` stay `InventoryOnly`. Zendesk documents
//!   the ticket update as a partial one — "The PUT request takes a ticket object
//!   that lists the values to update. All properties are optional" — which is
//!   not `NaturalMethod` evidence, and publishes no consequence for a repeat.
//! * `user.create_or_update` stays `InventoryOnly` too, and for the opposite
//!   reason: Zendesk documents it as repeat-safe — "Creates a user if the user
//!   does not already exist, or updates an existing user identified by e-mail
//!   address or external ID" — and it is a `POST`, which spec 010 §7's
//!   `NaturalMethod` does not admit. An operation a provider documents as
//!   repeat-safe wants a class that *keeps* the retry, and that class does not
//!   exist; ADR 063's is not it.
//!
//! # `email` is deliberately absent from `user.update`
//!
//! Zendesk publishes a trap on that endpoint: "If only email is provided, the
//! provided email is added to the user as a secondary email ... The primary
//! email remains unmodified", and the field table repeats it — "Writeable on
//! create only. On update, a secondary email is added." A declaration carrying
//! `email` there would accumulate a new identity on every send. It is left out,
//! which is also what keeps the operation's repeat consequence empty.

use std::sync::LazyLock;
use std::time::Duration;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, FieldClassification, OriginSpec};
use crate::sdk::effect::{
    AbsenceSearch, Effect, ExplicitKeyEvidence, IdempotencyBinding, NoIdempotencyEvidence,
};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "zendesk";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key that fills the templated host: the
/// account's own `zendesk.com` label.
pub const SUBDOMAIN: &str = "subdomain";

/// The deploy-time configuration key carrying the account address.
///
/// It is not a secret — Zendesk pairs it with the API token, which is — so it
/// lives in `config.settings` and is published as a non-secret credential field.
pub const EMAIL: &str = "email";

/// "Most endpoints limit this to a maximum of 100."
const PAGE_SIZE: &str = "100";

/// "To specify an idempotency key, provide a `Idempotency-Key: {unique_key}`
/// header with your request."
pub const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

/// "Keys expire after two hours."
const KEY_RETENTION: Duration = Duration::from_secs(2 * 60 * 60);

/// The margin Donat keeps below Zendesk's published window.
///
/// It is strictly smaller than the retention, which is what the evidence
/// constructor enforces: a key Zendesk has already forgotten is a fresh ticket,
/// not a replay.
const KEY_MARGIN: Duration = Duration::from_secs(5 * 60);

/// One deployment's declaration.
///
/// `email` is the account address; the Basic username Zendesk publishes is that
/// address with its own `/token` suffix — "Use the following format for the
/// credentials: `{email_address}/token:{api_token}`".
pub fn connector(email: &str) -> Result<Connector, OperationError> {
    validate_email(email)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::templated_host(
            "https",
            "{subdomain}.zendesk.com",
            None,
        )?)
        .credential(
            CredentialSpec::for_plan(AuthPlan::basic(&format!("{email}/token"))?)
                .with_field(EMAIL, FieldClassification::NonSecret),
        )
        .operations(operations()?)
        .build()
}

/// The account address's grammar, checked where the declaration is built so a
/// mistyped address is a startup refusal rather than a `401` on the first
/// activity attempt.
///
/// `AuthPlan::basic` already refuses an empty username, a colon, and anything
/// outside printable ASCII. What is added here is Zendesk's own requirement that
/// the value is an email address, and the refusal of a value that already
/// carries the `/token` suffix this module appends.
fn validate_email(email: &str) -> Result<(), OperationError> {
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || email.contains('/')
        || email.chars().any(char::is_whitespace)
    {
        return Err(OperationError::new(
            "the Zendesk credential setting must be the account's email address, which this \
             connector sends as `{email_address}/token`",
        ));
    }
    Ok(())
}

/// The declaration a reviewer and the registry read, with a placeholder address
/// no deployment uses.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("deployment.configured@example.invalid")
}

/// The ordered error map.
///
/// Zendesk's error body publishes an `error` string — "`error` | string | The
/// type of error. Examples: "unavailable", "invalid"" — but publishes no closed
/// set of its values, so only the one value whose meaning is a contract is keyed
/// on and everything else is answered by the documented status table.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error")
            // "If you create a request using the same idempotency key but with a
            // different body, you'll receive the following error: 400 Bad
            // Request: {"error": "IdempotentRequestError", ...}". A durable
            // activity that reached this has changed its own request between
            // attempts, which no retry fixes.
            .on_code("IdempotentRequestError", ConnectorErrorClass::Permanent)
            // "The request couldn't be understood, usually because the JSON or
            // XML body contains an error", and "422 Unprocessable Entity — ...
            // the content itself is not processable by the server."
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            // "A 403 response means the server has determined the user or the
            // account doesn't have the required permissions to use the API."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "A 409 response indicates a conflict with the resource you're
            // trying to create or update ... If you receive a 409 error, you can
            // retry your request after resolving the conflict" — which is a
            // change to the request, not a repeat of it.
            .on_statuses([404, 405, 409, 410, 415], ConnectorErrorClass::Permanent)
            // "If the rate limit is exceeded, the API responds with a 429 Too
            // Many Requests status code."
            .on_status(429, ConnectorErrorClass::Http429)
            // "When building an API client, we recommend treating any 500 status
            // codes as a warning or temporary state", and "A 503 response with a
            // Retry-After header indicates a database timeout or deadlock."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Zendesk error map is a valid declaration")
    });
    &MAP
}

/// Decode one Zendesk response: the declared success statuses, then the declared
/// contract.
///
/// It is the declaration-driven answer written out per module rather than
/// inherited, so that the serving runtime asks every connector in this batch the
/// same question and each one answers with its own error map.
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

/// The continuation plan of each walked collection.
///
/// Offset pagination, whose stop condition Zendesk publishes as an absence —
/// "Stop paging when the `next_page` attribute is null" — and whose continuation
/// is a whole URL on this account's own origin, which is resolved and
/// origin-checked exactly as a `Link` header is. See the module documentation
/// for why the cursor regime is not declared and why the search is not walked.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    fn plan(items: &str) -> Pagination {
        Pagination::next_uri_in_body(items, "/next_page")
            .expect("the Zendesk offset continuation plan is valid")
    }
    static TICKETS: LazyLock<Pagination> = LazyLock::new(|| plan("/tickets"));
    static USERS: LazyLock<Pagination> = LazyLock::new(|| plan("/users"));
    static COMMENTS: LazyLock<Pagination> = LazyLock::new(|| plan("/comments"));
    match operation_id {
        "ticket.list" => Some(&TICKETS),
        "user.list" => Some(&USERS),
        "comment.list" => Some(&COMMENTS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        // "you must set an Accept: application/json header on all requests".
        .static_header("Accept", "application/json")
}

/// The reason every write in this module that is not the ticket create carries.
const NO_KEY: &str = "Zendesk publishes an idempotency key for exactly one operation — \"The \
                      Ticketing API lets you specify an idempotency key that allows you to retry a \
                      ticket creation request without the risk of creating duplicate records\" — \
                      and publishes it for no other endpoint; the string `idempot` does not occur \
                      once in its own published OpenAPI description of the Support API, which \
                      declares no header parameters at all. A documented restriction to one \
                      endpoint is stronger evidence than an absence";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The published ticket fields a process reads.
fn ticket_output(builder: OperationBuilder, root: &str) -> OperationBuilder {
    builder
        .output_pointer(
            "id",
            &format!("{root}/id"),
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer(
            "subject",
            &format!("{root}/subject"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "status",
            &format!("{root}/status"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "priority",
            &format!("{root}/priority"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "requester_id",
            &format!("{root}/requester_id"),
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "url",
            &format!("{root}/url"),
            ValueScalar::String,
            Required::No,
        )
        // "`created_at` | string", "`updated_at` | string" — ISO 8601 text, not
        // an epoch integer.
        .output_pointer(
            "created_at",
            &format!("{root}/created_at"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "updated_at",
            &format!("{root}/updated_at"),
            ValueScalar::String,
            Required::No,
        )
}

/// The published user fields a process reads.
fn user_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/user/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("name", "/user/name", ValueScalar::String, Required::No)
        .output_pointer("email", "/user/email", ValueScalar::String, Required::No)
        .output_pointer("role", "/user/role", ValueScalar::String, Required::No)
        .output_pointer(
            "external_id",
            "/user/external_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "created_at",
            "/user/created_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "updated_at",
            "/user/updated_at",
            ValueScalar::String,
            Required::No,
        )
}

/// The continuation both walked collections publish beside their items.
fn collection_output(builder: OperationBuilder, items: &str) -> OperationBuilder {
    builder
        .output_pointer("items", items, ValueScalar::Json, Required::Yes)
        .output_pointer("next_page", "/next_page", ValueScalar::String, Required::No)
        .output_pointer("count", "/count", ValueScalar::Int64, Required::No)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let ticket_get = ticket_output(
        common(Operation::get("ticket.get", "/api/v2/tickets/{ticket_id}"))
            .path_param("ticket_id", ValueScalar::Int64)
            .success_statuses([StatusCode::OK]),
        "/ticket",
    )
    .effect(Effect::read_only())
    .build()?;

    let ticket_list = collection_output(
        common(Operation::get("ticket.list", "/api/v2/tickets"))
            // "If you omit the `page[size]` parameter, offset pagination is
            // used", which is the regime this module walks.
            .query_static("per_page", PAGE_SIZE)
            .success_statuses([StatusCode::OK]),
        "/tickets",
    )
    .effect(Effect::read_only())
    .build()?;

    // "The Search API returns up to 1,000 results per query, with a maximum of
    // 100 results per page. ... If you request a page past the limit (`page=11`
    // at 100 results per page), a 422 Insufficient Resource Error is returned."
    let ticket_search = common(Operation::get("ticket.search", "/api/v2/search"))
        .query_input("query", "query")
        // Offset pagination is the only regime the search publishes, and this
        // connector does not walk it, so the page is the caller's.
        .query_input("page", "page")
        .query_static("per_page", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("results", "/results", ValueScalar::Json, Required::Yes)
        // "The `count` property shows the actual number of results. For example,
        // if a query has 5,000 results, the count value will be 5,000, even if
        // the API only returns the first 1,000 results."
        .output_pointer("count", "/count", ValueScalar::Int64, Required::No)
        .output_pointer("next_page", "/next_page", ValueScalar::String, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // "The only required property is `comment`. ... All writable properties
    // listed in JSON Format are optional." The durable activity's own stable key
    // fills the idempotency header, so it is a supplied input rather than a
    // caller's.
    let ticket_create = ticket_output(
        common(Operation::post("ticket.create", "/api/v2/tickets"))
            .body(JsonTemplate::object([(
                "ticket",
                JsonTemplate::object([
                    ("subject", JsonTemplate::input("subject")),
                    ("comment", JsonTemplate::input("comment")),
                    ("requester_id", JsonTemplate::input("requester_id")),
                    ("priority", JsonTemplate::input("priority")),
                    ("tags", JsonTemplate::input("tags")),
                    ("external_id", JsonTemplate::input("external_id")),
                ]),
            )]))
            .declared_input("comment", ValueScalar::Json, Required::Yes)
            .declared_input("tags", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::CREATED]),
        "/ticket",
    )
    .effect(Effect::provider_idempotent_explicit_key(
        ExplicitKeyEvidence::documented(
            IdempotencyBinding::header(IDEMPOTENCY_HEADER)?,
            "one Zendesk account — every request is authenticated to one subdomain — and one \
             request body, which Zendesk establishes by refusing a reused key with a different \
             body: \"Request parameters don't match the given idempotency key\". Zendesk publishes \
             the binding and the retention in words and does not publish the scope, so this is the \
             narrowest scope its published behaviour supports",
            KEY_RETENTION,
            KEY_MARGIN,
            "\"The Ticketing API lets you specify an idempotency key that allows you to retry a \
             ticket creation request without the risk of creating duplicate records\", \"To \
             specify an idempotency key, provide a `Idempotency-Key: {unique_key}` header with \
             your request\", \"If you repeat the same request using the same body and idempotency \
             key, another ticket is not created. Instead, you'll get the same response as before \
             that is cached under the idempotency key\", and \"Keys expire after two hours. If a \
             request with a duplicate key is sent two hours after the original request, the \
             request will create a new ticket.\"",
        )?,
    ))
    .build()?;

    let ticket_update = ticket_output(
        common(Operation::put(
            "ticket.update",
            "/api/v2/tickets/{ticket_id}",
        ))
        .path_param("ticket_id", ValueScalar::Int64)
        .body(JsonTemplate::object([(
            "ticket",
            JsonTemplate::object([
                ("status", JsonTemplate::input("status")),
                ("priority", JsonTemplate::input("priority")),
                ("assignee_id", JsonTemplate::input("assignee_id")),
                ("tags", JsonTemplate::input("tags")),
            ]),
        )]))
        .declared_input("tags", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
        "/ticket",
    )
    .effect(Effect::inventory_only(
        "Zendesk documents this as a partial update — \"The PUT request takes a ticket object that \
         lists the values to update. All properties are optional\" — which is not the write to a \
         fixed resource identity spec 010 §7's NaturalMethod evidence needs, and it publishes \
         nothing about what a second identical send produces, which is what ADR 063 admits a class \
         on. Its idempotency key is published for ticket creation only",
    )?)
    .build()?;

    // "Ticket comments are created by including a `comment` object in the ticket
    // object when creating or updating the ticket." So this is the same route as
    // the update, with the one field that makes it an append.
    let comment_add = ticket_output(
        common(Operation::put("comment.add", "/api/v2/tickets/{ticket_id}"))
            .path_param("ticket_id", ValueScalar::Int64)
            .body(JsonTemplate::object([(
                "ticket",
                JsonTemplate::object([("comment", JsonTemplate::input("comment"))]),
            )]))
            .declared_input("comment", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::OK]),
        "/ticket",
    )
    .effect(at_most_once(
        "a second comment appended to the same ticket, with a new id — and, for a public comment, \
         a second notification to the requester. Zendesk's own ceiling makes the accumulation \
         visible: \"up to 5000 comments in total ... Once this limit is reached, any additional \
         attempts to add comments results in a 422 error\"",
    )?)
    .build()?;

    let comment_list = collection_output(
        common(Operation::get(
            "comment.list",
            "/api/v2/tickets/{ticket_id}/comments",
        ))
        .path_param("ticket_id", ValueScalar::Int64)
        .query_static("per_page", PAGE_SIZE)
        .success_statuses([StatusCode::OK]),
        "/comments",
    )
    .effect(Effect::read_only())
    .build()?;

    let user_get = user_output(
        common(Operation::get("user.get", "/api/v2/users/{user_id}"))
            .path_param("user_id", ValueScalar::Int64)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let user_list = collection_output(
        common(Operation::get("user.list", "/api/v2/users"))
            .query_static("per_page", PAGE_SIZE)
            .success_statuses([StatusCode::OK]),
        "/users",
    )
    .effect(Effect::read_only())
    .build()?;

    // "`name` | string | false | true | The user's name" — the one mandatory
    // user field.
    let user_create = user_output(
        common(Operation::post("user.create", "/api/v2/users"))
            .body(JsonTemplate::object([(
                "user",
                JsonTemplate::object([
                    ("name", JsonTemplate::input("name")),
                    ("email", JsonTemplate::input("email")),
                    ("role", JsonTemplate::input("role")),
                    ("external_id", JsonTemplate::input("external_id")),
                ]),
            )]))
            .declared_input("name", ValueScalar::String, Required::Yes)
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second user with a new id, unless the payload carries an `external_id`, which Zendesk \
         publishes as unique per account — \"External id has to be unique for each user under the \
         same account\" — where the repeat is refused instead. Neither outcome is the first \
         send's",
    )?)
    .build()?;

    // `email` is deliberately absent; see the module documentation.
    let user_update = user_output(
        common(Operation::put("user.update", "/api/v2/users/{user_id}"))
            .path_param("user_id", ValueScalar::Int64)
            .body(JsonTemplate::object([(
                "user",
                JsonTemplate::object([
                    ("name", JsonTemplate::input("name")),
                    ("role", JsonTemplate::input("role")),
                    ("external_id", JsonTemplate::input("external_id")),
                    ("user_fields", JsonTemplate::input("user_fields")),
                ]),
            )]))
            .declared_input("user_fields", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Zendesk publishes no statement that this request replaces the user, so spec 010 §7's \
         NaturalMethod evidence is not there to cite even though the method is right, and it \
         publishes no consequence for a repeat of the fields this declaration sends. The one \
         field whose repeat Zendesk *does* publish a consequence for — \"On update, a secondary \
         email is added\" — is deliberately not declared here",
    )?)
    .build()?;

    // "Creates a user if the user does not already exist, or updates an existing
    // user identified by e-mail address or external ID."
    let user_create_or_update = user_output(
        common(Operation::post(
            "user.create_or_update",
            "/api/v2/users/create_or_update",
        ))
        .body(JsonTemplate::object([(
            "user",
            JsonTemplate::object([
                ("name", JsonTemplate::input("name")),
                ("email", JsonTemplate::input("email")),
                ("role", JsonTemplate::input("role")),
                ("external_id", JsonTemplate::input("external_id")),
            ]),
        )]))
        .declared_input("name", ValueScalar::String, Required::Yes)
        // "If the user already exists in Zendesk, a successful request returns a
        // 200 OK status code. If the user does not exist in Zendesk and is
        // created, the request returns a 201 Created status code."
        .success_statuses([StatusCode::OK, StatusCode::CREATED]),
    )
    .effect(Effect::inventory_only(
        "Zendesk documents this operation as repeat-safe — \"Creates a user if the user does not \
         already exist, or updates an existing user identified by e-mail address or external ID\", \
         with the second send answering `200` where the first answered `201` — but publishes it as \
         a `POST`, and spec 010 §7 admits NaturalMethod for PUT and DELETE only. An operation a \
         provider documents as repeat-safe wants a class that keeps the retry, which ADR 063's \
         at-most-once class is not",
    )?)
    .build()?;

    Ok(vec![
        ticket_get,
        ticket_list,
        ticket_search,
        ticket_create,
        ticket_update,
        comment_add,
        comment_list,
        user_get,
        user_list,
        user_create,
        user_update,
        user_create_or_update,
    ])
}
