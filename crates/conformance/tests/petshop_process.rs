//! The checked-in Petshop example executed end to end over HTTP.
//!
//! These cases run the real engine binary against the example's own metadata
//! and migrations, with the mock providers answered by a local HTTP stub. A
//! shopper calls the module's entry-point Command through `/v1/graphql`, and
//! the durable Process carries the order the rest of the way: quote, tax,
//! checkout, and provider authorization.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use donat_conformance::provider_stub::{self, ProviderStub, ScriptedResponse};
use donat_conformance::{Suite, apply_sql_migration_dir};
use postgres::NoTls;
use serde_json::{Value as Json, json};

const CUSTOMER: &str = "customer-1";
const TAX_QUOTE_PATH: &str = "/v1/tax-quotes";
const AUTHORIZE_PATH: &str = "/v1/payment-authorizations";

fn petshop_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop")
}

fn start_store(stub: &ProviderStub, name: &str) -> donat_conformance::Running {
    let root = petshop_root();
    let metadata = donat_metadata::load_metadata_dir(&root.join("metadata"))
        .expect("the checked-in Petshop metadata loads");
    let running = Suite::new(name)
        .initial_metadata(metadata)
        .with_migrations()
        .env("RUST_LOG", "donat=debug,tower_http=debug")
        .env("PETSHOP_PAYMENT_BASE_URL", stub.base_url())
        .env("PETSHOP_PAYMENT_API_TOKEN", "petshop-test-payment")
        .env("PETSHOP_TAX_BASE_URL", stub.base_url())
        .env("PETSHOP_TAX_API_TOKEN", "petshop-test-tax")
        .env("DONAT_MOCK_CARRIER_BASE_URL", stub.base_url())
        .env("DONAT_MOCK_CARRIER_TOKEN", "petshop-test-carrier")
        .env("PETSHOP_NOTIFICATION_BASE_URL", stub.base_url())
        .env(
            "PETSHOP_NOTIFICATION_API_TOKEN",
            "petshop-test-notification",
        )
        .env("PETSHOP_PAYOUT_BASE_URL", stub.base_url())
        .env("PETSHOP_PAYOUT_API_TOKEN", "petshop-test-payout")
        .start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations"))
        .expect("the example's own migrations apply");
    running
}

/// One open cart holding a single in-stock variant.
fn seed_cart(database_url: &str) -> i64 {
    let mut client =
        postgres::Client::connect(database_url, NoTls).expect("connect to the Petshop database");
    let cart_id: i64 = client
        .query_one(
            "INSERT INTO cart (customer_id) VALUES ($1) RETURNING id",
            &[&CUSTOMER],
        )
        .expect("seed one open cart")
        .get(0);
    client
        .execute(
            "INSERT INTO cart_line (cart_id, variant_id, quantity) VALUES ($1, 2, 1)",
            &[&cart_id],
        )
        .expect("seed one cart line for an active, in-stock variant");
    cart_id
}

fn script_providers(stub: &ProviderStub) {
    // The tax provider echoes the quote it was asked about, because the Process
    // asserts the answer belongs to the checkout quote it sent.
    stub.set_default(
        TAX_QUOTE_PATH,
        ScriptedResponse::ok(json!({
            "tax_quote_id": "tax_quote_petshop_1",
            "checkout_quote_id": "$request:/checkout_quote_id",
            "tax_minor": 160,
            "tax_code": "US-STANDARD",
            "currency": "USD"
        })),
    );
    stub.set_default(
        AUTHORIZE_PATH,
        ScriptedResponse::ok(json!({
            "provider_event_id": "evt_petshop_authorize_1",
            "authorization_id": "auth_petshop_1",
            "provider_reference": "ref_petshop_1",
            "status": "authorized",
            "normalized_payload": { "gateway": "mock", "captured": false }
        })),
    );
}

fn start_checkout(
    suite: &donat_conformance::Running,
    cart_id: i64,
    request_id: &str,
) -> (u16, Json) {
    suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ start_checkout(cart_id: {cart_id}, request_id: \"{request_id}\") {{ cart_id owner_user_id }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "customer".to_owned()),
            ("X-Donat-User-Id".to_owned(), CUSTOMER.to_owned()),
        ],
    )
}

