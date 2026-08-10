//! Linear's GraphQL API — one endpoint, seven checked-in documents.
//!
//! Ground truth is Linear's own published documentation and its own published
//! GraphQL schema, read on 2026-08-10:
//!
//! * <https://linear.app/developers/graphql> — "Linear's GraphQL endpoint is:
//!   `https://api.linear.app/graphql`", "The Linear API supports personal API
//!   keys and OAuth2 authentication", "To authenticate your requests, you need
//!   to pass the API key with header: `Authorization: <API_KEY>`" beside the
//!   OAuth form "`Authorization: Bearer <ACCESS_TOKEN>`", the `POST` with
//!   `Content-Type: application/json` and a `{ "query": … }` body, and
//!   "GraphQL queries can partially succeed with a 200 HTTP status, returning
//!   some data while including errors for failed fields."
//! * <https://linear.app/developers/rate-limiting> — "response http status code
//!   will be 400, but you can catch these by inspecting the errors in the
//!   response body containing the `RATELIMITED` error code", and the
//!   `X-RateLimit-Requests-*`, `X-RateLimit-Endpoint-Requests-*` and
//!   `X-RateLimit-Complexity-*` headers. Linear publishes **no** `Retry-After`.
//! * The published schema
//!   (<https://github.com/linear/linear>, `packages/sdk/src/schema.graphql`) for
//!   every field, argument, and input type these documents name, and
//!   `packages/sdk/src/error.ts` for the `extensions.type` vocabulary Linear's
//!   own client classifies on.
//!
//! # The credential is the header value
//!
//! Linear publishes both forms side by side and they are not interchangeable: an
//! OAuth access token is sent as `Authorization: Bearer <ACCESS_TOKEN>` and a
//! personal API key as `Authorization: <API_KEY>`. This connector is the API-key
//! one, so it declares [`AuthPlan::authorization_credential`] — the SDK plan that
//! sends the credential as the whole header value. `Bearer` would authenticate
//! as nobody and `ApiKeyHeader` refuses the `Authorization` name on purpose, so
//! neither existing plan could describe what reaches the wire.
//!
//! # A document is a constant
//!
//! Each operation carries one checked-in `.graphql` document, included at
//! compile time from `providers/linear/queries/`, and binds **only** typed
//! variables into `variables`. The `query` leaf of the request body is a
//! [`JsonTemplate::literal`], so there is no input name a caller could fill to
//! supply a query, a fragment, an alias, a directive, or one more field in a
//! selection set. What a caller supplies is an issue id, a filter object Linear
//! validates against its own schema, and a cursor.
//!
//! The page size is declaration material for the same reason: `$first` is bound
//! from a literal, so a caller cannot ask one call for the whole workspace.
//!
//! # A `200` is not a success
//!
//! Linear answers a rejected request with a `200` — or a `400` for a rate
//! limit — and reports what happened in the GraphQL `errors` array. [`decode`]
//! therefore refuses any response carrying a non-empty `errors`, whatever the
//! status and whatever `data` is beside it. Linear's own documentation is
//! explicit that a query "can partially succeed with a 200 HTTP status,
//! returning some data while including errors for failed fields"; a partial
//! answer is not the declared output contract, so it is a failure here rather
//! than a success with holes in it.
//!
//! # The cursor is a variable, not a pagination plan
//!
//! Linear's continuation lives in the request *body* — `after` is a GraphQL
//! variable and `pageInfo.endCursor` is a field of the response — and every SDK
//! pagination plan spends its continuation as a query parameter or follows it as
//! a URL. This module therefore declares no [`Pagination`](crate::sdk::Pagination)
//! plan at all: `after` is a declared input the caller echoes back verbatim, and
//! `has_next_page`/`end_cursor` are declared outputs. One call is one page, its
//! size fixed by the declaration and its bytes by the SDK's response ceiling.
//! See `knowledgebase/declarative-saas/decisions/055-*`.
//!
//! # Effect classification
//!
//! `issue.create` and `comment.create` are `AtMostOnce` (ADR 063) and
//! `issue.update` stays `InventoryOnly` — a repeat of a documented partial
//! update sets the same fields — and the evidence behind all three is a
//! documented *exclusion* rather than an absence. Linear publishes a client-supplied
//! idempotency key and publishes exactly where it applies:
//! `OAuthApplicationCreateInput.idempotencyKey` — "Optional client-supplied
//! idempotency key. Reusing the same key with the same managing OAuth
//! application returns the existing OAuth application instead of creating a
//! duplicate." No such field exists on `IssueCreateInput`, `IssueUpdateInput`,
//! or `CommentCreateInput`.
//!
//! Spec 016 §2 asks specifically whether the client-supplied mutation identifier
//! deduplicates, and the answer is no: `IssueCreateInput.id` is documented only
//! as "The identifier in UUID v4 format. If none is provided, the backend will
//! generate one." Linear publishes no statement about what a second create with
//! the same `id` does and no retention window it would hold one for, and ADR 042
//! admits `ExplicitKey` only on a binding *plus* a documented retention with a
//! clock safety margin under it. So the identifier is real and the class is not.
//!
//! `issue.update` is inventory-only twice over: the schema documents its input as
//! "A partial issue object to update the issue with", and a GraphQL mutation is a
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
pub const NAME: &str = "linear";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "Linear's GraphQL endpoint is: `https://api.linear.app/graphql`".
const ORIGIN: &str = "https://api.linear.app";

