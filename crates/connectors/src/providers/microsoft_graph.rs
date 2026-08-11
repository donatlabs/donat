//! What the four Microsoft 365 connectors share (spec 015).
//!
//! Ground truth is Microsoft's own published reference on `learn.microsoft.com`,
//! read on 2026-08-10: the Microsoft Graph v1.0 API reference, *Microsoft Graph
//! error responses and resource types*, *Microsoft Graph throttling guidance*,
//! *Paging Microsoft Graph data in your app*, *Customize Microsoft Graph
//! responses with query parameters*, and — for the credential half —
//! *Microsoft identity platform and OAuth 2.0 authorization code flow*,
//! *Refresh tokens in the Microsoft identity platform*, and *Scopes and
//! permissions in the Microsoft identity platform*. Every path, permission,
//! header, status, and response field in these four modules is read from those
//! pages; nothing here is copied from a third-party integration.
//!
//! All four connectors answer to one origin, `https://graph.microsoft.com`, and
//! one credential shape — an Entra ID app registration with delegated
//! permissions and a refresh token — so the five things below live here once
//! rather than four times.
//!
//! # 1. One error envelope, and the code is the machine-readable half
//!
//! *Error responses*: "The error response is a single JSON object that contains
//! a single property named **error**."
//!
//! ```text
//! { "error": { "code": "badRequest",
//!              "message": "Uploaded fragment overlaps with existing data.",
//!              "innerError": { "code": "invalidRange", "request-id": …,
//!                              "date": … } } }
//! ```
//!
//! Microsoft is unusually explicit about which half a client may depend on:
//! "The **code** property contains a machine-readable value that you can take a
//! dependency on in your code", and "The **message** property is a
//! human-readable value that describes the error condition. **Don't take any
//! dependency on the content of this value in your code.**" [`error_map`]
//! therefore declares `/error/code` as its code pointer and reads nothing else
//! out of a failure body — the message never crosses this boundary, in either
//! direction.
//!
//! The fifteen codes the OneDrive/Graph *Error responses* page publishes as the
//! complete set a client must handle — "The `code` property contains one of 15
//! possible values. Your apps must be prepared to handle any one of these
//! errors." — are mapped by name below, ahead of the status rules, because two
//! of them (`activityLimitReached`, `serviceNotAvailable`) mean "try again
//! later" under statuses that would otherwise be permanent.
//!
//! # 2. Throttling is a documented status with a documented hint
//!
//! *Throttling guidance*: when a threshold is exceeded Graph "Returns HTTP
//! status code **429 Too Many Requests** and the requests fail" and "Returns a
//! suggested wait time in the response header of the failed request", with the
//! body carrying `"code": "TooManyRequests"`. The guidance is "Wait the number
//! of seconds specified in the `Retry-After` header." That hint reaches the
//! failure through the SDK's own clamp ([`crate::sdk::errors::MAX_RETRY_AFTER_SECONDS`]),
//! because a hint is advice and a ceiling is this workspace's.
//!
//! `509 Bandwidth Limit Exceeded` — "Your app has been throttled for exceeding
//! the maximum bandwidth cap. Your app can retry the request again after more
//! time has elapsed." — is the same class by Microsoft's own description.
//!
//! # 3. `@odata.nextLink` is a destination, so it is bounded to the origin
//!
//! *Paging*: "Microsoft Graph returns an `@odata.nextLink` property in the
//! response that contains a URL to the next page of results", and "Use the
//! entire URL in the `@odata.nextLink` property in a GET request to retrieve
//! the next page of results. … **Don't try to extract the `$skiptoken` or
//! `$skip` value and use it in a different request.**"
//!
//! That is the sharpest origin-escape surface in the whole connector programme:
//! the provider chooses an absolute URL and the walk follows it. It is declared
//! as [`crate::sdk::pagination::Pagination::next_uri_in_body`], the SDK's plan
//! for a body-carried *destination*, which resolves the value against the
//! compiled origin and refuses it with `connector_pagination_cross_origin` when
//! it lands anywhere else — a foreign host, a different scheme, or a different
//! port — before any request is made. [`next_link`] is the one constructor, so
//! no connector in this batch can spell the plan any other way.
//!
//! # 4. A permission is a property of an operation, and it has two spellings
//!
//! Every Graph reference page publishes, per method, a "Least privileged
//! permission" and a set of "Higher privileged permissions", which is exactly
//! the alternatives-with-a-least-member shape
//! `knowledgebase/declarative-saas/decisions/056-*` recorded for Google. Two
//! things are Microsoft's own, and [`PermissionRequirement`] answers both:
//!
//! * *Scopes and permissions*: "if the resource identifier is omitted in the
//!   scope parameter, the resource is assumed to be Microsoft Graph. For
//!   example, `scope=User.Read` is equivalent to
//!   `https://graph.microsoft.com/User.Read`." Both spellings are the same
//!   grant, so a deployment that writes either is right.
//! * Microsoft's own pages disagree about case — the permissions reference
//!   writes `Mail.Read` while the protocol reference writes
//!   `https://graph.microsoft.com/mail.read` — so the comparison is
//!   ASCII-case-insensitive.
//!
//! # 5. `offline_access` is a protocol scope, not an API permission
//!
//! *Scopes and permissions*: "On the Microsoft identity platform (requests made
//! to the v2.0 endpoint), your app must explicitly request the `offline_access`
//! scope, to receive refresh tokens." A deployment of any of these four
//! connectors that omits it from `config.oauth2.scopes` is authorized once and
//! can never refresh.
//!
//! It is nevertheless **not** required in the declaration, and the reason is
//! Microsoft's next sentence: "If any delegated permission is granted,
//! offline_access is implicitly granted. You can assume that the application has
//! offline_access if there are any delegated permissions granted." A grant that
//! is implicit is a grant the token response need not name, and `donat connector
//! authorize` refuses to write a row whose granted set does not cover the
//! declared one — so demanding `offline_access` in the declaration would refuse
//! a complete authorization. It is declared in every fixture and documented
//! here, and [`PROTOCOL_SCOPES`] keeps it (and the three OpenID Connect scopes)
//! out of the surplus report. See
//! `knowledgebase/declarative-saas/decisions/057-*`.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use serde_json::Value as JsonValue;

