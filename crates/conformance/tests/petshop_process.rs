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
const REVIEWER: &str = "veterinarian-1";
const TAX_QUOTE_PATH: &str = "/v1/tax-quotes";
const AUTHORIZE_PATH: &str = "/v1/payment-authorizations";
const VOID_PATH: &str = "/v1/payment-authorizations/*/voids";
const LOOKUP_PATH: &str = "/v1/payment-operation-lookups";
const LABEL_PATH: &str = "/v1/shipments/*/labels";
const RETURN_LABEL_PATH: &str = "/v1/returns/*/labels";
const REFUND_PATH: &str = "/v1/payment-authorizations/*/refunds";
const CAPTURE_PATH: &str = "/v1/payment-authorizations/*/captures";

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

fn void_response() -> ScriptedResponse {
    ScriptedResponse::ok(json!({
        "provider_event_id": "evt_petshop_void_1",
        "void_id": "void_petshop_1",
        "provider_reference": "ref_petshop_1",
        "status": "voided",
        "normalized_payload": { "gateway": "mock", "voided": true }
    }))
}

/// The provider proving it never carried out the mutation it was asked about.
/// The lookup contract is non-null throughout, so an absence still has to
/// answer every declared field.
fn lookup_absence_response() -> ScriptedResponse {
    ScriptedResponse::ok(json!({
        "found": false,
        "terminal_absence_proven": true,
        "provider_event_id": "evt_petshop_lookup_absent",
        "provider_mutation_id": "",
        "provider_reference": "",
        "outcome": "failed",
        "amount_minor": 0,
        "currency": "USD",
        "normalized_payload": { "gateway": "mock", "found": false }
    }))
}

fn cancel_order(
    suite: &donat_conformance::Running,
    order_id: &str,
    request_id: &str,
) -> (u16, Json) {
    suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ cancel_order(order_id: \"{order_id}\", request_id: \"{request_id}\") {{ order_id order_status }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "customer".to_owned()),
            ("X-Donat-User-Id".to_owned(), CUSTOMER.to_owned()),
        ],
    )
}

fn request_cancellation(
    suite: &donat_conformance::Running,
    order_id: &str,
    request_id: &str,
) -> (u16, Json) {
    suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ request_authorized_order_cancellation(order_id: \"{order_id}\", reason: \"changed mind\", request_id: \"{request_id}\") {{ order_id }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "customer".to_owned()),
            ("X-Donat-User-Id".to_owned(), CUSTOMER.to_owned()),
        ],
    )
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
    let signals = client
        .query(
            "SELECT process_name, signal_name, status, coalesce(correlation_json::text,'null')
             FROM donat.process_signal_requests WHERE source_name = 'default' ORDER BY id",
            &[],
        )
        .unwrap_or_default();
    let fanout = client
        .query(
            "SELECT state_name, ordinal, status, coalesce(failure_json::text, 'null')
             FROM donat.process_fanout_items WHERE source_name = 'default'
             ORDER BY state_name, ordinal",
            &[],
        )
        .unwrap_or_default();
    let stuck_events = client
        .query(
            "SELECT kind, status, attempts, left(payload_json::text, 400)
             FROM donat.process_events
             WHERE source_name = 'default' AND status <> 'consumed'
             ORDER BY id",
            &[],
        )
        .unwrap_or_default();
    let mut report = String::new();
    for item in fanout {
        report.push_str(&format!(
            "fanout {}[{}] status={} failure={}; ",
            item.get::<_, String>(0),
            item.get::<_, i32>(1),
            item.get::<_, String>(2),
            item.get::<_, String>(3)
        ));
    }
    for event in stuck_events {
        report.push_str(&format!(
            "event {} status={} attempts={} payload={}; ",
            event.get::<_, String>(0),
            event.get::<_, String>(1),
            event.get::<_, i32>(2),
            event.get::<_, String>(3)
        ));
    }
    for signal in signals {
        report.push_str(&format!(
            "signal {}.{} status={} correlate={}; ",
            signal.get::<_, String>(0),
            signal.get::<_, String>(1),
            signal.get::<_, String>(2),
            signal.get::<_, String>(3)
        ));
    }
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