/// The one path every operation of this connector renders.
const PATH: &str = "/graphql";

/// "The number of items to forward paginate (used with after). Defaults to 50."
///
/// It is bound from a literal rather than from input, so one call is one page of
/// a size this declaration chose.
const PAGE_SIZE: i64 = 50;

// The checked-in documents. Each is a compile-time constant, and it is the only
// thing that ever reaches Linear's `query` field.
const ISSUE_GET: &str = include_str!("linear/queries/issue_get.graphql");
const ISSUE_LIST: &str = include_str!("linear/queries/issue_list.graphql");
const ISSUE_CREATE: &str = include_str!("linear/queries/issue_create.graphql");
const ISSUE_UPDATE: &str = include_str!("linear/queries/issue_update.graphql");
const COMMENT_CREATE: &str = include_str!("linear/queries/comment_create.graphql");
const TEAM_LIST: &str = include_str!("linear/queries/team_list.graphql");
const USER_LIST: &str = include_str!("linear/queries/user_list.graphql");

/// The document one operation sends, for a reviewer and for the test that reads
/// every one of them.
///
/// There is no setter beside it and no path from a request to one: this is the
/// declaration's own material exposed for inspection.
pub fn document(operation_id: &str) -> Option<&'static str> {
    match operation_id {
        "issue.get" => Some(ISSUE_GET),
        "issue.list" => Some(ISSUE_LIST),
        "issue.create" => Some(ISSUE_CREATE),
        "issue.update" => Some(ISSUE_UPDATE),
        "comment.create" => Some(COMMENT_CREATE),
        "team.list" => Some(TEAM_LIST),
        "user.list" => Some(USER_LIST),
        _ => None,
    }
}

/// Every operation this connector publishes, with the document it sends.
pub fn documents() -> &'static [(&'static str, &'static str)] {
    &[
        ("issue.get", ISSUE_GET),
        ("issue.list", ISSUE_LIST),
        ("issue.create", ISSUE_CREATE),
        ("issue.update", ISSUE_UPDATE),
        ("comment.create", COMMENT_CREATE),
        ("team.list", TEAM_LIST),
        ("user.list", USER_LIST),
    ]
}

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Linear's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::authorization_credential(),
            ))
            .operations(operations().expect("the Linear declarations are valid"))
            .build()
            .expect("the Linear declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// The code pointer is `extensions.type` of the first error, because that is the
/// field Linear's own published client classifies on: `packages/sdk/src/error.ts`
/// maps `error.extensions.type` through a fixed table of "ratelimited",
/// "authentication error", "forbidden", "invalid input", "feature not
/// accessible", "internal error", "usage limit exceeded", "lock timeout", "user
/// error", and "graphql error".
///
/// Linear's rate-limiting page spells the same condition once more, in upper
/// case, at `extensions.code`: "`RATELIMITED`". A map reads one pointer, so both
/// spellings are declared at the `type` position and the upper-case one is also
/// declared for a body that carries it there. A `400` that carries the code at
/// `extensions.code` and *nothing* at `extensions.type` therefore classifies by
/// its status as `validation` — which is not retried, the same safe direction the
/// GitHub connector takes for its ambiguous `403`.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/errors/0/extensions/type")
            .on_code("ratelimited", ConnectorErrorClass::Http429)
            .on_code("RATELIMITED", ConnectorErrorClass::Http429)
            .on_code("authentication error", ConnectorErrorClass::Authentication)
            .on_code("forbidden", ConnectorErrorClass::Authentication)
            .on_code("invalid input", ConnectorErrorClass::Validation)
            .on_code("user error", ConnectorErrorClass::Validation)
            .on_code("graphql error", ConnectorErrorClass::Validation)
            // A plan limit and a missing feature both need a change on the
            // workspace's side; repeating the request cannot help.
            .on_code("usage limit exceeded", ConnectorErrorClass::Permanent)
            .on_code("feature not accessible", ConnectorErrorClass::Permanent)
            .on_code("internal error", ConnectorErrorClass::Http5xx)
            .on_code("lock timeout", ConnectorErrorClass::Http5xx)
            // The statuses, for a response that never reached GraphQL at all.
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Linear error map is a valid declaration")
    });
    &MAP
}

