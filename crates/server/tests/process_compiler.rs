use std::collections::BTreeMap;
use std::sync::Arc;

use donat_connector_abi::OperationId;
use donat_ir::{
    ProcessStartPolicy, TypeRef, ValueContractCatalog, ValueContractField, ValueScalar, ValueType,
};
use donat_metadata::{
    Metadata, ProcessErrorKind, ProcessField, ProcessLifecycle, ProcessOwner,
    ProcessStateOperation, ProcessValue, SourceKind,
};
use donat_rules::RuleType;
use donat_server::connectors::ConnectorRegistry;
use donat_server::processes::{
    ProcessCommandDescriptor, ProcessDecisionDescriptor, ProcessDependencyCatalog,
    ProcessRuleDescriptor, ResolvedProcessConnectorOperation,
    build_process_effect_contract_catalog, compile_process_catalog,
};
use serde_json::json;

struct Dependencies {
    types: BTreeMap<String, RuleType>,
    commands: BTreeMap<(String, String), ProcessCommandDescriptor>,
    rules: BTreeMap<String, ProcessRuleDescriptor>,
    decisions: BTreeMap<String, ProcessDecisionDescriptor>,
    connectors: ConnectorRegistry,
}

impl ProcessDependencyCatalog for Dependencies {
    fn declared_type(&self, name: &str) -> Option<RuleType> {
        self.types.get(name).cloned()
    }

    fn command(&self, source: &str, name: &str) -> Option<ProcessCommandDescriptor> {
        self.commands
            .get(&(source.to_owned(), name.to_owned()))
            .cloned()
    }

    fn rule(&self, name: &str) -> Option<ProcessRuleDescriptor> {
        self.rules.get(name).cloned()
    }

    fn decision_table(&self, name: &str) -> Option<ProcessDecisionDescriptor> {
        self.decisions.get(name).cloned()
    }

    fn connector_operation(
        &self,
        source: &str,
        instance: &str,
        operation: &str,
    ) -> Result<Option<ResolvedProcessConnectorOperation>, String> {
        donat_server::processes::ProcessConnectorCatalog::connector_operation(
            &self.connectors,
            source,
            instance,
            operation,
        )
    }
}

fn required(scalar: ValueScalar) -> ValueContractField {
    ValueContractField {
        required: true,
        type_ref: TypeRef {
            nullable: false,
            value_type: ValueType::Scalar { scalar },
        },
    }
}

fn contract(fields: impl IntoIterator<Item = (&'static str, ValueScalar)>) -> ValueContractCatalog {
    ValueContractCatalog {
        roots: fields
            .into_iter()
            .map(|(name, scalar)| (name.to_owned(), required(scalar)))
            .collect(),
        named_objects: BTreeMap::new(),
    }
}

fn base_metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": "postgres://unused" }
            }
        }],
        "connectors": [{
            "name": "payments",
            "module": "http",
            "config": {
                "endpoint_identity": "process-test-payments-v1",
                "credential_identity": "process-test-credential",
                "base_url": "https://payments.example.test"
            },
            "operations": [{
                "name": "authorize",
                "version": "1.0.0",
                "method": "POST",
                "path": "/authorizations",
                "input_contract": { "order_id": "uuid!" },
                "body": { "order_id": { "input": "order_id" } },
                "success_statuses": [200],
                "response": {
                    "status": {
                        "json_pointer": "/status",
                        "type": "string!",
                        "max_bytes": 64
                    }
                },
                "effect": {
                    "provider_idempotent": {
                        "side_effect_steps": [{
                            "step": "request",
                            "fixed_binding": { "header": "Idempotency-Key" },
                            "scope": "payment-authorize",
                            "minimum_retention_ms": 16100,
                            "clock_safety_margin_ms": 1000,
                            "evidence": {
                                "source_record_id": "source.process-test.payments.v1",
                                "fact_ids": ["fact.fixed-idempotency-key"]
                            }
                        }]
                    }
                },
                "bounds": {
                    "deadline_ms": 2000,
                    "maximum_calls": 1,
                    "maximum_pages": 1,
                    "maximum_items": 1,
                    "maximum_aggregate_request_bytes": 1024,
                    "maximum_aggregate_response_bytes": 1024,
                    "maximum_output_canonical_bytes": 1024,
                    "maximum_redirects": 0,
                    "maximum_json_depth": 4,
                    "maximum_json_nodes": 16
                },
                "error_map": {
                    "rules": [{
                        "statuses": [429],
                        "class": "http_429",
                        "code": "rate_limited"
                    }],
                    "fallback": {
                        "class": "permanent",
                        "code": "provider_error"
                    }
                },
                "capacity": {
                    "max_in_flight": 4,
                    "rate_limit": { "permits": 10, "per": "1s", "burst": 4 }
                },
                "timeout": "2s",
                "retry": {
                    "maximum_attempts": 2,
                    "backoff": "100ms",
                    "retry_on": ["transport", "timeout"]
                },
                "idempotency": { "header": "Idempotency-Key" }
            }]
        }],
        "processes": [{
            "name": "checkout",
            "kind": "process",
            "version": 1,
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "input": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "output": [{ "name": "status", "type": "string!" }],
            "idempotency": {
                "key": { "input": "request_id" },
                "scope": [{ "input": "order_id" }]
            },
            "start_at": "authorize",
            "states": [
                {
                    "id": "authorize",
                    "request": {
                        "connector": "payments",
                        "operation": "authorize",
                        "input": { "order_id": { "input": "order_id" } },
                        "idempotency_key": {
                            "stable": { "run": "id", "state": "authorize" }
                        },
                        "timeout": {
                            "schedule_to_start": "1s",
                            "start_to_close": "2s"
                        },
                        "retry": {
                            "retry_on": ["transport", "timeout"],
                            "max_attempts": 2,
                            "initial_interval": "100ms",
                            "max_interval": "1s",
                            "jitter": "deterministic_full"
                        },
                        "next": "done"
                    }
                },
                {
                    "id": "done",
                    "output": { "values": { "status": { "literal": "authorized" } } }
                }
            ]
        }]
    }))
    .expect("base process metadata deserializes")
}

