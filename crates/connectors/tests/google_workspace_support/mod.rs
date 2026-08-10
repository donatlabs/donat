//! What the four Google Workspace connector test files share.
//!
//! No test reaches Google, and no test carries a real credential: the stored
//! access token is [`SECRET_SENTINEL`], which doubles as the value every
//! redaction assertion looks for.

#![allow(dead_code)]

use donat_connectors::sdk::testing::{ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AccessToken, AuthPlan, ConnectorErrorClass, Credential, Effect, EffectClass, Operation,
    OperationRejection, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

/// The complete `Authorization` value one attempt's credential seam produces.
pub fn applied_credential() -> AccessToken {
    AccessToken::new(format!("Bearer {SECRET_SENTINEL}"))
}

/// Render one operation the way a deployment would: the request from the
/// declaration, the credential from the source-local store.
pub fn render(stub: &ProviderStub, operation: &Operation, input: JsonValue) -> RequestPlan {
    let mut request = operation
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::oauth2_authorization_code()
        .apply(
            &Credential::from_fields([]),
            &mut request,
            Some(&applied_credential()),
        )
        .expect("the declared plan applies the stored credential");
    request
}

/// Google's canonical error object, as Drive's *Resolve errors* page prints it.
/// The message carries the sort of thing a provider really says, so a redaction
/// assertion has something to find.
pub fn google_error(status: u16, reason: &str) -> JsonValue {
    json!({
        "error": {
            "code": status,
            "message": format!(
                "the request failed on shard db-7.internal with token {SECRET_SENTINEL}"
            ),
            "errors": [{
                "domain": "global",
                "reason": reason,
                "message": "human readable prose that never crosses the boundary",
            }],
            "status": "PERMISSION_DENIED",
        }
    })
}

/// The statuses and reasons Google documents across these four APIs.
pub fn documented_failures() -> Vec<(u16, &'static str, ConnectorErrorClass)> {
    vec![
        (400, "badRequest", ConnectorErrorClass::Validation),
        (401, "authError", ConnectorErrorClass::Authentication),
        (403, "forbidden", ConnectorErrorClass::Authentication),
        (403, "rateLimitExceeded", ConnectorErrorClass::Http429),
        (403, "userRateLimitExceeded", ConnectorErrorClass::Http429),
        (403, "dailyLimitExceeded", ConnectorErrorClass::Http429),
        (403, "quotaExceeded", ConnectorErrorClass::Http429),
        (404, "notFound", ConnectorErrorClass::Permanent),
        (409, "duplicate", ConnectorErrorClass::Permanent),
        (410, "deleted", ConnectorErrorClass::Permanent),
        (412, "conditionNotMet", ConnectorErrorClass::Permanent),
        (429, "rateLimitExceeded", ConnectorErrorClass::Http429),
        (500, "backendError", ConnectorErrorClass::Http5xx),
        (502, "backendError", ConnectorErrorClass::Http5xx),
        (503, "backendError", ConnectorErrorClass::Http5xx),
        (504, "backendError", ConnectorErrorClass::Http5xx),
        // Nothing Google documents: the declared fallback answers.
        (418, "teapot", ConnectorErrorClass::Permanent),
    ]
}

/// Assert the effect class of every operation one connector declares, that an
/// inventory-only one cannot be enabled, and that none of them claims an
/// idempotency binding Google does not publish.
pub fn assert_effects(
    connector: &'static donat_connectors::sdk::Connector,
    expected: &[(&str, EffectClass)],
) {
    assert_eq!(
        connector.operations().len(),
        expected.len(),
        "every declared operation of `{}` is classified here",
        connector.name()
    );
    for (id, class) in expected {
        let operation = connector
            .operation(id)
            .unwrap_or_else(|| panic!("the declaration publishes {id}"));
        assert_eq!(operation.effect_class(), Some(*class), "{id}");
        match class {
            EffectClass::InventoryOnly => {
                assert_eq!(
                    connector.admit_operation(id),
                    Err(OperationRejection::InventoryOnly),
                    "{id} must not be enablable by a deployment"
                );
                assert!(
                    operation
                        .effect()
                        .and_then(Effect::inventory_reason)
                        .is_some_and(|reason| !reason.is_empty()),
                    "{id} records why it is not executable"
                );
            }
            _ => {
                assert!(connector.admit_operation(id).is_ok(), "{id}");
                assert!(operation.is_executable(), "{id}");
            }
        }
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: Google publishes no idempotency key to bind"
        );
    }
}
