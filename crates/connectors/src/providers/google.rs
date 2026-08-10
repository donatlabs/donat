//! What the four Google Workspace connectors share (spec 014).
//!
//! Ground truth is Google's own published material, read on 2026-08-10: the
//! machine-readable discovery document of each API —
//! `https://sheets.googleapis.com/$discovery/rest?version=v4`,
//! `https://www.googleapis.com/discovery/v1/apis/drive/v3/rest`,
//! `https://gmail.googleapis.com/$discovery/rest?version=v1`,
//! `https://www.googleapis.com/discovery/v1/apis/calendar/v3/rest` (revisions
//! `20260803`, `20260805`, `20260803`, `20260803`) — plus each API's own
//! error-handling guide. Every scope list, path, parameter name, and response
//! field in these four modules is read from those documents; nothing here is
//! copied from a third-party integration.
//!
//! Three things live here because all four connectors answer them identically,
//! and duplicating any of them four times is how two descriptions of one
//! provider start to disagree.
//!
//! # 1. The error envelope
//!
//! Google publishes one error object for all four APIs. Drive's *Resolve
//! errors* page prints it:
//!
//! ```text
//! { "error": { "code": [HTTP_CODE],
//!              "errors": [ { "domain": "global", "reason": "[ERROR_REASON]",
//!                            "message": "[DESCRIPTION]", "location": …,
//!                            "locationType": … } ],
//!              "message": "[ERROR_MESSAGE]" } }
//! ```
//!
//! The `reason` is the stable machine-readable half, which is why
//! [`error_map`] declares `/error/errors/0/reason` as its code pointer and
//! reads nothing else out of a failure body.
//!
//! # 2. A quota refusal is not always a 429
//!
//! This is the one classification that a status-only map gets wrong. All four
//! APIs document rate and quota exhaustion under **`403`** as well as `429`:
//! Drive lists `rateLimitExceeded`, `userRateLimitExceeded`,
//! `dailyLimitExceeded`, and `sharingRateLimitExceeded`; Gmail lists
//! `dailyLimitExceeded`, `rateLimitExceeded`, and `userRateLimitExceeded`;
//! Calendar lists `userRateLimitExceeded`, `rateLimitExceeded`, and
//! `quotaExceeded`. Every one of them means "try again later", and a Process
//! that declares `retry_on: [http_429]` must route them there — so the ordered
//! map puts the code rules ahead of the bare `403` rule, and the bare `403`
//! (permission denied, insufficient scope) stays `authentication`.
//!
//! Google publishes no `Retry-After` for these APIs — its guidance is
//! "truncated exponential backoff" with a maximum of "typically 32 or 64
//! seconds" — so the retry hint on these failures is whatever the response
//! actually carried, clamped by the SDK, and absent when Google sent none.
//!
//! # 3. A success envelope that carries a failure is not a success
//!
//! Two of these APIs report a per-item failure *inside* a `200`, and reporting
//! either as a success would hand a Process a partial answer it cannot tell
//! from a complete one:
//!
//! * Calendar's `freeBusy` reply carries `calendars.<id>.errors[]` and
//!   `groups.<id>.errors[]`, each an `Error { reason, domain }` whose
//!   documented reasons are "`groupTooBig`", "`tooManyCalendarsRequested`",
//!   "`notFound`", and "`internalError`".
//! * Drive's `FileList` carries `incompleteSearch`: "Whether the search process
//!   was incomplete. If true, then some search results might be missing, since
//!   all documents were not searched."
//!
//! [`refuse_error_envelope`] adds the fail-closed half that is **this
//! workspace's own rule and not a Google statement**: a `2xx` body carrying
//! Google's canonical top-level `error` object is refused rather than decoded.
//! No response schema of any operation these four connectors declare has a
//! top-level `error` property — the only schema in the four discovery documents
//! that does is Drive's long-running `Operation`, which none of them returns —
//! so the rule costs a legitimate success nothing and closes the one shape in
//! which a failure could otherwise be extracted as output.
//!
//! See `knowledgebase/declarative-saas/decisions/056-*`.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use serde_json::Value as JsonValue;

use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{Operation, OperationError};
use crate::sdk::transport::MAX_HTTP_BODY_BYTES;

/// Where Google publishes the stable machine-readable half of a failure.
const REASON_POINTER: &str = "/error/errors/0/reason";

/// A `2xx` whose body is Google's error object.
pub const SUCCESS_CARRIES_ERROR: ConnectorFailure = ConnectorFailure::new(
    ConnectorErrorClass::Permanent,
    "google_success_envelope_carries_error",
    "the provider answered a success status with an error envelope",
);