/// The cancellation module runs on the order the checkout module just created:
/// it voids the authorization at the provider and finalizes the cancellation.
#[test]
fn a_shopper_cancels_an_authorized_order_and_the_process_voids_the_payment() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    stub.set_default(VOID_PATH, void_response());
    let suite = start_store(&stub, "petshop_order_cancellation");
    let cart_id = seed_cart(suite.db_url());

    let (status, body) = start_checkout(&suite, cart_id, "550e8400-e29b-41d4-a716-446655440910");
    assert_eq!(status, 200, "start_checkout status: {body}");
    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");
    await_terminal(&mut client, "checkout_payment");

    let order_id: String = client
        .query_one(
            "SELECT id::text FROM orders WHERE customer_id = $1",
            &[&CUSTOMER],
        )
        .expect("the checkout Process committed one order")
        .get(0);

    let (status, body) =
        request_cancellation(&suite, &order_id, "550e8400-e29b-41d4-a716-446655440911");
    assert_eq!(status, 200, "cancellation mutation status: {body}");

    let output = await_terminal(&mut client, "authorized_order_cancellation");
    assert_eq!(
        output.pointer("/order_id").and_then(Json::as_str),
        Some(order_id.as_str()),
        "the cancellation Process reports the order it cancelled"
    );

    let voided = client
        .query_one(
            "
            SELECT count(*)
            FROM payment
            WHERE order_id::text = $1 AND status = 'voided'
            ",
            &[&order_id],
        )
        .expect("the cancellation Process voided the authorization in domain state");
    assert_eq!(voided.get::<_, i64>(0), 1, "the payment is voided");

    let calls = stub.calls();
    assert!(
        calls.iter().any(|call| call.path.contains("/voids")),
        "the provider received the void: {:?}",
        calls
            .iter()
            .map(|call| call.path.clone())
            .collect::<Vec<_>>()
    );
}

/// The booking module has no provider at all: it reserves a hold, parks on a
/// typed signal, and the shopper's confirmation Command advances it.
#[test]
fn a_shopper_confirms_a_grooming_hold_and_the_process_completes() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    let suite = start_store(&stub, "petshop_grooming_booking");

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ start_grooming_booking(service_resource_id: \"550e8400-e29b-41d4-a716-446655440920\", slot_key: \"2026-08-01T10:00\", starts_at: \"2030-01-02T10:00:00Z\", hold_expires_at: \"2030-01-01T10:00:00Z\", request_id: \"550e8400-e29b-41d4-a716-446655440921\") {{ slot_key }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "customer".to_owned()),
            ("X-Donat-User-Id".to_owned(), CUSTOMER.to_owned()),
        ],
    );
    assert_eq!(status, 200, "start_grooming_booking status: {body}");

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");

    let booking_id = await_row_id(
        &mut client,
        "grooming_booking",
        "the booking Process never reserved its hold",
    );

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ confirm_booking(booking_id: \"{booking_id}\", request_id: \"550e8400-e29b-41d4-a716-446655440922\") {{ booking_id }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "customer".to_owned()),
            ("X-Donat-User-Id".to_owned(), CUSTOMER.to_owned()),
        ],
    );
    assert_eq!(status, 200, "confirm_booking status: {body}");

    let output = await_terminal(&mut client, "grooming_booking");
    assert_eq!(
        output.pointer("/booking_id").and_then(Json::as_str),
        Some(booking_id.as_str()),
        "the booking Process reports the hold it confirmed: {output}"
    );
}

/// The carrier and the payment provider answering a shipped allocation.
fn script_fulfilment_providers(stub: &ProviderStub) {
    stub.set_default(
        LABEL_PATH,
        ScriptedResponse::ok(json!({
            "shipment_id": "$request:/shipment_id",
            "shipment_key": "$request:/shipment_key",
            "carrier_shipment_reference": "carrier_ref_petshop_1",
            "tracking_number": "TRACK-PETSHOP-1",
            "label_url": "https://carrier.example/labels/petshop-1.pdf",
            "event_id": "evt_petshop_label_1",
            "outcome": "label_created"
        })),
    );
    stub.set_default(
        CAPTURE_PATH,
        ScriptedResponse::ok(json!({
            "payment_id": "$request:/payment_id",
            "shipment_id": "$request:/shipment_id",
            "amount_minor": "$request:/amount_minor",
            "provider_event_id": "evt_petshop_capture_1",
            "capture_id": "cap_petshop_1",
            "provider_reference": "ref_petshop_1",
            "status": "captured",
            "normalized_payload": { "gateway": "mock", "captured": true }
        })),
    );
}