fn base_dependencies(retention_ms: u64) -> Dependencies {
    let mut metadata = base_metadata();
    let donat_metadata::ConnectorOperationProfile::Http(operation) =
        &mut metadata.connectors[0].operations[0].profile
    else {
        unreachable!("test operation has an HTTP profile");
    };
    let effect = operation.effect.as_mut().unwrap();
    let donat_metadata::ConnectorEffect::ProviderIdempotent {
        provider_idempotent,
    } = effect
    else {
        unreachable!("test operation is provider-idempotent");
    };
    provider_idempotent.side_effect_steps[0].minimum_retention_ms = retention_ms;
    Dependencies {
        types: BTreeMap::new(),
        commands: BTreeMap::new(),
        rules: BTreeMap::new(),
        decisions: BTreeMap::new(),
        connectors: ConnectorRegistry::build(&metadata).expect("test connector registry compiles"),
    }
}

fn request_state_mut(metadata: &mut Metadata) -> &mut donat_metadata::ProcessRequestState {
    match &mut metadata.processes[0].states[0].operation {
        ProcessStateOperation::Request { request } => request,
        _ => panic!("base metadata starts with a request state"),
    }
}

fn replace_process_states(metadata: &mut Metadata, start_at: &str, states: serde_json::Value) {
    let mut document = serde_json::to_value(&*metadata).expect("metadata serializes");
    document["processes"][0]["start_at"] = json!(start_at);
    document["processes"][0]["states"] = states;
    *metadata = serde_json::from_value(document).expect("replacement process states deserialize");
}

fn command_metadata() -> Metadata {
    let mut metadata = base_metadata();
    replace_process_states(
        &mut metadata,
        "execute",
        json!([
            {
                "id": "execute",
                "command": {
                    "name": "record_checkout",
                    "run_as": "caller",
                    "arguments": { "order_id": { "input": "order_id" } },
                    "next": "done"
                }
            },
            {
                "id": "done",
                "output": { "values": { "status": { "literal": "recorded" } } }
            }
        ]),
    );
    metadata
}

fn command_dependencies() -> Dependencies {
    let mut dependencies = base_dependencies(16_100);
    dependencies.commands.insert(
        ("default".to_owned(), "record_checkout".to_owned()),
        ProcessCommandDescriptor {
            source: "default".to_owned(),
            name: "record_checkout".to_owned(),
            arguments: contract([("order_id", ValueScalar::Uuid)]),
            result: contract([("status", ValueScalar::String)]),
            allowed_roles: ["customer".to_owned()].into_iter().collect(),
            required_session_variables: BTreeMap::new(),
            definition_fingerprint: "record-checkout-v1".to_owned(),
        },
    );
    dependencies
}

#[test]
fn process_compiler_pins_exact_horizon_and_dependency_revision() {
    // 2 * (2s + 5s takeover) + (100ms retry upper bound + 1s schedule) = 15.1s.
    let catalog = compile_process_catalog(&base_metadata(), &base_dependencies(16_100))
        .expect("horizon equality is executable");
    let process = catalog
        .source("default")
        .unwrap()
        .process("checkout")
        .unwrap();

    assert_eq!(
        process.states["authorize"].maximum_send_horizons_ms["request"],
        15_100
    );
    assert_eq!(process.dependencies.connector_operations.len(), 1);
    assert_ne!(process.definition_fingerprint, process.revision_fingerprint);
}

#[test]
fn process_compiler_rejects_horizon_one_millisecond_over_usable_retention() {
    let error = compile_process_catalog(&base_metadata(), &base_dependencies(16_099))
        .expect_err("one millisecond beyond usable provider retention must fail closed");

    assert_eq!(error.path, "processes[0].states[0].request.retry");
    assert!(error.message.contains("15,100 ms"));
    assert!(error.message.contains("15,099 ms"));
}

#[test]
fn process_compiler_rejects_invalid_sources_and_graphs() {
    let dependencies = base_dependencies(16_100);
    let mut cases = Vec::new();

    let mut unknown_source = base_metadata();
    unknown_source.processes[0].source = "missing".to_owned();
    cases.push((
        "unknown source",
        unknown_source,
        "processes[0].source",
        "does not exist",
    ));

    let mut non_postgres = base_metadata();
    non_postgres.sources[0].kind = SourceKind::Sqlite;
    cases.push((
        "non-Postgres source",
        non_postgres,
        "processes[0].source",
        "must be postgres",
    ));

    let mut missing_start = base_metadata();
    missing_start.processes[0].start_at = "missing".to_owned();
    cases.push((
        "missing start",
        missing_start,
        "processes[0].start_at",
        "does not exist",
    ));

    let mut duplicate_state = base_metadata();
    duplicate_state.processes[0].states[1].id = "authorize".to_owned();
    cases.push((
        "duplicate state",
        duplicate_state,
        "processes[0].states[1].id",
        "declared more than once",
    ));

    let mut dangling_target = base_metadata();
    request_state_mut(&mut dangling_target).next = "missing".to_owned();
    cases.push((
        "dangling target",
        dangling_target,
        "processes[0].states[0]",
        "transition target `missing` does not exist",
    ));

    let mut unreachable_state = base_metadata();
    let mut orphan = unreachable_state.processes[0].states[1].clone();
    orphan.id = "orphan".to_owned();
    unreachable_state.processes[0].states.push(orphan);
    cases.push((
        "unreachable state",
        unreachable_state,
        "processes[0].states[2].id",
        "unreachable from start_at",
    ));

    let mut cyclic = base_metadata();
    replace_process_states(
        &mut cyclic,
        "first",
        json!([
            {
                "id": "first",
                "when": {
                    "cases": [{ "matches": { "branch": "forward" }, "next": "second" }],
                    "default": "second"
                }
            },
            {
                "id": "second",
                "when": {
                    "cases": [{ "matches": { "branch": "back" }, "next": "first" }],
                    "default": "first"
                }
            }
        ]),
    );
    cases.push(("cycle", cyclic, "processes[0].states[0]", "acyclic graph"));

    for (case, metadata, expected_path, expected_message) in cases {
        let error = compile_process_catalog(&metadata, &dependencies)
            .unwrap_err_or_else(|| panic!("{case} must fail closed"));
        assert_eq!(error.path, expected_path, "{case}");
        assert!(
            error.message.contains(expected_message),
            "{case}: {}",
            error.message
        );
    }
}

