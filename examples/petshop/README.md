# Petshop example

A classic pet-store running on **donat** — a small catalogue of pets in
categories, customers, and their orders — wired up with the permission set a
normal store needs: a public catalogue, authenticated shoppers, and store
staff. Every access goes through an explicit role permission — there is no
admin role.

```
docker compose up
```

All services use the same prebuilt public engine image
(`ghcr.io/donatlabs/donat`, published by the release workflow) and follow
the project's deploy model:

1. **`migrate`** — `donat migrate` applies the versioned DDL in
   [`migrations/`](migrations) (one `V{n}__create_<table>.sql` per table) via
   refinery, tracked in `refinery_schema_history`. This is the only thing that
   runs DDL.
2. **`validate`** — `donat validate` loads the [`metadata/`](metadata),
   introspects the migrated database, and exits non-zero if anything tracked
   is missing, so a bad deploy fails before the server boots.
3. **`engine`** — serves the data plane over three transports, all sharing the
   same per-role permissions and auth: GraphQL at
   <http://localhost:8080/v1/graphql>, RESTified endpoints under
   <http://localhost:8080/api/rest/> (see [REST endpoints](#rest-endpoints)),
   and an MCP server at <http://localhost:8080/mcp> (see [MCP](#mcp)). The
   schema (tables + foreign keys) comes from the migrated database; the
   metadata directory adds relationships, the per-role permissions below, the
   saved queries in [`metadata/query_collections.yaml`](metadata/query_collections.yaml),
   and the REST routes in [`metadata/rest_endpoints.yaml`](metadata/rest_endpoints.yaml).
   The serving engine never runs DDL and exposes no runtime `run_sql`.
   All three surfaces are on by default; restrict them at deploy time with
   `DONAT_GRAPHQL_ENABLED_APIS` (comma-separated `graphql`/`rest`/`mcp`), e.g.
   `DONAT_GRAPHQL_ENABLED_APIS=graphql` to expose GraphQL only (REST and MCP
   then return `404`).

> The image is built and pushed only on release tags (`v*`). Before the first
> release exists, build it locally from the repo root instead:
> `docker build -t ghcr.io/donatlabs/donat:latest .`
> (The image needs the `migrate`/`validate` subcommands, so build from a
> revision that includes them.)

## Data model

| Table        | Purpose                                            |
|--------------|----------------------------------------------------|
| `category`   | Catalogue sections (Dogs, Cats, …)                 |
| `pet`        | Items for sale, with `status` available/pending/sold |
| `customer`   | Shoppers; `id` is the `X-Donat-User-Id` value     |
| `orders`     | A customer's order with a fulfilment `status`      |
| `order_item` | Line items linking an order to pets                |

Relationships: `pet.category`, `category.pets`, `orders.customer`,
`customer.orders`, `orders.items`, `order_item.order`, `order_item.pet`.

## Roles

| Role        | Who                | Can do                                                                 |
|-------------|--------------------|-----------------------------------------------------------------------|
| `anonymous` | unauthenticated    | Browse categories and **available** pets only. No customer/order data.|
| `customer`  | a logged-in shopper| See own profile/orders, browse available pets, place orders for self.  |
| `staff`     | store employee     | Full inventory CRUD, read every customer/order, update order status.   |

There is **no admin role**: every request runs as one of the roles above,
each scoped by its explicit permissions. `anonymous` is the
`DONAT_GRAPHQL_UNAUTHORIZED_ROLE` — any request with no/role-less auth falls
back to it. The secret `petshop-secret` (see `docker-compose.yml`) marks a
request as *trusted* so it may assert a role via the `X-Donat-Role` header (a
demo stand-in for edge auth); a trusted request must still name a role. In
production, issue JWTs instead of passing roles by hand.

## Try it

All examples below `POST` to `http://localhost:8080/v1/graphql`.

### Public catalogue (anonymous)

No headers needed — only the 4 available pets come back; `Nemo` (sold) and
`Shadow` (pending) are filtered out by the permission.

```bash
curl -s localhost:8080/v1/graphql -H 'content-type: application/json' -d '{
  "query": "{ category { name pets { name price status } } }"
}'
```

### Shopper (customer, impersonated as customer id 1)

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' \
  -H 'x-donat-role: customer' \
  -H 'x-donat-user-id: 1' \
  -d '{ "query": "{ customer { name email orders { id status items { quantity pet { name } } } } }" }'
```

Returns only customer `1`'s own profile and orders — `customer 2`'s data is
invisible. Browsing `pet` still shows only available pets.

Place an order (the `customer_id` is forced to the session user by a preset, so
shoppers cannot order on someone else's behalf):

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' \
  -H 'x-donat-role: customer' \
  -H 'x-donat-user-id: 1' \
  -d '{ "query": "mutation { insert_orders_one(object: {status: \"placed\"}) { id customer_id status } }" }'
```

