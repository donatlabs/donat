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
        // The customer avatar is stored in an object store, and the engine
        // still signs the call reporting an upload finished.
        .env("PETSHOP_FILE_SIGNING_SECRET", "petshop-test-file-signing")
        // The avatar column names an object store. These suites never upload,
        // but the registry resolves every credential before the listener binds.
        .env("PETSHOP_S3_KEY", "petshop-test-key")
        .env("PETSHOP_S3_SECRET", "petshop-test-secret")
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