/// Block until a Process has committed the row a later step needs, and answer
/// its id.
///
/// Every wait in this file shares one budget. These polls used to carry their
/// own, shorter one, which held under a single suite and expired under a full
/// workspace run where the engine competes with every other suite for the
/// machine.
fn await_row_id(client: &mut postgres::Client, table: &str, what: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let row = client
            .query_opt(&format!("SELECT id::text FROM {table} LIMIT 1"), &[])
            .unwrap_or_else(|error| panic!("poll {table}: {error}"));
        if let Some(row) = row {
            return row.get(0);
        }
        assert!(
            Instant::now() < deadline,
            "{what}: {}",
            process_diagnostics(client)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Block until a Process wait is receptive.
///
/// Signals are never buffered, and a signal committed before its wait became
/// receptive is deliberately auditable as `unexpected_state` rather than
/// matched late. Entering a wait is its own transition: the instance reaches
/// the state first, and the wait's timer event is inserted by the transition
/// that consumes the entry token. A case that sends a signal must wait for
/// that timer, not merely for the state name.
fn await_receptive_wait(client: &mut postgres::Client, process: &str, state: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let receptive: bool = client
            .query_one(
                "SELECT EXISTS (
                     SELECT 1
                     FROM donat.process_events event
                     JOIN donat.process_instances instance
                       ON instance.source_name = event.source_name
                      AND instance.id = event.instance_id
                     WHERE event.source_name = 'default'
                       AND event.kind = 'timer'
                       AND event.status = 'pending'
                       AND event.payload_json ->> 'wait_state' = $2
                       AND instance.process_name = $1
                       AND instance.current_state = $2
                 )",
                &[&process, &state],
            )
            .expect("poll the durable Process instance")
            .get(0);
        if receptive {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Process '{process}' never became receptive in '{state}': {}",
            process_diagnostics(client)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Carry one seeded cart all the way to a captured payment: checkout, then the
/// fulfilment module. Returns the order that a later module works on.
fn checkout_and_fulfil(
    suite: &donat_conformance::Running,
    client: &mut postgres::Client,
    request_prefix: &str,
) -> String {
    let cart_id = seed_cart(suite.db_url());
    let (status, body) = start_checkout(suite, cart_id, &format!("{request_prefix}0"));
    assert_eq!(status, 200, "start_checkout status: {body}");
    let checkout = await_terminal(client, "checkout_payment");
    let order_id = checkout
        .pointer("/order_id")
        .and_then(Json::as_str)
        .expect("checkout published its order")
        .to_owned();

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ start_order_fulfilment(order_id: \"{order_id}\", destination_region: \"northeast\", allocation_request_id: \"{request_prefix}1\") {{ order_id }} }}"
            )
        }),
        &[("X-Donat-Role".to_owned(), "fulfilment".to_owned())],
    );
    assert_eq!(status, 200, "start_order_fulfilment status: {body}");
    assert!(
        body.get("errors").is_none(),
        "start_order_fulfilment reported errors: {body}"
    );
    await_terminal(client, "partial_fulfilment");
    order_id
}

/// Fulfilment allocates the authorized order, packs it, ships it through the
/// carrier, and captures exactly the shipped value — the whole bounded fan-out
/// driven from one entry-point Command.
#[test]
fn fulfilment_allocates_ships_and_captures_the_shipped_value() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    script_fulfilment_providers(&stub);
    let suite = start_store(&stub, "petshop_partial_fulfilment");
    let cart_id = seed_cart(suite.db_url());

    let (status, body) = start_checkout(&suite, cart_id, "550e8400-e29b-41d4-a716-446655440950");
    assert_eq!(status, 200, "start_checkout status: {body}");
    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");
    let checkout = await_terminal(&mut client, "checkout_payment");
    let order_id = checkout
        .pointer("/order_id")
        .and_then(Json::as_str)
        .expect("checkout published its order")
        .to_owned();

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ start_order_fulfilment(order_id: \"{order_id}\", destination_region: \"northeast\", allocation_request_id: \"550e8400-e29b-41d4-a716-446655440951\") {{ order_id order_status }} }}"
            )
        }),
        &[("X-Donat-Role".to_owned(), "fulfilment".to_owned())],
    );
    assert_eq!(status, 200, "start_order_fulfilment status: {body}");
    assert!(
        body.get("errors").is_none(),
        "start_order_fulfilment reported errors: {body}"
    );

    let output = await_terminal(&mut client, "partial_fulfilment");
    assert_eq!(
        output.pointer("/status").and_then(Json::as_str),
        Some("partial"),
        "the fulfilment Process publishes its bounded outcome: {output}"
    );
    assert_eq!(
        output
            .pointer("/unshipped_allocations")
            .and_then(Json::as_array)
            .map(Vec::len),
        Some(0),
        "every allocation shipped: {output}"
    );
    assert_eq!(
        output
            .pointer("/shipment_outcomes/0/outcome")
            .and_then(Json::as_str),
        Some("label_created"),
        "the carrier label is recorded against the shipment: {output}"
    );

    let (shipped_value, captured): (i64, i64) = {
        let row = client
            .query_one(
                "SELECT (SELECT coalesce(sum(shipped_value_minor), 0)::bigint FROM shipment WHERE status = 'shipped'),
                        (SELECT coalesce(sum(amount_minor), 0)::bigint FROM payment_capture)",
                &[],
            )
            .expect("read the shipped and captured totals");
        (row.get(0), row.get(1))
    };
    assert!(shipped_value > 0, "the shipment carries a value to capture");
    assert_eq!(
        captured, shipped_value,
        "capture takes exactly the shipped value, never the order total"
    );
}

