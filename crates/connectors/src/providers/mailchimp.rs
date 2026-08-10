//! Mailchimp's Marketing API, v3.0 — the audience surface.
//!
//! Ground truth is Mailchimp's own published documentation and its own published
//! API description, read on 2026-08-10:
//!
//! * <https://api.mailchimp.com/schema/3.0/Swagger.json?expand> — Mailchimp's
//!   published `swagger: "2.0"` description, whose `host` is
//!   `server.api.mailchimp.com`, `basePath` is `/3.0`, `schemes` is `["https"]`,
//!   and whose one security definition is `{"basicAuth": {"type": "basic"}}`.
//!   Every path, parameter, required body field, and response field below is
//!   taken from it.
//! * <https://mailchimp.com/developer/marketing/guides/quick-start/> — the data
//!   centre and the credential: "log into your Mailchimp account and look at the
//!   URL in your browser. You'll see something like
//!   `https://us19.admin.mailchimp.com/`; the `us19` part is the server prefix",
//!   with the worked call `curl -sS "https://${dc}.api.mailchimp.com/3.0/ping"
//!   --user "anystring:${apikey}"`.
//! * <https://mailchimp.com/developer/marketing/docs/errors/> — "We expose API
//!   errors in two ways: standard HTTP response codes and human-readable
//!   messages in JSON format", the `Problem Detail Document`
//!   (`{type, title, status, detail, instance}`), and the status glossary this
//!   module's error map is built from.
//!
//! # The data centre is a host label, and it is deploy-time
//!
//! Mailchimp's own description writes the host as `server.api.mailchimp.com`:
//! one variable label in front of a constant suffix, which is exactly
//! `OriginSpec::TemplatedHost` — the first of the three shapes in
//! [[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]].
//! The label is the data centre in the API key, and it is configuration rather
//! than input: an operation argument that could move it would be an argument
//! choosing an authority.
//!
//! # The credential is Basic, and the key is the password half
//!
//! Mailchimp publishes `--user "anystring:${apikey}"`: any username, the key as
//! the password. That is `AuthPlan::basic` with a declared constant username —
//! the opposite of Freshdesk's, where the key is the *username* and the password
//! is the constant ([[064-a-credentials-scheme-and-its-username-are-the-providers]]).
//! The username here is declaration material and carries no secret, which is
//! what makes the ordinary plan the right one.
//!
//! OAuth2 is Mailchimp's other published option and is not declared: its token
//! endpoint answers a token whose metadata endpoint is where the data centre
//! comes from, which would make the origin a value read out of a provider
//! response — the thing spec 010 §4 exists to forbid. A deployment configures
//! the data centre it already knows.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur once in
//! Mailchimp's published 10.7 MB Swagger description, and neither does `dedup`.
//!
//! `member.upsert` is nevertheless executable, and on the strongest evidence in
//! this batch. Mailchimp publishes it as `PUT
//! /lists/{list_id}/members/{subscriber_hash}` — a fixed resource identity,
//! "The MD5 hash of the lowercase version of the list member's email address" —
//! titled "Add or update list member" and described as "Add or update a list
//! member". Its request body then makes the repeat semantics explicit rather
//! than implied: `status_if_new` is required, and is documented as "Subscriber's
//! status. This value is required only if the email address is not already
//! present on the list". A provider that publishes a *different field for the
//! first send* has published what the second one does. That is
//! `ProviderIdempotent::NaturalMethod` over the method spec 010 §7 admits it
//! for, and it is the contrast with `salesforce.record.upsert` and
//! `zoho_crm.record.upsert`, which are the same semantics over `PATCH` and
//! `POST` and stay unreachable.
//!
//! `POST /lists/{list_id}/members` — "Add a new member to the list" — is
//! deliberately **not declared**. It is the same effect as the upsert with a
//! worse contract, and a connector that published both would be publishing one
//! operation a Process can reach and one it cannot, for the same intent.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "mailchimp";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key carrying the data centre label.
///
/// It is spelled `server` because that is what Mailchimp's own description calls
/// the host variable and what its quick-start calls the value: "the `us19` part
/// is the server prefix".
pub const SERVER: &str = "server";

