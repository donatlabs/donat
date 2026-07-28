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
            "capacity": {
                "max_in_flight": 8,
                "rate_limit": { "permits": 20, "per": "1s", "burst": 8 },
                "serialize_by": { "input": "order_id" }
            }
        }]
    })
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
            "name": "create_checkout_session",
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
