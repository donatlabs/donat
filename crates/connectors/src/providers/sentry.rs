//! Sentry's Web API and its integration-platform webhook deliveries.
//!
//! Ground truth is Sentry's own published documentation, read on 2026-08-10:
//!
//! * <https://docs.sentry.io/api/requests/> — "All API requests should be made
//!   to the `/api/0/` prefix, and will return JSON as the response", with the
//!   worked example against `https://sentry.io/api/0/`.
//! * <https://docs.sentry.io/api/auth/> — "Authentication tokens are passed
//!   using an auth header", with the published example
//!   `curl -H 'Authorization: Bearer {TOKEN}' https://sentry.io/api/0/organizations/{organization_slug}/projects/`.
//! * <https://docs.sentry.io/api/permissions/> — the scope table this module's
//!   operations need: `event:read`, `project:read`, `org:read`,
//!   `project:releases`.
//! * <https://docs.sentry.io/api/pagination/> — "Pagination in the API is
//!   handled via the Link header standard", and the `results="[true|false]"`
//!   indicator.
//! * <https://docs.sentry.io/api/ratelimits/> — "Sentry rate limits every API
//!   request made to prevent abuse and resource overuse", and the five
//!   `X-Sentry-Rate-Limit-*` headers.
//! * The endpoint references for
//!   [Retrieve an Issue](https://docs.sentry.io/api/events/retrieve-an-issue/),
//!   [List an Organization's Issues](https://docs.sentry.io/api/events/list-an-organizations-issues/),
//!   [Update an Issue](https://docs.sentry.io/api/events/update-an-issue/),
//!   [Retrieve an Event for a Project](https://docs.sentry.io/api/events/retrieve-an-event-for-a-project/),
//!   [List a Project's Error Events](https://docs.sentry.io/api/events/list-a-projects-error-events/),
//!   [Retrieve a Project](https://docs.sentry.io/api/projects/retrieve-a-project/),
//!   [List an Organization's Projects](https://docs.sentry.io/api/organizations/list-an-organizations-projects/),
//!   [Retrieve an Organization's Release](https://docs.sentry.io/api/releases/retrieve-an-organizations-release/),
//!   and [List an Organization's Releases](https://docs.sentry.io/api/releases/list-an-organizations-releases/).
//! * <https://docs.sentry.io/organization/integrations/integration-platform/webhooks/>
//!   and its
//!   [issues](https://docs.sentry.io/organization/integrations/integration-platform/webhooks/issues/)
//!   page for the inbound half.
//!
//! # Region
//!
//! Sentry publishes region-specific hosts — "US region is hosted on
//! `us.sentry.io`", "DE region is hosted on `de.sentry.io`" — and says of the
//! default that "many of Sentry's APIs use `sentry.io` as the host for API
//! endpoints". An origin is part of a connector's identity, so this declaration
//! is the `sentry.io` origin only; a regional deployment is a separate
//! declaration rather than a configured host.
//!
//! # Issue paths are organization-scoped
//!
//! Sentry's current reference documents `GET`/`PUT
//! /api/0/organizations/{organization_id_or_slug}/issues/{issue_id}/` and
//! publishes no bare `/api/0/issues/{issue_id}/` route at all. The declaration
//! follows the reference rather than the older shape. The project-scoped issue
//! list is not declared either: Sentry marks it "**Deprecated**: This endpoint
//! has been replaced with the Organization Issues endpoint".
//!
//! # Pagination
//!
//! Sentry paginates with a `Link` header, and this connector declares **no
//! continuation plan for any collection**. That is a deliberate refusal rather
//! than an oversight: Sentry documents that "cursors will always be returned for
//! both a previous and a next page, **even if there are no results on these
//! pages**", and marks exhaustion with a `results="false"` link parameter. No
//! plan in spec 010 §8's closed set reads a link parameter, so a `LinkHeader`
//! walk here would follow `rel="next"` forever and fail on its own budget
//! instead of returning. Each collection therefore asks for one page of the
//! documented maximum size, exactly as `typeform.response.list` does for the
//! cursor Typeform does not publish. A plan that stops on `results="false"` is
//! the missing piece and belongs in `sdk/pagination.rs` with its own test.
//!
//! Note also that the page-size parameter is *not* uniform: the organization
//! issue list documents `limit` ("The maximum number of issues to affect. The
//! maximum is 100."), the project and release lists document `per_page`
//! ("Default and maximum allowed is 100."), and the project event list documents
//! neither. Each declaration spells the one its own endpoint publishes.
//!
//! # Effect classification
//!
//! Sentry's Web API publishes **no** idempotency key and no client-supplied
//! request identifier: the term occurs once in the whole reference, and it is a
//! behavioural note on a different endpoint ("Link a repository to a project …
//! Idempotent: returns 200 if the link already exists, 201 if created").
//!
//! `issue.update` is a `PUT`, and it is still `InventoryOnly`. Sentry documents
//! it as "Update an individual issue's attributes. **Only the attributes
//! submitted are modified.**" — which is a partial update, not a write to a
//! fixed resource identity — and publishes nothing about the effect of repeating
//! one. Spec 010 §7's `NaturalMethod` needs the provider's statement, and a
//! method alone is not it (ADR 042). ADR 063 does not reach it either: what a
//! repeated partial update produces is exactly what Sentry does not publish.
//! Everything else here is a `GET`.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::providers::inbound::{EventIdentifier, TriggerEvent};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec, Trigger};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::webhook::{SignatureEncoding, WebhookVerifier};