#[test]
fn process_compiler_rejects_invalid_activity_references_and_contracts() {
    let dependencies = base_dependencies(16_100);
    let mut cases = Vec::new();

    let mut unknown_connector = base_metadata();
    request_state_mut(&mut unknown_connector).connector = "missing".to_owned();
    cases.push((
        "unknown connector",
        unknown_connector,
        "processes[0].states[0].request.operation",
        "is not executable",
    ));

    let mut unknown_operation = base_metadata();
    request_state_mut(&mut unknown_operation).operation = "missing".to_owned();
    cases.push((
        "unknown operation",
        unknown_operation,
        "processes[0].states[0].request.operation",
        "is not executable",
    ));

    let mut missing_stable_key = base_metadata();
    request_state_mut(&mut missing_stable_key).idempotency_key = None;
    cases.push((
        "missing provider key",
        missing_stable_key,
        "processes[0].states[0].request.idempotency_key",
        "requires a stable activity key",
    ));

    let mut wrong_stable_state = base_metadata();
    request_state_mut(&mut wrong_stable_state)
        .idempotency_key
        .as_mut()
        .unwrap()
        .stable
        .state = "done".to_owned();
    cases.push((
        "wrong stable state",
        wrong_stable_state,
        "processes[0].states[0].request.idempotency_key.stable.state",
        "must equal owning state",
    ));

    let mut forward_reference = base_metadata();
    request_state_mut(&mut forward_reference).input.insert(
        "order_id".to_owned(),
        ProcessValue::State {
            state: "done".to_owned(),
            field: "status".to_owned(),
            project: None,
            as_: None,
            require_non_null: false,
        },
    );
    cases.push((
        "forward state reference",
        forward_reference,
        "processes[0].states[0].request.input.order_id",
        "must target a transition ancestor",
    ));

    let mut type_mismatch = base_metadata();
    type_mismatch.processes[0].input[0].type_ = "string!".to_owned();
    cases.push((
        "input type mismatch",
        type_mismatch,
        "processes[0].states[0].request.input.order_id",
        "String is not assignable to Uuid",
    ));

    let mut invalid_duration = base_metadata();
    request_state_mut(&mut invalid_duration)
        .timeout
        .schedule_to_start = "01s".to_owned();
    cases.push((
        "non-canonical duration",
        invalid_duration,
        "processes[0].states[0].request.timeout.schedule_to_start",
        "canonical positive base-10",
    ));

    let mut invalid_retry = base_metadata();
    request_state_mut(&mut invalid_retry).retry.max_attempts = 0;
    cases.push((
        "zero attempts",
        invalid_retry,
        "processes[0].states[0].request.retry.max_attempts",
        "greater than zero",
    ));

    let mut non_retryable_kind = base_metadata();
    request_state_mut(&mut non_retryable_kind).retry.retry_on =
        vec![ProcessErrorKind::Authentication];
    cases.push((
        "non-retryable error kind",
        non_retryable_kind,
        "processes[0].states[0].request.retry.retry_on[0]",
        "only transport, timeout, http_429, and http_5xx are retryable",
    ));

    for (case, metadata, expected_path, expected_message) in cases {
        let error = compile_process_catalog(&metadata, &dependencies)
            .unwrap_err_or_else(|| panic!("{case} must fail closed"));
        assert_eq!(error.path, expected_path, "{case}");
        assert!(
            error.message.contains(expected_message),
            "{case}: {}",
            error.message
        );
    }

    let mut wrong_source = base_metadata();
    wrong_source.sources[0].name = "other".to_owned();
    wrong_source.processes[0].source = "other".to_owned();
    let error = compile_process_catalog(&wrong_source, &dependencies)
        .expect_err("connector lookup must remain source-local");
    assert_eq!(error.path, "processes[0].states[0].request.operation");
    assert!(error.message.contains("other.payments.authorize"));
}

#[test]
fn process_input_requires_explicit_non_null_refinement() {
    let dependencies = base_dependencies(16_100);
    let mut nullable = base_metadata();
    nullable.processes[0].input.push(ProcessField {
        name: "optional_order_id".to_owned(),
        type_: "uuid".to_owned(),
    });
    request_state_mut(&mut nullable).input.insert(
        "order_id".to_owned(),
        ProcessValue::Input {
            input: "optional_order_id".to_owned(),
            as_: None,
            require_non_null: false,
        },
    );

    let error = compile_process_catalog(&nullable, &dependencies)
        .expect_err("nullable input must not flow into a required connector field");
    assert_eq!(error.path, "processes[0].states[0].request.input.order_id");
    assert!(
        error
            .message
            .contains("nullable Uuid is not assignable to Uuid")
    );

    let mut document = serde_json::to_value(nullable).expect("metadata serializes");
    document["processes"][0]["states"][0]["request"]["input"]["order_id"]["require_non_null"] =
        json!(true);
    let refined: Metadata =
        serde_json::from_value(document).expect("input refinement metadata deserializes");

    compile_process_catalog(&refined, &dependencies)
        .expect("an explicit input non-null assertion satisfies the required contract");
}

