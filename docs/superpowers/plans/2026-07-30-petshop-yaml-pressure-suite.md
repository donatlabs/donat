# Petshop YAML Pressure Suite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Author the complete desired Petshop business-logic contract as
active modular YAML before adding migrations, Rust runtime behavior, or test
coverage.

**Architecture:** Keep one single-tenant Petshop data plane and split atomic
domain Commands, durable Flows, provider-neutral Connectors, Rules, and
decision tables into focused include files. The YAML deliberately leads the
current runtime and may leave the active example RED until later
implementation plans close each dependency.

**Tech Stack:** Donat v2 directory metadata, YAML `!include`, declarative
Commands, Rules and decision tables, product-derived durable Flows, compiled
HTTP Connectors.

## Global Constraints

- Modify only `examples/petshop/metadata`, `examples/petshop/README.md`, and
  this plan during the YAML-contract phase.
- Do not modify Rust, SQL migrations, conformance fixtures, snapshots, or test
  code in this phase.
- `commands.yaml`, `flows.yaml`, and `connectors.yaml` are active metadata,
  not detached sketches.
- The active example is expected to be RED because Flow metadata, bounded
  fan-out, catalog relations, and some Command semantics are not implemented.
- Petshop is single-tenant. Do not add `tenant_id`, tenant arguments, tenant
  filters, or tenant-derived roles.
- Marketplace vendors, customers, organizations, and locations are domain
  entities within one tenant.
- One command file owns one atomic operation. One flow file owns one durable
  orchestration.
- No SQL, executable code, dynamic relation, dynamic role, general loop,
  recursion, or child-flow language is allowed in metadata.
- Command roles are fixed classic explicit roles and still require underlying
  table permissions.
- Money is integer minor units plus explicit currency.
- Provider mutation operations declare timeout, retry classification,
  idempotency, capacity, and redaction.
- Repository content is English.
- Do not dispatch Judge or per-commit reviewers; the user requested one later
  whole-range review.

## File Map

### Index and policy files

- `examples/petshop/metadata/commands.yaml` — ordered command includes.
- `examples/petshop/metadata/flows.yaml` — ordered flow includes.
- `examples/petshop/metadata/connectors.yaml` — ordered connector includes.
- `examples/petshop/metadata/rules.yaml` — finite types, Rules, decision
  tables, and table test cases.
- `examples/petshop/README.md` — marks YAML-first RED state and links every
  module.

### Command directories

- `commands/checkout/` — checkout, cancellation, expiry.
- `commands/payments/` — authorization, capture, refund, chargeback,
  reconciliation.
- `commands/fulfilment/` — allocation, packing, shipment, delivery.
- `commands/returns/` — RMA, inspection, refund/exchange routing.
- `commands/subscriptions/` — renewal, dunning outcome, pause, cancellation.
- `commands/b2b/` — quote, approval, credit.
- `commands/marketplace/` — vendor orders, payout, disputes.
- `commands/booking/` — slot reservation and lifecycle.
- `commands/prescription/` — review and expiry.
- `commands/operations/` — fraud and notification outcomes.

### Flow and connector directories

- `flows/` — the nine approved product flows.
- `connectors/` — mock payment, carrier, tax, notification, and payout HTTP
  modules.

---

### Task 1: Create active indexes and shared rule catalog

**Files:**

- Create: `examples/petshop/metadata/commands.yaml`
- Create: `examples/petshop/metadata/flows.yaml`
- Create: `examples/petshop/metadata/connectors.yaml`
- Create: `examples/petshop/metadata/rules.yaml`

**Interfaces:**

- Produces command includes for every file named in Tasks 2-10.
- Produces flow includes for:
  `checkout-payment`, `partial-fulfilment`, `return-refund`,
  `subscription-renewal`, `b2b-order-approval`, `vendor-payout`,
  `grooming-booking`, `prescription-review`, and
  `payment-reconciliation`.
- Produces connector includes for `mock-payment`, `mock-carrier`, `mock-tax`,
  `mock-notification`, and `mock-payout`.

- [ ] **Step 1: Create ordered include indexes**

Use quoted includes so the existing directory loader quirk remains covered:

```yaml
- "!include commands/checkout/begin-checkout.yaml"
```

Order commands by module and lifecycle, not alphabetically across modules.

- [ ] **Step 2: Declare finite Rule types**

