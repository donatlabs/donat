//! The `local.` reserved connector namespace (spec 018 §1 and §6).
//!
//! A local capability reuses [`ConnectorInstance`] and
//! [`ProcessRequestActivity`] unchanged, so metadata exported from an existing
//! Donat project still loads and a deployment learns no second spelling. What
//! changes is what a `local.*` instance is *allowed* to say: it has no origin,
//! no base URL, no header, and no credential, because there is nothing on the
//! other side of it. Declaring one of those would describe a request that is
//! never made, and a declaration the runtime ignores is a defect
//! (`knowledgebase/declarative-saas/decisions/034-*`).
//!
//! This module owns the rules; the compiled capability table lives in
//! `donat-connectors` and reaches them through [`LocalCapabilityCatalog`]. The
//! metadata crate deliberately does not depend on the connector crate: what the
//! binary was built with is a fact about the binary, not about the metadata.

use std::fmt;

use crate::types::{
    ConnectorInstance, ConnectorOperationProfile, Metadata, Process, ProcessForEachState,
    ProcessRequestActivity, ProcessStateOperation, ProcessWaitState,
};

/// The reserved namespace. Nothing outside the compiled capability table may
/// be named inside it.
pub const LOCAL_NAMESPACE: &str = "local.";

/// Whether a connector name or module selects a local capability.
pub fn is_local(name: &str) -> bool {
    name.starts_with(LOCAL_NAMESPACE)
}

/// What the serving binary knows about the capabilities compiled into it.
///
/// The two questions metadata validation has to ask are "does this capability
/// exist here" and "does it advertise this operation, and how long may it
/// run" — no more, because everything else about a capability is its own
/// business.
pub trait LocalCapabilityCatalog {
    /// The operations a compiled capability advertises as executable, or
    /// `None` when this binary carries no such capability.
    fn operations(&self, capability: &str) -> Option<Vec<String>>;

    /// The declared `cpu_deadline` of one operation, in milliseconds.
    fn cpu_deadline_ms(&self, capability: &str, operation: &str) -> Option<u64>;
}

/// One refusal, naming the exact metadata path that earned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCapabilityError {
    pub path: String,
    pub message: String,
}

impl LocalCapabilityError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for LocalCapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

/// Every `local.*` rule, applied to one deployment's metadata.
pub fn validate_local_capabilities(
    metadata: &Metadata,
    catalog: &dyn LocalCapabilityCatalog,
) -> Vec<LocalCapabilityError> {
    let mut errors = Vec::new();
    for (index, instance) in metadata.connectors.iter().enumerate() {
        if !is_local(&instance.name) && !is_local(&instance.module) {
            continue;
        }
        validate_instance(
            instance,
            &format!("connectors[{index}]"),
            catalog,
            &mut errors,
        );
    }
    for process in &metadata.processes {
        validate_process(process, metadata, catalog, &mut errors);
    }
    errors
}

