//! Grafana's HTTP API — alert rules and dashboards, on the instance a
//! deployment operates.
//!
//! Ground truth is Grafana's own published documentation and the API
//! description Grafana ships in its own repository, read on 2026-08-10:
//!
//! * `public/api-merged.json` in `grafana/grafana` — "Grafana HTTP API.", a
//!   Swagger 2.0 document with `basePath: /api`, whose only two security
//!   definitions are an `apiKey` in the `Authorization` header and `basic`.
//! * <https://grafana.com/docs/grafana/latest/developer-resources/api-reference/http-api/folder_dashboard_search/>
//!   — `GET /api/search/`, its query parameters ("**limit** – Limit the number
//!   of returned results (max is 5000; default is 1000)", "**page** – Use this
//!   parameter to access hits beyond limit. Numbering starts at 1. limit param
//!   acts as page size."), and the worked request carrying `Authorization:
//!   Bearer <SERVICE_ACCOUNT_TOKEN>`.
//! * <https://grafana.com/docs/grafana/latest/developer-resources/api-reference/http-api/alerting_provisioning/>
//!   — "Get all the alert rules.", "Get a specific alert rule by UID.", "Update
//!   an existing alert rule.", each with the same `Authorization: Bearer` form.
//! * <https://grafana.com/docs/grafana/latest/developer-resources/api-reference/http-api/dashboard/>
//!   — `GET /api/dashboards/uid/:uid`.
//!
//! Grafana publishes a deprecation note beside all of these: "Starting in
//! Grafana 13, `/api` endpoints are being deprecated in favor of the `/apis`
//! route … **This change doesn't disrupt or break your current setup**. Legacy
//! APIs are not being disabled and remain fully accessible and operative, but
//! `/api` routes will no longer be updated." This connector declares the `/api`
//! surface, which is the one Grafana documents endpoint-by-endpoint today; the
//! `/apis` surface is a different declaration when Grafana finishes publishing
//! it, exactly as a regional host is a different declaration.
//!
//! # The origin is the instance, and the deployment names it
//!
//! A Grafana instance is the deployment's own — self-hosted at whatever host it
//! owns, or a Grafana Cloud stack — so there is no vendor suffix a templated
//! host could sit under. This connector declares `OriginSpec::DeploymentOrigin`
//! for the reason recorded in
//! `knowledgebase/declarative-saas/decisions/082-*` and validates the configured
//! value before a listener opens: `https` only, because the declared credential
//! is a bearer token, and no path, because an origin is a scheme, a host and a
//! port.
//!
//! # Pagination
//!
//! Only the search publishes a paging regime — `page` numbered from 1 with
//! `limit` acting as the page size — so only the search declares a plan, and it
//! ends on a page shorter than the one asked for. The alert-rule list publishes
//! no paging parameters at all: it answers a bare array of every rule, so it
//! declares no plan rather than inventing one
//! ([[058-a-declared-walk-is-the-executors-walk]]).
//!
//! # Effect classification
//!
//! **Machine-readable description, no key in it.** The string `idempot` does not
//! occur anywhere in `api-merged.json` — Grafana's own description of its whole
//! HTTP API, every endpoint and every definition — and no endpoint declared here
//! publishes a client-supplied request identifier or deduplication key.
//!
//! `alert_rule.update` is a `PUT` against a fixed UID and it is still
//! `InventoryOnly`. Grafana publishes it as "Update an existing alert rule." and
//! publishes nothing about the effect of repeating one; spec 010 §7's
//! `NaturalMethod` needs the provider's own repeat statement, and a method alone
//! is not it (ADR 042). Everything else here is a `GET`.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{
    JsonTemplate, Operation, OperationBuilder, OperationError, Origin, Required,
};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "grafana";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key carrying the instance's whole origin.
pub const INSTANCE_ORIGIN: &str = "instance_origin";

/// `basePath: /api`.
const PREFIX: &str = "/api";

/// Inside Grafana's own published ceiling: "max is 5000; default is 1000". A
/// page this size stays well inside the SDK's 1 MB body bound while still
/// walking a large instance in few requests.
const PAGE_SIZE: u32 = 100;

/// This connector's declaration.
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::deployment_origin(INSTANCE_ORIGIN)
                    .expect("the Grafana origin key is valid"),
            )
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Grafana declarations are valid"))
            .build()
            .expect("the Grafana declaration is valid")
    });
    &CONNECTOR
}

/// Whether a configured instance origin is one this connector may send its
/// declared credential to.
pub fn validate_instance_origin(value: &str) -> Result<(), OperationError> {
    let origin = Origin::parse(value)?;
    if origin.as_url().scheme() != "https" {
        return Err(OperationError::new(
            "a Grafana instance origin must be https: this connector's credential is a service \
             account token and an http instance would carry it in clear",
        ));
    }
    Ok(())
}