use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{Operation, OperationError};
use crate::sdk::pagination::Pagination;
use crate::sdk::transport::MAX_HTTP_BODY_BYTES;

/// The one origin all four connectors render against.
pub const ORIGIN: &str = "https://graph.microsoft.com";

/// The resource identifier a Graph permission may be prefixed with.
///
/// "if the resource identifier is omitted in the scope parameter, the resource
/// is assumed to be Microsoft Graph."
pub const RESOURCE_PREFIX: &str = "https://graph.microsoft.com/";

/// Where Microsoft publishes the machine-readable half of a failure.
const CODE_POINTER: &str = "/error/code";

/// Every Graph collection carries its items here.
pub const ITEMS_POINTER: &str = "/value";

/// The continuation Microsoft publishes for a collection that has another page.
pub const NEXT_LINK_POINTER: &str = "/@odata.nextLink";

/// A `2xx` whose body is Graph's error object.
pub const SUCCESS_CARRIES_ERROR: ConnectorFailure = ConnectorFailure::new(
    ConnectorErrorClass::Permanent,
    "microsoft_graph_success_envelope_carries_error",
    "the provider answered a success status with an error envelope",
);

/// The ordered error map every Microsoft 365 connector in this batch shares.
///
/// The code rules come first because Microsoft says the code is the half to
/// depend on, and because two of the fifteen documented codes mean "try again
/// later" under a status that would otherwise be permanent.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer(CODE_POINTER)
            // Throttling guidance: `"code": "TooManyRequests"` under `429`.
            .on_code("TooManyRequests", ConnectorErrorClass::Http429)
            // "activityLimitReached — The app or user has been throttled."
            .on_code("activityLimitReached", ConnectorErrorClass::Http429)
            // "serviceNotAvailable — The service is not available. Try the
            // request again after a delay. There may be a Retry-After header."
            .on_code("serviceNotAvailable", ConnectorErrorClass::Http5xx)
            // "unauthenticated — The caller is not authenticated."
            .on_code("unauthenticated", ConnectorErrorClass::Authentication)
            // "accessDenied — The caller doesn't have permission to perform the
            // action."
            .on_code("accessDenied", ConnectorErrorClass::Authentication)
            // "invalidRequest — The request is malformed or incorrect." /
            // "invalidRange — The specified byte range is invalid or
            // unavailable." Both are a request this deployment must change.
            .on_code("invalidRequest", ConnectorErrorClass::Validation)
            .on_code("invalidRange", ConnectorErrorClass::Validation)
            .on_code("badRequest", ConnectorErrorClass::Validation)
            // The remaining documented codes are answers rather than
            // conditions: the same request gets the same answer.
            .on_code("itemNotFound", ConnectorErrorClass::Permanent)
            .on_code("nameAlreadyExists", ConnectorErrorClass::Permanent)
            .on_code("notAllowed", ConnectorErrorClass::Permanent)
            .on_code("notSupported", ConnectorErrorClass::Permanent)
            .on_code("resourceModified", ConnectorErrorClass::Permanent)
            .on_code("resyncRequired", ConnectorErrorClass::Permanent)
            .on_code("malwareDetected", ConnectorErrorClass::Permanent)
            .on_code("quotaLimitReached", ConnectorErrorClass::Permanent)
            // "generalException — An unspecified error has occurred", which
            // Microsoft pairs with `500`; the status rule below decides it, so
            // it is deliberately not a code rule.
            //
            // "400 Bad Request: Can't process the request because it's
            // malformed or incorrect."
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Unauthorized: Required authentication information is either
            // missing or not valid for the resource." / "403 Forbidden: Access
            // is denied to the requested resource. The user does not have
            // enough permission or does not have a required license."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // Answers: not found, method not allowed, not acceptable, conflict,
            // gone, length required, precondition failed, entity too large,
            // unsupported media type, range not satisfiable, unprocessable,
            // locked, not implemented, insufficient storage.
            //
            // "423 Locked: The resource that is being accessed is locked" is
            // the judgement call in this list. A coauthoring lock does clear on
            // its own, so `permanent` is the safe direction rather than the
            // accurate one: it never retries a request whose lock a retry
            // cannot shorten, and a deployment that wants one waits above the
            // activity. Unlike the quota codes above, Microsoft publishes no
            // wait hint for it.
            .on_statuses(
                [
                    404, 405, 406, 409, 410, 411, 412, 413, 415, 416, 422, 423, 501, 507,
                ],
                ConnectorErrorClass::Permanent,
            )
            // "402 Payment Required: The payment requirements for the API
            // haven't been met" — a deployment-level condition a retry does not
            // change.
            .on_status(402, ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            // "509 Bandwidth Limit Exceeded: … Your app can retry the request
            // again after more time has elapsed."
            .on_status(509, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // Graph publishes both on every response; neither is a secret and
            // both are what Microsoft support asks for.
            .correlation_header("request_id", "request-id")
            .correlation_header("client_request_id", "client-request-id")
            .build()
            .expect("the shared Microsoft Graph error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of one Graph collection.
///
/// This is the only way a connector in this batch may declare pagination, so
/// "an `@odata.nextLink` is resolved against the compiled origin and refused
/// when it lands anywhere else" has exactly one description.
pub fn next_link(items_pointer: &str) -> Result<Pagination, OperationError> {
    Pagination::next_uri_in_body(items_pointer, NEXT_LINK_POINTER)
}

// ---------------------------------------------------------------------------
// Decoding one response
// ---------------------------------------------------------------------------

/// A module's own check on a body the provider called a success.
pub type SuccessGuard = fn(&str, &JsonValue) -> Result<(), ConnectorFailure>;

/// The one decode path all four connectors take.
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
    // Graph answers a `DELETE` with "204 No Content … It doesn't return
    // anything in the response body", and `sendMail` and `send` with "202
    // Accepted … It doesn't return anything in the response body".
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
/// failure shape. The shared envelope check still runs before it.
pub fn no_partial_failures(_operation: &str, _value: &JsonValue) -> Result<(), ConnectorFailure> {
    Ok(())
}

/// A `2xx` carrying Graph's canonical error object is not a success.
///
/// This is the same fail-closed rule Batch C recorded for Google, and it is
/// this workspace's rather than Microsoft's: no response schema of any
/// operation these four connectors declare has a top-level `error` property, so
/// the rule costs a legitimate success nothing and closes the one shape in
/// which a provider failure could be extracted into an activity's output as
/// though it were data.
pub fn refuse_error_envelope(value: &JsonValue) -> Result<(), ConnectorFailure> {
    match value.get("error") {
        Some(JsonValue::Object(_)) => Err(SUCCESS_CARRIES_ERROR),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

/// The delegated permissions one operation may be authorized by.
///
/// Every Graph reference page publishes one "Least privileged permission" and a
/// set of "Higher privileged permissions" for each method, which is what this
/// carries: the one a deployment should ask for, and every one Microsoft
/// documents as sufficient, so a deployment already holding a broader
/// documented permission is never told to authorize a narrower one it would not
/// use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionRequirement {
    least_privileged: &'static str,
    accepted: &'static [&'static str],
}

impl PermissionRequirement {
    /// `least_privileged` must itself be one of `accepted`; a declaration that
    /// says otherwise is a defect in the module, not a deployment error.
    pub const fn documented(
        least_privileged: &'static str,
        accepted: &'static [&'static str],
    ) -> Self {
        Self {
            least_privileged,
            accepted,
        }
    }

    /// The permission this connector asks a deployment to grant.
    pub const fn least_privileged(&self) -> &'static str {
        self.least_privileged
    }

    /// Every permission Microsoft documents as sufficient for the method.
    pub const fn accepted(&self) -> &'static [&'static str] {
        self.accepted
    }

    pub fn is_satisfied_by(&self, granted: &[String]) -> bool {
        granted.iter().any(|held| self.accepts(held))
    }

    /// Whether this operation is one of the reasons to hold `scope`.
    ///
    /// The comparison normalizes the two spellings Microsoft documents as the
    /// same grant — with and without the `https://graph.microsoft.com/` resource
    /// identifier — and ignores ASCII case, because Microsoft's own pages write
    /// both `Mail.Read` and `mail.read`.
    pub fn accepts(&self, scope: &str) -> bool {
        let held = strip_resource_prefix(scope);
        self.accepted
            .iter()
            .any(|permission| permission.eq_ignore_ascii_case(held))
    }
}