#[test]
fn process_string_to_enum_requires_explicit_nominal_refinement() {
    let mut metadata = command_metadata();
    metadata.processes[0].output[0].type_ = "CheckoutState!".to_owned();
    replace_process_states(
        &mut metadata,
        "execute",
        json!([
            {
                "id": "execute",
                "command": {
                    "name": "record_checkout",
                    "run_as": "caller",
                    "arguments": { "order_id": { "input": "order_id" } },
                    "next": "done"
                }
            },
            {
                "id": "done",
                "output": {
                    "values": {
                        "status": {
                            "state": "execute",
                            "field": "status",
                            "as": "CheckoutState"
                        }
                    }
                }
            }
        ]),
    );
    let mut dependencies = command_dependencies();
    dependencies.types.insert(
        "CheckoutState".to_owned(),
        RuleType::Enum {
            name: "CheckoutState".to_owned(),
            symbols: ["recorded".to_owned()].into_iter().collect(),
        },
    );

    compile_process_catalog(&metadata, &dependencies)
        .expect("an explicit string-to-enum refinement satisfies a nominal output contract");

    let mut unrefined = serde_json::to_value(metadata).expect("metadata serializes");
    unrefined["processes"][0]["states"][1]["output"]["values"]["status"]
        .as_object_mut()
        .expect("state output is an object")
        .remove("as");
    let unrefined: Metadata =
        serde_json::from_value(unrefined).expect("unrefined metadata deserializes");
    let error = compile_process_catalog(&unrefined, &dependencies)
        .expect_err("plain strings must not widen implicitly to a nominal enum");
    assert_eq!(error.path, "processes[0].states[1].output.values.status");
    assert!(
        error
            .message
            .contains("String is not assignable to enum CheckoutState")
    );
}

trait ExpectErrOrElse<T, E> {
    fn unwrap_err_or_else(self, fallback: impl FnOnce() -> E) -> E;
}

impl<T, E> ExpectErrOrElse<T, E> for Result<T, E> {
    fn unwrap_err_or_else(self, fallback: impl FnOnce() -> E) -> E {
        match self {
            Ok(_) => fallback(),
            Err(error) => error,
        }
    }
}

#[test]
fn process_compiler_rejects_unknown_commands_and_wrong_roles() {
    let metadata = command_metadata();
    let mut missing = command_dependencies();
    missing.commands.clear();
    let error =
        compile_process_catalog(&metadata, &missing).expect_err("unknown command must fail closed");
    assert_eq!(error.path, "processes[0].states[0].command.name");
    assert!(error.message.contains("default.record_checkout"));

    let mut wrong_role = command_dependencies();
    wrong_role
        .commands
        .get_mut(&("default".to_owned(), "record_checkout".to_owned()))
        .unwrap()
        .allowed_roles = ["worker".to_owned()].into_iter().collect();
    let error = compile_process_catalog(&metadata, &wrong_role)
        .expect_err("caller role outside command permissions must fail closed");
    assert_eq!(error.path, "processes[0].states[0].command.run_as");
    assert!(error.message.contains("process caller role `customer`"));

    let mut fixed_metadata = command_metadata();
    let ProcessStateOperation::Command { command } =
        &mut fixed_metadata.processes[0].states[0].operation
    else {
        unreachable!("command metadata starts with a command");
    };
    command.run_as = "customer".to_owned();
    let mut fixed_dependencies = command_dependencies();
    fixed_dependencies
        .commands
        .get_mut(&("default".to_owned(), "record_checkout".to_owned()))
        .unwrap()
        .required_session_variables = BTreeMap::from([(
        "customer".to_owned(),
        BTreeMap::from([(
            "x-donat-user-id".to_owned(),
            TypeRef {
                nullable: false,
                value_type: ValueType::Scalar {
                    scalar: ValueScalar::Uuid,
                },
            },
        )]),
    )]);
    let error = compile_process_catalog(&fixed_metadata, &fixed_dependencies)
        .expect_err("fixed Process roles cannot invent an ambient session");
    assert_eq!(error.path, "processes[0].states[0].command.run_as");
    assert!(
        error
            .message
            .contains("fixed Process command role `customer` requires session variables")
    );
}

#[test]
fn process_compiler_rejects_unknown_rules_decisions_and_signals() {
    let dependencies = base_dependencies(16_100);
    let cases = [
        (
            "unknown rule",
            json!([
                {
                    "id": "route",
                    "when": {
                        "cases": [{ "rule": "missing_rule", "with": {}, "next": "done" }],
                        "default": "done"
                    }
                },
                {
                    "id": "done",
                    "output": { "values": { "status": { "literal": "done" } } }
                }
            ]),
            "processes[0].states[0].when.cases[0].rule",
            "rule `missing_rule` does not exist",
        ),
        (
            "unknown decision table",
            json!([
                {
                    "id": "route",
                    "when": {
                        "decision_table": "missing_decision",
                        "input": {},
                        "cases": [{ "matches": { "outcome": "ok" }, "next": "done" }],
                        "default": "done"
                    }
                },
                {
                    "id": "done",
                    "output": { "values": { "status": { "literal": "done" } } }
                }
            ]),
            "processes[0].states[0].when.decision_table",
            "decision table `missing_decision` does not exist",
        ),
        (
            "unknown signal",
            json!([
                {
                    "id": "await_signal",
                    "wait": {
                        "signal": "missing_signal",
                        "role": "customer",
                        "verification": "required",
                        "persist_before_match": true,
                        "correlate": {},
                        "deadline": "1d",
                        "next": "done",
                        "on_timeout": "done"
                    }
                },
                {
                    "id": "done",
                    "output": { "values": { "status": { "literal": "done" } } }
                }
            ]),
            "processes[0].states[0].wait.signal",
            "signal `missing_signal` is not declared by the process",
        ),
    ];

    for (case, states, expected_path, expected_message) in cases {
        let mut metadata = base_metadata();
        let start_at = states[0]["id"].as_str().unwrap().to_owned();
        replace_process_states(&mut metadata, &start_at, states);
        let error = compile_process_catalog(&metadata, &dependencies)
            .unwrap_err_or_else(|| panic!("{case} must fail closed"));
        assert_eq!(error.path, expected_path, "{case}");
        assert!(
            error.message.contains(expected_message),
            "{case}: {}",
            error.message
        );
    }
}

