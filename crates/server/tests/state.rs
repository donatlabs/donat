use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_catalog::Catalog;
use donat_ir::ProcessStartPolicy;
use donat_metadata::{Metadata, ProcessLifecycle};
use donat_schema::FinalizedCommandEffect;
use donat_server::{
    connectors::ConnectorRegistry,
    migrate::check_consistency,
    state::{
        ConnectorStartupError, compile_pure_engine_candidate, validate_connector_metadata,
        validate_connector_startup,
    },
};
use serde_json::{Value as Json, json};

static NEXT_METADATA_DIR: AtomicUsize = AtomicUsize::new(0);

fn process_candidate_metadata() -> Metadata {
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
            "name": "begin_checkout",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "request_id", "type": "uuid!" },
                { "name": "order_id", "type": "uuid!" }
            ],
            "steps": [],
            "result": { "order_id": { "arg": "order_id" } },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": "command"
            },
            "effects": [{
                "start_process": {
                    "process": "checkout",
                    "input": { "order_id": { "arg": "order_id" } },
                    "idempotency_key": { "argument": "request_id" }
                }
            }]
        }],
        "processes": [{
            "name": "checkout",
            "kind": "process",
            "version": 1,
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "input": [{ "name": "order_id", "type": "uuid!" }],
            "output": [{ "name": "status", "type": "string!" }],
            "start_at": "done",
            "states": [{
                "id": "done",
                "output": {
                    "values": { "status": { "literal": "ready" } }
                }
            }]
        }]
    }))
    .expect("pure candidate metadata deserializes")
}

#[test]
fn process_candidate_stages_are_pure_and_pin_one_snapshot() {
    let metadata = process_candidate_metadata();
    let catalogs = HashMap::from([("default".to_owned(), Catalog::default())]);
    let connectors = ConnectorRegistry::build(&metadata).expect("empty registry compiles");

    let candidate = compile_pure_engine_candidate(&metadata, &catalogs, &connectors, true)
        .expect("all seven candidate stages compile");

    assert_eq!(candidate.process_catalog.len(), 1);
    assert!(candidate.rule_catalog().rule("ambient_rule").is_none());
    let process = candidate
        .process_catalog
        .source("default")
        .unwrap()
        .process("checkout")
        .unwrap();
    let finalized = candidate
        .finalized_command_catalog
        .source("default")
        .unwrap()
        .command("begin_checkout")
        .unwrap();
    let FinalizedCommandEffect::Start(effect) = &finalized.effects[0] else {
        panic!("begin_checkout must retain one typed start effect");
    };
    assert_eq!(effect.process_revision, process.revision_fingerprint);
    assert_eq!(
        finalized.command.descriptor().definition_fingerprint,
        candidate
            .command_catalog
            .source("default")
            .unwrap()
            .command("begin_checkout")
            .unwrap()
            .descriptor()
            .definition_fingerprint
    );
    assert!(
        candidate
            .compiled
            .as_deref()
            .expect("serving schema is compiled")
            .command_catalog()
            .source("default")
            .unwrap()
            .command("begin_checkout")
            .is_some()
    );
}

#[test]
fn process_effect_catalog_retains_explicit_retired_policy() {
    let mut metadata = process_candidate_metadata();
    metadata.processes[0].lifecycle = ProcessLifecycle::Retired;
    let catalogs = HashMap::from([("default".to_owned(), Catalog::default())]);
    let connectors = ConnectorRegistry::build(&metadata).expect("empty registry compiles");

    let candidate = compile_pure_engine_candidate(&metadata, &catalogs, &connectors, true)
        .expect("retired definitions remain resolvable");
    let contract = &candidate.process_effects.sources["default"]["checkout"];
    assert_eq!(contract.start_policy, ProcessStartPolicy::RejectRetired);
    let finalized = candidate
        .finalized_command_catalog
        .source("default")
        .unwrap()
        .command("begin_checkout")
        .unwrap();
    let FinalizedCommandEffect::Start(effect) = &finalized.effects[0] else {
        panic!("begin_checkout must retain one typed start effect");
    };
    assert_eq!(effect.start_policy, ProcessStartPolicy::RejectRetired);
}