Declare enums `PaymentOutcome`, `ApprovalDecision`, `InspectionDecision`,
`BookingOutcome`, and `ReconciliationDecision`. Declare only object types
actually referenced by Rules.

- [ ] **Step 3: Declare named Rules**

At minimum declare:

```yaml
rules:
  - name: can_reserve_stock
    parameters: { on_hand: int!, reserved: int!, requested: int! }
    result: bool!
    expression: "requested > 0 && reserved + requested <= on_hand"
  - name: add_int
    parameters: { left: int!, right: int! }
    result: int!
    expression: "left + right"
  - name: payment_was_authorized
    parameters: { outcome: PaymentOutcome! }
    result: bool!
    expression: "outcome == PaymentOutcome::authorized"
```

Add transition Rules for checkout, payment, shipment, return, subscription,
approval, payout, booking, prescription, fraud, and reconciliation states.

- [ ] **Step 4: Declare decision tables with executable examples**

Define `price_list_route`, `promotion_route`, `tax_route`,
`shipping_service_route`, `inventory_location_route`, `b2b_approval_route`,
`marketplace_commission_route`, `return_disposition_route`,
`dunning_schedule`, and `fraud_route`. Every table has `hit_policy`, ordered
rows, and at least one exact `test_cases` entry.

- [ ] **Step 5: Review index closure**

Run:

```bash
find examples/petshop/metadata/commands -type f -name '*.yaml' | sort
find examples/petshop/metadata/flows -type f -name '*.yaml' | sort
find examples/petshop/metadata/connectors -type f -name '*.yaml' | sort
```

Expected during Task 1: directories are still incomplete; closure becomes
green in Task 11.

---

### Task 2: Author checkout and payment Commands

**Files:**

- Create: `commands/checkout/begin-checkout.yaml`
- Create: `commands/checkout/cancel-order.yaml`
- Create: `commands/checkout/release-expired-checkout.yaml`
- Create: `commands/payments/record-payment-outcome.yaml`
- Create: `commands/payments/authorize-payment.yaml`
- Create: `commands/payments/capture-payment.yaml`
- Create: `commands/payments/void-authorization.yaml`
- Create: `commands/payments/complete-refund.yaml`
- Create: `commands/payments/record-chargeback.yaml`
- Create: `commands/payments/reconcile-payment.yaml`

**Interfaces:**

- `begin_checkout(cart_id, request_id)` returns `order_id`, `payment_id`,
  `total_minor`, `currency`, and ordered allocation candidates.
- Its idempotency key is `request_id`; scope is
  `x-donat-user-id`.
- Payment ingress Commands use fixed `payment_worker`.

- [ ] **Step 1: Author `begin_checkout`**

Declare ordered `select_many` pricing rows, aggregate totals, stock
`update_many`, immutable order and line inserts, reservations, pending payment,
cart closure, and ordered result. Bind customer ownership only from
`x-donat-user-id`.

- [ ] **Step 2: Author cancellation and expiry**

Both Commands release only remaining reservations. Customer cancellation is
owner-scoped; support cancellation is the same Command permission, not a
second implementation. Expiry uses fixed `payment_worker`.

- [ ] **Step 3: Author the payment lifecycle**

Keep authorization, capture, void, refund, chargeback, callback normalization,
and reconciliation as separate atomic Commands. Partial capture/refund
arguments carry `amount_minor` and must bind bounded Rules.

- [ ] **Step 4: Review deterministic keys**

Every provider event argument includes one provider event ID. Every mutation
Command declares replay protection whose scope cannot be supplied by an
untrusted dynamic role.

---

### Task 3: Author checkout Flow and payment/tax Connectors

**Files:**

- Create: `flows/checkout-payment.yaml`
- Create: `connectors/mock-payment.yaml`
- Create: `connectors/mock-tax.yaml`

**Interfaces:**

- `checkout_payment` supports public `customer` ownership.
- It runs `begin_checkout` as caller and payment Commands as
  `payment_worker`.
- It terminates with `authorized`, `declined`, `expired`, or typed provider
  failure.

- [ ] **Step 1: Author provider-neutral operations**

`mock_payment` declares authorize, capture, void, refund, and reconcile HTTP
operations. Each mutation has a stable `Idempotency-Key`, timeout, retry
classes, capacity, and redacted authorization header.

- [ ] **Step 2: Author tax quote operation**

`mock_tax.quote_order` is read-only/provider-idempotent and accepts destination,
currency, and ordered taxable lines.