/// Durable evidence for a Process that did not finish the way the case expects.
fn process_diagnostics(client: &mut postgres::Client) -> String {
    let rows = client
        .query(
            "
            SELECT process_name, status, current_state,
                   coalesce(terminal_output_json::text, 'null')
            FROM donat.process_instances
            WHERE source_name = 'default'
            ORDER BY created_at
            ",
            &[],
        )
        .expect("read durable Process instances");
    let jobs = client
        .query(
            "
            SELECT status, attempts, coalesce(last_error_json::text, 'null')
            FROM donat.process_activity_jobs
            WHERE source_name = 'default'
            ORDER BY id
            ",
            &[],
        )
        .expect("read the durable activity journal");
    let mut report = String::new();
    for row in rows {
        report.push_str(&format!(
            "instance {} status={} state={} output={}; ",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, String>(3)
        ));
    }
    for job in jobs {
        report.push_str(&format!(
            "job status={} attempts={} error={}; ",
            job.get::<_, String>(0),
            job.get::<_, i32>(1),
            job.get::<_, String>(2)
        ));
    }
    report
}

fn await_terminal(client: &mut postgres::Client, process: &str) -> Json {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let row = client
            .query_opt(
                "
                SELECT status, terminal_output_json::text
                FROM donat.process_instances
                WHERE source_name = 'default' AND process_name = $1
                ",
                &[&process],
            )
            .expect("poll the durable Process instance");
        if let Some(row) = row {
            let status: String = row.get(0);
            if status != "running" {
                assert_eq!(
                    status,
                    "terminal",
                    "Process '{process}' did not complete: {}",
                    process_diagnostics(client)
                );
                let output: String = row
                    .get::<_, Option<String>>(1)
                    .expect("a terminal instance publishes its declared output");
                return serde_json::from_str(&output).expect("terminal output is valid JSON");
            }
        }
        assert!(
            Instant::now() < deadline,
            "Process '{process}' never reached a terminal state: {}",
            process_diagnostics(client)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_shopper_checks_out_and_the_process_authorizes_the_order() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    let suite = start_store(&stub, "petshop_checkout_payment");
    let cart_id = seed_cart(suite.db_url());

    let (status, body) = start_checkout(&suite, cart_id, "550e8400-e29b-41d4-a716-446655440900");
    assert_eq!(status, 200, "start_checkout status: {body}");
    assert_eq!(
        body.pointer("/data/start_checkout/owner_user_id")
            .and_then(Json::as_str),
        Some(CUSTOMER),
        "the entry-point Command answers its own admitted request: {body}"
    );

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");
    let output = await_terminal(&mut client, "checkout_payment");

    assert_eq!(
        output.pointer("/payment_status"),
        Some(&json!("authorized")),
        "the checkout Process reaches its authorized outcome"
    );

    // The durable outcome is domain state, not just a Process row: the order
    // exists, its payment is authorized, and the cart is closed.
    let authorized = client
        .query_one(
            "
            SELECT count(*)
            FROM orders
            JOIN payment ON payment.order_id = orders.id
            WHERE orders.customer_id = $1 AND payment.status = 'authorized'
            ",
            &[&CUSTOMER],
        )
        .expect("the Process committed one order with an authorized payment");
    assert_eq!(
        authorized.get::<_, i64>(0),
        1,
        "the durable outcome is domain state, not only a Process row"
    );

    // Both providers were reached exactly once, with the compiled inputs.
    assert_eq!(stub.count_for(TAX_QUOTE_PATH), 1, "one tax quote");
    assert_eq!(stub.count_for(AUTHORIZE_PATH), 1, "one authorization");
    let authorize = &stub.calls_for(AUTHORIZE_PATH)[0];
    assert_eq!(
        authorize.body.pointer("/currency").and_then(Json::as_str),
        Some("USD")
    );
    assert!(
        authorize
            .body
            .pointer("/amount_minor")
            .and_then(Json::as_i64)
            .is_some_and(|amount| amount > 0),
        "the provider receives the order total: {}",
        authorize.body
    );
    assert_eq!(
        authorize.header("authorization"),
        Some("petshop-test-payment"),
        "the fixed-origin request carries its resolved credential"
    );
}