fn validate_instance(
    instance: &ConnectorInstance,
    path: &str,
    catalog: &dyn LocalCapabilityCatalog,
    errors: &mut Vec<LocalCapabilityError>,
) {
    // A local capability has nothing to configure, so two instances of one
    // capability would be two names for one thing. The instance *is* the
    // capability, which is also what lets a Process name it directly.
    if instance.name != instance.module {
        errors.push(LocalCapabilityError::new(
            format!("{path}.name"),
            format!(
                "local capability instance `{}` must be named after the capability it selects (`{}`)",
                instance.name, instance.module
            ),
        ));
    }
    if !is_local(&instance.module) {
        errors.push(LocalCapabilityError::new(
            format!("{path}.module"),
            format!(
                "the `{LOCAL_NAMESPACE}` namespace is reserved for local capabilities; instance `{}` selects module `{}`",
                instance.name, instance.module
            ),
        ));
        return;
    }
    let Some(operations) = catalog.operations(&instance.module) else {
        errors.push(LocalCapabilityError::new(
            format!("{path}.module"),
            format!(
                "local capability `{}` is not compiled into this binary",
                instance.module
            ),
        ));
        return;
    };

    // The four declarations a local capability must not carry. Each is
    // reported on its own path, because "your connector configuration is
    // wrong" is not an operator-actionable message.
    let config = &instance.config;
    if config.base_url.is_some() {
        errors.push(refuse(
            path,
            "config.base_url",
            "a base URL",
            &instance.name,
        ));
    }
    if !config.endpoint_identity.is_empty() {
        errors.push(refuse(
            path,
            "config.endpoint_identity",
            "an origin identity",
            &instance.name,
        ));
    }
    if config.network_policy.is_some() {
        errors.push(refuse(
            path,
            "config.network_policy",
            "a network policy",
            &instance.name,
        ));
    }
    if !config.headers.is_empty() {
        errors.push(refuse(path, "config.headers", "a header", &instance.name));
    }
    if !config.credential_identity.is_empty() {
        errors.push(refuse(
            path,
            "config.credential_identity",
            "a credential identity",
            &instance.name,
        ));
    }
    if config.secret_key.is_some() {
        errors.push(refuse(
            path,
            "config.secret_key",
            "a credential",
            &instance.name,
        ));
    }
    if config.webhook_secret.is_some() {
        errors.push(refuse(
            path,
            "config.webhook_secret",
            "a credential",
            &instance.name,
        ));
    }
    if config.oauth2.is_some() {
        errors.push(refuse(
            path,
            "config.oauth2",
            "a credential",
            &instance.name,
        ));
    }
    if config.api_version.is_some() {
        errors.push(refuse(
            path,
            "config.api_version",
            "a provider API version",
            &instance.name,
        ));
    }

    for (index, operation) in instance.operations.iter().enumerate() {
        let operation_path = format!("{path}.operations[{index}]");
        if !operations.contains(&operation.name) {
            errors.push(LocalCapabilityError::new(
                format!("{operation_path}.name"),
                format!(
                    "local capability `{}` does not advertise operation `{}`",
                    instance.module, operation.name
                ),
            ));
        }
        if matches!(operation.profile, ConnectorOperationProfile::Http(_)) {
            errors.push(LocalCapabilityError::new(
                operation_path,
                format!(
                    "local capability `{}` operation `{}` must not declare an HTTP request",
                    instance.module, operation.name
                ),
            ));
        }
    }
}

fn refuse(path: &str, field: &str, what: &str, instance: &str) -> LocalCapabilityError {
    LocalCapabilityError::new(
        format!("{path}.{field}"),
        format!("local capability instance `{instance}` must not declare {what}"),
    )
}

