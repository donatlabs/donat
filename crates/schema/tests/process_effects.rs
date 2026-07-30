use std::collections::{BTreeMap, BTreeSet, HashMap};

use donat_catalog::Catalog;
use donat_ir::{
    ProcessStartPolicy, TypeRef, ValueContractCatalog, ValueContractField, ValueScalar, ValueType,
};
use donat_metadata::Metadata;
use donat_rules::compile_catalog;
use donat_schema::{
    CompiledMultiSourceSchema, FinalizedCommandEffect, ProcessEffectContract,
    ProcessEffectContractCatalog, ProcessEffectContractSource, ProcessSignalEffectContract,
    compile_command_catalog, finalize_command_effects,
};
use serde_json::json;

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

fn command_metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": "postgres://unused" }
            }
        }],
        "commands": [{
            "name": "kickoff",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "request_id", "type": "uuid!" },
                { "name": "order_id", "type": "uuid!" },
                { "name": "decision", "type": "String!" }
            ],
            "steps": [],
            "result": {
                "order_id": { "arg": "order_id" }
            },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": "command"
            },
            "effects": [
                {
                    "start_process": {
                        "process": "checkout",
                        "process_key": { "arg": "order_id" },
                        "input": {
                            "order_id": { "arg": "order_id" }
                        },
                        "idempotency_key": { "argument": "request_id" }
                    }
                },
                {
                    "signal_process": {
                        "process": "approval",
                        "signal": "approval_decision",
                        "correlate": {
                            "order_id": { "arg": "order_id" }
                        },
                        "payload": {
                            "decision": { "arg": "decision" },
                            "optional_note": { "literal": null }
                        },
                        "idempotency_key": { "argument": "request_id" }
                    }
                }
            ]
        }]
    }))
    .expect("command metadata deserializes")
}

fn effect_contracts() -> ProcessEffectContractCatalog {
    let mut approval_payload = contract([("decision", ValueScalar::String)]);
    approval_payload.roots.insert(
        "optional_note".to_owned(),
        ValueContractField {
            required: false,
            type_ref: TypeRef {
                nullable: true,
                value_type: ValueType::Scalar {
                    scalar: ValueScalar::String,
                },
            },
        },
    );
    ProcessEffectContractCatalog {
        sources: BTreeMap::from([(
            "default".to_owned(),
            BTreeMap::from([
                (
                    "approval".to_owned(),
                    ProcessEffectContract {
                        process_name: "approval".to_owned(),
                        current_revision: "approval-r1".to_owned(),
                        start_policy: ProcessStartPolicy::Enabled,
                        start_input: contract([]),
                        process_key: None,
                        signals: BTreeMap::from([(
                            "approval_decision".to_owned(),
                            ProcessSignalEffectContract {
                                signal_name: "approval_decision".to_owned(),
                                contract_revision: "approval-r1".to_owned(),
                                correlation: contract([("order_id", ValueScalar::Uuid)]),
                                payload: approval_payload,
                                compatible_revisions: BTreeSet::from([
                                    "approval-r1".to_owned(),
                                    "approval-r0".to_owned(),
                                ]),
                            },
                        )]),
                    },
                ),
                (
                    "checkout".to_owned(),
                    ProcessEffectContract {
                        process_name: "checkout".to_owned(),
                        current_revision: "checkout-r4".to_owned(),
                        start_policy: ProcessStartPolicy::RejectRetired,
                        start_input: contract([("order_id", ValueScalar::Uuid)]),
                        process_key: Some(TypeRef {
                            nullable: false,
                            value_type: ValueType::Scalar {
                                scalar: ValueScalar::Uuid,
                            },
                        }),
                        signals: BTreeMap::new(),
                    },
                ),
            ]),
        )]),
    }
}

fn compiled_commands() -> donat_schema::CompiledCommandCatalog {
    compile_command_catalog(
        &command_metadata(),
        &HashMap::from([("default".to_owned(), Catalog::default())]),
        &compile_catalog(&[], &[]).expect("empty Rules catalog compiles"),
        true,
    )
    .expect("pre-process Commands compile")
}

#[test]
fn process_effect_contract_source_is_neutral_and_object_safe() {
    let catalog = effect_contracts();
    let source: &dyn ProcessEffectContractSource = &catalog;
    let contract = source
        .process_effect_contract("default", "checkout")
        .expect("source-qualified process exists");

    assert_eq!(contract.current_revision, "checkout-r4");
    assert_eq!(contract.start_policy, ProcessStartPolicy::RejectRetired);
    assert!(
        source
            .process_effect_contract("other", "checkout")
            .is_none(),
        "a process name never crosses its database source"
    );
}

