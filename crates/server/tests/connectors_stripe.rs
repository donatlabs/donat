use donat_server::connectors::{ConnectorRegistry, stripe::StripeConnector};
use serde_json::json;

fn postgres_sources() -> serde_json::Value {
    json!([{
        "name": "default",
        "kind": "postgres",
        "configuration": {}
    }])
}

#[test]
fn stripe_registry_rejects_endpoint_overrides_and_unadvertised_operations() {
    // This fails if deployment metadata can turn the narrow module into a
    // caller-configured HTTP client or enable an operation absent from the
    // compiled Stripe Checkout contract.
    const API_KEY_ENV: &str = "DONAT_STRIPE_TEST_API_KEY";
    const WEBHOOK_SECRET_ENV: &str = "DONAT_STRIPE_TEST_WEBHOOK_SECRET";
    unsafe {
        std::env::set_var(API_KEY_ENV, "sk_test_registry_key");
        std::env::set_var(WEBHOOK_SECRET_ENV, "whsec_registry_secret");
    }
    let metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": postgres_sources(),
        "connectors": [{
            "name": "payments",
            "module": "stripe",
            "config": {
                "endpoint_identity": "stripe_live_api_2026_07",
                "credential_identity": "stripe_primary",
                "base_url": "https://attacker.example.test",
                "secret_key": { "value_from_env": API_KEY_ENV },
                "webhook_secret": { "value_from_env": WEBHOOK_SECRET_ENV },
                "api_version": "2026-07-27"
            },
            "operations": [{
                "name": "customers.create",
                "capacity": {
                    "max_in_flight": 1,
                    "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
                }
            }]
        }]
    }))
    .expect("Stripe fixture metadata deserializes before compiled validation");

    let error = match ConnectorRegistry::build(&metadata) {
        Ok(_) => panic!(
            "a narrow Stripe instance cannot override its endpoint or enable an arbitrary operation"
        ),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("stripe connector does not accept base_url"),
        "endpoint rejection is deterministic and contains no resolved secret: {error}"
    );
    assert!(
        !error.to_string().contains("sk_test_registry_key")
            && !error.to_string().contains("whsec_registry_secret"),
        "registry configuration errors retain only environment variable names, never resolved secrets: {error}"
    );

    let mut unknown_operation = metadata;
    unknown_operation.connectors[0].config.base_url = None;
    let unknown_error = match ConnectorRegistry::build(&unknown_operation) {
        Ok(_) => panic!("an arbitrary Stripe operation cannot be enabled"),
        Err(error) => error,
    };
    assert!(
        unknown_error
            .to_string()
            .contains("stripe connector operation is not compiled into this binary"),
        "the Stripe registry exposes only Checkout Session creation: {unknown_error}"
    );
    let _ = core::mem::size_of::<StripeConnector>();
    unsafe {
        std::env::remove_var(API_KEY_ENV);
        std::env::remove_var(WEBHOOK_SECRET_ENV);
    }
}