/// A `2xx` whose body reports a failure against one of the items asked for.
pub const PARTIAL_FAILURE: ConnectorFailure = ConnectorFailure::new(
    ConnectorErrorClass::Permanent,
    "google_partial_failure",
    "the provider answered successfully for some of the requested items and failed for others",
);

/// A `2xx` aggregate the provider itself says is incomplete.
pub const INCOMPLETE_RESULT: ConnectorFailure = ConnectorFailure::new(
    ConnectorErrorClass::Permanent,
    "google_incomplete_result",
    "the provider answered with a result it reports as incomplete",
);

/// The ordered error map every Google Workspace connector in this batch shares.
///
/// The rules that read a `reason` come first, because the same `403` is a
/// permission refusal or a quota refusal depending on it, and only one of the
/// two is worth retrying.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer(REASON_POINTER)
            // Every documented rate/quota reason, whatever status carries it.
            // Drive: "rateLimitExceeded", "userRateLimitExceeded",
            // "dailyLimitExceeded", "sharingRateLimitExceeded". Gmail adds
            // nothing new; Calendar adds "quotaExceeded".
            .on_code("rateLimitExceeded", ConnectorErrorClass::Http429)
            .on_code("userRateLimitExceeded", ConnectorErrorClass::Http429)
            .on_code("dailyLimitExceeded", ConnectorErrorClass::Http429)
            .on_code("sharingRateLimitExceeded", ConnectorErrorClass::Http429)
            .on_code("quotaExceeded", ConnectorErrorClass::Http429)
            // Gmail: "backendError" — "Backend Error" from unexpected server
            // issues — is a 500 by status as well, and is named here so that a
            // provider that sends it under another status is still routed as a
            // server failure.
            .on_code("backendError", ConnectorErrorClass::Http5xx)
            // "400 Bad Request: A required field or parameter hasn't been
            // provided" / "The value supplied … is invalid".
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Unauthorized: Invalid Credentials", and the 403 that is left
            // once the quota reasons above are gone: "User lacks permission",
            // which includes an insufficient scope.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 Not Found", "409" (Calendar: the requested identifier
            // already exists), "410 Gone" (Calendar: a sync token that is no
            // longer valid, or an event already deleted), "412 Precondition
            // Failed" (Calendar: "The etag supplied in the If-match header no
            // longer corresponds to the current etag of the resource").
            .on_statuses([404, 409, 410, 412], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the shared Google error map is a valid declaration")
    });
    &MAP
}

// ---------------------------------------------------------------------------
// Decoding one response
// ---------------------------------------------------------------------------

/// A module's own check on a body the provider called a success.
///
/// It runs after the JSON is parsed and before the declared output pointers are
/// read, so an operation whose envelope reports a per-item failure never
/// reaches [`Operation::extract_output`].
pub type SuccessGuard = fn(&str, &JsonValue) -> Result<(), ConnectorFailure>;

/// The one decode path all four connectors take.
///
/// A non-success status is classified by the shared map; a success is parsed,
/// checked by the connector's own guard, and only then read through the
/// operation's declared output pointers.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &reqwest::header::HeaderMap,
    body: &[u8],
    guard: SuccessGuard,
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    if body.len() > MAX_HTTP_BODY_BYTES {
        return Err(ConnectorFailure::new(
            ConnectorErrorClass::Validation,
            "connector_response_too_large",
            "connector provider response exceeds the declared ceiling",
        )
        .with_provider_status(status));
    }
    // Google answers a `DELETE` with `204 No Content` and an empty body, which
    // is a success with nothing to guard and nothing to extract.
    if operation.is_no_content_success(status) && body.iter().all(u8::is_ascii_whitespace) {
        return operation.extract_output(&JsonValue::Object(serde_json::Map::new()));
    }
    let value: JsonValue = serde_json::from_slice(body)
        .map_err(|_| ConnectorFailure::validation("connector provider returned malformed JSON"))?;
    refuse_error_envelope(&value)?;
    guard(operation.id(), &value)?;
    operation.extract_output(&value)
}

/// The guard of a connector whose declared operations publish no per-item
/// failure shape at all. The shared envelope check still runs before it.
pub fn no_partial_failures(_operation: &str, _value: &JsonValue) -> Result<(), ConnectorFailure> {
    Ok(())
}

