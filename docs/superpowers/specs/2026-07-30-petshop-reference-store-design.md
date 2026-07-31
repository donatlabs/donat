# Petshop reference store design

Date: 2026-07-30

Decision: approved product-first direction — turn Petshop into a conventional
pet-supplies store and derive the minimum declarative runtime from executable
business scenarios.

## Purpose

The existing `examples/petshop` is a permission-aware CRUD demonstration. It
does not yet represent a safe store: a customer creates an empty order and
then supplies line prices, inventory is a three-value `pet.status`, order
transitions are not guarded, and payment, fulfilment, cancellation, and refund
records do not exist.

The replacement is a reference SaaS application, not a second application
framework. It must exercise Donat through its public GraphQL, REST, MCP, and
connector-webhook surfaces while remaining one Rust binary backed by Postgres.
There is no UI, application microservice, runtime plugin, arbitrary code step,
admin role, or permission bypass.

## Chosen scope

Petshop becomes a conventional pet-supplies store:

- products have independently purchasable variants and unique SKUs;
- stock is quantitative and can be reserved without overselling;
- a customer owns one open cart and edits its lines through ordinary
  permission-aware CRUD/upsert;
- the server, never the customer, calculates prices and order totals;
- checkout atomically freezes order lines, reserves stock, and records a
  pending payment under an idempotency key;
- payment is exercised through real HTTP requests to a provider-neutral mock
  endpoint, not through Stripe-specific code;
- fulfilment, cancellation, and refund are explicit state transitions;
- all customer reads and commands are owner-scoped;
- staff, support, and fulfilment capabilities are separate explicit roles.

The implementation is split into two independently testable subprojects.

1. **Store core:** catalog, cart, stock, atomic checkout, order/payment state,
   fulfilment, cancellation, and refund commands.
2. **Mock payment flow:** durable outbound HTTP, retry and idempotency,
   provider callback, deadline, recovery, and safe run visibility.

The store core is implemented and reviewed first. Its failing product cases
are the requirements for the mock payment flow; the earlier broad process
proposal is not implemented wholesale.

## Reused Donat surfaces

Ordinary data access remains on the existing data plane:

- GraphQL supplies permission-filtered catalog, cart, order, and staff views;
- REST routes are saved GraphQL operations, not a separate execution path;
- MCP keeps generic CRUD for explicitly permitted operator roles and exposes
  business commands through the same GraphQL planning path;
- declarative Commands own atomic multi-table business changes;
- Rules own deterministic validation and state-transition decisions;
- the compiled HTTP connector owns the outbound mock request;
- connector ingress owns callback authentication and duplicate suppression.

Petshop does not introduce an entity DSL, repository layer, service objects,
or Petshop-specific Rust handlers.

## Domain model

All money is stored as signed 64-bit integer minor units plus an ISO 4217
currency code. The first fixture uses `USD`; floating-point money is forbidden.
Timestamps are `timestamptz`. Public IDs and idempotency keys are UUIDs.

### Store-core relations

| Relation | Responsibility |
| --- | --- |
| `category` | Stable catalog grouping with unique slug. |
| `product` | Product identity, copy, publication state, and category. |
| `product_variant` | Purchasable SKU, option title, price minor units, currency, and active flag. |
| `inventory_stock` | One row per variant with `on_hand` and `reserved` quantities. |
| `customer` | Shopper identity keyed by the authenticated session user. |
| `customer_address` | Owner-scoped shipping/billing address. |
| `cart` | One open cart per customer and currency. |
| `cart_line` | Variant and requested quantity; no client-owned price. |
| `orders` | Immutable customer/currency/totals plus lifecycle states. |
| `order_line` | Frozen SKU, title, unit price, quantity, and line total. |
| `inventory_reservation` | Order/variant quantity, expiry, and release/commit state. |
| `payment` | Provider-neutral amount, state, stable request key, and provider reference. |
| `payment_event` | Unique provider event and normalized accepted outcome. |
| `shipment` | Fulfilment state and tracking data. |
| `refund` | Requested and completed refund amounts and state. |

`inventory_stock` enforces `on_hand >= 0`, `reserved >= 0`, and
`reserved <= on_hand`. Order, payment, shipment, reservation, and refund state
columns use database check constraints in addition to declarative Rules.
Order lines and totals are immutable after checkout.

### State ownership

- customer CRUD may edit only the customer's open cart and line quantity;
- customer commands may begin checkout and request cancellation before
  fulfilment;