/// Reconciliation compares the provider's answer against what the store
/// recorded. An answer that agrees on every compared fact is matched
/// automatically; anything else would park on a support decision instead.
#[test]
fn a_reconciliation_matches_a_payment_the_provider_confirms() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    let suite = start_store(&stub, "petshop_payment_reconciliation");
    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");

    let cart_id = seed_cart(suite.db_url());
    let (status, body) = start_checkout(&suite, cart_id, "550e8400-e29b-41d4-a716-446655440a00");
    assert_eq!(status, 200, "start_checkout status: {body}");
    let checkout = await_terminal(&mut client, "checkout_payment");
    let payment_id = checkout
        .pointer("/payment_id")
        .and_then(Json::as_str)
        .expect("checkout published its payment")
        .to_owned();

    let (amount_minor, currency, provider_reference): (i64, String, String) = {
        let row = client
            .query_one(
                "SELECT amount_minor::bigint, currency, provider_reference
                 FROM payment WHERE id::text = $1",
                &[&payment_id],
            )
            .expect("read the recorded payment");
        (row.get(0), row.get(1), row.get(2))
    };

    // The provider agrees with the store on every compared fact.
    stub.set_default(
        "/v1/payment-reconciliations",
        ScriptedResponse::ok(json!({
            "provider_event_id": "$request:/provider_event_id",
            "reconciliation_id": "recon_petshop_1",
            "amount_minor": amount_minor,
            "currency": currency,
            "status": "authorized",
            "provider_reference": provider_reference,
            "normalized_payload": { "gateway": "mock", "reconciled": true }
        })),
    );

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ start_payment_reconciliation(payment_id: \"{payment_id}\", provider_event_id: \"evt_petshop_reconcile_1\", expected_status: authorized, request_id: \"550e8400-e29b-41d4-a716-446655440a01\") {{ payment_id }} }}"
            )
        }),
        &[(
            "X-Donat-Role".to_owned(),
            "reconciliation_worker".to_owned(),
        )],
    );
    assert_eq!(status, 200, "start_payment_reconciliation status: {body}");
    assert!(
        body.get("errors").is_none(),
        "start_payment_reconciliation reported errors: {body}"
    );

    let output = await_terminal(&mut client, "payment_reconciliation");
    assert_eq!(
        output
            .pointer("/reconciliation_status")
            .and_then(Json::as_str),
        Some("matched"),
        "an agreeing provider answer is matched without support: {output}"
    );
    assert!(
        output
            .pointer("/reconciliation_id")
            .and_then(Json::as_str)
            .is_some(),
        "the match publishes the durable reconciliation it recorded: {output}"
    );
}

/// One marketplace payout cycle: the Process selects the cycle's candidates,
/// pays each vendor through the provider, and records the outcome against the
/// payout row. The candidate view pins this cycle id.
const PAYOUT_CYCLE: &str = "00000000-0000-0000-0000-000000000001";
const PAYOUT_PATH: &str = "/v1/payouts";