- [ ] **Step 3: Author the durable checkout Flow**

Use `command -> request -> wait -> when -> command -> output/fail`. Persist
callback input before matching. On deadline or retry exhaustion, run
`release_expired_checkout`. Business decline is an output; exhausted transport
is a typed failure after compensation.

---

### Task 4: Author multi-location fulfilment

**Files:**

- Create: `commands/fulfilment/allocate-inventory.yaml`
- Create: `commands/fulfilment/mark-order-packed.yaml`
- Create: `commands/fulfilment/create-shipment.yaml`
- Create: `commands/fulfilment/record-shipment-result.yaml`
- Create: `commands/fulfilment/record-delivery.yaml`
- Create: `flows/partial-fulfilment.yaml`
- Create: `connectors/mock-carrier.yaml`

**Interfaces:**

- Allocation returns a finite ordered list keyed by `allocation_id`.
- The Flow uses bounded `for_each` with `max_items`,
  `max_concurrency`, `item_key`, and `completion: collect`.
- Each successful shipment captures no more than its shipped-line value.

- [ ] **Step 1: Author allocation and shipment Commands**

Use location-ranking decision output, ordered line allocations, exact
quantities, and line-level shipment results. Backordered quantities stay
explicit.

- [ ] **Step 2: Author carrier Connector**

Declare quote, create-label, and track operations with stable shipment keys,
timeouts, retry, capacity, and redaction.

- [ ] **Step 3: Author partial fulfilment Flow**

Allocate, fan out one carrier request per allocation, collect ordered results,
record successes/failures, capture shipped value, and output partial status
without promoting unshipped lines.

---

### Task 5: Author returns, exchanges, and refunds

**Files:**

- Create: `commands/returns/request-return.yaml`
- Create: `commands/returns/approve-return.yaml`
- Create: `commands/returns/reject-return.yaml`
- Create: `commands/returns/receive-return.yaml`
- Create: `commands/returns/record-return-inspection.yaml`
- Create: `commands/returns/create-exchange.yaml`
- Create: `flows/return-refund.yaml`

**Interfaces:**

- Customer starts the RMA; support approves/rejects; fulfilment records receipt
  and inspection; payment worker completes bounded refund.

- [ ] **Step 1: Author RMA Commands**

Represent requested/approved/received/inspected quantities independently from
the original line quantity.

- [ ] **Step 2: Author return Flow**

Wait for support approval, request a carrier label, wait for receipt and
inspection, route through `return_disposition_route`, then refund, exchange,
or reject with complete audit output.

---

### Task 6: Author subscription renewal and dunning

**Files:**

- Create: `commands/subscriptions/create-subscription-order.yaml`
- Create: `commands/subscriptions/record-renewal-outcome.yaml`
- Create: `commands/subscriptions/pause-subscription.yaml`
- Create: `commands/subscriptions/cancel-subscription.yaml`
- Create: `flows/subscription-renewal.yaml`

**Interfaces:**

- A cron occurrence and subscription ID form the stable renewal key.
- Dunning uses the finite `dunning_schedule` table; there is no general loop.

- [ ] **Step 1: Author repeat-safe subscription Commands**

Create one renewal order per scheduled occurrence and make pause/cancel
terminal state transitions explicit.

- [ ] **Step 2: Author renewal Flow**

Attempt payment, wait the table-selected delay, retry the finite declared
states, then confirm the next renewal or pause. Do not encode recursive YAML.

---

### Task 7: Author B2B approval

**Files:**

- Create: `commands/b2b/submit-quote.yaml`
- Create: `commands/b2b/approve-purchase.yaml`
- Create: `commands/b2b/reject-purchase.yaml`
- Create: `commands/b2b/consume-credit.yaml`
- Create: `flows/b2b-order-approval.yaml`

**Interfaces:**

- Organization roles are `b2b_buyer`, `b2b_approver`, and
  `b2b_finance`; none is a tenant or admin.

- [ ] **Step 1: Author quote and credit Commands**

Snapshot quoted pricing and make credit consumption atomic with approval.

- [ ] **Step 2: Author approval Flow**

Use `b2b_approval_route` to auto-approve, wait for a fixed approver role,
escalate to finance, or reject on deadline.

---

### Task 8: Author marketplace payout and dispute

**Files:**