- payment outcomes enter only through the declared mock-payment ingress;
- fulfilment commands belong only to the `fulfilment` role;
- paid cancellation/refund commands belong to `support`;
- `staff` manages the catalog and reads operational projections but is not a
  universal role;
- no role may directly mutate lifecycle columns through generated CRUD.

## Business commands

Cart creation and line editing remain ordinary GraphQL CRUD/upsert. Insert and
update permissions follow the cart relationship back to
`X-Donat-User-Id`; customers may write only `variant_id` and `quantity`.
Database constraints reject non-positive quantity, and an insert permission
rejects unpublished/inactive variants. A pricing view computes current cart
totals without persisting a client-owned price.

The command vocabulary starts where an operation crosses relations or changes
an irreversible lifecycle:

| Command | Role | Atomic result |
| --- | --- | --- |
| `begin_checkout` | customer | Reprices the cart, reserves stock, freezes one order, and creates one pending payment. |
| `record_payment_outcome` | fixed payment ingress | Applies one unique mock-provider event. |
| `expire_checkout` | fixed worker role | Releases an unpaid reservation exactly once. |
| `mark_order_packed` | fulfilment | Moves a paid order into fulfilment. |
| `mark_order_shipped` | fulfilment | Records shipment/tracking exactly once. |
| `cancel_order` | customer or support | Cancels an eligible order and releases uncommitted stock. |
| `complete_refund` | fixed payment ingress | Records a bounded refund and terminal payment state. |

Every command remains source-local and one Postgres statement. Checkout uses
the authenticated customer ID from the session, recomputes prices from current
variant rows, and uses `(customer_id, request_id)` as its idempotency scope.
The same key with the same canonical input returns the original result; the
same key with changed input is rejected.

In the Store Core subproject, `begin_checkout` returns `order_id` and
`payment_id` and leaves the payment in `pending`. The Mock Payment Flow
subproject then adds the atomic durable start effect and `run_id` without
changing checkout's pricing, reservation, or idempotency semantics.

The existing Phase-1 Command grammar cannot safely implement a multi-line
checkout: `select_one`, `update`, and `delete` are primary-key/single-row
operations, and an update cannot express a guarded arithmetic stock change.
The store-core plan must therefore begin with failing Petshop cases and add
only the set-based command capability those cases require. It must not add
free-form SQL, a generic query DSL, or a function-call escape hatch.

The required extension is a bounded relational batch:

- select an ordered row set from one tracked table or view under the command
  role's permissions;
- reduce that row set to one typed summary with the closed operations
  `sum`, `count`, `min`, `max`, and `count_distinct`, without grouping or an
  arbitrary expression;
- use that row set as the input to later insert/update steps;
- apply typed arithmetic assignments and rule guards to the target row;
- require an exact affected-row count for every inventory item;
- preserve the one-statement CTE and in-database JSON result invariant.

The exact metadata grammar is specified by the failing Store Core plan tests,
then recorded as an amendment to Spec 003 before its implementation lands.

## Mock payment contract

No live payment provider is called. CI starts an in-process recording HTTP
server owned by the conformance harness. The example accepts a configured base
URL so a developer may point it at RequestBin, webhook.site, or an equivalent
request-capture/mock endpoint.

The outbound contract is:

```http
POST /payments
Idempotency-Key: <stable activity key>
Content-Type: application/json

{
  "order_id": "<uuid>",
  "amount_minor": 1299,
  "currency": "USD",
  "callback_url": "<declared connector ingress>"
}
```

A successful mock response is:

```json
{
  "payment_id": "mock_pay_001",
  "status": "pending"
}
```

The callback contains a unique `event_id`, the `payment_id`, and one of
`paid`, `failed`, or `refunded`. CI signs it with a fixture-only shared secret;
production-grade provider authentication remains connector-specific. Invalid
authentication changes no business state. A duplicate event is acknowledged
without a second transition.

The first payment flow has the closed shape:

```text
begin_checkout
  -> HTTP create payment
  -> wait for callback or payment deadline
  -> paid: record_payment_outcome
  -> failed/deadline: release reservation and fail
```

The request executes only after the checkout transaction commits. Attempts are
at least once with one stable provider idempotency key. A database transaction
is never held across HTTP. Restart after remote acceptance converges through
the same key. A callback that arrives before the worker registers its wait is
persisted and later consumed; it is never discarded as an unexpected state.

