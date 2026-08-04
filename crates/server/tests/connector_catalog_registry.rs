use std::sync::Arc;

use donat_connector_abi::OperationId;
use donat_connector_catalog::OperationEffect;
use donat_ir::{ValueScalar, ValueType};
use donat_server::connectors::ConnectorRegistry;
use serde_json::json;

fn metadata_with_sources(sources: serde_json::Value) -> donat_metadata::Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": sources,
        "connectors": [{
            "name": "mock_tax",
            "module": "http",
            "config": {
                "endpoint_identity": "mock_tax_v1",
                "credential_identity": "mock_tax_fixture",
                "base_url": "https://mock-tax.example.test"
            },
            "operations": [{
                "name": "quote_order",
                "version": "v1",
                "method": "POST",
                "path": "/v1/tax-quotes",
                "input_contract": {
                    "amount_minor": "bigint!",
                    "currency": "string!"
                },
                "body": {
                    "amount_minor": { "input": "amount_minor" },
                    "currency": { "input": "currency" }
                },
                "success_statuses": [200],
                "response": {
                    "approved": {
                        "json_pointer": "/approved",
                        "type": "bool!"
                    }
                },
                "effect": "read_only",
                "bounds": {
                    "deadline_ms": 2000,
                    "maximum_calls": 1,
                    "maximum_pages": 1,
                    "maximum_items": 1,
                    "maximum_aggregate_request_bytes": 4096,
                    "maximum_aggregate_response_bytes": 4096,
                    "maximum_output_canonical_bytes": 4096,
                    "maximum_redirects": 0,
                    "maximum_json_depth": 8,
                    "maximum_json_nodes": 128
                },
                "error_map": {
                    "rules": [],
                    "fallback": {
                        "class": "permanent",
                        "code": "tax_provider_error"
                    }
                },
                "capacity": {
                    "max_in_flight": 8,
                    "rate_limit": {
                        "permits": 20,
                        "per": "1s",
                        "burst": 8
                    },
                    "serialize_by": { "input": "amount_minor" }
                }
            }]
        }]
    }))
    .expect("catalog registry fixture is valid metadata")
}

fn postgres_source(name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "kind": "postgres",
        "configuration": {}
    })
}

#[test]
fn registry_returns_catalog_owned_operation_only_for_its_bound_source() {
    let metadata = metadata_with_sources(json!([postgres_source("default")]));
    let registry = ConnectorRegistry::build(&metadata).expect("one-source registry compiles");
    let operation = OperationId::parse("quote_order").expect("typed operation ID");

    let spec = registry
        .operation_spec("default", "mock_tax", operation)
        .expect("an executable operation is published for its Postgres source");
    let first_handle = registry
        .operation_spec_handle("default", "mock_tax", operation)
        .expect("the executable operation has a shared immutable handle");
    let second_handle = registry
        .operation_spec_handle("default", "mock_tax", operation)
        .expect("repeated lookup resolves the same immutable snapshot");

    assert!(spec.operation == operation);
    assert!(
        Arc::ptr_eq(&first_handle, &second_handle),
        "compiled dependencies share the registry snapshot without cloning it"
    );
    assert!(
        std::ptr::eq(spec, first_handle.as_ref()),
        "borrowed and owned lookup APIs resolve the exact same snapshot"
    );
    assert!(matches!(spec.effect, OperationEffect::ReadOnly));
    assert!(matches!(
        spec.input.roots["amount_minor"].type_ref.value_type,
        ValueType::Scalar {
            scalar: ValueScalar::Int64
        }
    ));
    assert!(spec.output.roots["approved"].required);
    assert!(
        registry
            .operation_spec("secondary", "mock_tax", operation)
            .is_none(),
        "a source-local compiler cannot resolve another source's connector"
    );
    assert!(
        registry
            .operation_spec_handle("secondary", "mock_tax", operation)
            .is_none(),
        "shared handles remain source-local too"
    );
    assert!(
        registry
            .operation_spec("default", "other_instance", operation)
            .is_none(),
        "lookup is also bound to the deployment connector instance"
    );
    assert!(
        registry
            .operation_spec(
                "default",
                "mock_tax",
                OperationId::parse("other_operation").expect("typed absent operation ID"),
            )
            .is_none(),
        "lookup never widens an absent operation to another catalog entry"
    );
}

#[test]
fn registry_rejects_implicit_connector_binding_when_postgres_source_is_ambiguous() {
    let metadata = metadata_with_sources(json!([
        postgres_source("default"),
        postgres_source("secondary")
    ]));

    let error = match ConnectorRegistry::build(&metadata) {
        Ok(_) => panic!("implicit source binding must fail closed with two Postgres sources"),
        Err(error) => error,
    };

    assert!(
        error.to_string().contains("exactly one Postgres source"),
        "startup explains the closed Phase-1 binding rule: {error}"
    );
}

#[test]
fn registry_rejects_implicit_connector_binding_without_a_postgres_source() {
    let metadata = metadata_with_sources(json!([]));

    let error = match ConnectorRegistry::build(&metadata) {
        Ok(_) => panic!("implicit source binding must fail closed without a Postgres source"),
        Err(error) => error,
    };

    assert!(
        error
            .to_string()
            .contains("exactly one Postgres source; found 0"),
        "startup reports the missing real source without selecting an ambient database: {error}"
    );
}

#[test]
fn registry_does_not_publish_inventory_only_http_operations() {
    let mut metadata = metadata_with_sources(json!([postgres_source("default")]));
    match &mut metadata.connectors[0].operations[0].profile {
        donat_metadata::ConnectorOperationProfile::Http(operation) => {
            assert!(operation.effect.is_some(), "fixture starts executable");
            operation.effect = None;
        }
        donat_metadata::ConnectorOperationProfile::Undeclared(_) => {
            panic!("fixture operation is HTTP")
        }
    }

    let registry =
        ConnectorRegistry::build(&metadata).expect("inventory metadata remains deployable");
    let operation = OperationId::parse("quote_order").expect("typed operation ID");

    assert!(
        registry
            .operation_spec("default", "mock_tax", operation)
            .is_none(),
        "an operation without the complete executable effect contract stays inventory-only"
    );
}
