//! Cloudflare's Client API v4 — zones and their DNS records.
//!
//! Ground truth is Cloudflare's own published OpenAPI, read on 2026-08-10:
//! <https://github.com/cloudflare/api-schemas>, `openapi.json`, whose single
//! declared server is `https://api.cloudflare.com/client/v4` ("Client API") and
//! whose `api_token` security scheme is `type: http`, `scheme: bearer`.
//!
//! The endpoint statements this module is built from, quoted from that document:
//!
//! * "List Zones — Lists, searches, sorts, and filters your zones."
//! * "Zone Details — Retrieves detailed information about a specific zone
//!   identified by its zone ID."
//! * "Create Zone — Creates a new zone (domain) in your Cloudflare account. The
//!   zone is created in a pending state and must be activated by updating your
//!   domain's nameservers to point to Cloudflare".
//! * "Edit Zone — Edits a zone. Only one zone property can be changed at a
//!   time."
//! * "List DNS Records — List, search, sort, and filter a zones' DNS records."
//! * "DNS Record Details — Retrieves details for a specific DNS record in the
//!   zone."
//! * "Create DNS Record — Create a new DNS record for a zone."
//! * "Update DNS Record — Update an existing DNS record." (`PATCH`)
//! * "Overwrite DNS Record — Overwrite an existing DNS record." (`PUT`)
//!
//! # A success envelope that can carry a failure
//!
//! Every response is the same envelope: `errors`, `messages`, `success`, and
//! `result`. `success` is *required*, and on the documented success response
//! Cloudflare constrains it to `enum: [true]` — which makes a `2xx` carrying
//! `"success": false` a failure the provider published in advance. [`decode`] is
//! therefore a body gate between the status check and the output pointers, the
//! Slack precedent
//! ([[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]),
//! and it is also this connector's page gate, so a failing page of a walk fails
//! the walk rather than contributing an empty item list.
//!
//! # Pagination
//!
//! Cloudflare publishes `page` (default `1`) and `per_page` beside a
//! `result_info` block, and the declared plan is that page-number regime. It
//! ends on a page shorter than the one asked for, which is the absence every
//! plan in spec 010 §8's closed set reads; `result_info.total_pages` is not
//! read, because no plan reads a count and a walk that derived its next page
//! from a provider value could be restarted by one.
//!
//! Page sizes are Cloudflare's own: zones publish "per_page … default 20 …
//! maximum 50", and DNS records publish a default of 100.
//!
//! # Effect classification
//!
//! **`dns_record.update` is the batch's one `NaturalMethod`.** Cloudflare
//! publishes two update verbs on the same fixed record id and names them
//! differently: `PATCH` is "Update an existing DNS record" and `PUT` is
//! "Overwrite DNS Record — Overwrite an existing DNS record." A statement that
//! the request *overwrites* the record at a fixed identity is the provider's own
//! replacement semantics, and the contrast with the partial verb published one
//! line below it is what makes it a statement rather than an inference from the
//! method (ADR 042). Two identical sends leave one record, which
//! `cloudflare_effects_are_classified` proves against the stub, as spec 010 §7
//! requires.
//!
//! `zone.update` is the `PATCH`, and it stays `InventoryOnly`: "Only one zone
//! property can be changed at a time" is a partial update, and Cloudflare
//! publishes nothing about repeating one.
//!
//! **No idempotency key anywhere on this surface.** The term `idempot` occurs 17
//! times in Cloudflare's whole published OpenAPI and every occurrence is a
//! repeat-safety note on some other product's endpoint — a SAML certificate set,
//! a WARP subnet delete, an origin cloud-region mapping, a registrar domain
//! name. No zone or DNS-record endpoint publishes a client-supplied request
//! identifier or deduplication key, so the two creates are `AtMostOnce`
//! (ADR 063).

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
pub const NAME: &str = "cloudflare";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Cloudflare's published API host.
const ORIGIN: &str = "https://api.cloudflare.com";

/// The declared server is `https://api.cloudflare.com/client/v4`.
const PREFIX: &str = "/client/v4";

/// "per_page … default: 20 … maximum: 50" on the zone list.
const ZONE_PAGE_SIZE: u32 = 50;

/// The DNS record list publishes a default of 100.
const RECORD_PAGE_SIZE: u32 = 100;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Cloudflare's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Cloudflare declarations are valid"))
            .build()
            .expect("the Cloudflare declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// Cloudflare publishes `errors` as a list of `{code, message}` and publishes no
/// enumeration of the codes across products, so the map is keyed on statuses
/// only and reads nothing from the body. `429` is Cloudflare's documented
/// rate-limit status for the Client API.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 405, 409, 410], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Cloudflare error map is a valid declaration")
    });
    &MAP
}