#[test]
fn a_payout_cycle_pays_each_accepted_vendor_order() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    stub.set_default(
        PAYOUT_PATH,
        ScriptedResponse::ok(json!({
            "local_payout_id": "$request:/payout_id",
            "vendor_id": "$request:/vendor_id",
            "provider_event_id": "evt_petshop_payout_1",
            "payout_id": "payout_petshop_1",
            "outcome": "paid",
            "normalized_payload": { "rail": "mock", "paid": true }
        })),
    );
    let suite = start_store(&stub, "petshop_vendor_payout");
    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");

    let cart_id = seed_cart(suite.db_url());
    let (status, body) = start_checkout(&suite, cart_id, "550e8400-e29b-41d4-a716-446655440990");
    assert_eq!(status, 200, "start_checkout status: {body}");
    await_terminal(&mut client, "checkout_payment");

    // One accepted vendor order is what makes this cycle have anything to pay.
    client
        .execute(
            "INSERT INTO vendor_order
                 (order_id, order_line_id, vendor_id, offer_id, line_sequence,
                  product_category, gross_minor, currency, commission_tier,
                  commission_bps, status)
             SELECT order_line.order_id, order_line.id,
                    '00000000-0000-0000-0000-0000000000a1'::uuid,
                    '00000000-0000-0000-0000-0000000000b1'::uuid,
                    1, 'services', order_line.line_subtotal_minor, order_line.currency,
                    'standard', 1000, 'accepted'
             FROM order_line",
            &[],
        )
        .expect("seed one accepted vendor order");

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ start_vendor_payout(payout_cycle_id: \"{PAYOUT_CYCLE}\", request_id: \"550e8400-e29b-41d4-a716-446655440991\") {{ payout_cycle_id }} }}"
            )
        }),
        &[(
            "X-Donat-Role".to_owned(),
            "marketplace_worker".to_owned(),
        )],
    );
    assert_eq!(status, 200, "start_vendor_payout status: {body}");
    assert!(
        body.get("errors").is_none(),
        "start_vendor_payout reported errors: {body}"
    );

    let output = await_terminal(&mut client, "vendor_payout");
    assert_eq!(
        output
            .pointer("/failures")
            .and_then(Json::as_array)
            .map(Vec::len),
        Some(0),
        "every candidate was paid: {output}"
    );
    assert_eq!(
        output
            .pointer("/payouts")
            .and_then(Json::as_array)
            .map(Vec::len),
        Some(1),
        "the cycle paid its one candidate: {output}"
    );

    let (status_text, net_minor): (String, i64) = {
        let row = client
            .query_one(
                "SELECT status, net_minor FROM vendor_payout WHERE payout_cycle_id::text = $1",
                &[&PAYOUT_CYCLE],
            )
            .expect("read the payout row");
        (row.get(0), row.get(1))
    };
    assert_eq!(
        status_text, "paid",
        "the payout row records the provider outcome"
    );
    assert_eq!(
        net_minor, 2250,
        "the vendor is paid gross (2499) minus its 10% commission (249)"
    );
}

/// A B2B quote above the automatic-credit ceiling: the decision table routes it
/// to approver review, the Process opens that review and parks, and the
/// approver's verified decision consumes the organization's credit.
#[test]
fn a_large_b2b_quote_is_routed_to_an_approver_who_consumes_the_credit() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    let suite = start_store(&stub, "petshop_b2b_order_approval");
    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");

    let organization_id: String = client
        .query_one(
            "INSERT INTO organization (currency, available_credit_minor)
             VALUES ('USD', 100000) RETURNING id::text",
            &[],
        )
        .expect("seed one organization with credit")
        .get(0);
    client
        .execute(
            "INSERT INTO organization_membership (organization_id, user_id)
             SELECT id, $2 FROM organization WHERE id::text = $1",
            &[&organization_id, &CUSTOMER],
        )
        .expect("seed the buyer's membership");
    let cart_id: i64 = client
        .query_one(
            "INSERT INTO cart (customer_id) VALUES ($1) RETURNING id",
            &[&CUSTOMER],
        )
        .expect("seed one open cart")
        .get(0);
    // Five units clear the automatic-credit ceiling, so the quote must be
    // routed to a human approver rather than consuming credit on its own.
    client
        .execute(
            "INSERT INTO cart_line (cart_id, variant_id, quantity) VALUES ($1, 2, 5)",
            &[&cart_id],
        )
        .expect("seed one cart line above the automatic ceiling");

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ submit_quote(organization_id: \"{organization_id}\", cart_id: {cart_id}, request_id: \"550e8400-e29b-41d4-a716-446655440980\") {{ quote_id approval_id total_minor }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "b2b_buyer".to_owned()),
            ("X-Donat-User-Id".to_owned(), CUSTOMER.to_owned()),
        ],
    );
    assert_eq!(status, 200, "submit_quote status: {body}");
    assert!(
        body.get("errors").is_none(),
        "submit_quote reported errors: {body}"
    );
    let approval_id = body
        .pointer("/data/submit_quote/approval_id")
        .and_then(Json::as_str)
        .expect("the quote command answers with its approval")
        .to_owned();

    await_receptive_wait(&mut client, "b2b_order_approval", "await_approver");
    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ approve_purchase(approval_id: \"{approval_id}\", request_id: \"550e8400-e29b-41d4-a716-446655440981\") {{ approval_id approval_status }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "b2b_approver".to_owned()),
            ("X-Donat-User-Id".to_owned(), "approver-1".to_owned()),
        ],
    );
    assert_eq!(status, 200, "approve_purchase status: {body}");
    assert!(
        body.get("errors").is_none(),
        "approve_purchase reported errors: {body}"
    );

    let output = await_terminal(&mut client, "b2b_order_approval");
    assert_eq!(
        output.pointer("/approval_status").and_then(Json::as_str),
        Some("approved"),
        "the approver's decision approves the quote: {output}"
    );

    let (available, consumed): (i64, i64) = {
        let row = client
            .query_one(
                "SELECT available_credit_minor, consumed_credit_minor
                 FROM organization WHERE id::text = $1",
                &[&organization_id],
            )
            .expect("read the organization's credit");
        (row.get(0), row.get(1))
    };
    assert_eq!(
        (available, consumed),
        (100000 - 12495, 12495),
        "approving the quote moves exactly its total out of available credit"
    );
}

