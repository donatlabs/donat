use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use donat_metadata::{
    Command, CommandIdempotencyScopeSpec, CommandResultValue, CommandStepOperation, CommandValue,
    ConnectorBounds, ConnectorEffect, ConnectorErrorMap, ConnectorInstance, ConnectorRedaction,
    ConnectorRetry, ConnectorSuccessContract, Process, load_metadata_dir,
};
use serde_yaml::Value;

fn petshop_metadata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop/metadata")
}

fn mapping_key_set(value: &Value) -> BTreeSet<&str> {
    value
        .as_mapping()
        .expect("contract node must be a mapping")
        .keys()
        .filter_map(Value::as_str)
        .collect()
}

fn yaml_files_below(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("Petshop metadata directory must be readable") {
        let path = entry.expect("directory entry must be readable").path();
        if path.is_dir() {
            yaml_files_below(&path, files);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("yaml") {
            files.push(path);
        }
    }
}

#[test]
fn every_petshop_command_file_uses_the_closed_command_grammar() {
    let mut files = Vec::new();
    yaml_files_below(&petshop_metadata_dir().join("commands"), &mut files);
    files.sort();
    assert_eq!(files.len(), 65);

    for path in files {
        let yaml = std::fs::read_to_string(&path).expect("command file must be readable");
        let command = serde_yaml::from_str::<Command>(&yaml)
            .unwrap_or_else(|error| panic!("{} must load: {error}", path.display()));
        let serialized = serde_yaml::to_string(&command)
            .unwrap_or_else(|error| panic!("{} must serialize: {error}", path.display()));
        serde_yaml::from_str::<Command>(&serialized)
            .unwrap_or_else(|error| panic!("{} must round-trip: {error}", path.display()));
    }
}

#[test]
fn every_petshop_flow_file_uses_the_closed_process_grammar() {
    let mut files = Vec::new();
    yaml_files_below(&petshop_metadata_dir().join("flows"), &mut files);
    files.sort();
    assert_eq!(files.len(), 11);

    for path in files {
        let yaml = std::fs::read_to_string(&path).expect("flow file must be readable");
        let process = serde_yaml::from_str::<Process>(&yaml)
            .unwrap_or_else(|error| panic!("{} must load: {error}", path.display()));
        let serialized = serde_yaml::to_string(&process)
            .unwrap_or_else(|error| panic!("{} must serialize: {error}", path.display()));
        serde_yaml::from_str::<Process>(&serialized)
            .unwrap_or_else(|error| panic!("{} must round-trip: {error}", path.display()));
    }
}

#[test]
fn quoted_includes_load_command_connector_and_process_sections_together() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/petshop-contract");
    let metadata = load_metadata_dir(&fixture).expect("quoted Petshop contract includes must load");

    assert_eq!(metadata.commands.len(), 1);
    assert_eq!(metadata.commands[0].name, "project_order");
    assert_eq!(metadata.connectors.len(), 1);
    assert_eq!(metadata.connectors[0].name, "mock_reader");
    assert_eq!(metadata.processes.len(), 1);
    assert_eq!(metadata.processes[0].name, "read_order");
}

