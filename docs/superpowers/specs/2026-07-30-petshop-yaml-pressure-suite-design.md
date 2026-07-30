# Petshop YAML pressure-suite design

Date: 2026-07-30

Decision: model a broad, single-tenant commerce platform in active Petshop
metadata before implementing the missing runtime behavior.

## Purpose

Petshop is the product-shaped pressure suite for Donat's declarative SaaS
runtime. It must cover materially different business processes instead of
proving only one happy-path checkout. The example remains understandable by
splitting those processes into modules that share one catalog, customer,
inventory, order, and accounting core.

The YAML is written first in `examples/petshop/metadata`. It is active
metadata, not a detached sketch. The example may therefore remain
intentionally red while the runtime and database model catch up. Rust,
conformance fixtures, and implementation tests follow only after the YAML has
been reviewed as a user-facing contract.

## Boundary: single tenant

Petshop is deliberately single-tenant. It does not add `tenant_id` arguments,
columns, filters, session conventions, or per-module tenant checks.

Multitenancy is a separate engine-wide capability. It must eventually compose
with catalog compilation, permissions, IR, generated SQL, command journals,
flow runs, connector credentials, and operational APIs. Repeating tenant
logic in each Petshop command would hide gaps in that platform boundary.

Marketplace vendors, B2B organizations, customers, and store locations are
domain entities inside one tenant. They are not substitutes for platform
tenants.

## Organization

The active metadata is an include-based index:

```text
examples/petshop/metadata/
├── commands.yaml
├── commands/
│   ├── checkout/
│   ├── fulfilment/
│   ├── returns/
│   ├── subscriptions/
│   ├── b2b/
│   ├── marketplace/
│   ├── booking/
│   ├── prescription/
│   └── operations/
├── flows.yaml
├── flows/
│   ├── checkout-payment.yaml
│   ├── partial-fulfilment.yaml
│   ├── return-refund.yaml
│   ├── subscription-renewal.yaml
│   ├── b2b-order-approval.yaml
│   ├── vendor-payout.yaml
│   ├── grooming-booking.yaml
│   ├── prescription-review.yaml
│   └── payment-reconciliation.yaml
├── connectors.yaml
├── connectors/
│   ├── mock-payment.yaml
│   ├── mock-carrier.yaml
│   ├── mock-tax.yaml
│   ├── mock-notification.yaml
│   └── mock-payout.yaml
└── rules.yaml
```

Each command file contains one atomic domain operation. Each flow file
contains one explicitly versioned `kind: process` durable orchestration.
Commands remain synchronous; `start_process` and `signal_process` effects
commit transactional-outbox intent with the domain statement. Only Process
definitions receive durable state, activity attempts, signal inboxes, timers,
and recovery semantics. `commands.yaml`, `flows.yaml`, and `connectors.yaml`
contain only ordered `!include` entries. `rules.yaml` owns named rules, finite
types, and decision tables because those artifacts must be compiled and
fingerprinted together.

## Scenario modules

| Module | Product behavior | Runtime pressure |
| --- | --- | --- |
| Retail checkout | price lists, promotions, coupons, tax, shipping quote, price snapshot, idempotent checkout | Rules, decision tables, relational batches, atomic multi-table commands |
| Multi-location inventory | allocation, split shipment, backorder, partial cancellation | concurrent reservation, bounded set processing, exact affected-row gates |
| Payments | authorize, capture, void, asynchronous challenge, partial refund, chargeback, reconciliation | HTTP request, retry, callback, wait, deadline, idempotency, late events |
| Returns and exchanges | RMA request, approval, return label, receipt, inspection, refund or exchange | human signal, external request, long deadline, compensation |
| Subscriptions | pet-food autoship, renewal, pause, cancellation, dunning | cron start, durable timers, repeat-safe commands, payment retry |
| B2B procurement | organization membership, quote, credit limit, multi-step approval, invoice terms | explicit roles, decision tables, human approval, escalation |
| Marketplace | vendor offers, vendor-order split, commission, payout, dispute | ownership boundaries, deterministic partitioning, fan-out and reconciliation |
| Booking | grooming resource and slot reservation, confirmation, reschedule, no-show | unique-resource concurrency, timers, cancellation |
| Prescription | veterinary approval, rejection, expiry, audited release | human-in-the-loop wait, deadline, immutable decision audit |
| Operations | notifications, fraud review, provider reconciliation, replay-safe webhooks | event triggers, connector calls, manual recovery, redaction |