fn metadata(connectors: Json) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [],
        "connectors": connectors,
    }))
    .expect("connector metadata deserializes")
}

fn write_metadata_dir(connectors: Json) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "donat-connector-metadata-{}-{}",
        std::process::id(),
        NEXT_METADATA_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("databases")).expect("metadata directory creates");
    std::fs::write(dir.join("version.yaml"), "version: 3\n").expect("version writes");
    std::fs::write(dir.join("databases/databases.yaml"), "[]\n")
        .expect("empty databases section writes");
    std::fs::write(
        dir.join("connectors.yaml"),
        serde_yaml::to_string(&connectors).expect("connectors serialize"),
    )
    .expect("connectors section writes");
    dir
}

fn valid_http_connector() -> Json {
    json!({
        "name": "logistics_api",
        "module": "http",
        "config": {
            "endpoint_identity": "logistics_prod_eu_2026_07",
            "credential_identity": "logistics_primary",
            "base_url": "https://logistics.example.test"
        },
        "operations": [{
            "name": "create_shipment",
            "version": "v1",
            "method": "POST",
            "path": "/v1/shipments/{input.order_id}",
            "body": { "order_id": { "input": "order_id" } },
            "success_statuses": [200],
            "idempotency": { "header": "Idempotency-Key" },
            "capacity": {
                "max_in_flight": 8,
                "rate_limit": { "permits": 20, "per": "1s", "burst": 8 },
                "serialize_by": { "input": "order_id" }
            }
        }]
    })
}