#[test]
fn petshop_contract_loads_complete_active_grammar() {
    let metadata =
        load_metadata_dir(&petshop_metadata_dir()).expect("real Petshop metadata must load");

    assert_eq!(metadata.commands.len(), 65);
    assert_eq!(metadata.connectors.len(), 5);

    let serialized =
        serde_yaml::to_value(&metadata).expect("loaded Petshop metadata must serialize");
    let processes = serialized["processes"]
        .as_sequence()
        .expect("flows.yaml must load into the processes collection");
    assert_eq!(processes.len(), 11);

    let command_operations = serialized["commands"]
        .as_sequence()
        .expect("commands must serialize as a sequence")
        .iter()
        .flat_map(|command| {
            command["steps"]
                .as_sequence()
                .into_iter()
                .flatten()
                .flat_map(mapping_key_set)
                .filter(|key| *key != "name")
        })
        .collect::<BTreeSet<_>>();
    assert!(
        [
            "decision",
            "decision_many",
            "project",
            "project_many",
            "fixed_rows",
            "allocate_many",
            "assert_when",
            "update_when",
            "insert_when",
        ]
        .into_iter()
        .all(|operation| command_operations.contains(operation)),
        "every active Petshop command operation must be retained: {command_operations:?}"
    );

    let process_states = processes
        .iter()
        .flat_map(|process| {
            process["states"]
                .as_sequence()
                .expect("process states must be a sequence")
        })
        .collect::<Vec<_>>();
    let process_operations = process_states
        .iter()
        .flat_map(|state| mapping_key_set(state))
        .filter(|key| *key != "id")
        .collect::<BTreeSet<_>>();
    assert_eq!(
        process_operations,
        BTreeSet::from([
            "command", "fail", "for_each", "output", "request", "wait", "when",
        ]),
        "the real flows must retain all seven closed state forms"
    );
    let wait_forms = process_states
        .iter()
        .filter_map(|state| state["wait"].as_mapping())
        .flat_map(|wait| wait.keys().filter_map(Value::as_str))
        .filter(|key| *key == "signal" || *key == "timer")
        .collect::<BTreeSet<_>>();
    assert_eq!(
        wait_forms,
        BTreeSet::from(["signal", "timer"]),
        "both signal and timer waits must remain distinct"
    );

    let operations = serialized["connectors"]
        .as_sequence()
        .expect("connectors must serialize as a sequence")
        .iter()
        .flat_map(|connector| {
            connector["operations"]
                .as_sequence()
                .expect("connector operations must be a sequence")
        })
        .collect::<Vec<_>>();
    for required in [
        "input_contract",
        "effect",
        "bounds",
        "error_map",
        "timeout",
        "retry",
        "redaction",
    ] {
        assert!(
            operations
                .iter()
                .all(|operation| operation.get(required).is_some()),
            "every active connector operation must retain {required}"
        );
    }
    let effects = operations
        .iter()
        .map(|operation| &operation["effect"])
        .map(|effect| {
            effect
                .as_str()
                .or_else(|| {
                    effect
                        .as_mapping()
                        .and_then(|mapping| mapping.keys().find_map(Value::as_str))
                })
                .expect("connector effect must use a closed form")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        effects,
        BTreeSet::from(["provider_idempotent", "read_only"])
    );
    let success_contracts = operations
        .iter()
        .filter_map(|operation| operation.get("success_contract"))
        .map(mapping_key_set)
        .flatten()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        success_contracts,
        BTreeSet::from(["cases", "discriminator", "status", "unproven_absence"]),
        "both status and lookup success contracts must remain typed"
    );

    let prepare_quote = metadata
        .commands
        .iter()
        .find(|command| command.name == "prepare_checkout_quote")
        .expect("checkout quote command must be loaded");
    assert!(matches!(
        &prepare_quote.steps[2].operation,
        CommandStepOperation::SelectMany { select_many }
            if select_many.maximum_rows == Some(64)
    ));
    assert!(matches!(
        &prepare_quote.steps[12].operation,
        CommandStepOperation::InsertMany { insert_many }
            if insert_many.maximum_items == Some(64)
    ));
    assert!(matches!(
        prepare_quote.result.get("taxable_lines"),
        Some(CommandResultValue::Step {
            as_: Some(as_name),
            ..
        }) if as_name == "TaxableLine"
    ));

    let allocation = metadata
        .commands
        .iter()
        .find(|command| command.name == "allocate_inventory")
        .expect("allocation command must be loaded");
    assert!(matches!(
        &allocation.steps[5].operation,
        CommandStepOperation::Aggregate { aggregate }
            if matches!(
                &aggregate.from,
                CommandValue::Step {
                    field: Some(field),
                    ..
                } if field == "groups"
            )
    ));
    assert!(matches!(
        allocation
            .idempotency
            .as_ref()
            .map(|idempotency| &idempotency.scope),
        Some(CommandIdempotencyScopeSpec::Command(_))
    ));
    assert!(matches!(
        allocation.result.get("allocations"),
        Some(CommandResultValue::Step {
            field: Some(field),
            maximum_items: Some(64),
            ..
        }) if field == "groups"
    ));
    assert!(matches!(
        allocation.result.get("allocation_order"),
        Some(CommandResultValue::Array(values)) if values.len() == 4
    ));

    let shipment_result = metadata
        .commands
        .iter()
        .find(|command| command.name == "record_shipment_result")
        .expect("shipment result command must be loaded");
    assert!(matches!(
        shipment_result.result.get("capture_eligible"),
        Some(CommandResultValue::ProjectedStep {
            maximum_items: 1,
            ..
        })
    ));

    let connector_operations = metadata
        .connectors
        .iter()
        .flat_map(|connector| &connector.operations)
        .filter_map(|operation| operation.http())
        .collect::<Vec<_>>();
    let connector_yaml =
        serde_yaml::to_string(&metadata.connectors).expect("Petshop connectors must serialize");
    serde_yaml::from_str::<Vec<ConnectorInstance>>(&connector_yaml)
        .expect("Petshop connectors must round-trip");
    assert!(
        connector_operations
            .iter()
            .any(|operation| matches!(&operation.effect, Some(ConnectorEffect::ReadOnly(_))))
    );
    assert!(connector_operations.iter().any(|operation| matches!(
        &operation.effect,
        Some(ConnectorEffect::ProviderIdempotent { .. })
    )));
    assert!(connector_operations.iter().any(|operation| matches!(
        &operation.success_contract,
        Some(ConnectorSuccessContract::Status { .. })
    )));
    assert!(connector_operations.iter().any(|operation| matches!(
        &operation.success_contract,
        Some(ConnectorSuccessContract::Lookup { .. })
    )));

    let authorization = metadata
        .connectors
        .iter()
        .find(|connector| connector.name == "mock_payment")
        .and_then(|connector| {
            connector
                .operations
                .iter()
                .find(|operation| operation.name == "authorize")
        })
        .and_then(|operation| operation.http())
        .expect("mock payment authorization contract must load");
    assert_eq!(authorization.input_contract["request_id"], "string!");
    assert_eq!(
        authorization
            .bounds
            .as_ref()
            .expect("authorization bounds")
            .maximum_json_nodes,
        128
    );
    assert_eq!(
        authorization
            .response
            .get("provider_reference")
            .expect("bounded response field")
            .max_bytes,
        Some(256)
    );
    assert_eq!(
        authorization
            .retry
            .as_ref()
            .expect("authorization retry")
            .maximum_attempts,
        3
    );
    assert!(matches!(
        &authorization.effect,
        Some(ConnectorEffect::ProviderIdempotent {
            provider_idempotent
        }) if provider_idempotent.side_effect_steps[0]
            .evidence
            .source_record_id == "source.petshop.mock-providers.v1"
    ));
}