/// A scheduled renewal cycle: the Process opens the renewal order and payment,
/// authorizes it with the provider, and records the renewal outcome. The
/// dunning ladder exists for declines; a first-attempt authorization skips it.
#[test]
fn a_scheduled_subscription_renewal_authorizes_and_renews() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    let suite = start_store(&stub, "petshop_subscription_renewal");
    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");
    let subscription_id: String = client
        .query_one(
            "INSERT INTO subscription
                 (customer_id, variant_id, quantity, unit_price_minor, line_total_minor,
                  currency, status)
             VALUES ($1, 2, 1, 2499, 2499, 'USD', 'active')
             RETURNING id::text",
            &[&CUSTOMER],
        )
        .expect("seed one active subscription")
        .get(0);

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ start_subscription_renewal(subscription_id: \"{subscription_id}\", cron_occurrence: \"2030-02-01T00:00:00Z\") {{ subscription_id status }} }}"
            )
        }),
        &[(
            "X-Donat-Role".to_owned(),
            "subscription_worker".to_owned(),
        )],
    );
    assert_eq!(status, 200, "start_subscription_renewal status: {body}");
    assert!(
        body.get("errors").is_none(),
        "start_subscription_renewal reported errors: {body}"
    );

    let output = await_terminal(&mut client, "subscription_renewal");
    assert_eq!(
        output.pointer("/status").and_then(Json::as_str),
        Some("renewed"),
        "a first-attempt authorization renews without entering dunning: {output}"
    );

    // The audit records every attempt, the scheduled cycle included. One row
    // at attempt 0 means the ladder was never walked.
    let attempts: Vec<(i32, String)> = client
        .query(
            "SELECT attempt, outcome FROM subscription_dunning_attempt ORDER BY attempt",
            &[],
        )
        .expect("read the renewal audit")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        attempts,
        vec![(0, "authorized".to_owned())],
        "only the scheduled attempt is recorded; no dunning retry ran"
    );
}