### Store staff

Staff see every pet (including sold/pending) and every order, and can change an
order's fulfilment status:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' \
  -H 'x-donat-role: staff' \
  -d '{ "query": "mutation { update_orders(where: {id: {_eq: 1}}, _set: {status: \"shipped\"}) { affected_rows } }" }'
```

> A request with **no** role — even with the secret — runs as `anonymous`
> (the unauthorized-role fallback); there is no admin role or bypass. To read
> across all customers, ask as `staff`. (If `DONAT_GRAPHQL_UNAUTHORIZED_ROLE`
> were unset, a trusted role-less request would instead be rejected with
> `x-donat-role header is required`.)

## REST endpoints

The same data is also reachable over Donat **RESTified endpoints**: each route
in [`metadata/rest_endpoints.yaml`](metadata/rest_endpoints.yaml) maps an HTTP
method + URL template to a saved GraphQL operation in
[`metadata/query_collections.yaml`](metadata/query_collections.yaml). They run
through the *same* permission system as GraphQL — no admin bypass — so the rows
you get depend on your role. Path params, query-string keys, and JSON-body keys
bind the operation's GraphQL variables (precedence: path > query > body). A
successful call returns the GraphQL `data` object directly.

| Method & URL                  | Saved query     | Notes                                    |
|-------------------------------|-----------------|------------------------------------------|
| `GET /api/rest/pet/:id`       | `PetById`       | One pet; available-only for shoppers     |
| `GET /api/rest/pets?limit=N`  | `AvailablePets` | The catalogue the role may browse        |
| `GET /api/rest/categories`    | `Categories`    | Categories with their visible pets        |
| `POST /api/rest/pet`          | `CreatePet`     | Add inventory (staff only); body → vars  |

Browse the catalogue as the public (no headers → `anonymous`):

```bash
curl -s 'localhost:8080/api/rest/pets?limit=3'
# {"pet":[{"id":1,"name":"Rex",...},{"id":2,...},{"id":3,...}]}
```

The permission travels with the route — `Shadow` (pending) is hidden from the
public but visible to staff:

```bash
curl -s localhost:8080/api/rest/pet/4
# {"pet_by_pk":null}

curl -s localhost:8080/api/rest/pet/4 \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff'
# {"pet_by_pk":{"id":4,"name":"Shadow","status":"pending",...,"category":{"name":"Cats"}}}
```

Add a pet (staff only — the same call as `anonymous` comes back with a
`validation-failed` error and changes nothing):

```bash
curl -s localhost:8080/api/rest/pet \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff' \
  -d '{"name":"Coco","category_id":3,"price":45,"status":"available","description":"Talkative parrot"}'
# {"insert_pet":{"affected_rows":1,"returning":[{"id":7,"name":"Coco",...}]}}
```

An unknown route is `404`; a known route called with the wrong method is `405`.

## MCP

The engine also speaks the **Model Context Protocol** over streamable HTTP at
`POST /mcp` (JSON-RPC 2.0, JSON mode), so an LLM client can read and write the
store under a role. It exposes six generic, table-parameterized tools —
`list_tables`, `describe_table`, `query`, `insert`, `update`, `delete` — each of
which runs as the request's role through the same permission system (a tool
call lacking permission comes back as `isError`, never a bypass).

Point an HTTP-capable MCP client at `http://localhost:8080/mcp` and send the
role headers with each request (here the demo secret + `X-Donat-Role`; in
production, a JWT). List the tools:

```bash
curl -s localhost:8080/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

Query the inventory as staff (arguments are passed as GraphQL variables — a
`where` filter, `order_by`, `limit`, …):

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"query",
        "arguments":{"table":"pet","columns":["id","name","status"],
                     "where":{"status":{"_eq":"pending"}},"order_by":{"id":"asc"}}}}'
# result.structuredContent: [{"id":4,"name":"Shadow","status":"pending"}]
```

Insert as staff (`update`/`delete` take a `where` + `set` the same way):

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' -H 'x-donat-role: staff' \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
        "name":"insert",
        "arguments":{"table":"pet","objects":[{"name":"Milo","category_id":2,"price":80,"status":"available"}],
                     "returning":["id","name"]}}}'