/// The ordered error map.
///
/// Grafana publishes its statuses per endpoint: `400`, `401`, `403`, `404`,
/// `406` and `500` on the dashboard read, `401`, `422` and `500` on the search,
/// `403` and `404` on the alert-rule reads. `429` is not published for the HTTP
/// API at all; a Grafana instance is served by an ordinary host that may
/// throttle, and answering that `permanent` would end a Process for a condition
/// that clears by itself.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 406, 409], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Grafana error map is a valid declaration")
    });
    &MAP
}

/// Decode one Grafana response: the declared success statuses, then the declared
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

/// The continuation plan of each collection.
///
/// Only the search publishes one, and it is a page-number regime whose first
/// page is 1. The response is a bare array, so the items pointer is RFC 6901's
/// empty pointer.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static SEARCH: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("", "page", "limit", PAGE_SIZE)
            .expect("the Grafana search plan is valid")
    });
    match operation_id {
        "dashboard.search" => Some(&SEARCH),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Get all the alert rules." The response is a bare array of
    // `ProvisionedAlertRule`, and the endpoint publishes no paging parameters.
    let alert_rule_list = common(Operation::get(
        "alert_rule.list",
        &format!("{PREFIX}/v1/provisioning/alert-rules"),
    ))
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Get a specific alert rule by UID."
    let alert_rule_get = common(Operation::get(
        "alert_rule.get",
        &format!("{PREFIX}/v1/provisioning/alert-rules/{{uid}}"),
    ))
    .path_param("uid", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("uid", "/uid", ValueScalar::String, Required::Yes)
    .output_pointer("title", "/title", ValueScalar::String, Required::Yes)
    .output_pointer("folderUID", "/folderUID", ValueScalar::String, Required::No)
    .output_pointer("ruleGroup", "/ruleGroup", ValueScalar::String, Required::No)
    .output_pointer("condition", "/condition", ValueScalar::String, Required::No)
    .output_pointer("isPaused", "/isPaused", ValueScalar::Boolean, Required::No)
    .output_pointer("updated", "/updated", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "Update an existing alert rule." Grafana's required body fields are
    // `orgID`, `folderUID`, `ruleGroup`, `title`, `condition`, `data`,
    // `noDataState`, `execErrState` and `for`.
    let alert_rule_update = common(Operation::put(
        "alert_rule.update",
        &format!("{PREFIX}/v1/provisioning/alert-rules/{{uid}}"),
    ))
    .path_param("uid", ValueScalar::String)
    .body(JsonTemplate::object([
        ("title", JsonTemplate::input("title")),
        ("folderUID", JsonTemplate::input("folderUID")),
        ("ruleGroup", JsonTemplate::input("ruleGroup")),
        ("condition", JsonTemplate::input("condition")),
        ("data", JsonTemplate::input("data")),
        ("noDataState", JsonTemplate::input("noDataState")),
        ("execErrState", JsonTemplate::input("execErrState")),
        ("for", JsonTemplate::input("for")),
        ("isPaused", JsonTemplate::input("isPaused")),
    ]))
    .declared_input("data", ValueScalar::Json, Required::Yes)
    .success_statuses([StatusCode::OK])
    .effect(Effect::inventory_only(
        "Grafana publishes this endpoint as \"Update an existing alert rule.\" and publishes no \
         statement at all about the effect of repeating one: the string `idempot` does not occur \
         anywhere in `api-merged.json`, Grafana's own description of its whole HTTP API. Spec 010 \
         §7's NaturalMethod needs the provider's own repeat statement and a `PUT` alone is not it \
         (ADR 042), and ADR 063 needs a recorded consequence, which a replacement whose effect the \
         provider never described does not have",
    )?)
    .build()?;

    // "Get dashboard by uid." The response is `{"dashboard": …, "meta": …}`.
    let dashboard_get = common(Operation::get(
        "dashboard.get",
        &format!("{PREFIX}/dashboards/uid/{{uid}}"),
    ))
    .path_param("uid", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("uid", "/dashboard/uid", ValueScalar::String, Required::Yes)
    .output_pointer(
        "title",
        "/dashboard/title",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "version",
        "/dashboard/version",
        ValueScalar::Int64,
        Required::No,
    )
    .output_pointer(
        "folderUid",
        "/meta/folderUid",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("url", "/meta/url", ValueScalar::String, Required::No)
    .output_pointer(
        "updated",
        "/meta/updated",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Search folders and dashboards". The response is a bare array of hits.
    let dashboard_search = common(Operation::get(
        "dashboard.search",
        &format!("{PREFIX}/search"),
    ))
    // "**query** – Search Query". It is the one filter Grafana publishes an
    // "everything" value for — its own worked request is
    // `GET /api/search?query=&starred=false` — and a declared query input
    // renders on every request, so it is the only one declared.
    .query_input("query", "query")
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        alert_rule_list,
        alert_rule_get,
        alert_rule_update,
        dashboard_get,
        dashboard_search,
    ])
}