#[test]
fn retry_horizon_includes_single_attempt_grace_and_rejects_overflow() {
    let mut single_attempt = base_metadata();
    request_state_mut(&mut single_attempt).retry.max_attempts = 1;
    let catalog = compile_process_catalog(&single_attempt, &base_dependencies(8_000))
        .expect("2s start_to_close plus 5s takeover equals the 7s usable window");
    assert_eq!(
        catalog
            .source("default")
            .unwrap()
            .process("checkout")
            .unwrap()
            .states["authorize"]
            .maximum_send_horizons_ms["request"],
        7_000
    );

    let mut overflow = base_metadata();
    let request = request_state_mut(&mut overflow);
    request.timeout.start_to_close = "18446744073709551s".to_owned();
    request.retry.max_attempts = 2;
    let error = compile_process_catalog(&overflow, &base_dependencies(u64::MAX))
        .expect_err("checked horizon arithmetic must reject overflow");
    assert_eq!(error.path, "processes[0].states[0].request.retry");
    assert_eq!(error.message, "activity horizon overflowed");

    let mut many_attempts = base_metadata();
    request_state_mut(&mut many_attempts).retry.max_attempts = u32::MAX;
    let catalog = compile_process_catalog(&many_attempts, &base_dependencies(u64::MAX))
        .expect("a large bounded retry count compiles without a linear loop");
    assert!(
        catalog
            .source("default")
            .unwrap()
            .process("checkout")
            .unwrap()
            .states["authorize"]
            .maximum_send_horizons_ms["request"]
            > 15_100
    );
}

#[test]
fn process_revision_is_stable_closed_and_pins_the_catalog_arc() {
    let metadata = base_metadata();
    let dependencies = base_dependencies(16_100);
    let first = compile_process_catalog(&metadata, &dependencies).expect("baseline compiles");
    let second = compile_process_catalog(&metadata, &dependencies).expect("repeat compiles");
    let first_process = first
        .source("default")
        .unwrap()
        .process("checkout")
        .unwrap();
    let second_process = second
        .source("default")
        .unwrap()
        .process("checkout")
        .unwrap();
    assert_eq!(
        first_process.definition_fingerprint,
        second_process.definition_fingerprint
    );
    assert_eq!(
        first_process.revision_fingerprint,
        second_process.revision_fingerprint
    );

    let operation = OperationId::parse("authorize").unwrap();
    let registry_handle = dependencies
        .connectors
        .operation_spec_handle("default", "payments", operation)
        .unwrap();
    let pinned = first_process
        .dependencies
        .connector_operations
        .get(&("default".to_owned(), "payments".to_owned(), operation))
        .unwrap();
    assert!(
        Arc::ptr_eq(&registry_handle, &pinned.spec),
        "the process closure must retain the exact catalog-owned OperationSpec"
    );

    let changed = compile_process_catalog(&metadata, &base_dependencies(16_101))
        .expect("changed executable connector dependency still compiles");
    let changed_process = changed
        .source("default")
        .unwrap()
        .process("checkout")
        .unwrap();
    assert_eq!(
        first_process.definition_fingerprint, changed_process.definition_fingerprint,
        "dependency changes do not rewrite the declarative definition identity"
    );
    assert_ne!(
        first_process.revision_fingerprint, changed_process.revision_fingerprint,
        "a used connector behavior change creates a new executable revision"
    );

    let mut with_unused_dependency = base_dependencies(16_100);
    with_unused_dependency.commands.insert(
        ("default".to_owned(), "unused".to_owned()),
        ProcessCommandDescriptor {
            source: "default".to_owned(),
            name: "unused".to_owned(),
            arguments: ValueContractCatalog {
                roots: BTreeMap::new(),
                named_objects: BTreeMap::new(),
            },
            result: ValueContractCatalog {
                roots: BTreeMap::new(),
                named_objects: BTreeMap::new(),
            },
            allowed_roles: ["customer".to_owned()].into_iter().collect(),
            required_session_variables: BTreeMap::new(),
            definition_fingerprint: "unused-v1".to_owned(),
        },
    );
    let unchanged = compile_process_catalog(&metadata, &with_unused_dependency)
        .expect("an unrelated dependency does not enter the closure");
    assert_eq!(
        first_process.revision_fingerprint,
        unchanged
            .source("default")
            .unwrap()
            .process("checkout")
            .unwrap()
            .revision_fingerprint
    );
}

#[test]
fn custom_owner_types_and_lifecycle_publish_exact_effect_policy() {
    let mut metadata = base_metadata();
    metadata.processes[0].input.push(ProcessField {
        name: "tenant_key".to_owned(),
        type_: "TenantKey!".to_owned(),
    });
    metadata.processes[0].owner = Some(ProcessOwner {
        type_: "TenantKey!".to_owned(),
        capture: ProcessValue::Input {
            input: "tenant_key".to_owned(),
            as_: None,
            require_non_null: false,
        },
    });
    metadata.processes[0].lifecycle = ProcessLifecycle::Retired;
    let mut dependencies = base_dependencies(16_100);
    dependencies.types.insert(
        "TenantKey".to_owned(),
        RuleType::Enum {
            name: "TenantKey".to_owned(),
            symbols: ["north".to_owned(), "south".to_owned()]
                .into_iter()
                .collect(),
        },
    );

    let compiled = compile_process_catalog(&metadata, &dependencies)
        .expect("owner declarations resolve through the real declared-type catalog");
    let effects =
        build_process_effect_contract_catalog(&compiled).expect("effect contracts publish");
    assert_eq!(
        effects.sources["default"]["checkout"].start_policy,
        ProcessStartPolicy::RejectRetired
    );

    metadata.processes[0].lifecycle = ProcessLifecycle::Active;
    let active =
        compile_process_catalog(&metadata, &dependencies).expect("active process compiles");
    let effects = build_process_effect_contract_catalog(&active).unwrap();
    assert_eq!(
        effects.sources["default"]["checkout"].start_policy,
        ProcessStartPolicy::Enabled
    );
}

fn petshop_metadata() -> Metadata {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop/metadata");
    donat_metadata::load_metadata_dir(&root).expect("Petshop metadata loads")
}