/// The whole return module: the shopper requests a return of a delivered line,
/// support approves it, the carrier issues a return label, the warehouse
/// receives and inspects it, and the provider refunds exactly the inspected
/// amount. Three verified human signals drive one durable Process.
#[test]
fn a_returned_line_is_approved_received_inspected_and_refunded() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    script_fulfilment_providers(&stub);
    stub.set_default(
        RETURN_LABEL_PATH,
        ScriptedResponse::ok(json!({
            "provider_event_id": "evt_petshop_return_label_1",
            "return_id": "$request:/return_key",
            "tracking_number": "RETURN-TRACK-1",
            "label_url": "https://carrier.example/returns/petshop-1.pdf",
            "status": "created",
            "normalized_payload": { "carrier": "mock", "label": true }
        })),
    );
    stub.set_default(
        REFUND_PATH,
        ScriptedResponse::ok(json!({
            "provider_event_id": "evt_petshop_refund_1",
            "refund_id": "refund_petshop_1",
            "provider_reference": "ref_petshop_1",
            "status": "refunded",
            "normalized_payload": { "gateway": "mock", "refunded": true }
        })),
    );
    let suite = start_store(&stub, "petshop_return_refund");
    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");
    let order_id = checkout_and_fulfil(&suite, &mut client, "550e8400-e29b-41d4-a716-44665544096");

    let (order_line_id, refund_amount_minor): (String, i64) = {
        let row = client
            .query_one(
                "SELECT id::text, line_subtotal_minor::bigint FROM order_line WHERE order_id::text = $1",
                &[&order_id],
            )
            .expect("the fulfilled order has one line to return");
        (row.get(0), row.get(1))
    };

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ start_return(order_id: \"{order_id}\", lines: [{{order_line_id: \"{order_line_id}\", requested_quantity: 1}}], reason: \"wrong size\", replacement_requested: false, request_id: \"550e8400-e29b-41d4-a716-446655440970\") {{ order_id }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "customer".to_owned()),
            ("X-Donat-User-Id".to_owned(), CUSTOMER.to_owned()),
        ],
    );
    assert_eq!(status, 200, "start_return status: {body}");
    assert!(
        body.get("errors").is_none(),
        "start_return reported errors: {body}"
    );

    await_receptive_wait(&mut client, "return_refund", "await_support_decision");
    let (return_id, return_item_id): (String, String) = {
        let row = client
            .query_one(
                "SELECT return_request.id::text, return_item.id::text
                 FROM return_request
                 JOIN return_item ON return_item.return_request_id = return_request.id",
                &[],
            )
            .expect("the Process opened one return request with one item");
        (row.get(0), row.get(1))
    };

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ approve_return(return_id: \"{return_id}\", lines: [{{return_item_id: \"{return_item_id}\", approved_quantity: 1}}], decision_id: \"550e8400-e29b-41d4-a716-446655440971\", note: \"approved\") {{ return_id status }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "support".to_owned()),
            ("X-Donat-User-Id".to_owned(), "support-1".to_owned()),
        ],
    );
    assert_eq!(status, 200, "approve_return status: {body}");
    assert!(
        body.get("errors").is_none(),
        "approve_return reported errors: {body}"
    );

    await_receptive_wait(&mut client, "return_refund", "await_warehouse_receipt");
    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ receive_return(return_id: \"{return_id}\", lines: [{{return_item_id: \"{return_item_id}\", received_quantity: 1}}], receipt_id: \"550e8400-e29b-41d4-a716-446655440972\", received_at: \"2030-01-02T00:00:00Z\") {{ return_id status }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "fulfilment".to_owned()),
            ("X-Donat-User-Id".to_owned(), "warehouse-1".to_owned()),
        ],
    );
    assert_eq!(status, 200, "receive_return status: {body}");
    assert!(
        body.get("errors").is_none(),
        "receive_return reported errors: {body}"
    );

    await_receptive_wait(&mut client, "return_refund", "await_inspection");
    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ record_return_inspection(return_id: \"{return_id}\", lines: [{{return_item_id: \"{return_item_id}\", inspected_quantity: 1}}], inspection: accepted, refund_amount_minor: {refund_amount_minor}, inspection_id: \"550e8400-e29b-41d4-a716-446655440973\", note: \"as described\") {{ return_id status }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "fulfilment".to_owned()),
            ("X-Donat-User-Id".to_owned(), "warehouse-1".to_owned()),
        ],
    );
    assert_eq!(status, 200, "record_return_inspection status: {body}");
    assert!(
        body.get("errors").is_none(),
        "record_return_inspection reported errors: {body}"
    );

    let output = await_terminal(&mut client, "return_refund");
    assert_eq!(
        output.pointer("/status").and_then(Json::as_str),
        Some("refunded"),
        "the return Process refunds the inspected line: {output}"
    );
    assert_eq!(
        output
            .pointer("/refund_amount_minor")
            .and_then(Json::as_i64),
        Some(refund_amount_minor),
        "the refund is exactly the inspected amount, never the order total: {output}"
    );
    assert_eq!(
        output.pointer("/tracking_number").and_then(Json::as_str),
        Some("RETURN-TRACK-1"),
        "the carrier return label is published to the shopper: {output}"
    );
}