/// The connector name a deployment selects.
pub const NAME: &str = "sentry";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Sentry's published SaaS host.
const ORIGIN: &str = "https://sentry.io";

/// "The maximum number of issues to affect. The maximum is 100."
const ISSUE_PAGE_SIZE: &str = "100";

/// "Limit the number of rows to return in the result. Default and maximum
/// allowed is 100."
const PAGE_SIZE: &str = "100";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        let mut builder = Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Sentry's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Sentry declarations are valid"));
        for event in events() {
            builder = builder.trigger(
                Trigger::webhook(event.provider_event(), VERSION, verification())
                    .expect("a Sentry trigger declaration is valid"),
            );
        }
        builder.build().expect("the Sentry declaration is valid")
    });
    &CONNECTOR
}

/// "`Sentry-Hook-Signature`" — "This header represents a cryptographic hash
/// generated by your *Client Secret*."
pub const SIGNATURE_HEADER: &str = "Sentry-Hook-Signature";

/// "`Sentry-Hook-Resource` … This header lets you know which resource from the
/// list below triggered an action". Every trigger this connector declares is
/// the `issue` resource.
pub const RESOURCE_HEADER: &str = "Sentry-Hook-Resource";

/// "The `Request-ID` header provides a unique identifier for tracking and
/// debugging specific events."
pub const REQUEST_ID_HEADER: &str = "Request-ID";

/// Sentry's inbound signature scheme.
///
/// Sentry's own verification samples compute
/// `hmac.new(key=client_secret, msg=body, digestmod=hashlib.sha256).hexdigest()`
/// and compare it to `sentry-hook-signature` with `hmac.compare_digest` — so
/// HMAC-SHA256, the integration's Client Secret as the key, lowercase
/// hexadecimal, no prefix, and a constant-time comparison.
///
/// Both of Sentry's samples HMAC a *re-serialized* body (`JSON.stringify` /
/// `json.dumps`) rather than the bytes that arrived, which only agrees with
/// Sentry's own signer when a serializer reproduces its output exactly. This
/// connector verifies the raw bytes, which is what the header's value is
/// actually computed over.
///
/// `Sentry-Hook-Timestamp` is deliberately not part of the declared scheme:
/// Sentry lists the header and never describes it — no format, no unit, no
/// tolerance window, and no role in the signature — so there is nothing
/// published to verify against, and a window invented here would be Donat
/// policy wearing a provider's name.
pub fn verification() -> WebhookVerifier {
    WebhookVerifier::hmac_body(SIGNATURE_HEADER, SignatureEncoding::Hex)
        .expect("the Sentry signature scheme is a valid declaration")
}