fn petshop_dependencies(metadata: &Metadata) -> Dependencies {
    let mut registry_metadata = metadata.clone();
    for instance in &mut registry_metadata.connectors {
        instance.config.base_url = Some(donat_metadata::ConnectorBaseUrl::Literal(format!(
            "https://{}.example.test",
            instance.name.replace('_', "-")
        )));
        instance.config.headers.clear();
    }
    let connectors =
        ConnectorRegistry::build(&registry_metadata).expect("Petshop connector catalog compiles");
    let mut types = resolve_declared_types(metadata);
    types.insert(
        "ShipmentCaptureEligibility".to_owned(),
        RuleType::Object {
            name: "ShipmentCaptureEligibility".to_owned(),
            fields: BTreeMap::from([
                ("shipment_id".to_owned(), RuleType::Uuid),
                ("allocation_id".to_owned(), RuleType::Uuid),
                ("shipped_value_minor".to_owned(), RuleType::Int64),
                ("currency".to_owned(), RuleType::String),
            ]),
        },
    );

    let commands = metadata
        .commands
        .iter()
        .map(|command| {
            let argument_types = command
                .arguments
                .iter()
                .map(|argument| (argument.name.clone(), argument.type_.clone()))
                .collect();
            let result_types = command_result_types(&command.name);
            (
                (command.source.clone(), command.name.clone()),
                ProcessCommandDescriptor {
                    source: command.source.clone(),
                    name: command.name.clone(),
                    arguments: contract_from_sources(&argument_types, &types),
                    result: contract_from_sources(&result_types, &types),
                    allowed_roles: command
                        .permissions
                        .iter()
                        .map(|permission| permission.role.clone())
                        .collect(),
                    required_session_variables: BTreeMap::new(),
                    definition_fingerprint: format!("petshop-command:{}:v1", command.name),
                },
            )
        })
        .collect();
    let rules = metadata
        .rules
        .rules
        .iter()
        .map(|rule| {
            (
                rule.name.clone(),
                ProcessRuleDescriptor {
                    name: rule.name.clone(),
                    bindings: rule
                        .parameters
                        .iter()
                        .map(|(name, source)| (name.clone(), rule_type_from_source(source, &types)))
                        .collect(),
                    result: rule_type_from_source(&rule.result, &types),
                    definition_fingerprint: format!("petshop-rule:{}:v1", rule.name),
                },
            )
        })
        .collect();
    let decisions = metadata
        .rules
        .decision_tables
        .iter()
        .map(|table| {
            (
                table.name.clone(),
                ProcessDecisionDescriptor {
                    name: table.name.clone(),
                    inputs: table
                        .inputs
                        .iter()
                        .map(|(name, source)| (name.clone(), rule_type_from_source(source, &types)))
                        .collect(),
                    output: table
                        .output
                        .iter()
                        .map(|(name, source)| (name.clone(), rule_type_from_source(source, &types)))
                        .collect(),
                    definition_fingerprint: format!("petshop-decision:{}:v1", table.name),
                },
            )
        })
        .collect();
    Dependencies {
        types,
        commands,
        rules,
        decisions,
        connectors,
    }
}

#[test]
fn all_petshop_processes_compile_and_publish_effect_contracts() {
    let metadata = petshop_metadata();
    assert_eq!(metadata.processes.len(), 11);
    let mut counts = [0_usize; 7];
    for process in &metadata.processes {
        for state in &process.states {
            match &state.operation {
                donat_metadata::ProcessStateOperation::Command { .. } => counts[0] += 1,
                donat_metadata::ProcessStateOperation::Request { .. } => counts[1] += 1,
                donat_metadata::ProcessStateOperation::When { .. } => counts[2] += 1,
                donat_metadata::ProcessStateOperation::Wait { .. } => counts[3] += 1,
                donat_metadata::ProcessStateOperation::ForEach { .. } => counts[4] += 1,
                donat_metadata::ProcessStateOperation::Output { .. } => counts[5] += 1,
                donat_metadata::ProcessStateOperation::Fail { .. } => counts[6] += 1,
            }
        }
    }
    assert_eq!(counts, [57, 18, 23, 10, 15, 29, 16]);
    assert_eq!(counts.iter().sum::<usize>(), 168);

    let dependencies = petshop_dependencies(&metadata);
    let catalog =
        compile_process_catalog(&metadata, &dependencies).expect("all Petshop flows compile");
    assert_eq!(catalog.len(), 11);
    let contracts =
        build_process_effect_contract_catalog(&catalog).expect("effect contracts publish");
    assert_eq!(contracts.sources["default"].len(), 11);
    for (source_name, source) in catalog.sources() {
        for (process_name, process) in source.iter() {
            let contract = &contracts.sources[source_name][process_name];
            for (signal_name, signal) in &contract.signals {
                assert_eq!(
                    signal.contract_revision, process.revision_fingerprint,
                    "signal `{source_name}.{process_name}.{signal_name}` must pin the executable Process revision"
                );
                assert_eq!(signal.compatible_revisions.len(), 1);
                assert!(
                    signal
                        .compatible_revisions
                        .contains(&process.revision_fingerprint),
                    "the current Process revision must be signal-compatible"
                );
            }
        }
    }
    assert_eq!(
        metadata
            .commands
            .iter()
            .map(|command| command.effects.len())
            .sum::<usize>(),
        17,
        "every Petshop start/signal effect has a compiled target contract"
    );
}