- Create: `commands/marketplace/split-vendor-orders.yaml`
- Create: `commands/marketplace/record-vendor-acceptance.yaml`
- Create: `commands/marketplace/create-vendor-payout.yaml`
- Create: `commands/marketplace/record-payout-outcome.yaml`
- Create: `commands/marketplace/open-vendor-dispute.yaml`
- Create: `flows/vendor-payout.yaml`
- Create: `connectors/mock-payout.yaml`

**Interfaces:**

- Vendors are single-tenant domain owners with role `vendor`.
- Commission is selected by `marketplace_commission_route`.
- Payout fan-out is keyed by `vendor_id`.

- [ ] **Step 1: Author vendor Commands**

Split immutable vendor-order balances, record acceptance, calculate commission,
and create payout candidates without exposing another vendor's rows.

- [ ] **Step 2: Author payout Connector and Flow**

Fan out bounded provider requests, collect per-vendor outcomes, record them
idempotently, and wait for support reconciliation on mismatches.

---

### Task 9: Author booking and prescription review

**Files:**

- Create: `commands/booking/reserve-grooming-slot.yaml`
- Create: `commands/booking/confirm-booking.yaml`
- Create: `commands/booking/reschedule-booking.yaml`
- Create: `commands/booking/cancel-booking.yaml`
- Create: `commands/booking/record-no-show.yaml`
- Create: `flows/grooming-booking.yaml`
- Create: `commands/prescription/submit-prescription-review.yaml`
- Create: `commands/prescription/approve-prescription.yaml`
- Create: `commands/prescription/reject-prescription.yaml`
- Create: `commands/prescription/expire-prescription.yaml`
- Create: `flows/prescription-review.yaml`

**Interfaces:**

- Slot reservations use one stable slot key and reject double booking.
- Prescription reviewers use fixed role `veterinary_reviewer`.

- [ ] **Step 1: Author booking Command and Flow lifecycle**

Reserve atomically, wait for confirmation, expire holds, and express
reschedule/cancel/no-show as guarded transitions.

- [ ] **Step 2: Author prescription Command and Flow lifecycle**

Wait for one verified reviewer signal, record one immutable decision, expire
on deadline, and expose only safe customer output.

---

### Task 10: Author operations, reconciliation, and notifications

**Files:**

- Create: `commands/operations/route-fraud-review.yaml`
- Create: `commands/operations/resolve-fraud-review.yaml`
- Create: `commands/operations/record-notification-delivery.yaml`
- Create: `flows/payment-reconciliation.yaml`
- Create: `connectors/mock-notification.yaml`

**Interfaces:**

- Support can resolve review but cannot call payment ingress Commands as an
  arbitrary role.
- Notification delivery records contain normalized status, not credentials or
  unrestricted provider bodies.

- [ ] **Step 1: Author fraud and notification Commands**

Use `fraud_route`, immutable decision audit, provider message ID, and replay
protection.

- [ ] **Step 2: Author reconciliation Flow**

Auto-resolve exact matches and wait for support input on mismatched amount,
currency, status, or provider reference.

- [ ] **Step 3: Author notification Connector**

Declare email and webhook operations with stable message keys, retry,
capacity, and redacted credentials.

---

### Task 11: Close and document the YAML contract

**Files:**

- Modify: `examples/petshop/README.md`

**Interfaces:**

- README names every active module and clearly states that runtime support is
  intentionally incomplete.

- [ ] **Step 1: Verify include closure**

Compare every YAML file below `commands/`, `flows/`, and `connectors/` against
its index. Reject missing, duplicate, or orphaned includes.

- [ ] **Step 2: Scan forbidden surfaces**

Run:

```bash
rg -n "tenant_id|admin|run_sql|raw_sql|script:|javascript:|wasm:|dynamic_role" \
  examples/petshop/metadata
```

Expected: no matches except explanatory comments, which should be removed from
active metadata.

- [ ] **Step 3: Perform document-only verification**

Run:

```bash
git diff --check
rg -n "T[B]D|TO[D]O|FI[X]ME" \
  examples/petshop/metadata examples/petshop/README.md
```

Expected: both commands succeed with no findings. Do not claim Petshop runtime
or conformance is green.

- [ ] **Step 4: Commit the active YAML contract**

```bash
git add examples/petshop/metadata examples/petshop/README.md
git commit -m "feat(petshop): declare complete YAML pressure suite"
```

The commit message body must state that the active example intentionally leads
runtime support and that tests/Rust follow only after user YAML review.