/// A `2xx` carrying Google's canonical error object is not a success.
///
/// This is the fail-closed rule, not a Google statement: see the module
/// documentation for why no legitimate response of these connectors can trip
/// it.
pub fn refuse_error_envelope(value: &JsonValue) -> Result<(), ConnectorFailure> {
    match value.get("error") {
        Some(JsonValue::Object(_)) => Err(SUCCESS_CARRIES_ERROR),
        _ => Ok(()),
    }
}

/// Whether a JSON value is a non-empty array — the shape every Google per-item
/// `errors` field takes when something inside a success went wrong.
pub fn reports_item_errors(value: Option<&JsonValue>) -> bool {
    matches!(value, Some(JsonValue::Array(items)) if !items.is_empty())
}

// ---------------------------------------------------------------------------
// Scopes
// ---------------------------------------------------------------------------

/// The OAuth2 scopes one operation may be authorized by.
///
/// Google's discovery document lists, per method, *every* scope that admits it —
/// `spreadsheets.values.get` names five, from `spreadsheets.readonly` to the
/// whole of `drive`. A connector therefore cannot publish "the scope" for an
/// operation; it publishes the smallest one Google admits, which is what
/// `donat connector authorize` should ask for, and the full documented set,
/// which is what a deployment's declaration is checked against. A deployment
/// that already holds a broader Google-documented scope is not asked to
/// re-authorize for a narrower one it would never use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeRequirement {
    least: &'static str,
    accepted: &'static [&'static str],
}

impl ScopeRequirement {
    /// `least` must itself be one of `accepted`; a declaration that says
    /// otherwise is a defect in the module, not a deployment error.
    pub const fn documented(least: &'static str, accepted: &'static [&'static str]) -> Self {
        Self { least, accepted }
    }

    /// The scope this connector asks a deployment to grant.
    pub const fn least(&self) -> &'static str {
        self.least
    }

    /// Every scope Google documents as sufficient for the method.
    pub const fn accepted(&self) -> &'static [&'static str] {
        self.accepted
    }

    pub fn is_satisfied_by(&self, granted: &[String]) -> bool {
        self.accepted
            .iter()
            .any(|scope| granted.iter().any(|held| held == scope))
    }

    /// Whether this operation is one of the reasons to hold `scope`.
    pub fn accepts(&self, scope: &str) -> bool {
        self.accepted.contains(&scope)
    }
}

/// Google's OpenID Connect scopes.
///
/// They grant no Workspace API access; they exist so a token response can name
/// the account an operator sees in `donat connector credentials list`. A
/// deployment may declare them, and no enabled operation will ever "use" one,
/// so [`scope_report`] does not report them as surplus.
pub const IDENTITY_SCOPES: &[&str] = &[
    "openid",
    "email",
    "profile",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
];

/// What one deployment's declared scope set is missing, and what it holds for
/// no reason.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScopeReport {
    /// One entry per enabled operation whose documented scope set is not met,
    /// carrying the operation and the least scope that would meet it.
    pub missing: Vec<(String, &'static str)>,
    /// Declared scopes no enabled operation is authorized by.
    pub surplus: Vec<String>,
}

impl ScopeReport {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.surplus.is_empty()
    }
}

/// Compare one deployment's declared scopes against the operations it enabled.
///
/// This is the whole of "scope sets are per operation group" (spec 014 §1): the
/// group is the set of enabled operations, the requirement is their union, and
/// a deployment that enables only reads is never asked for a write scope
/// because no read operation accepts one.
///
/// An operation name this connector does not declare is ignored here — the
/// registry's own admission check refuses it, with its metadata path, and
/// reporting it twice under two different messages helps nobody.
pub fn scope_report(
    scopes: impl Fn(&str) -> Option<ScopeRequirement>,
    enabled: &[String],
    declared: &[String],
) -> ScopeReport {
    let mut report = ScopeReport::default();
    let mut requirements = Vec::new();
    for operation in enabled {
        let Some(requirement) = scopes(operation) else {
            continue;
        };
        if !requirement.is_satisfied_by(declared) {
            report
                .missing
                .push((operation.clone(), requirement.least()));
        }
        requirements.push(requirement);
    }
    let mut seen = BTreeSet::new();
    for scope in declared {
        if IDENTITY_SCOPES.contains(&scope.as_str()) {
            continue;
        }
        if requirements
            .iter()
            .any(|requirement| requirement.accepts(scope))
        {
            continue;
        }
        if seen.insert(scope.clone()) {
            report.surplus.push(scope.clone());
        }
    }
    report
}