fn resolve_declared_types(metadata: &Metadata) -> BTreeMap<String, RuleType> {
    fn resolve(
        name: &str,
        metadata: &Metadata,
        resolved: &mut BTreeMap<String, RuleType>,
        pending: &mut std::collections::BTreeSet<String>,
    ) -> RuleType {
        if let Some(type_) = resolved.get(name) {
            return type_.clone();
        }
        assert!(pending.insert(name.to_owned()), "recursive type `{name}`");
        let declaration = metadata
            .rules
            .types
            .iter()
            .find(|declaration| declaration.name == name)
            .unwrap_or_else(|| panic!("declared type `{name}` exists"));
        let type_ = if let Some(symbols) = &declaration.enum_values {
            RuleType::Enum {
                name: name.to_owned(),
                symbols: symbols.clone(),
            }
        } else if let Some(fields) = &declaration.object {
            RuleType::Object {
                name: name.to_owned(),
                fields: fields
                    .iter()
                    .map(|(field, source)| {
                        (
                            field.clone(),
                            parse_rule_type(source, metadata, resolved, pending),
                        )
                    })
                    .collect(),
            }
        } else if let Some(bounds) = &declaration.opaque_json {
            RuleType::OpaqueJson {
                name: name.to_owned(),
                maximum_bytes: bounds.maximum_bytes,
                maximum_depth: bounds.maximum_depth,
                maximum_nodes: bounds.maximum_nodes,
            }
        } else {
            panic!("type `{name}` has one declaration body");
        };
        pending.remove(name);
        resolved.insert(name.to_owned(), type_.clone());
        type_
    }

    fn parse_rule_type(
        source: &str,
        metadata: &Metadata,
        resolved: &mut BTreeMap<String, RuleType>,
        pending: &mut std::collections::BTreeSet<String>,
    ) -> RuleType {
        let parsed = TypeRef::parse(source).expect("Petshop type parses");
        let mut type_ = match parsed.value_type {
            ValueType::Scalar { scalar } => scalar_rule_type(scalar),
            ValueType::List { element } => RuleType::List(Box::new(type_ref_rule_type(
                *element, metadata, resolved, pending,
            ))),
            ValueType::Ref { name } if name == "bigint" => RuleType::Int64,
            ValueType::Ref { name } => resolve(&name, metadata, resolved, pending),
            ValueType::Enum { name, values } => RuleType::Enum {
                name,
                symbols: values,
            },
            ValueType::Object { .. } => unreachable!("type source has no inline object"),
        };
        if parsed.nullable {
            type_ = RuleType::nullable(type_);
        }
        type_
    }

    fn type_ref_rule_type(
        parsed: TypeRef,
        metadata: &Metadata,
        resolved: &mut BTreeMap<String, RuleType>,
        pending: &mut std::collections::BTreeSet<String>,
    ) -> RuleType {
        let mut type_ = match parsed.value_type {
            ValueType::Scalar { scalar } => scalar_rule_type(scalar),
            ValueType::List { element } => RuleType::List(Box::new(type_ref_rule_type(
                *element, metadata, resolved, pending,
            ))),
            ValueType::Ref { name } if name == "bigint" => RuleType::Int64,
            ValueType::Ref { name } => resolve(&name, metadata, resolved, pending),
            ValueType::Enum { name, values } => RuleType::Enum {
                name,
                symbols: values,
            },
            ValueType::Object { .. } => unreachable!("type source has no inline object"),
        };
        if parsed.nullable {
            type_ = RuleType::nullable(type_);
        }
        type_
    }

    let mut resolved = BTreeMap::new();
    for declaration in &metadata.rules.types {
        resolve(
            &declaration.name,
            metadata,
            &mut resolved,
            &mut std::collections::BTreeSet::new(),
        );
    }
    resolved
}

fn scalar_rule_type(scalar: ValueScalar) -> RuleType {
    match scalar {
        ValueScalar::Boolean => RuleType::Bool,
        ValueScalar::String | ValueScalar::Custom { .. } | ValueScalar::Json => RuleType::String,
        ValueScalar::Int32 => RuleType::Int,
        ValueScalar::Int64 | ValueScalar::UInt64 => RuleType::Int64,
        ValueScalar::Decimal => RuleType::Decimal,
        ValueScalar::Uuid => RuleType::Uuid,
        ValueScalar::Date => RuleType::Date,
        ValueScalar::Timestamp | ValueScalar::TimestampTz => RuleType::Timestamp,
    }
}

fn rule_type_from_source(source: &str, types: &BTreeMap<String, RuleType>) -> RuleType {
    let parsed = TypeRef::parse(source).expect("type source parses");
    fn convert(parsed: TypeRef, types: &BTreeMap<String, RuleType>) -> RuleType {
        let mut type_ = match parsed.value_type {
            ValueType::Scalar { scalar } => scalar_rule_type(scalar),
            ValueType::Ref { name } if name == "bigint" => RuleType::Int64,
            ValueType::Ref { name } => types
                .get(&name)
                .unwrap_or_else(|| panic!("type `{name}` is declared"))
                .clone(),
            ValueType::List { element } => RuleType::List(Box::new(convert(*element, types))),
            ValueType::Enum { name, values } => RuleType::Enum {
                name,
                symbols: values,
            },
            ValueType::Object { .. } => unreachable!("source type has no inline object"),
        };
        if parsed.nullable {
            type_ = RuleType::nullable(type_);
        }
        type_
    }
    convert(parsed, types)
}

fn contract_from_sources(
    fields: &BTreeMap<String, String>,
    types: &BTreeMap<String, RuleType>,
) -> ValueContractCatalog {
    ValueContractCatalog {
        roots: fields
            .iter()
            .map(|(name, source)| {
                let type_ref = type_ref_from_rule(&rule_type_from_source(source, types));
                (
                    name.clone(),
                    ValueContractField {
                        required: !type_ref.nullable,
                        type_ref,
                    },
                )
            })
            .collect(),
        named_objects: BTreeMap::new(),
    }
}

fn type_ref_from_rule(type_: &RuleType) -> TypeRef {
    match type_ {
        RuleType::Nullable(inner) => {
            let mut type_ref = type_ref_from_rule(inner);
            type_ref.nullable = true;
            type_ref
        }
        RuleType::Bool => test_scalar(ValueScalar::Boolean),
        RuleType::String => test_scalar(ValueScalar::String),
        RuleType::Int => test_scalar(ValueScalar::Int32),
        RuleType::Int64 => test_scalar(ValueScalar::Int64),
        RuleType::Decimal => test_scalar(ValueScalar::Decimal),
        RuleType::Uuid => test_scalar(ValueScalar::Uuid),
        RuleType::Date => test_scalar(ValueScalar::Date),
        RuleType::Timestamp => test_scalar(ValueScalar::TimestampTz),
        RuleType::Enum { name, symbols } => TypeRef {
            nullable: false,
            value_type: ValueType::Enum {
                name: name.clone(),
                values: symbols.clone(),
            },
        },
        RuleType::List(element) => TypeRef {
            nullable: false,
            value_type: ValueType::List {
                element: Box::new(type_ref_from_rule(element)),
            },
        },
        RuleType::Object { fields, .. } => TypeRef {
            nullable: false,
            value_type: ValueType::Object {
                fields: fields
                    .iter()
                    .map(|(name, type_)| {
                        let type_ref = type_ref_from_rule(type_);
                        (
                            name.clone(),
                            ValueContractField {
                                required: !type_ref.nullable,
                                type_ref,
                            },
                        )
                    })
                    .collect(),
            },
        },
        RuleType::OpaqueJson { .. } => test_scalar(ValueScalar::Json),
    }
}