/// Whether one Cloudflare envelope reports success.
///
/// `None` is a body that is not an envelope at all, which is outside the
/// declared contract rather than a failure of the operation.
fn envelope_success(body: &[u8]) -> Option<bool> {
    serde_json::from_slice::<JsonValue>(body)
        .ok()?
        .pointer("/success")?
        .as_bool()
}

/// Decode one Cloudflare response: the declared success statuses, then the
/// envelope, then the declared contract.
///
/// The middle step is the point. Cloudflare's success schema constrains
/// `success` to `true`, so a `2xx` whose envelope says otherwise is a failure it
/// published in advance, and reading the output pointers out of it would publish
/// a `null` result as an answer.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    match envelope_success(body) {
        Some(true) => operation.decode_response(status, body),
        Some(false) => Err(error_map().classify(status, headers, body)),
        None => Err(ConnectorFailure::invariant(
            "connector provider answered outside its declared contract",
        )),
    }
}

/// The continuation plan of each collection.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static ZONES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("/result", "page", "per_page", ZONE_PAGE_SIZE)
            .expect("the Cloudflare zone plan is valid")
    });
    static RECORDS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("/result", "page", "per_page", RECORD_PAGE_SIZE)
            .expect("the Cloudflare DNS record plan is valid")
    });
    match operation_id {
        "zone.list" => Some(&ZONES),
        "dns_record.list" => Some(&RECORDS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The searched documentation behind every at-most-once class here.
const NO_KEY: &str = "the term `idempot` occurs 17 times in Cloudflare's own published OpenAPI \
                      (`cloudflare/api-schemas`, `openapi.json`, server \
                      `https://api.cloudflare.com/client/v4`) and not once on a zone or DNS-record \
                      endpoint: every occurrence is a repeat-safety note on another product — a \
                      SAML certificate set, a WARP IP subnet delete, an origin cloud-region \
                      mapping, and a registrar note that a domain name is \"a natural idempotency \
                      key for registration requests\". Neither create declared here publishes a \
                      request header, a body property or a query parameter carrying a \
                      client-supplied request identifier or a deduplication behaviour";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The published zone properties a Process reads, out of the envelope's
/// `result`.
fn zone_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/result/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/result/name", ValueScalar::String, Required::No)
        .output_pointer(
            "status",
            "/result/status",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("type", "/result/type", ValueScalar::String, Required::No)
        .output_pointer(
            "paused",
            "/result/paused",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer(
            "created_on",
            "/result/created_on",
            ValueScalar::String,
            Required::No,
        )
}

/// The published DNS record properties a Process reads.
fn record_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/result/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/result/name", ValueScalar::String, Required::No)
        .output_pointer("type", "/result/type", ValueScalar::String, Required::No)
        .output_pointer(
            "content",
            "/result/content",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("ttl", "/result/ttl", ValueScalar::Int64, Required::No)
        .output_pointer(
            "proxied",
            "/result/proxied",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer(
            "modified_on",
            "/result/modified_on",
            ValueScalar::String,
            Required::No,
        )
}

/// One paginated collection, read through the pointers the walk's aggregate
/// lands on.
fn collection(builder: OperationBuilder) -> OperationBuilder {
    builder
        .success_statuses([StatusCode::OK])
        .output_pointer("result", "/result", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "total_count",
            "/result_info/total_count",
            ValueScalar::Int64,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let zone_get = zone_output(
        common(Operation::get(
            "zone.get",
            &format!("{PREFIX}/zones/{{zone_id}}"),
        ))
        .path_param("zone_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // Cloudflare publishes ten filters for this collection and no value meaning
    // "everything" for any of them. A declared query input renders on every
    // request and would therefore be mandatory, so none is declared and the
    // list is the account's zones.
    let zone_list = collection(common(Operation::get(
        "zone.list",
        &format!("{PREFIX}/zones"),
    )))
    .effect(Effect::read_only())
    .build()?;

    // "Creates a new zone (domain) in your Cloudflare account. The zone is
    // created in a pending state".
    let zone_create = zone_output(
        common(Operation::post("zone.create", &format!("{PREFIX}/zones")))
            .body(JsonTemplate::object([
                ("name", JsonTemplate::input("name")),
                (
                    "account",
                    // The slot is `account` rather than `account_id`: the SDK
                    // reserves that name for the AWS connectors' deploy-time
                    // target, and a reserved name is one no connector may
                    // publish anywhere.
                    JsonTemplate::object([("id", JsonTemplate::input("account"))]),
                ),
                ("type", JsonTemplate::input("type")),
            ]))
            .declared_input("name", ValueScalar::String, Required::Yes)
            .declared_input("account", ValueScalar::String, Required::Yes)
            .success_statuses([StatusCode::OK]),
    )
    .effect(at_most_once(
        "either a second pending zone for the same domain, or Cloudflare's refusal of the \
         duplicate — it publishes neither outcome for a repeated create, so which one a Process \
         gets is not something this connector can promise either way, and the failing case is a \
         create the Process cannot tell from a create that never happened",
    )?)
    .build()?;

    // "Edits a zone. Only one zone property can be changed at a time."
    let zone_update = zone_output(
        common(Operation::patch(
            "zone.update",
            &format!("{PREFIX}/zones/{{zone_id}}"),
        ))
        .path_param("zone_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("paused", JsonTemplate::input("paused")),
            ("type", JsonTemplate::input("type")),
        ]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Cloudflare publishes this endpoint as \"Edits a zone. Only one zone property can be \
         changed at a time.\" — a partial update rather than a write to a fixed resource identity, \
         which is not spec 010 §7's NaturalMethod evidence — and publishes no statement about what \
         a second identical send produces. Its `PUT` twin, one line down on the DNS record, is \
         published as \"Overwrite\" and is classified accordingly",
    )?)
    .build()?;

    let record_get = record_output(
        common(Operation::get(
            "dns_record.get",
            &format!("{PREFIX}/zones/{{zone_id}}/dns_records/{{dns_record_id}}"),
        ))
        .path_param("zone_id", ValueScalar::String)
        .path_param("dns_record_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let record_list = collection(
        common(Operation::get(
            "dns_record.list",
            &format!("{PREFIX}/zones/{{zone_id}}/dns_records"),
        ))
        // Cloudflare publishes thirty-odd filters here, none with a value
        // meaning "everything"; the unfiltered list is the zone's records.
        .path_param("zone_id", ValueScalar::String),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Create a new DNS record for a zone."
    let record_create = record_output(
        common(Operation::post(
            "dns_record.create",
            &format!("{PREFIX}/zones/{{zone_id}}/dns_records"),
        ))
        .path_param("zone_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("type", JsonTemplate::input("type")),
            ("name", JsonTemplate::input("name")),
            ("content", JsonTemplate::input("content")),
            ("ttl", JsonTemplate::input("ttl")),
            ("proxied", JsonTemplate::input("proxied")),
            ("comment", JsonTemplate::input("comment")),
        ]))
        .declared_input("type", ValueScalar::String, Required::Yes)
        .declared_input("name", ValueScalar::String, Required::Yes)
        .declared_input("content", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(at_most_once(
        "a second DNS record with a new id at the same name — which, for the record types \
         Cloudflare allows more than one of at a name, means traffic for that name is answered by \
         both",
    )?)
    .build()?;

    // "Overwrite DNS Record — Overwrite an existing DNS record", against the
    // record id in the path. This is the batch's one NaturalMethod.
    let record_update = record_output(
        common(Operation::put(
            "dns_record.update",
            &format!("{PREFIX}/zones/{{zone_id}}/dns_records/{{dns_record_id}}"),
        ))
        .path_param("zone_id", ValueScalar::String)
        .path_param("dns_record_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("type", JsonTemplate::input("type")),
            ("name", JsonTemplate::input("name")),
            ("content", JsonTemplate::input("content")),
            ("ttl", JsonTemplate::input("ttl")),
            ("proxied", JsonTemplate::input("proxied")),
            ("comment", JsonTemplate::input("comment")),
        ]))
        .declared_input("type", ValueScalar::String, Required::Yes)
        .declared_input("name", ValueScalar::String, Required::Yes)
        .declared_input("content", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::provider_idempotent_natural_method(
        "Cloudflare publishes this endpoint as \"Overwrite DNS Record — Overwrite an existing DNS \
         record.\", a `PUT` against the `dns_record_id` in the path, and publishes the partial verb \
         separately on the same identity as \"Update DNS Record — Update an existing DNS record.\" \
         (`PATCH`). A request that overwrites the record at a fixed id leaves the same one record \
         however many times it is sent",
    )?)
    .build()?;

    Ok(vec![
        zone_get,
        zone_list,
        zone_create,
        zone_update,
        record_get,
        record_list,
        record_create,
        record_update,
    ])
}