/// Decode one Linear response: the status, then the GraphQL envelope, then the
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

/// One operation's request body: the checked-in document, and the declared
/// variables beside it.
///
/// The document is a literal leaf, so no input name reaches it. `variables` is a
/// static object whose keys are the declaration's and whose values are the only
/// slots a caller can fill.
fn request(document: &'static str, variables: JsonTemplate) -> JsonTemplate {
    JsonTemplate::object([
        ("query", JsonTemplate::literal(json!(document))),
        ("variables", variables),
    ])
}

fn common(id: &str) -> OperationBuilder {
    Operation::post(id, PATH)
        .version(VERSION)
        .success_statuses([StatusCode::OK])
}

/// The two continuation fields every connection publishes.
fn page_info(builder: OperationBuilder, connection: &str) -> OperationBuilder {
    builder
        .output_pointer(
            "has_next_page",
            &format!("/data/{connection}/pageInfo/hasNextPage"),
            ValueScalar::Boolean,
            Required::Yes,
        )
        .output_pointer(
            "end_cursor",
            &format!("/data/{connection}/pageInfo/endCursor"),
            ValueScalar::String,
            Required::No,
        )
}

/// The reason every mutation in this module carries.
const NO_KEY: &str = "Linear publishes a client-supplied idempotency key and publishes exactly \
                      where it applies — `OAuthApplicationCreateInput.idempotencyKey`, \"Reusing \
                      the same key with the same managing OAuth application returns the existing \
                      OAuth application instead of creating a duplicate\" — and no such field \
                      exists on this mutation's input; `IssueCreateInput.id` is documented only as \
                      an optional UUID the backend would otherwise generate, with no deduplication \
                      and no retention, and a GraphQL mutation is a POST, which spec 010 §7 does \
                      not admit for NaturalMethod";