/// Every scope the declaration of one connector could ever ask for, in the
/// order its operations declare them, deduplicated.
///
/// Nothing on the request path reads this; it exists so a connector test can
/// assert that a module's scope table covers exactly the operations it declares
/// and no others.
pub fn declared_scopes(
    operations: &[Operation],
    scopes: impl Fn(&str) -> Option<ScopeRequirement>,
) -> Result<Vec<&'static str>, OperationError> {
    let mut least = Vec::new();
    for operation in operations {
        let requirement = scopes(operation.id()).ok_or_else(|| {
            OperationError::new("every Google operation declares the scopes it needs")
        })?;
        if !requirement.accepts(requirement.least()) {
            return Err(OperationError::new(
                "the least scope of an operation must be one Google documents for it",
            ));
        }
        if !least.contains(&requirement.least()) {
            least.push(requirement.least());
        }
    }
    Ok(least)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SPREADSHEETS: &str = "https://www.googleapis.com/auth/spreadsheets";
    const READONLY: &str = "https://www.googleapis.com/auth/spreadsheets.readonly";

    fn scopes(operation: &str) -> Option<ScopeRequirement> {
        match operation {
            "values.get" => Some(ScopeRequirement::documented(
                READONLY,
                &[READONLY, SPREADSHEETS],
            )),
            "values.update" => Some(ScopeRequirement::documented(SPREADSHEETS, &[SPREADSHEETS])),
            _ => None,
        }
    }

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    /// A deployment that enables only the read is asked for the read scope and
    /// refused the write one; enabling the write moves both.
    #[test]
    fn a_scope_set_follows_the_enabled_operations() {
        let read_only = scope_report(scopes, &owned(&["values.get"]), &owned(&[READONLY]));
        assert!(read_only.is_empty(), "{read_only:?}");

        // A grant no enabled operation is authorized by is surplus. It has to
        // come from another product to be one: Google's scope lists are nested,
        // so the write scope also authorizes the read and declaring it is a
        // broader grant rather than an unused one — only the deployment can say
        // which it meant.
        let foreign = scope_report(
            scopes,
            &owned(&["values.get"]),
            &owned(&[READONLY, "https://www.googleapis.com/auth/youtube.readonly"]),
        );
        assert_eq!(
            foreign.surplus,
            vec!["https://www.googleapis.com/auth/youtube.readonly".to_owned()]
        );
        assert!(foreign.missing.is_empty());

        let broader = scope_report(
            scopes,
            &owned(&["values.get"]),
            &owned(&[READONLY, SPREADSHEETS]),
        );
        assert!(
            broader.is_empty(),
            "the write scope authorizes the read too, so it is a broader grant rather than an \
             unused one: {broader:?}"
        );

        let shortfall = scope_report(
            scopes,
            &owned(&["values.get", "values.update"]),
            &owned(&[READONLY]),
        );
        assert_eq!(
            shortfall.missing,
            vec![("values.update".to_owned(), SPREADSHEETS)]
        );

        // The broader scope alone satisfies both, and is then not surplus.
        let broad = scope_report(
            scopes,
            &owned(&["values.get", "values.update"]),
            &owned(&[SPREADSHEETS]),
        );
        assert!(broad.is_empty(), "{broad:?}");
    }

    /// An OpenID Connect scope is never surplus: it grants no API access and is
    /// how a token response names the account an operator reads.
    #[test]
    fn an_identity_scope_is_not_surplus() {
        let report = scope_report(
            scopes,
            &owned(&["values.get"]),
            &owned(&[READONLY, "openid", "email"]),
        );
        assert!(report.is_empty(), "{report:?}");
    }

    /// The fail-closed rule and the two documented per-item shapes.
    #[test]
    fn a_success_body_that_carries_a_failure_is_refused() {
        assert_eq!(
            refuse_error_envelope(&json!({ "error": { "code": 403, "message": "nope" } }))
                .expect_err("an error envelope in a 2xx is not a success")
                .code(),
            SUCCESS_CARRIES_ERROR.code()
        );
        // A field *named* error that is not the envelope object is untouched:
        // the rule is about Google's error object, not about a string.
        refuse_error_envelope(&json!({ "error": "not-an-object" }))
            .expect("only the canonical object is refused");
        refuse_error_envelope(&json!({ "files": [] })).expect("an ordinary body decodes");

        assert!(reports_item_errors(Some(
            &json!([{ "reason": "notFound" }])
        )));
        assert!(!reports_item_errors(Some(&json!([]))));
        assert!(!reports_item_errors(None));
    }
}