/// One scope with Microsoft's optional resource identifier removed.
pub fn strip_resource_prefix(scope: &str) -> &str {
    let bytes = scope.as_bytes();
    let prefix = RESOURCE_PREFIX.as_bytes();
    if bytes.len() > prefix.len() && bytes[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return &scope[prefix.len()..];
    }
    scope
}

/// The OpenID Connect scopes plus `offline_access`.
///
/// None of them is an API permission — "The Microsoft identity platform
/// implementation of OpenID Connect has a few well-defined scopes … `openid`,
/// `email`, `profile`, and `offline_access`" — so no enabled operation will ever
/// be authorized *by* one, and [`permission_report`] does not call them surplus.
/// `offline_access` is the one a deployment of these connectors must actually
/// declare: it is what makes the token response carry a refresh token at all.
pub const PROTOCOL_SCOPES: &[&str] = &["openid", "email", "profile", "offline_access"];

/// What one deployment's declared permission set is missing, and what it holds
/// for no reason.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PermissionReport {
    /// One entry per enabled operation no declared permission authorizes,
    /// carrying the operation and the least privileged permission that would.
    pub missing: Vec<(String, &'static str)>,
    /// Declared permissions no enabled operation is authorized by.
    pub surplus: Vec<String>,
}

impl PermissionReport {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.surplus.is_empty()
    }
}