fn validate_process(
    process: &Process,
    metadata: &Metadata,
    catalog: &dyn LocalCapabilityCatalog,
    errors: &mut Vec<LocalCapabilityError>,
) {
    for (index, state) in process.states.iter().enumerate() {
        let path = format!("processes.{}.states[{index}]", process.name);
        match &state.operation {
            ProcessStateOperation::Request { request } => {
                validate_request(
                    &request.connector,
                    &request.operation,
                    &request.timeout.start_to_close,
                    &format!("{path}.request"),
                    metadata,
                    catalog,
                    errors,
                );
            }
            ProcessStateOperation::ForEach { for_each } => {
                if let ProcessForEachState::Request { request, .. } = for_each.as_ref() {
                    let request: &ProcessRequestActivity = request;
                    validate_request(
                        &request.connector,
                        &request.operation,
                        &request.timeout.start_to_close,
                        &format!("{path}.for_each.request"),
                        metadata,
                        catalog,
                        errors,
                    );
                }
            }
            // A local capability publishes no trigger, because nothing calls
            // back into a function of its input.
            ProcessStateOperation::Wait { wait } => {
                if let ProcessWaitState::Webhook(webhook) = wait
                    && is_local(&webhook.webhook.connector)
                {
                    errors.push(LocalCapabilityError::new(
                        format!("{path}.wait.webhook.connector"),
                        format!(
                            "local capability `{}` publishes no trigger to wait for",
                            webhook.webhook.connector
                        ),
                    ));
                }
            }
            ProcessStateOperation::Command { .. }
            | ProcessStateOperation::When { .. }
            | ProcessStateOperation::Output { .. }
            | ProcessStateOperation::Fail { .. } => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_request(
    connector: &str,
    operation: &str,
    start_to_close: &str,
    path: &str,
    metadata: &Metadata,
    catalog: &dyn LocalCapabilityCatalog,
    errors: &mut Vec<LocalCapabilityError>,
) {
    if !is_local(connector) {
        return;
    }
    let declared = metadata
        .connectors
        .iter()
        .any(|instance| instance.name == connector);
    if !declared {
        errors.push(LocalCapabilityError::new(
            format!("{path}.connector"),
            format!("local capability `{connector}` is not enabled by this deployment"),
        ));
        return;
    }
    let Some(operations) = catalog.operations(connector) else {
        // The instance itself was already refused as uncompiled.
        return;
    };
    if !operations.iter().any(|declared| declared == operation) {
        errors.push(LocalCapabilityError::new(
            format!("{path}.operation"),
            format!("local capability `{connector}` does not advertise operation `{operation}`"),
        ));
        return;
    }
    // Spec 018 §4: the declared cpu deadline is always at most the activity's
    // `start_to_close`. An activity that gives its capability less time than
    // the capability declared is one whose timeout can never mean what it says.
    let Some(cpu_deadline_ms) = catalog.cpu_deadline_ms(connector, operation) else {
        return;
    };
    match parse_duration_ms(start_to_close) {
        Some(start_to_close_ms) if start_to_close_ms < cpu_deadline_ms => {
            errors.push(LocalCapabilityError::new(
                format!("{path}.timeout.start_to_close"),
                format!(
                    "local capability `{connector}` operation `{operation}` declares a cpu deadline of {cpu_deadline_ms}ms, which exceeds start_to_close {start_to_close}"
                ),
            ));
        }
        _ => {}
    }
}

/// The duration grammar durable process metadata already uses: a positive
/// integer and one of `ms`, `s`, `m`, `h`, `d`. Anything else is left to the
/// process compiler, which reports it against its own path.
fn parse_duration_ms(source: &str) -> Option<u64> {
    let (digits, multiplier) = [
        ("ms", 1_u64),
        ("s", 1_000),
        ("m", 60_000),
        ("h", 3_600_000),
        ("d", 86_400_000),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier)| Some((source.strip_suffix(suffix)?, multiplier)))?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(multiplier)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The compiled table, as this test's binary knows it.
    struct Catalog;

    impl LocalCapabilityCatalog for Catalog {
        fn operations(&self, capability: &str) -> Option<Vec<String>> {
            match capability {
                "local.echo" => Some(vec!["value.echo".to_owned(), "text.artifact".to_owned()]),
                _ => None,
            }
        }

        fn cpu_deadline_ms(&self, capability: &str, operation: &str) -> Option<u64> {
            match (capability, operation) {
                ("local.echo", "value.echo") => Some(1_000),
                ("local.echo", "text.artifact") => Some(2_000),
                _ => None,
            }
        }
    }

    fn metadata(value: serde_json::Value) -> Metadata {
        serde_json::from_value(value).expect("test metadata deserializes")
    }

    fn messages(value: serde_json::Value) -> String {
        validate_local_capabilities(&metadata(value), &Catalog)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Spec 018 §8 `local_capability_has_no_origin_or_credential`.
    #[test]
    fn local_capability_has_no_origin_or_credential() {
        // The declaration a deployment is allowed to write: a name, the
        // capability it selects, and the operations it enables.
        assert_eq!(
            messages(json!({
                "version": 3,
                "connectors": [{
                    "name": "local.echo",
                    "module": "local.echo",
                    "operations": [{ "name": "value.echo" }]
                }]
            })),
            "",
            "a bare local capability is the whole of a valid declaration"
        );

        // An origin, a base URL, a header, and a credential: each refused, each
        // on its own path.
        let refused = messages(json!({
            "version": 3,
            "connectors": [{
                "name": "local.echo",
                "module": "local.echo",
                "config": {
                    "endpoint_identity": "echo_endpoint",
                    "credential_identity": "echo_credential",
                    "base_url": "https://provider.example.test",
                    "network_policy": "egress",
                    "api_version": "2026-07-27",
                    "headers": [{ "name": "X-Key", "value_from_env": "DONAT_ECHO_KEY" }],
                    "secret_key": { "value_from_env": "DONAT_ECHO_SECRET" },
                    "webhook_secret": { "value_from_env": "DONAT_ECHO_WHSEC" }
                },
                "operations": []
            }]
        }));
        for field in [
            "config.base_url",
            "config.endpoint_identity",
            "config.network_policy",
            "config.headers",
            "config.credential_identity",
            "config.secret_key",
            "config.webhook_secret",
            "config.api_version",
        ] {
            assert!(
                refused.contains(&format!("connectors[0].{field}:")),
                "{field} must be refused on its own path: {refused}"
            );
        }

        // An unknown capability, and an operation the capability does not
        // advertise, are both answered from the compiled table.
        assert!(
            messages(json!({
                "version": 3,
                "connectors": [{ "name": "local.ledger", "module": "local.ledger" }]
            }))
            .contains("local capability `local.ledger` is not compiled into this binary")
        );
        assert!(
            messages(json!({
                "version": 3,
                "connectors": [{
                    "name": "local.echo",
                    "module": "local.echo",
                    "operations": [{ "name": "pdf.render" }]
                }]
            }))
            .contains("does not advertise operation `pdf.render`")
        );

        // The namespace is reserved in both directions: a provider module may
        // not take a `local.` name, and a local instance may not be renamed.
        assert!(
            messages(json!({
                "version": 3,
                "connectors": [{
                    "name": "local.payments",
                    "module": "stripe",
                    "config": { "endpoint_identity": "s", "credential_identity": "c" }
                }]
            }))
            .contains("namespace is reserved")
        );
        assert!(
            messages(json!({
                "version": 3,
                "connectors": [{ "name": "documents", "module": "local.echo" }]
            }))
            .contains("must be named after the capability it selects")
        );

        // And an operation that tries to smuggle a request in through the
        // declarative HTTP profile is refused with it.
        assert!(
            messages(json!({
                "version": 3,
                "connectors": [{
                    "name": "local.echo",
                    "module": "local.echo",
                    "operations": [{
                        "name": "value.echo",
                        "method": "POST",
                        "path": "/v1/echo"
                    }]
                }]
            }))
            .contains("must not declare an HTTP request")
        );
    }

    /// Spec 018 §8 `local_capability_is_activity_only`.
    ///
    /// A local capability is reachable from a durable activity and from
    /// nowhere else. Two halves: the grammar offers a command, a rule, and a
    /// permission no way to name a connector at all, and the one non-activity
    /// position that *can* name one — a webhook wait — refuses a local
    /// capability.
    #[test]
    fn local_capability_is_activity_only() {
        // A request activity: the supported position, and it validates.
        assert_eq!(
            messages(process_with(json!({
                "id": "render",
                "request": {
                    "connector": "local.echo",
                    "operation": "value.echo",
                    "input": { "value": { "literal": "x" } },
                    "timeout": { "schedule_to_start": "10s", "start_to_close": "20s" },
                    "retry": { "retry_on": ["timeout"], "max_attempts": 2, "initial_interval": "1s", "max_interval": "5s", "jitter": "1s" },
                    "next": "done"
                }
            }))),
            ""
        );

        // A command state cannot reach one: a command names a command, and
        // adding a connector to it is not a field the grammar has.
        let command = serde_json::from_value::<Metadata>(process_with(json!({
            "id": "render",
            "command": {
                "name": "render_invoice",
                "run_as": "app",
                "connector": "local.echo",
                "arguments": {},
                "next": "done"
            }
        })));
        assert!(
            command.is_err(),
            "a command state has no connector field to fill"
        );

        // Neither can a rule or a permission. Both are named expressions over
        // declared inputs and neither has a connector position at all, so a
        // `connector:` key written into one is not a weaker reference — it is
        // not a reference: the model has no field to hold it, and nothing
        // downstream can read what was never parsed.
        for value in [
            json!({ "version": 3, "rules": { "rules": [{
                "name": "is_large", "connector": "local.echo",
                "result": "Bool", "expression": "true"
            }] } }),
            json!({ "version": 3, "sources": [{
                "name": "default", "kind": "postgres",
                "configuration": { "connection_info": { "database_url": "postgresql://localhost/x" } },
                "tables": [{
                    "table": { "schema": "public", "name": "pet" },
                    "select_permissions": [{
                        "role": "user", "permission": { "columns": ["id"], "filter": {}, "connector": "local.echo" }
                    }]
                }]
            }] }),
        ] {
            let parsed = serde_json::from_value::<Metadata>(value)
                .expect("the surrounding declaration is otherwise valid");
            assert!(parsed.connectors.is_empty());
            let round_tripped = serde_json::to_string(&parsed).expect("metadata always serializes");
            assert!(
                !round_tripped.contains("local.echo"),
                "a rule or permission has no connector field for a capability to land in: {round_tripped}"
            );
            assert!(validate_local_capabilities(&parsed, &Catalog).is_empty());
        }

        // The one non-activity position that names a connector refuses a local
        // one, because a function of its input calls nobody back.
        assert!(
            messages(process_with(json!({
                "id": "await",
                "wait": {
                    "webhook": {
                        "connector": "local.echo",
                        "trigger": "value.echoed",
                        "correlate": {}
                    },
                    "deadline": "1h",
                    "next": "done",
                    "on_timeout": "done"
                }
            })))
            .contains("publishes no trigger to wait for")
        );
    }

    /// A capability may not be given less time than it declared it needs:
    /// spec 018 §4's "always ≤ the activity's `start_to_close`".
    #[test]
    fn a_local_activity_gives_its_capability_at_least_its_declared_deadline() {
        let refused = messages(process_with(json!({
            "id": "render",
            "request": {
                "connector": "local.echo",
                "operation": "text.artifact",
                "input": {},
                "timeout": { "schedule_to_start": "10s", "start_to_close": "500ms" },
                "retry": { "retry_on": ["timeout"], "max_attempts": 1, "initial_interval": "1s", "max_interval": "5s", "jitter": "1s" },
                "next": "done"
            }
        })));
        assert!(
            refused.contains("exceeds start_to_close 500ms"),
            "an activity shorter than the capability's own deadline is refused: {refused}"
        );
    }

    /// A process referencing a capability the deployment never enabled is
    /// refused before anything runs.
    #[test]
    fn a_process_may_only_reference_an_enabled_capability() {
        let value = json!({
            "version": 3,
            "processes": [{
                "name": "render", "kind": "process", "version": 1, "source": "default",
                "start_at": "render",
                "states": [{
                    "id": "render",
                    "request": {
                        "connector": "local.echo",
                        "operation": "value.echo",
                        "input": {},
                        "timeout": { "schedule_to_start": "10s", "start_to_close": "20s" },
                        "retry": { "retry_on": ["timeout"], "max_attempts": 1, "initial_interval": "1s", "max_interval": "5s", "jitter": "1s" },
                        "next": "done"
                    }
                }]
            }]
        });
        assert!(messages(value).contains("is not enabled by this deployment"));
    }

    /// One process, with the capability enabled and one state under test.
    fn process_with(state: serde_json::Value) -> serde_json::Value {
        json!({
            "version": 3,
            "connectors": [{
                "name": "local.echo",
                "module": "local.echo",
                "operations": [{ "name": "value.echo" }, { "name": "text.artifact" }]
            }],
            "processes": [{
                "name": "render",
                "kind": "process",
                "version": 1,
                "source": "default",
                "start_at": "render",
                "states": [state]
            }]
        })
    }
}