fn test_scalar(scalar: ValueScalar) -> TypeRef {
    TypeRef {
        nullable: false,
        value_type: ValueType::Scalar { scalar },
    }
}

fn command_result_types(name: &str) -> BTreeMap<String, String> {
    let fields: &[(&str, &str)] = match name {
        "request_authorized_order_cancellation" => &[
            ("order_id", "uuid!"),
            ("owner_user_id", "string!"),
            ("payment_id", "uuid!"),
            ("authorization_id", "uuid!"),
            ("reason", "string!"),
        ],
        "submit_quote" => &[
            ("quote_id", "uuid!"),
            ("approval_id", "uuid!"),
            ("total_minor", "bigint!"),
            ("available_credit_minor", "bigint!"),
            ("owner_user_id", "string!"),
        ],
        "cancel_order" => &[
            ("order_id", "uuid!"),
            ("owner_user_id", "string!"),
            ("payment_id", "uuid!"),
            ("authorization_activity_key", "string!"),
            ("reason", "string!"),
        ],
        "prepare_checkout_quote" => &[
            ("checkout_quote_id", "uuid!"),
            ("destination_country_code", "string!"),
            ("currency", "string!"),
            ("taxable_lines", "[TaxableLine!]!"),
        ],
        "begin_checkout" => &[
            ("order_id", "uuid!"),
            ("payment_id", "uuid!"),
            ("authorization_activity_key", "string!"),
            ("total_minor", "bigint!"),
            ("currency", "string!"),
        ],
        "allocate_inventory" => &[
            ("allocations", "[AllocationGroup!]!"),
            ("backorders", "[Backorder!]!"),
            ("payment_id", "uuid!"),
            ("authorization_id", "uuid!"),
        ],
        "mark_order_packed" => &[
            ("allocation_id", "uuid!"),
            ("order_id", "uuid!"),
            ("stock_location_code", "string!"),
            ("quantity", "int!"),
            ("status", "string!"),
            ("packed_at", "timestamptz!"),
        ],
        "create_shipment" => &[
            ("shipment_id", "uuid!"),
            ("allocation_id", "uuid!"),
            ("shipment_key", "string!"),
            ("stock_location_code", "string!"),
            ("shipped_value_minor", "bigint!"),
            ("currency", "string!"),
            ("items", "[AllocationItem!]!"),
        ],
        "record_shipment_result" => &[
            ("shipment_id", "uuid!"),
            ("allocation_id", "uuid!"),
            ("outcome", "string!"),
            ("tracking_number", "string"),
            ("failure_code", "string"),
            ("requires_reconciliation", "bool!"),
            ("capture_eligible", "[ShipmentCaptureEligibility!]!"),
        ],
        "claim_payment_captures" => &[("capture_claims", "[PaymentCaptureClaimInput!]!")],
        "capture_payment" | "release_absent_capture_claim" => &[
            ("shipment_id", "uuid!"),
            ("payment_id", "uuid!"),
            ("capture_id", "uuid"),
            ("amount_minor", "bigint!"),
            ("status", "PaymentState!"),
            ("requires_reconciliation", "bool!"),
        ],
        "request_return" => &[
            ("return_id", "uuid!"),
            ("replacement_requested", "bool!"),
            ("currency", "string!"),
            ("return_from", "ReturnAddress!"),
            ("items", "[ReturnCarrierItem!]!"),
        ],
        "complete_refund" => &[("refund_id", "bigint!"), ("amount_minor", "bigint!")],
        "create_exchange" => &[("exchange_id", "uuid!")],
        "finalize_return_refund" => &[("status", "string!")],
        "finalize_return_rejection" => &[("status", "string!"), ("public_reason_code", "string!")],
        "create_subscription_order" => &[
            ("renewal_id", "uuid!"),
            ("order_id", "uuid!"),
            ("payment_id", "uuid!"),
            ("amount_minor", "bigint!"),
            ("currency", "string!"),
        ],
        "consume_credit" => &[("approval_id", "uuid!"), ("approval_status", "string!")],
        "finalize_finance_rejection" | "finalize_unroutable_rejection" => {
            &[("approval_id", "uuid!"), ("approval_status", "string!")]
        }
        "create_vendor_payout" => &[("payouts", "[VendorPayoutCandidate!]!")],
        "record_payout_outcome" => &[
            ("payout_id", "uuid!"),
            ("vendor_id", "uuid!"),
            ("provider_payout_id", "string"),
            ("outcome", "PayoutState!"),
            ("requires_reconciliation", "bool!"),
        ],
        "reserve_grooming_slot" => &[
            ("booking_id", "uuid!"),
            ("slot_key", "string!"),
            ("hold_expires_at", "timestamptz!"),
        ],
        "expire_booking_hold" => &[
            ("booking_id", "uuid!"),
            ("slot_key", "string!"),
            ("status", "string!"),
        ],
        "submit_prescription_review" => &[
            ("prescription_id", "uuid!"),
            ("review_deadline", "timestamptz!"),
        ],
        "expire_prescription" => &[("prescription_id", "uuid!"), ("status", "string!")],
        "reconcile_payment" => &[("reconciliation_id", "uuid!")],
        "materialize_cancellation_authorization" => &[("authorization_id", "uuid!")],
        "finalize_authorized_order_cancellation" => {
            &[("order_id", "uuid!"), ("order_status", "string!")]
        }
        "finalize_pending_order_cancellation" => {
            &[("order_id", "uuid!"), ("order_status", "string!")]
        }
        _ => &[],
    };
    fields
        .iter()
        .map(|(name, type_)| ((*name).to_owned(), (*type_).to_owned()))
        .collect()
}