#[tokio::test]
async fn consistency_rejects_static_http_operation_profile_errors_without_resolving_environment_values()
 {
    let mut unsupported_method = valid_http_connector();
    unsupported_method["name"] = json!("unsupported_method");
    unsupported_method["operations"][0]["method"] = json!("TRACE");

    let mut invalid_path = valid_http_connector();
    invalid_path["name"] = json!("invalid_path");
    invalid_path["operations"][0]["path"] = json!("https://attacker.invalid/override");

    let mut invalid_header = valid_http_connector();
    invalid_header["name"] = json!("invalid_header");
    invalid_header["operations"][0]["headers"] =
        json!([{ "name": "Bad Header", "value": "fixed" }]);

    let mut invalid_status = valid_http_connector();
    invalid_status["name"] = json!("invalid_status");
    invalid_status["operations"][0]["success_statuses"] = json!([999]);

    let mut missing_version = valid_http_connector();
    missing_version["name"] = json!("missing_version");
    missing_version["operations"][0]
        .as_object_mut()
        .expect("operation is a JSON object")
        .remove("version");

    let mut missing_profile = valid_http_connector();
    missing_profile["name"] = json!("missing_profile");
    missing_profile["operations"] = json!([{
        "name": "create_shipment",
        "capacity": {
            "max_in_flight": 8,
            "rate_limit": { "permits": 20, "per": "1s", "burst": 8 }
        }
    }]);

    let mut invalid_rate_period = valid_http_connector();
    invalid_rate_period["name"] = json!("invalid_rate_period");
    invalid_rate_period["operations"][0]["capacity"]["rate_limit"]["per"] = json!("forever");

    let mut invalid = json!([
        unsupported_method,
        invalid_path,
        invalid_header,
        invalid_status,
        missing_version,
        missing_profile,
        invalid_rate_period,
    ]);
    invalid[0]["config"]["base_url"] =
        json!({ "value_from_env": "DONAT_TEST_UNRESOLVED_BASE_URL" });
    invalid[0]["config"]["headers"] = json!([{
        "name": "Authorization",
        "value_from_env": "DONAT_TEST_UNRESOLVED_CREDENTIAL"
    }]);

    let dir = write_metadata_dir(invalid);
    let result = check_consistency("postgres://unreachable", &dir)
        .await
        .expect(
            "HTTP profile errors are static metadata errors, not environment or database errors",
        );
    let _ = std::fs::remove_dir_all(&dir);
    let rendered = result.join("\n");

    assert!(
        rendered.contains("method must be one of GET, POST, PUT, PATCH, or DELETE"),
        "{rendered}"
    );
    assert!(
        rendered.contains("path must be a static absolute path without authority"),
        "{rendered}"
    );
    assert!(
        rendered.contains("operation header name is invalid"),
        "{rendered}"
    );
    assert!(
        rendered.contains("success statuses must be 2xx"),
        "{rendered}"
    );
    assert!(
        rendered.contains("connector operation version is required"),
        "{rendered}"
    );
    assert!(
        rendered.contains("http connector operations must declare an HTTP operation profile"),
        "{rendered}"
    );
    assert!(
        rendered.contains("connector operation capacity is invalid"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("environment value is unavailable"),
        "deploy-time validation must not resolve credentials: {rendered}"
    );
}

#[tokio::test]
async fn consistency_rejects_static_http_config_errors_without_resolving_environment_values() {
    let mut private_network = valid_http_connector();
    private_network["name"] = json!("private_network");
    private_network["config"]["base_url"] =
        json!({ "value_from_env": "DONAT_TEST_UNRESOLVED_HTTP_BASE_URL" });
    private_network["config"]["headers"] = json!([{
        "name": "Authorization",
        "value_from_env": "DONAT_TEST_UNRESOLVED_HTTP_CREDENTIAL"
    }]);
    private_network["config"]["network_policy"] = json!("private_allowed");

    let mut invalid_scheme = valid_http_connector();
    invalid_scheme["name"] = json!("invalid_scheme");
    invalid_scheme["config"]["base_url"] = json!("ftp://logistics.example.test");

    let mut invalid_userinfo = valid_http_connector();
    invalid_userinfo["name"] = json!("invalid_userinfo");
    invalid_userinfo["config"]["base_url"] =
        json!("https://username:password@logistics.example.test");

    let mut invalid_query = valid_http_connector();
    invalid_query["name"] = json!("invalid_query");
    invalid_query["config"]["base_url"] = json!("https://logistics.example.test?next=other");

    let mut invalid_fragment = valid_http_connector();
    invalid_fragment["name"] = json!("invalid_fragment");
    invalid_fragment["config"]["base_url"] = json!("https://logistics.example.test#other");

    let mut duplicate_operation = valid_http_connector();
    duplicate_operation["name"] = json!("duplicate_operation");
    let repeated = duplicate_operation["operations"][0].clone();
    duplicate_operation["operations"]
        .as_array_mut()
        .expect("operations is an array")
        .push(repeated);

    let dir = write_metadata_dir(json!([
        private_network,
        invalid_scheme,
        invalid_userinfo,
        invalid_query,
        invalid_fragment,
        duplicate_operation,
    ]));
    let problems = check_consistency("postgres://unreachable", &dir)
        .await
        .expect("static HTTP configuration errors are returned before DB or env resolution");
    let _ = std::fs::remove_dir_all(&dir);
    let rendered = problems.join("\n");

    assert!(
        rendered.contains("http connector does not accept network_policy"),
        "{rendered}"
    );
    assert!(
        rendered.contains("base_url must be an absolute HTTP(S) URL"),
        "{rendered}"
    );
    assert!(
        rendered.contains("base URL must not contain userinfo"),
        "{rendered}"
    );
    assert!(
        rendered.contains("base_url must not contain query or fragment"),
        "{rendered}"
    );
    assert!(
        rendered.contains("http connector operation is declared more than once"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("environment value is unavailable"),
        "consistency must not resolve environment values: {rendered}"
    );
}

#[test]
fn connector_startup_accepts_non_secret_identities_and_named_capacity() {
    let metadata = metadata(json!([valid_http_connector()]));

    assert!(validate_connector_metadata(&metadata).is_empty());
    validate_connector_startup(&metadata).expect("static connector needs no environment value");
}

/// A deployment that renders a document, reads a spreadsheet, or draws a code
/// declares a `local.*` instance in the same `connectors.yaml` a provider uses.
/// Connector validation must step over it: it has no module in the compiled
/// connector table, and `donat_metadata::validate_local_capabilities` refuses
/// the very `endpoint_identity`/`credential_identity` this validator requires,
/// so validating it here refused every such deployment twice over. Nothing
/// caught it because every local-capability test built its registry from a
/// `Metadata` value directly, never through the startup path a listener opens
/// behind.
#[test]
fn connector_startup_accepts_a_local_capability_instance_and_still_refuses_an_unknown_module() {
    let local = metadata(json!([{
        "name": "documents",
        "module": "local.document",
        "operations": [{ "name": "pdf.render" }]
    }]));
    assert!(
        validate_connector_metadata(&local).is_empty(),
        "a local capability instance is validated by its own validator, not this one: {:?}",
        validate_connector_metadata(&local)
    );
    validate_connector_startup(&local).expect("a local capability needs no environment value");

    // The skip is keyed on the reserved namespace, not on "the table does not
    // know it" — an ordinary typo must still be refused by name.
    let unknown = metadata(json!([{
        "name": "documents",
        "module": "locally.document",
        "operations": [{ "name": "pdf.render" }]
    }]));
    let rendered = validate_connector_metadata(&unknown)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("unknown connector module `locally.document`"),
        "{rendered}"
    );
}

#[test]
fn connector_startup_rejects_missing_env_without_revealing_a_value() {
    let metadata = metadata(json!([{
        "name": "stripe",
        "module": "stripe",
        "config": {
            "endpoint_identity": "stripe_api_2025_06_30",
            "credential_identity": "stripe_primary",
            "secret_key": { "value_from_env": "DONAT_CONNECTOR_TEST_MISSING_SECRET" },
            "webhook_secret": { "value_from_env": "DONAT_CONNECTOR_TEST_MISSING_WEBHOOK_SECRET" },
            "api_version": "2025-06-30.basil"
        },
        "operations": [{
            "name": "checkout.create_session",
            "capacity": {
                "max_in_flight": 16,
                "rate_limit": { "permits": 80, "per": "1m", "burst": 20 }
            }
        }]
    }]));

    let error = validate_connector_startup(&metadata)
        .expect_err("a missing required value prevents the connector from starting");
    match &error {
        ConnectorStartupError::MissingEnvironment { instance, variable } => {
            assert_eq!(instance, "stripe");
            assert_eq!(variable, "DONAT_CONNECTOR_TEST_MISSING_SECRET");
        }
        other => panic!("expected a startup configuration error, got {other:?}"),
    }
    let rendered = error.to_string();
    assert!(rendered.contains("DONAT_CONNECTOR_TEST_MISSING_SECRET"));
    assert!(
        !rendered.contains("sk_live_should_never_be_serialized"),
        "startup failures are configuration errors, not activity failures with secret values"
    );
}

#[tokio::test]
async fn connector_static_configuration_errors_are_reported_before_database_validation() {
    let mut duplicate = valid_http_connector();
    duplicate["config"]["base_url"] = json!("https://user:password@logistics.example.test");
    let mut invalid_environment_variable = valid_http_connector();
    invalid_environment_variable["name"] = json!("invalid_environment_variable");
    invalid_environment_variable["config"]["base_url"] =
        json!({ "value_from_env": "INVALID-CONNECTOR-ENV" });
    let invalid = json!([
        valid_http_connector(),
        duplicate,
        invalid_environment_variable,
        {
            "name": "unknown",
            "module": "shell-command",
            "config": {
                "endpoint_identity": "unknown_api",
                "credential_identity": "unknown_primary"
            },
            "operations": [{ "name": "run" }]
        },
        {
            "name": "missing_config",
            "module": "http",
            "config": {},
            "operations": [{ "name": "send" }]
        }
    ]);
    let metadata = metadata(invalid.clone());
    let errors = validate_connector_metadata(&metadata);
    let rendered = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("duplicate connector instance name `logistics_api`"));
    assert!(rendered.contains("unknown connector module `shell-command`"));
    assert!(rendered.contains("endpoint_identity is required"));
    assert!(rendered.contains("credential_identity is required"));
    assert!(rendered.contains("base URL must not contain userinfo"));
    assert!(rendered.contains(
        "value_from_env `INVALID-CONNECTOR-ENV` is not a valid environment variable name"
    ));
    assert!(rendered.contains("capacity is required"));

    let dir = write_metadata_dir(invalid);
    let result = check_consistency("postgres://unreachable", &dir)
        .await
        .expect("static connector errors do not require a database connection");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        result
            .iter()
            .any(|problem| problem.contains("unknown connector module `shell-command`")),
        "migrate validation reports connector configuration before attempting database validation: {result:#?}"
    );
}