The modules share product and order identity where that is natural. They do
not share lifecycle columns merely to reduce table count.

## Domain extensions

The later database-model task will add only the relations required by the
approved YAML:

- `sales_channel`, `region`, `price_list`, `variant_price`, `promotion`,
  `coupon`, and immutable `order_adjustment`;
- `stock_location`, `inventory_level`, `inventory_movement`, allocation,
  shipment item, and backorder records;
- payment attempt, authorization, capture, void, dispute, and reconciliation
  records;
- return request, return item, inspection, and exchange linkage;
- subscription, schedule, renewal, and dunning attempt;
- organization, membership, quote, approval, invoice term, and credit usage;
- vendor, offer, vendor order, commission, payout, and vendor dispute;
- service resource, time slot, booking, and attendance outcome;
- prescription request, review, decision, and expiry.

Money remains integer minor units plus an explicit currency. Every order keeps
immutable pricing, tax, discount, shipping, and allocation snapshots. Partial
payment, shipment, cancellation, return, and refund quantities are represented
at line level rather than inferred from one order status.

## Command catalog

Ordinary editable records remain GraphQL/REST/MCP CRUD. Commands begin at an
atomic cross-relation change or guarded lifecycle transition.

The first YAML catalog includes:

- checkout: `begin_checkout`, `cancel_order`,
  `release_expired_checkout`;
- payments: `record_payment_outcome`, `authorize_payment`,
  `capture_payment`, `void_authorization`, `complete_refund`,
  `record_chargeback`, `reconcile_payment`;
- fulfilment: `allocate_inventory`, `mark_order_packed`,
  `create_shipment`, `record_shipment_result`, `record_delivery`;
- returns: `request_return`, `approve_return`, `reject_return`,
  `receive_return`, `record_return_inspection`, `create_exchange`;
- subscriptions: `create_subscription_order`, `record_renewal_outcome`,
  `pause_subscription`, `cancel_subscription`;
- B2B: `submit_quote`, `approve_purchase`, `reject_purchase`,
  `consume_credit`;
- marketplace: `split_vendor_orders`, `record_vendor_acceptance`,
  `create_vendor_payout`, `record_payout_outcome`,
  `reconcile_vendor_payout`, `open_vendor_dispute`;
- booking: `reserve_grooming_slot`, `confirm_booking`,
  `reschedule_booking`, `cancel_booking`, `expire_booking_hold`,
  `record_no_show`;
- prescription: `submit_prescription_review`,
  `approve_prescription`, `reject_prescription`,
  `expire_prescription`;
- operations: `route_fraud_review`, `resolve_fraud_review`,
  `resolve_payment_reconciliation`, `record_notification_delivery`.

Command permissions name explicit classic roles. A fixed flow role still
needs the same command and table permissions as a direct caller. No command
contains SQL, a dynamic relation name, a runtime role, or an executable-code
escape hatch.

## Flow catalog

### `checkout_payment`

Runs `begin_checkout`, calls the payment connector after commit, waits for a
verified authorization event or deadline, and applies the normalized outcome.
Physical goods remain authorized rather than captured until fulfilment.
Provider failure after retry releases reservations. A late callback remains
auditable and cannot resurrect an expired order.

### `partial_fulfilment`

Allocates lines to locations, creates one shipment unit per allocation group,
calls the carrier with a stable item key, and records partial success without
marking unshipped lines complete. Each shipment captures at most the value of
its shipped lines; cancelling the remainder voids any unused authorization.

### `return_refund`

Waits for approval, obtains a return label, waits for warehouse receipt and
inspection, then routes to refund, exchange, or rejection. Every terminal path
retains the inspection decision.

### `subscription_renewal`

Starts from a cron occurrence, creates one idempotent renewal order, attempts
payment, follows a bounded dunning schedule, and either confirms the next
renewal or pauses the subscription.

### `b2b_order_approval`

`submit_quote` commits the quote, approval, and a `start_process` outbox intent
in one transaction. The Process mirrors that start contract, routes by amount,
organization policy, and available credit, and may complete automatically,
wait for one or more named approver roles, escalate on a deadline, or reject
without creating a payable order.