# result.structuredContent: {"affected_rows":1,"returning":[{"id":8,"name":"Milo"}]}
```

`list_tables` reports only what the role may touch — as `staff` it lists every
table with its allowed operations; as `anonymous` it shows just the catalogue.

## Active declarative YAML contract

The YAML under [`metadata/`](metadata) is an active **pressure-suite
contract**, intentionally ahead of the current runtime and generated schema.
It is not currently runnable and is not conformance-green; in particular, this
section does not claim that Rust support exists. It documents the declarative
surface under review so that implementation can follow a stable user contract.

### Commands

Commands are synchronous database transactions. They perform domain writes and
may atomically persist `effects.start_process` or `effects.signal_process`
intent with those writes in one generic transactional outbox. They do not call
providers directly.

| Domain | Active commands |
| --- | --- |
| Checkout | `prepare_checkout_quote`, `begin_checkout`, `finalize_declined_checkout`, `cancel_order`, `materialize_cancellation_authorization`, `finalize_pending_order_cancellation`, `request_authorized_order_cancellation`, `finalize_authorized_order_cancellation`, `release_expired_checkout` |
| Payments | `record_payment_outcome`, `authorize_payment`, `claim_payment_captures`, `release_absent_capture_claim`, `capture_payment`, `void_authorization`, `complete_refund`, `record_chargeback`, `reconcile_payment` |
| Fulfilment | `allocate_inventory`, `mark_order_packed`, `create_shipment`, `record_shipment_result`, `record_delivery` |
| Returns | `request_return`, `approve_return`, `reject_return`, `receive_return`, `record_return_inspection`, `finalize_return_refund`, `finalize_return_rejection`, `create_exchange` |
| Subscriptions | `create_subscription_order`, `record_renewal_outcome`, `pause_subscription`, `cancel_subscription` |
| B2B | `submit_quote`, `open_approver_review`, `approve_purchase`, `reject_purchase`, `consume_credit`, `escalate_purchase_approval`, `finance_approve_purchase`, `finance_reject_purchase`, `finalize_finance_rejection`, `finalize_unroutable_rejection` |
| Marketplace | `split_vendor_orders`, `record_vendor_acceptance`, `create_vendor_payout`, `record_payout_outcome`, `reconcile_vendor_payout`, `open_vendor_dispute` |
| Booking | `reserve_grooming_slot`, `confirm_booking`, `reschedule_booking`, `cancel_booking`, `expire_booking_hold`, `record_no_show` |
| Prescription | `submit_prescription_review`, `approve_prescription`, `reject_prescription`, `expire_prescription` |
| Operations | `route_fraud_review`, `resolve_fraud_review`, `resolve_payment_reconciliation`, `record_notification_delivery` |

### Processes and flows

Only a definition declaring `kind: process` is durable. A durable process
persists its state, signal inbox, activity attempts, and timers, so it can
recover after a crash. A command effect is consumed from the transactional
outbox to start or signal that process; the command itself remains synchronous.

| Module | Current intent |
| --- | --- |
| `checkout_payment` | Immutable pricing/tax/shipping quote snapshots and synchronous provider authorization with ambiguity lookup. |
| `checkout_cancellation` | Claim a pending authorization race, prove provider absence or materialize/void the authorization, then release reservations. |
| `authorized_order_cancellation` | Void an authorized payment before releasing reservations and finalizing cancellation. |
| `partial_fulfilment` | Allocate, pack, label, ship, and capture per fulfilment unit. |
| `return_refund` | Support approval, return label, receipt, inspection, refund, exchange, or rejection. |
| `subscription_renewal` | Renewal authorization with dunning timers and a terminal pause. |
| `b2b_order_approval` | Quote routing, automatic credit use, and approver/finance waits. |
| `vendor_payout` | Create bounded vendor payouts and record synchronous terminal provider outcomes per vendor. |
| `grooming_booking` | Reserve a grooming slot and await confirmation, cancellation, or hold expiry. |
| `prescription_review` | Submit a prescription review and await the recorded decision or expiry. |
| `payment_reconciliation` | Retrieve provider evidence, reconcile it, and await manual resolution when needed. |

All eleven active flow modules declare `kind: process` and `version: 1`. The YAML
remains the target contract, not an assertion that these modules execute today.

### Connectors

All active connectors are HTTP declarations with bounded retries, capacity
limits, redaction, and idempotency policy:

| Connector | Operations |
| --- | --- |
| `mock_payment` | `authorize`, `capture`, `lookup_capture`, `void`, `refund`, `reconcile`, `lookup_operation` |
| `mock_carrier` | `quote`, `create_label`, `create_return_label`, `lookup_label`, `track` |
| `mock_tax` | `quote_order` |
| `mock_notification` | `send_email`, `send_webhook` |
| `mock_payout` | `create_payout`, `lookup_payout` |

HTTP delivery is at-least-once at the provider boundary. Each mutation
declares `ProviderIdempotent` evidence for its fixed header binding, provider
scope, minimum key-retention window, and positive clock-safety margin. Every
side-effect step cites immutable Donat-owned facts in
[`provider-evidence/mock-providers-v1.yaml`](provider-evidence/mock-providers-v1.yaml).
The Process compiler derives each activity's `maximum_send_horizon_ms` from
its timeout and retry policy and checks it against provider retention minus
the safety margin; the horizon is not provider-supplied evidence.
Retries and worker takeover reuse the stable key. Ambiguous payment, carrier
label, capture, and payout mutations enter explicit read-only lookup states;
money or inventory stays claimed when lookup cannot prove the terminal effect
or its absence. A provider-proven terminal capture absence releases exactly
that shipment's capture claim; an unproven or failed lookup remains a bounded
manual-reconciliation failure.

### Rules and decision tables

`metadata/rules.yaml` defines typed enums/states for payment, approval,
inspection, booking, reconciliation, checkout, shipment, return,
subscription, payout, prescription, and fraud, plus typed return-line objects.
Its active Rules are:

- Arithmetic, inventory, and bounds: `can_reserve_stock`, `add_int`,
  `is_single_currency`, `subtract_int`, `add_minor`, `subtract_minor`,
  `basis_points_amount`, `can_release_stock`, `bounded_fan_out_count`.
- Payments and reconciliation: `payment_was_authorized`,
  `normalize_payment_outcome_state`, `can_authorize_payment_amount`,
  `can_reserve_capture_amount`, `can_complete_claimed_capture`,
  `can_claim_authorization_void`, `can_void_authorization`,
  `can_refund_payment_amount`, `approved_refund_matches_provider`,
  `can_record_chargeback_amount`, `reconciliation_state_for_decision`,
  `payment_reconciliation_supports_decision`.
- Lifecycle gates: `can_transition_checkout`, `can_transition_payment`,
  `can_transition_shipment`, `can_transition_return`,
  `can_transition_subscription`, `can_transition_approval`,
  `can_transition_payout`, `can_record_provider_payout_outcome`,
  `can_transition_booking`, `can_transition_prescription`,
  `can_transition_fraud`, `fraud_state_for_route`,
  `can_transition_reconciliation`.
- Returns and B2B: `return_approval_matches_request`,
  `return_approved_quantity_is_bounded`, `return_receipt_matches_approval`,
  `return_received_quantity_is_bounded`, `return_inspection_matches_receipt`,
  `return_inspected_quantity_is_bounded`, `return_refund_amount_is_bounded`,
  `return_is_exchange_eligible`, `return_decision_was_approved`,
  `approval_was_approved`, `approval_was_rejected`, `can_consume_credit`.

The first-match decision tables are `price_list_route`, `promotion_route`,
`tax_route`, `shipping_service_route`, `inventory_location_route`,
`b2b_approval_route`, `marketplace_commission_route`,
`return_disposition_route`, `dunning_schedule`, and `fraud_route`.

### B2B producer/consumer example

The following abbreviated form shows the intended hand-off. The quote Command
commits its domain change and a start intent; the approving Command commits its
domain change and a signal intent; the durable Process consumes that signal
while waiting. Both effects are transactional-outbox intent, not immediate
Process calls. This is declarative contract documentation, not a runnable
request today.

```yaml
# commands/b2b/submit-quote.yaml (producer: start intent)
name: submit_quote
effects:
  - start_process:
      process: b2b_order_approval
      process_key: { step: approval, column: id }
      input:
        quote_id: { step: quote, column: id }
        approval_id: { step: approval, column: id }
        total_minor: { step: quote, column: total_minor }
        available_credit_minor: { step: quote, column: available_credit_minor }
        owner_user_id: { step: cart, column: customer_id }
      idempotency_key: { argument: request_id }

# commands/b2b/approve-purchase.yaml (producer: signal intent)
name: approve_purchase
effects:
  - signal_process:
      process: b2b_order_approval
      signal: approver_decision
      correlate:
        approval_id: { step: approve, column: id }
      payload:
        decision: { literal: approved }
      idempotency_key: { argument: request_id }

# flows/b2b-order-approval.yaml (consumer)
name: b2b_order_approval
kind: process
version: 1
start:
  command: submit_quote
  input:
    quote_id: { command_result: quote_id }
    approval_id: { command_result: approval_id }
    total_minor: { command_result: total_minor }
    available_credit_minor: { command_result: available_credit_minor }
    owner_user_id: { command_result: owner_user_id }
  idempotency_key: { command_argument: request_id }
  process_key: { command_result: approval_id }
states:
  - id: open_approver_review
    command:
      name: open_approver_review
      run_as: b2b_finance
      next: await_approver
  - id: await_approver
    wait:
      signal: approver_decision
      role: b2b_approver
      verification: required
      correlate:
        approval_id: { input: approval_id }
      deadline: 2d
      next: route_approver_decision
```

## Reset

```bash
docker compose down -v   # also drops the seeded database volume
```