## Public data flow

1. An anonymous shopper reads published products and available variants.
2. An authenticated customer creates a cart and edits lines using
   permission-aware CRUD/upsert.
3. `begin_checkout` validates ownership, current prices, and stock in one SQL
   statement. Store Core returns `order_id` and `payment_id`; Mock Payment Flow
   extends the same command result with the durable run handle.
4. The same durable run performs the mock HTTP request. An attached caller may
   wait for a bounded interval; disconnect does not cancel the run.
5. The mock callback or deadline selects exactly one terminal route.
6. Customer status reads expose only a safe owner-scoped projection. Internal
   connector bodies, credentials, leases, and journal payloads are never
   returned.
7. Fulfilment and support continue the order through declared commands.

REST and MCP adapters invoke the same GraphQL fields. They do not create
parallel checkout implementations or call the engine over loopback HTTP.

## Failure contract

- invalid quantity, unpublished SKU, stale price, empty cart, and illegal
  state transition are deterministic `validation-failed` command errors;
- insufficient stock rejects the complete checkout and writes no order,
  reservation, payment, or run;
- concurrent checkout for the last stock permits exactly one winner;
- HTTP transport, timeout, `429`, and `5xx` failures follow the declared retry
  policy with the same idempotency key;
- validation/authentication/permanent failures are not retried;
- retry exhaustion or payment deadline releases reservations once;
- a late success after cancellation or expiry is retained for audit and cannot
  silently resurrect the order;
- customer B cannot read, cancel, or attach to customer A's checkout;
- no error or log contains credentials, raw callback secrets, or unrestricted
  provider bodies.

## Acceptance suites

The store is tested through public surfaces, not by calling Petshop-specific
Rust functions.

### Store core

1. variant/SKU publication and role-filtered catalog reads;
2. cart add/coalesce/update/remove with server-computed totals;
3. unavailable variant and invalid quantity reject atomically;
4. two customers race for the final stock and exactly one reserves it;
5. checkout freezes prices and creates one order on idempotent replay;
6. changed input under the same key conflicts;
7. direct customer mutation of price, total, stock, or lifecycle state is
   absent from schema;
8. paid-only fulfilment, bounded cancellation, and refund transitions.

### Mock payment flow

1. the recorder observes the exact request and stable idempotency header;
2. timeout/`500`/`200` retries reuse the same key;
3. callback authentication, duplicate suppression, and owner-scoped status;
4. callback-before-wait registration is consumed once;
5. deadline survives restart and releases stock once;
6. crash after remote acceptance converges without a second payment;
7. GraphQL, REST, and MCP expose one shared order/run result contract.

Tests use database-clock seeding and explicit worker hooks rather than
wall-clock sleeps. Narrow SQL assertions may inspect domain rows and journal
counts, but public JSON/status assertions remain the primary contract.

## Upstream behavior references

No upstream source or fixture is copied by this design. Donat-owned tests may
independently reproduce behavior observed in these permissive projects:

| Project | Immutable revision | Behavior inventory |
| --- | --- | --- |
| Medusa, MIT | `5b732d40ee78e4c9973fdb1e0ac247b319611f51` | variants, carts, reservations, checkout, payment sessions, fulfilment, and RMA |
| Saleor, BSD-3-Clause | `8e6164e3d12327496660f91f836d5c3222d8d2b6` | checkout completion, payment outcomes, fulfilment, cancellation, and webhook retry |
| Spree, BSD-3-Clause source | `b839535c9e634d61196b5ab341cd2b1ec062526c` | cart services, order state machine, fulfilment, and webhook idempotency |

Vendure revision `cefe2a5fb4be8085bb49448b338dc96fc65e9021`
is GPLv3/commercial and is excluded from source and fixture porting. It may be
used only as a documentation-level behavior reference.

Before any upstream bytes are copied, the authoritative reference-porting
register must record the exact upstream file, content hash, license/notice,
destination, and failing-first Donat test.

## Explicit non-goals

- storefront UI, authentication provider, tax engine, promotions, multi-store,
  multi-currency conversion, multi-warehouse allocation, subscriptions, and
  marketplace settlement;
- arbitrary workflow loops, parallel/fan-in, child flows, general scripting,
  JavaScript, WASM, or runtime-loaded Rust;
- a second CRUD/query language;
- live Stripe credentials or a Stripe-specific Petshop dependency;
- public workflow administration, raw journal access, or cancellation bypass.
