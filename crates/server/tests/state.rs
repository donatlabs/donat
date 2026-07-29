use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_metadata::Metadata;
use donat_server::{
    migrate::check_consistency,
    state::{ConnectorStartupError, validate_connector_metadata, validate_connector_startup},
};
use serde_json::{Value as Json, json};

static NEXT_METADATA_DIR: AtomicUsize = AtomicUsize::new(0);

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
        rendered.contains("network_policy must be public_only"),
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