/// `host: server.api.mailchimp.com`, `schemes: [https]`.
const HOST_TEMPLATE: &str = "{server}.api.mailchimp.com";

/// `basePath: /3.0`.
const PREFIX: &str = "/3.0";

/// "`count` — The number of records to return. Default value is 10. Maximum
/// value is 1000." 500 is this declaration's choice inside that ceiling: a
/// Mailchimp member record is large, and the SDK carries one bounded aggregate.
const PAGE_SIZE: u32 = 500;

/// The Basic username.
///
/// Mailchimp's own worked example is `--user "anystring:${apikey}"`: the
/// username is not read at all, and this is the literal it publishes. It is
/// declaration material and carries nothing secret.
pub const BASIC_USERNAME: &str = "anystring";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::templated_host("https", HOST_TEMPLATE, None)
                    .expect("Mailchimp's published host template is valid"),
            )
            .credential(CredentialSpec::for_plan(
                AuthPlan::basic(BASIC_USERNAME).expect("Mailchimp's published username is valid"),
            ))
            .operations(operations().expect("the Mailchimp declarations are valid"))
            .build()
            .expect("the Mailchimp declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map, from Mailchimp's published error glossary.
///
/// It is keyed on the status alone. Mailchimp's error body carries a `title` —
/// "API Key Invalid", "Resource Not Found", "TooManyRequests" — and publishes it
/// as human-readable prose beside the status rather than as a machine-readable
/// code set; the glossary itself is organised by status.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 … Bad Request: This is the generic error type for not being
            // able to process the request", "Invalid Resource: The submitted
            // POST body failed our input validation", "JSON Parse Exception".
            // "414 Resource Nesting Too Deep" and "422 InvalidMethodOverride"
            // are the same shape: the request this deployment sent.
            .on_statuses([400, 414, 422], ConnectorErrorClass::Validation)
            // "401 API Key Invalid: The API key is either invalid or disabled."
            .on_status(401, ConnectorErrorClass::Authentication)
            // "403 Forbidden: Either the user who created the API key no longer
            // has access to the account, or their user level doesn't allow
            // access to the endpoint", "404 Resource Not Found", "405 Method Not
            // Allowed", "426 … Please make your request via HTTPS rather than
            // HTTP." None is fixed by sending the same request again.
            .on_statuses([403, 404, 405, 426], ConnectorErrorClass::Permanent)
            // "429 TooManyRequests: You have exceeded the limit of 10
            // simultaneous connections."
            .on_status(429, ConnectorErrorClass::Http429)
            // "5xx status codes suggest a problem on Mailchimp's end." Its own
            // note about the CDN belongs here too: "If you receive an HTTP 502
            // error with an HTML body, your request may have timed out and been
            // closed by our CDN."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // The `X-Request-Id` Mailchimp's own error example carries.
            .correlation_header("request_id", "x-request-id")
            .build()
            .expect("the Mailchimp error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each collection.
///
/// Mailchimp publishes one regime for every collection — "`count` … `offset`:
/// Used for pagination, this is the number of records from a collection to
/// skip" — and each answer carries its own item list beside `total_items`.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static LISTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/lists", "offset", "count", PAGE_SIZE)
            .expect("the Mailchimp list plan is valid")
    });
    static MEMBERS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/members", "offset", "count", PAGE_SIZE)
            .expect("the Mailchimp member plan is valid")
    });
    match operation_id {
        "list.list" => Some(&LISTS),
        "member.list" => Some(&MEMBERS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The fields of one audience, as Mailchimp's `List` schema declares them.
fn list_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer(
            "date_created",
            "/date_created",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "member_count",
            "/stats/member_count",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "unsubscribe_count",
            "/stats/unsubscribe_count",
            ValueScalar::Int64,
            Required::No,
        )
}

/// The fields of one member, as Mailchimp's `Member` schema declares them.
fn member_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "email_address",
            "/email_address",
            ValueScalar::String,
            Required::No,
        )
        // "A unique identifier for the email address across all Mailchimp
        // lists."
        .output_pointer(
            "unique_email_id",
            "/unique_email_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer(
            "merge_fields",
            "/merge_fields",
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer("list_id", "/list_id", ValueScalar::String, Required::No)
        .output_pointer(
            "last_changed",
            "/last_changed",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes: spec 025 §3's Mailchimp surface,
/// which is the audience and its members and nothing else. Campaigns, automations,
/// reports, and the batch endpoint are out of scope (spec 025 §5).
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Get information about all lists in the account."
    let list_list = common(Operation::get("list.list", &format!("{PREFIX}/lists")))
        .success_statuses([StatusCode::OK])
        .output_pointer("lists", "/lists", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "total_items",
            "/total_items",
            ValueScalar::Int64,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "Get information about a specific list in your Mailchimp account."
    let list_get = list_outputs(
        common(Operation::get(
            "list.get",
            &format!("{PREFIX}/lists/{{list_id}}"),
        ))
        .path_param("list_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Get information about members in a specific Mailchimp list."
    let member_list = common(Operation::get(
        "member.list",
        &format!("{PREFIX}/lists/{{list_id}}/members"),
    ))
    .path_param("list_id", ValueScalar::String)
    // "`status` — The subscriber's status." Which members a deployment means is
    // the caller's, and Mailchimp's own default is every one of them.
    .query_input("status", "status")
    .success_statuses([StatusCode::OK])
    .output_pointer("members", "/members", ValueScalar::Json, Required::Yes)
    .output_pointer(
        "total_items",
        "/total_items",
        ValueScalar::Int64,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Get information about a specific list member, including a currently
    // subscribed, unsubscribed, or bounced member." The path segment is "The
    // MD5 hash of the lowercase version of the list member's email address.
    // This endpoint also accepts a list member's email address or contact_id."
    let member_get = member_outputs(
        common(Operation::get(
            "member.get",
            &format!("{PREFIX}/lists/{{list_id}}/members/{{subscriber_hash}}"),
        ))
        .path_param("list_id", ValueScalar::String)
        .path_param("subscriber_hash", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Add or update a list member." The two required body fields are
    // `email_address` — "required only if the email address is not already
    // present on the list" — and `status_if_new`.
    let member_upsert = member_outputs(
        common(Operation::put(
            "member.upsert",
            &format!("{PREFIX}/lists/{{list_id}}/members/{{subscriber_hash}}"),
        ))
        .path_param("list_id", ValueScalar::String)
        .path_param("subscriber_hash", ValueScalar::String)
        .body(JsonTemplate::object([
            ("email_address", JsonTemplate::input("email_address")),
            ("status_if_new", JsonTemplate::input("status_if_new")),
            ("status", JsonTemplate::input("status")),
            ("merge_fields", JsonTemplate::input("merge_fields")),
        ]))
        .declared_input("email_address", ValueScalar::String, Required::Yes)
        .declared_input("status_if_new", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::provider_idempotent_natural_method(
        "Mailchimp publishes this as `PUT /lists/{list_id}/members/{subscriber_hash}` against a \
         fixed resource identity — \"The MD5 hash of the lowercase version of the list member's \
         email address\" — titled \"Add or update list member\" and described as \"Add or update a \
         list member\". Its required `status_if_new` is documented as \"Subscriber's status. This \
         value is required only if the email address is not already present on the list\", which \
         is Mailchimp publishing what the *second* send does: it updates the one member the hash \
         names rather than adding another. A repeat therefore leaves exactly one member.",
    )?)
    .build()?;

    Ok(vec![
        list_list,
        list_get,
        member_list,
        member_get,
        member_upsert,
    ])
}