/// A shopper cancels while the payment is still pending. The claim beats the
/// in-flight authorization, and the cancellation Process asks the provider to
/// prove the authorization never committed before it releases the reservation.
///
/// The window is real, not slept for: the provider holds the authorize call
/// until the cancellation Command has committed its claim.
#[test]
fn a_shopper_cancels_a_pending_checkout_and_the_process_proves_no_authorization() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    stub.set_default(LOOKUP_PATH, lookup_absence_response());
    stub.hold(AUTHORIZE_PATH);
    let suite = start_store(&stub, "petshop_checkout_cancellation");
    let cart_id = seed_cart(suite.db_url());

    let (status, body) = start_checkout(&suite, cart_id, "550e8400-e29b-41d4-a716-446655440940");
    assert_eq!(status, 200, "start_checkout status: {body}");

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");
    assert!(
        stub.await_held(AUTHORIZE_PATH, Duration::from_secs(60)),
        "the checkout Process never reached its authorization request: {}",
        process_diagnostics(&mut client)
    );

    let order_id: String = client
        .query_one(
            "SELECT id::text FROM orders WHERE order_status = 'checkout_started'",
            &[],
        )
        .expect("checkout committed one order with a pending payment")
        .get(0);

    let (status, body) = cancel_order(&suite, &order_id, "550e8400-e29b-41d4-a716-446655440941");
    assert_eq!(status, 200, "cancel_order status: {body}");
    assert!(
        body.get("errors").is_none(),
        "cancel_order reported errors: {body}"
    );

    // The claim is committed. Let the authorization attempt finish the only way
    // it now can: the provider is unavailable, the retries exhaust, and the
    // checkout Process falls through to its own reconciliation.
    stub.script(
        AUTHORIZE_PATH,
        vec![
            ScriptedResponse::status(503),
            ScriptedResponse::status(503),
            ScriptedResponse::status(503),
        ],
    );
    stub.release(AUTHORIZE_PATH);

    let output = await_terminal(&mut client, "checkout_cancellation");
    assert_eq!(
        output.pointer("/status").and_then(Json::as_str),
        Some("cancelled"),
        "the cancellation Process cancelled the order without a void: {output}"
    );

    let released: i64 = client
        .query_one(
            "SELECT count(*) FROM inventory_reservation WHERE order_id::text = $1 AND status = 'released'",
            &[&order_id],
        )
        .expect("read the released reservations")
        .get(0);
    assert!(
        released > 0,
        "proving the authorization absent releases the reserved stock"
    );
    let voids = stub
        .calls()
        .into_iter()
        .filter(|call| call.path.ends_with("/voids"))
        .count();
    assert_eq!(
        voids, 0,
        "an authorization that never committed is never voided"
    );
}

/// The prescription module reviews a line of the order the checkout module
/// created: the shopper submits it, the Process waits, and a veterinary
/// reviewer's decision Command releases it.
#[test]
fn a_reviewer_approves_a_prescription_and_the_process_releases_the_line() {
    let stub = provider_stub::spawn();
    script_providers(&stub);
    let suite = start_store(&stub, "petshop_prescription_review");
    let cart_id = seed_cart(suite.db_url());

    let (status, body) = start_checkout(&suite, cart_id, "550e8400-e29b-41d4-a716-446655440930");
    assert_eq!(status, 200, "start_checkout status: {body}");
    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the Petshop database");
    await_terminal(&mut client, "checkout_payment");

    let order_line_id: String = client
        .query_one("SELECT id::text FROM order_line LIMIT 1", &[])
        .expect("the checkout Process committed order lines")
        .get(0);

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ start_prescription_review(order_line_id: \"{order_line_id}\", review_deadline: \"2030-01-01T00:00:00Z\", request_id: \"550e8400-e29b-41d4-a716-446655440931\") {{ order_line_id }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "customer".to_owned()),
            ("X-Donat-User-Id".to_owned(), CUSTOMER.to_owned()),
        ],
    );
    assert_eq!(status, 200, "start_prescription_review status: {body}");

    let prescription_id = await_row_id(
        &mut client,
        "prescription_request",
        "the prescription Process never submitted its review",
    );

    await_receptive_wait(
        &mut client,
        "prescription_review",
        "await_veterinary_decision",
    );

    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ approve_prescription(prescription_id: \"{prescription_id}\", decision_id: \"550e8400-e29b-41d4-a716-446655440932\", review_note: \"ok\") {{ prescription_id }} }}"
            )
        }),
        &[
            ("X-Donat-Role".to_owned(), "veterinary_reviewer".to_owned()),
            ("X-Donat-User-Id".to_owned(), REVIEWER.to_owned()),
        ],
    );
    assert_eq!(status, 200, "approve_prescription status: {body}");
    assert!(
        body.get("errors").is_none(),
        "approve_prescription reported errors: {body}"
    );

    let output = await_terminal(&mut client, "prescription_review");
    assert_eq!(
        output.pointer("/status").and_then(Json::as_str),
        Some("approved"),
        "the reviewer's decision released the line: {output}"
    );
}