/// One write whose repeat would leave a second thing behind (ADR 063).
///
/// The search is the module's and the consequence is the operation's: both are
/// what a Process author accepts when they declare `at_most_once`.
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let issue_get = common("issue.get")
        .body(request(
            ISSUE_GET,
            JsonTemplate::object([("id", JsonTemplate::input("id"))]),
        ))
        .declared_input("id", ValueScalar::String, Required::Yes)
        .output_pointer("id", "/data/issue/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "identifier",
            "/data/issue/identifier",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "title",
            "/data/issue/title",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "description",
            "/data/issue/description",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("url", "/data/issue/url", ValueScalar::String, Required::Yes)
        // "priority: Float!" — Linear types it as a float. The SDK's scalar set
        // has no float, and narrowing it to `Int64` would refuse the `2.0` the
        // schema permits, so the declaration publishes it as JSON rather than
        // publishing a type the provider does not promise.
        .output_pointer(
            "priority",
            "/data/issue/priority",
            ValueScalar::Json,
            Required::Yes,
        )
        .output_pointer(
            "state",
            "/data/issue/state",
            ValueScalar::Json,
            Required::Yes,
        )
        .output_pointer(
            "updated_at",
            "/data/issue/updatedAt",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::read_only_documented(
            "Linear's `issue(id: String!): Issue!` query is a read — \"The identifier of the issue \
             to retrieve\" — and reaches the API as a POST only because GraphQL has one endpoint \
             and one method",
        )?)
        .build()?;

    let issue_list = page_info(
        common("issue.list")
            .body(request(
                ISSUE_LIST,
                JsonTemplate::object([
                    ("filter", JsonTemplate::input("filter")),
                    ("first", JsonTemplate::literal(json!(PAGE_SIZE))),
                    ("after", JsonTemplate::input("after")),
                ]),
            ))
            .declared_input("filter", ValueScalar::Json, Required::Yes)
            // Linear types `after` as a nullable `String`, and the first page of
            // a walk is the one that has no cursor yet, so the slot is published
            // as JSON: an explicit `null` is a value it must admit.
            .declared_input("after", ValueScalar::Json, Required::Yes)
            .output_pointer(
                "nodes",
                "/data/issues/nodes",
                ValueScalar::Json,
                Required::Yes,
            ),
        "issues",
    )
    .effect(Effect::read_only_documented(
        "Linear's `issues` connection query is a read; the POST is GraphQL's one method",
    )?)
    .build()?;

    let issue_create = common("issue.create")
        .body(request(
            ISSUE_CREATE,
            JsonTemplate::object([("input", JsonTemplate::input("input"))]),
        ))
        .declared_input("input", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "success",
            "/data/issueCreate/success",
            ValueScalar::Boolean,
            Required::Yes,
        )
        .output_pointer(
            "id",
            "/data/issueCreate/issue/id",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "identifier",
            "/data/issueCreate/issue/identifier",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "url",
            "/data/issueCreate/issue/url",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(at_most_once(
            "a second issue with a new identifier — or, if the same optional `id` is supplied \
             twice, an outcome Linear does not publish",
        )?)
        .build()?;

    let issue_update = common("issue.update")
        .body(request(
            ISSUE_UPDATE,
            JsonTemplate::object([
                ("id", JsonTemplate::input("id")),
                ("input", JsonTemplate::input("input")),
            ]),
        ))
        .declared_input("id", ValueScalar::String, Required::Yes)
        .declared_input("input", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "success",
            "/data/issueUpdate/success",
            ValueScalar::Boolean,
            Required::Yes,
        )
        .output_pointer(
            "id",
            "/data/issueUpdate/issue/id",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "identifier",
            "/data/issueUpdate/issue/identifier",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::inventory_only(NO_KEY)?)
        .build()?;

    let comment_create = common("comment.create")
        .body(request(
            COMMENT_CREATE,
            JsonTemplate::object([("input", JsonTemplate::input("input"))]),
        ))
        .declared_input("input", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "success",
            "/data/commentCreate/success",
            ValueScalar::Boolean,
            Required::Yes,
        )
        .output_pointer(
            "id",
            "/data/commentCreate/comment/id",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "url",
            "/data/commentCreate/comment/url",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(at_most_once(
            "a second comment on the same issue, with a new identifier",
        )?)
        .build()?;

    let team_list = page_info(
        common("team.list")
            .body(request(
                TEAM_LIST,
                JsonTemplate::object([
                    ("first", JsonTemplate::literal(json!(PAGE_SIZE))),
                    ("after", JsonTemplate::input("after")),
                ]),
            ))
            .declared_input("after", ValueScalar::Json, Required::Yes)
            .output_pointer(
                "nodes",
                "/data/teams/nodes",
                ValueScalar::Json,
                Required::Yes,
            ),
        "teams",
    )
    .effect(Effect::read_only_documented(
        "Linear's `teams` connection query is a read; the POST is GraphQL's one method",
    )?)
    .build()?;

    let user_list = page_info(
        common("user.list")
            .body(request(
                USER_LIST,
                JsonTemplate::object([
                    ("first", JsonTemplate::literal(json!(PAGE_SIZE))),
                    ("after", JsonTemplate::input("after")),
                ]),
            ))
            .declared_input("after", ValueScalar::Json, Required::Yes)
            .output_pointer(
                "nodes",
                "/data/users/nodes",
                ValueScalar::Json,
                Required::Yes,
            ),
        "users",
    )
    .effect(Effect::read_only_documented(
        "Linear's `users` connection query is a read; the POST is GraphQL's one method",
    )?)
    .build()?;

    Ok(vec![
        issue_get,
        issue_list,
        issue_create,
        issue_update,
        comment_create,
        team_list,
        user_list,
    ])
}