#[test]
fn process_effect_finalization_pins_targets_without_changing_command_fingerprint() {
    let commands = compiled_commands();
    let before = commands
        .source("default")
        .unwrap()
        .command("kickoff")
        .unwrap()
        .descriptor()
        .definition_fingerprint
        .clone();

    let finalized =
        finalize_command_effects(commands.clone(), &effect_contracts()).expect("effects finalize");
    let command = finalized
        .source("default")
        .unwrap()
        .command("kickoff")
        .unwrap();

    assert_eq!(command.command.descriptor().definition_fingerprint, before);
    assert_eq!(command.effects.len(), 2);
    match &command.effects[0] {
        FinalizedCommandEffect::Start(effect) => {
            assert_eq!(effect.source, "default");
            assert_eq!(effect.process_name, "checkout");
            assert_eq!(effect.process_revision, "checkout-r4");
            assert_eq!(effect.start_policy, ProcessStartPolicy::RejectRetired);
            assert_eq!(effect.effect_position, 0);
        }
        other => panic!("expected a finalized start effect, got {other:?}"),
    }
    match &command.effects[1] {
        FinalizedCommandEffect::Signal(effect) => {
            assert_eq!(effect.source, "default");
            assert_eq!(effect.process_name, "approval");
            assert_eq!(effect.process_revision, "approval-r1");
            assert_eq!(effect.signal_name, "approval_decision");
            assert_eq!(
                effect.compatible_revisions,
                BTreeSet::from(["approval-r0".to_owned(), "approval-r1".to_owned()])
            );
            assert_eq!(effect.effect_position, 1);
        }
        other => panic!("expected a finalized signal effect, got {other:?}"),
    }
}

#[test]
fn process_effect_finalization_rejects_unknown_signal_and_incompatible_payload() {
    let commands = compiled_commands();
    let mut unknown = effect_contracts();
    unknown
        .sources
        .get_mut("default")
        .unwrap()
        .get_mut("approval")
        .unwrap()
        .signals
        .clear();
    let error = finalize_command_effects(commands.clone(), &unknown)
        .expect_err("an undeclared process signal must fail closed");
    assert_eq!(error.path, "commands[0].effects[1].signal_process.signal");
    assert_eq!(
        error.message,
        "process 'default.approval' has no signal 'approval_decision'"
    );

    let mut incompatible = effect_contracts();
    incompatible
        .sources
        .get_mut("default")
        .unwrap()
        .get_mut("approval")
        .unwrap()
        .signals
        .get_mut("approval_decision")
        .unwrap()
        .payload
        .roots
        .insert("decision".to_owned(), required(ValueScalar::Boolean));
    let error = finalize_command_effects(commands, &incompatible)
        .expect_err("a command payload cannot be widened into the process contract");
    assert_eq!(
        error.path,
        "commands[0].effects[1].signal_process.payload.decision"
    );
    assert_eq!(
        error.message,
        "command effect value of type String! is not assignable to Boolean!"
    );
}

#[test]
fn serving_schema_consumes_the_exact_finalized_effect_snapshot() {
    let metadata = command_metadata();
    let catalogs = HashMap::from([("default".to_owned(), Catalog::default())]);
    let rules = compile_catalog(&[], &[]).expect("empty Rules catalog compiles");
    let contracts = effect_contracts();
    let commands = compile_command_catalog(&metadata, &catalogs, &rules, true)
        .expect("pre-process Commands compile");
    let before = commands
        .source("default")
        .unwrap()
        .command("kickoff")
        .unwrap()
        .descriptor()
        .definition_fingerprint
        .clone();
    let finalized =
        finalize_command_effects(commands, &contracts).expect("process effects finalize");

    let schema = CompiledMultiSourceSchema::compile_with_command_catalog_and_process_effects(
        &metadata, &catalogs, &rules, &finalized, &contracts, true,
    )
    .expect("serving schema accepts the co-compiled effect snapshot");

    assert_eq!(
        schema
            .command_catalog()
            .source("default")
            .unwrap()
            .command("kickoff")
            .unwrap()
            .descriptor()
            .definition_fingerprint,
        before
    );

    let mut stale = finalized;
    stale
        .sources
        .get_mut("default")
        .unwrap()
        .commands
        .get_mut("kickoff")
        .unwrap()
        .effects
        .pop();
    let error = CompiledMultiSourceSchema::compile_with_command_catalog_and_process_effects(
        &metadata, &catalogs, &rules, &stale, &contracts, true,
    )
    .expect_err("a stale finalized effect list must fail closed");
    assert_eq!(error.path, "commands[0].effects");
    assert_eq!(
        error.message,
        "finalized command effects do not match the process contract snapshot"
    );
}