#[test]
fn command_bounds_and_new_payloads_reject_unrelated_unknown_fields() {
    let invalid_commands = [
        r#"
name: bad_select
source: default
steps:
  - name: row
    select_one:
      table: public.orders
      maximum_rows: 1
"#,
        r#"
name: bad_insert
source: default
steps:
  - name: row
    insert:
      table: public.orders
      maximum_items: 1
"#,
        r#"
name: bad_decision
source: default
steps:
  - name: row
    decision:
      decision_table: route
      input: {}
      returning: []
      arbitrary_expression: true
"#,
        r#"
name: bad_condition
source: default
steps:
  - name: row
    assert_when:
      when:
        argument_equals:
          argument: outcome
          value: accepted
          ignored: true
      rule: accepted
"#,
        r#"
name: bad_result
source: default
result:
  rows: { step: rows, maximum_items: 4, ignored: true }
"#,
    ];

    for yaml in invalid_commands {
        serde_yaml::from_str::<Command>(yaml)
            .expect_err("closed command payload must reject an unrelated field");
    }
}

#[test]
fn every_process_state_payload_rejects_unknown_fields() {
    let invalid_states = [
        "command: { name: run, run_as: worker, arguments: {}, next: done, ignored: true }",
        "request: { connector: mock, operation: read, input: {}, timeout: { schedule_to_start: 1s, start_to_close: 1s }, retry: { retry_on: [], max_attempts: 1, initial_interval: 1s, max_interval: 1s, jitter: deterministic_full }, next: done, ignored: true }",
        "when: { cases: [], default: done, ignored: true }",
        "wait: { signal: ready, role: worker, verification: required, correlate: {}, deadline: 1s, next: done, on_timeout: done, ignored: true }",
        "for_each: { input: { input: rows }, item_key: id, max_items: 1, max_concurrency: 1, completion: collect, command: { name: run, run_as: worker, arguments: {} }, next: done, ignored: true }",
        "output: { values: {}, ignored: true }",
        "fail: { code: failed, message: failed, ignored: true }",
    ];

    for state in invalid_states {
        let yaml = format!(
            "name: strict\nkind: process\nversion: 1\nsource: default\nstart_at: state\nstates:\n  - id: state\n    {state}\n"
        );
        serde_yaml::from_str::<Process>(&yaml)
            .expect_err("closed process state payload must reject an unrelated field");
    }
}

#[test]
fn connector_contract_payloads_reject_unknown_fields() {
    let bad_bounds = r#"
deadline_ms: 1
maximum_calls: 1
maximum_pages: 1
maximum_items: 1
maximum_aggregate_request_bytes: 1
maximum_aggregate_response_bytes: 1
maximum_output_canonical_bytes: 1
maximum_redirects: 0
maximum_json_depth: 1
maximum_json_nodes: 1
ignored: true
"#;
    serde_yaml::from_str::<ConnectorBounds>(bad_bounds)
        .expect_err("connector bounds must reject unknown fields");

    serde_yaml::from_str::<ConnectorEffect>(
        r#"
provider_idempotent:
  side_effect_steps:
    - step: request
      fixed_binding: { header: Idempotency-Key }
      scope: operation
      minimum_retention_ms: 1
      clock_safety_margin_ms: 1
      evidence:
        source_record_id: source
        fact_ids: [fact]
      ignored: true
"#,
    )
    .expect_err("provider idempotency must reject unknown fields");

    serde_yaml::from_str::<ConnectorErrorMap>(
        "{ rules: [], fallback: { class: permanent, code: failed }, ignored: true }",
    )
    .expect_err("connector error maps must reject unknown fields");
    serde_yaml::from_str::<ConnectorRetry>(
        "{ maximum_attempts: 1, backoff: 1s, retry_on: [], ignored: true }",
    )
    .expect_err("connector retry contracts must reject unknown fields");
    serde_yaml::from_str::<ConnectorRedaction>("{ request_headers: [], ignored: true }")
        .expect_err("connector redaction contracts must reject unknown fields");
    serde_yaml::from_str::<ConnectorSuccessContract>("{ status: accepted, ignored: true }")
        .expect_err("connector success contracts must reject unknown fields");
}