/// The inbound events this connector declares (spec 013 §3).
///
/// Sentry publishes the resource and the action separately — the header
/// `Sentry-Hook-Resource: issue` and the body's `action`, which "can be
/// `created`, `resolved`, `assigned`, `archived`, or `unresolved`" — so the two
/// trigger names here are the composite spec 013 §3 asks for and each one's
/// `action` field carries the provider's half.
///
/// The event identifier is the `Request-ID` header, the only per-delivery
/// identifier Sentry publishes: the payload envelope carries `action`,
/// `installation`, `data`, and `actor` and no id of its own. Sentry documents no
/// redelivery policy, so nothing here claims `Request-ID` is stable across one.
pub fn events() -> &'static [TriggerEvent] {
    static EVENTS: LazyLock<Vec<TriggerEvent>> = LazyLock::new(|| {
        let issue = |event: &'static str| {
            TriggerEvent::declare(
                event,
                EventIdentifier::Header(REQUEST_ID_HEADER),
                [
                    ("action", "/action", ValueScalar::String, Required::Yes),
                    (
                        "installation_uuid",
                        "/installation/uuid",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    // Sentry publishes issue ids as JSON *strings*
                    // (`"id": "1234567890"`), and this declaration types them
                    // as it finds them rather than as it would prefer them.
                    (
                        "issue_id",
                        "/data/issue/id",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "short_id",
                        "/data/issue/shortId",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "title",
                        "/data/issue/title",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "culprit",
                        "/data/issue/culprit",
                        ValueScalar::String,
                        Required::No,
                    ),
                    (
                        "status",
                        "/data/issue/status",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "project_slug",
                        "/data/issue/project/slug",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "web_url",
                        "/data/issue/web_url",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                ],
            )
            .expect("a Sentry issue event declaration is valid")
        };
        vec![issue("issue.created"), issue("issue.resolved")]
    });
    &EVENTS
}