/// Compare one deployment's declared scopes against the operations it enabled.
///
/// The group is the set of enabled operations and the requirement is their
/// union, so a deployment that enables only reads is never asked for a write
/// permission and cannot hold one by accident.
///
/// An operation name this connector does not declare is ignored here — the
/// registry's own admission check refuses it, with its metadata path.
pub fn permission_report(
    permissions: impl Fn(&str) -> Option<PermissionRequirement>,
    enabled: &[String],
    declared: &[String],
) -> PermissionReport {
    let mut report = PermissionReport::default();
    let mut requirements = Vec::new();
    for operation in enabled {
        let Some(requirement) = permissions(operation) else {
            continue;
        };
        if !requirement.is_satisfied_by(declared) {
            report
                .missing
                .push((operation.clone(), requirement.least_privileged()));
        }
        requirements.push(requirement);
    }
    let mut seen = BTreeSet::new();
    for scope in declared {
        if PROTOCOL_SCOPES
            .iter()
            .any(|protocol| protocol.eq_ignore_ascii_case(strip_resource_prefix(scope)))
        {
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

/// Every permission the declaration of one connector could ever ask for, in the
/// order its operations declare them, deduplicated.
///
/// Nothing on the request path reads this; it exists so a connector test can
/// assert that a module's permission table covers exactly the operations it
/// declares and no others.
pub fn declared_permissions(
    operations: &[Operation],
    permissions: impl Fn(&str) -> Option<PermissionRequirement>,
) -> Result<Vec<&'static str>, OperationError> {
    let mut least = Vec::new();
    for operation in operations {
        let requirement = permissions(operation.id()).ok_or_else(|| {
            OperationError::new("every Microsoft Graph operation declares the permissions it needs")
        })?;
        if !requirement.accepts(requirement.least_privileged()) {
            return Err(OperationError::new(
                "the least privileged permission of an operation must be one Microsoft documents \
                 for it",
            ));
        }
        if !least.contains(&requirement.least_privileged()) {
            least.push(requirement.least_privileged());
        }
    }
    Ok(least)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const READ: &str = "Mail.Read";
    const WRITE: &str = "Mail.ReadWrite";

    fn permissions(operation: &str) -> Option<PermissionRequirement> {
        match operation {
            "message.get" => Some(PermissionRequirement::documented(READ, &[READ, WRITE])),
            "message.update" => Some(PermissionRequirement::documented(WRITE, &[WRITE])),
            _ => None,
        }
    }

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    /// A deployment that enables only the read is asked for the read permission
    /// and refused a permission nothing uses.
    #[test]
    fn a_permission_set_follows_the_enabled_operations() {
        let read_only = permission_report(permissions, &owned(&["message.get"]), &owned(&[READ]));
        assert!(read_only.is_empty(), "{read_only:?}");

        let shortfall = permission_report(
            permissions,
            &owned(&["message.get", "message.update"]),
            &owned(&[READ]),
        );
        assert_eq!(
            shortfall.missing,
            vec![("message.update".to_owned(), WRITE)]
        );

        // The broader permission authorizes both, and is then not surplus.
        let broad = permission_report(
            permissions,
            &owned(&["message.get", "message.update"]),
            &owned(&[WRITE]),
        );
        assert!(broad.is_empty(), "{broad:?}");

        // A permission from another Graph workload is surplus wherever it is
        // declared.
        let foreign = permission_report(
            permissions,
            &owned(&["message.get"]),
            &owned(&[READ, "Sites.Read.All"]),
        );
        assert_eq!(foreign.surplus, vec!["Sites.Read.All".to_owned()]);
        assert!(foreign.missing.is_empty());
    }

    /// Microsoft documents `scope=User.Read` and
    /// `https://graph.microsoft.com/User.Read` as the same grant, and its own
    /// pages disagree about case.
    #[test]
    fn both_documented_spellings_of_one_permission_are_the_same_grant() {
        for spelling in [
            "Mail.Read",
            "mail.read",
            "https://graph.microsoft.com/Mail.Read",
            "https://graph.microsoft.com/mail.read",
        ] {
            let report =
                permission_report(permissions, &owned(&["message.get"]), &owned(&[spelling]));
            assert!(report.is_empty(), "{spelling}: {report:?}");
        }
        // The prefix is stripped only when it is the whole prefix.
        assert_eq!(strip_resource_prefix("Mail.Read"), "Mail.Read");
        assert_eq!(
            strip_resource_prefix("https://graph.microsoft.com.evil.test/Mail.Read"),
            "https://graph.microsoft.com.evil.test/Mail.Read"
        );
        assert_eq!(strip_resource_prefix(RESOURCE_PREFIX), RESOURCE_PREFIX);
    }

    /// A protocol scope is never surplus, and `offline_access` is the one a
    /// deployment must hold for the credential lifecycle to work at all.
    #[test]
    fn a_protocol_scope_is_not_an_api_permission() {
        let report = permission_report(
            permissions,
            &owned(&["message.get"]),
            &owned(&[READ, "offline_access", "openid", "email", "profile"]),
        );
        assert!(report.is_empty(), "{report:?}");
    }

    /// The fail-closed envelope rule.
    #[test]
    fn a_success_body_that_carries_a_failure_is_refused() {
        assert_eq!(
            refuse_error_envelope(&json!({ "error": { "code": "accessDenied" } }))
                .expect_err("an error envelope in a 2xx is not a success")
                .code(),
            SUCCESS_CARRIES_ERROR.code()
        );
        refuse_error_envelope(&json!({ "error": "not-an-object" }))
            .expect("only the canonical object is refused");
        refuse_error_envelope(&json!({ "value": [] })).expect("an ordinary body decodes");
    }

    /// The documented throttling response reaches `http_429` by its code as
    /// well as by its status, and the human message never leaves the map.
    #[test]
    fn the_documented_throttling_response_is_classified_by_its_code() {
        let throttled =
            br#"{"error":{"code":"TooManyRequests","message":"Please retry again later."}}"#;
        for status in [429, 503] {
            assert_eq!(
                error_map()
                    .classify(status, &reqwest::header::HeaderMap::new(), throttled)
                    .class(),
                ConnectorErrorClass::Http429,
                "status {status}"
            );
        }
        let failure = error_map().classify(
            403,
            &reqwest::header::HeaderMap::new(),
            br#"{"error":{"code":"activityLimitReached","message":"secret-detail"}}"#,
        );
        assert_eq!(failure.class(), ConnectorErrorClass::Http429);
        assert!(!format!("{failure:?}").contains("secret-detail"));
    }
}