### `vendor_payout`

Consumes eligible vendor-order balances, calculates commission through a
decision table, creates one stable payout request per vendor, and waits for
provider outcomes or manual reconciliation.

### `grooming_booking`

Atomically reserves one resource slot, requests customer confirmation, expires
unconfirmed holds, and handles reschedule, cancellation, and no-show without
double-booking.

### `prescription_review`

Waits for an authorized reviewer, records one immutable decision, expires
unreviewed requests, and releases an approved order line without exposing
private review details to unrelated roles.

### `payment_reconciliation`

Compares provider events with local attempts, auto-resolves exact matches, and
waits for a support decision when money and local state disagree.

## Flow grammar pressure

The sequential states remain `command`, `request`, `wait`, `when`, `output`,
and `fail`. Two approved scenarios independently require bounded fan-out:
carrier requests per allocation group and payout requests per vendor.

The YAML may therefore introduce one constrained `for_each` state only:

- input must be a finite list returned by a compiled command;
- `item_key` must be stable and unique;
- `max_items` and `max_concurrency` are required;
- the body is one declared `command` or `request`, not an arbitrary subgraph;
- completion policy is explicitly `all` or `collect`;
- every attempt derives a stable idempotency key from the run, state, and
  item key;
- results preserve input order and remain bounded flow state.

There is no general loop, recursion, dynamic state creation, child-flow
language, or arbitrary code step.

## Rules and decision tables

Rules own state-transition validity, typed arithmetic, ownership invariants,
and normalized provider outcomes. Decision tables own finite business choices:

- price-list and promotion eligibility;
- tax region and tax category;
- shipping service selection;
- inventory-location ranking;
- B2B approval route and credit policy;
- marketplace commission;
- return disposition;
- dunning schedule;
- fraud routing.

Tables contain data and typed results, not SQL or executable expressions.
Commands use named Rules; flows use named Rules and decision-table results.

## Connectors and events

The example uses deterministic provider-neutral mock connectors for payment,
carrier, tax, notification, and payout behavior. Manual configuration may
point generic HTTP operations at a request-capture endpoint.

Every mutation operation declares retry classification, timeout, capacity,
redaction, and provider idempotency. Process start and command-signal effects
are generic transactional outbox records, never immediate Process calls.
Verified inbound events are persisted before flow matching, so
callback-before-wait and duplicate delivery are safe. Provider payloads are
normalized before commands consume them.

## Public execution contract

Each public flow has one durable run and three views of that same run:

- `start_*` returns the owner-scoped run handle immediately;
- `execute_*` waits for a bounded interval and returns either terminal output
  or the same running handle;
- status reads return safe state and output without connector payloads,
  credentials, leases, or unrestricted journal data.

Disconnect never cancels a run. Retries and restart use stable keys. REST and
MCP adapt the same compiled GraphQL contract instead of introducing another
orchestration path.

## Failure and recovery

Business outcomes such as payment decline, rejected approval, expired booking,
or failed inspection are explicit terminal outputs. Infrastructure exhaustion
uses a typed `fail` result after declared compensation has completed.

Atomic commands roll back all domain writes and Process intent on rejection.
After commit, a generic dispatcher may redeliver outbox intent until the
Process inbox accepts it idempotently. HTTP never runs inside the command
transaction. Durable activities reuse one provider key across retry and worker
takeover; a provider without a sufficient idempotency guarantee requires an
explicit reconciliation path. Duplicate callbacks, cron occurrences,
approvals, and manual retries converge through declared idempotency keys.
Partial fulfilment and payout flows retain per-item outcomes for reconciliation
instead of pretending the batch was all-or-nothing.

## Delivery order

1. Commit this design and obtain user review.
2. Write every active Petshop command, Rule, decision table, connector, and
   flow YAML as the desired public contract.
3. Review the YAML without adding Rust or conformance coverage.
4. Rewrite implementation plans from the approved YAML dependencies.
5. Extend migrations and metadata types.
6. Add failing product cases module by module.
7. Implement only the generic runtime primitives required by those cases.
8. Perform one whole-range code review after the planned implementation.

The branch is allowed to be intentionally red between steps 2 and 7. Every
such commit must state that the active example is ahead of runtime support and
must name the next validation boundary.