/// The ordered error map.
///
/// Sentry publishes its statuses per endpoint — `400 Bad Request`, `401
/// Unauthorized`, `403 Forbidden`, `404 Not Found` — and publishes **no error
/// body schema at all**: every documented 4xx in the reference carries no body,
/// and the `{"detail": …}` shape a client sees in practice appears in the docs
/// only inside a captured customer event payload. The map therefore reads
/// nothing from the body.
///
/// `429` is mapped even though Sentry documents it only for event ingestion:
/// the Web API's rate-limit page says a request over the limit "will be
/// rejected" without naming a status, and `429` is the one HTTP defines for it.
/// Classifying it `Http429` is the difference between backing off and treating a
/// throttle as permanent.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_status(404, ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Sentry error map is a valid declaration")
    });
    &MAP
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let issue_get = common(Operation::get(
        "issue.get",
        "/api/0/organizations/{organization}/issues/{issue_id}/",
    ))
    .path_param("organization", ValueScalar::String)
    .path_param("issue_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("shortId", "/shortId", ValueScalar::String, Required::Yes)
    .output_pointer("title", "/title", ValueScalar::String, Required::Yes)
    .output_pointer("status", "/status", ValueScalar::String, Required::Yes)
    .output_pointer("level", "/level", ValueScalar::String, Required::No)
    .output_pointer("permalink", "/permalink", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    let issue_list = common(Operation::get(
        "issue.list",
        "/api/0/organizations/{organization}/issues/",
    ))
    .path_param("organization", ValueScalar::String)
    // "A default query of `is:unresolved` is applied. To return all results,
    // use an empty query value (i.e. `?query=`)" — so the query is a declared
    // input rather than an inherited default.
    .query_input("query", "query")
    .query_static("limit", ISSUE_PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    // The collection is a bare JSON array.
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let issue_update = common(Operation::put(
        "issue.update",
        "/api/0/organizations/{organization}/issues/{issue_id}/",
    ))
    .path_param("organization", ValueScalar::String)
    .path_param("issue_id", ValueScalar::String)
    .body(JsonTemplate::object([
        ("status", JsonTemplate::input("status")),
        ("assignedTo", JsonTemplate::input("assignedTo")),
    ]))
    .success_statuses([StatusCode::OK])
    // Sentry's published 200 example for this endpoint carries no body at all,
    // so the declaration admits the empty success rather than demanding a
    // resource it never promised.
    .no_content_statuses([StatusCode::OK])
    .effect(Effect::inventory_only(
        "Sentry documents this endpoint as \"Update an individual issue's attributes. Only the \
         attributes submitted are modified.\" — a partial update rather than a write to a fixed \
         resource identity — and publishes no statement about repeating one and no idempotency \
         key anywhere in its Web API",
    )?)
    .build()?;

    let event_get = common(Operation::get(
        "event.get",
        "/api/0/projects/{organization}/{project}/events/{event_id}/",
    ))
    .path_param("organization", ValueScalar::String)
    .path_param("project", ValueScalar::String)
    // "The ID of the event. It is a 32-character hexadecimal string as reported
    // by the client."
    .path_param("event_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("eventID", "/eventID", ValueScalar::String, Required::Yes)
    .output_pointer("groupID", "/groupID", ValueScalar::String, Required::No)
    .output_pointer("title", "/title", ValueScalar::String, Required::Yes)
    .output_pointer(
        "dateCreated",
        "/dateCreated",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    // This endpoint documents neither `per_page` nor `limit` — only `cursor` —
    // so the declaration asks for the page the provider chooses and adds no
    // parameter Sentry does not publish.
    let event_list = common(Operation::get(
        "event.list",
        "/api/0/projects/{organization}/{project}/events/",
    ))
    .path_param("organization", ValueScalar::String)
    .path_param("project", ValueScalar::String)
    // "`statsPeriod` … For example, `24h`."
    .query_input("statsPeriod", "statsPeriod")
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let project_get = common(Operation::get(
        "project.get",
        "/api/0/projects/{organization}/{project}/",
    ))
    .path_param("organization", ValueScalar::String)
    .path_param("project", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("slug", "/slug", ValueScalar::String, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::Yes)
    .output_pointer("platform", "/platform", ValueScalar::String, Required::No)
    .output_pointer(
        "dateCreated",
        "/dateCreated",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    let project_list = common(Operation::get(
        "project.list",
        "/api/0/organizations/{organization}/projects/",
    ))
    .path_param("organization", ValueScalar::String)
    .query_static("per_page", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let release_get = common(Operation::get(
        "release.get",
        "/api/0/organizations/{organization}/releases/{version}/",
    ))
    .path_param("organization", ValueScalar::String)
    // "The version identifier of the release" — a free-form string, so it is
    // percent-encoded into one path segment like every other bound value.
    .path_param("version", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("version", "/version", ValueScalar::String, Required::Yes)
    .output_pointer(
        "shortVersion",
        "/shortVersion",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "dateCreated",
        "/dateCreated",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "dateReleased",
        "/dateReleased",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    let release_list = common(Operation::get(
        "release.list",
        "/api/0/organizations/{organization}/releases/",
    ))
    .path_param("organization", ValueScalar::String)
    // "Case-insensitive substring match against the release version."
    .query_input("query", "query")
    .query_static("per_page", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        issue_get,
        issue_list,
        issue_update,
        event_get,
        event_list,
        project_get,
        project_list,
        release_get,
        release_list,
    ])
}
