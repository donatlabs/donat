# Petshop example

A pet store running on **donat** — a catalogue of products and their variants,
customers, carts and orders — wired up with the permission set a normal store
needs: a public catalogue, authenticated shoppers, and store staff. Every
access goes through an explicit role permission — there is no admin role.

```
docker compose up
```

All services use the same prebuilt public engine image
(`ghcr.io/donatlabs/donat`, published by the release workflow) and follow
the project's deploy model:

1. **`migrate`** — `donat migrate` applies two independently versioned sets of
   DDL through one `refinery_schema_history`: the engine's own schema from the
   repository's top-level [`migrations/`](../../migrations) — the `donat.*`
   tables holding durable Process journals, command claims and cron state,
   which ship in the repository rather than in the image — and this store's own
   [`migrations/`](migrations). They can share a history because both are
   versioned by timestamp; two sets of counters would both start at `V1`.
2. **`deploy`** — `donat migrate --metadata-dir … --source default` deploys the
   durable **Process** definitions. A Process revision is pinned in the
   database and the engine refuses to serve one that is not deployed as active,
   so without this step the engine boots into `revision … is not deployed as
   active` and retries forever.
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

A fourth service, **`mock-providers`**, answers the five external services the
connectors are declared against — payment, tax, carrier, payout and
notification — so the example runs end to end without an account anywhere. It
is a fixture, not a simulator: see
[`mock-providers/providers.py`](mock-providers/providers.py). Point the five
`*_BASE_URL` variables in the compose file at real providers and nothing else
changes; the metadata never carries an endpoint or a secret.

A fifth service, **`rauthy`**, is the identity provider. It is not optional:
this engine has no admin role and honours no role header on its own, so a role
reaches it from a verified JWT or an authentication hook and from nothing else.
A request carrying no token is `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` (`anonymous`)
— the public catalogue — whatever headers it sends.

So every example below carries a token. Get one:

```
TOKEN=$(curl -s -X POST localhost:8081/auth/v1/oidc/token \
  -d 'grant_type=password&client_id=petshop' \
  -d 'username=alice@example.com&password=petshop-demo-password' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"])')
```

`alice@example.com` and `bob@example.com` are shoppers, `sam@example.com` is
staff; all three share the demo password above. `X-Donat-Role` still appears
below, but only to *pick* between several roles one token carries — it can
never add one, and asking for a role the token does not carry is denied.

```
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -H 'X-Donat-Role: customer' \
  -d '{"query":"mutation { start_checkout(cart_id: 1, request_id: \"…\") { cart_id } }"}'
```

### How the login works

The provider is [Rauthy](https://github.com/sebadob/rauthy). donat itself has
no user store: it verifies the token against the provider's JWKS and turns its
claims into session variables. **Any OIDC provider works the same way** — point
`DONAT_GRAPHQL_JWT_SECRET`'s `jwk_url` / `issuer` / `audience` and `DONAT_OIDC`'s
two endpoints at yours and change nothing else. Providers disagree about which
token carries a deployment's roles and how a confidential client authenticates,
which is what `session_token` and `client_auth` are for.

The provider is configured entirely by the JSON files in
[`bootstrap/`](bootstrap), so there is nothing to click in its admin UI. They
declare the client, the `customer` and `staff` roles, three demo users, and the
`customer_id` attribute that carries each shopper's business id into the access
token.

Using a token with no role header at all:

```
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"query":"{ orders { customer_id order_status } }"}'
```

Alice sees only `customer-1`'s orders and Bob (`bob@example.com`) only
`customer-2`'s, because `X-Donat-User-Id` now arrives from the token and every
customer row filter compares it against `customer.customer_id`. Sam
(`sam@example.com`) has the `staff` role and sees all of them, with no extra
header: the role is read from the token as well. `X-Donat-Role` is only needed
to pick between several roles one token carries, and asking for a role the
token does not carry is denied.

Both role variables are mapped out of the token rather than written as
literals, and the difference matters if you copy this configuration. A
requested role is checked against the token's role set; a *default* role is
not. So a literal `x-donat-default-role` hands that role to every valid token,
including one whose claims never granted it.

The admin UI is at `localhost:8081` (`admin@petshop.local`, same password).

### Logging in from a browser

A browser cannot paste a bearer token into every request, so the engine serves
the login itself: `GET /auth/login` redirects to the provider
(`authorization_code` + PKCE), and `/auth/callback` puts the resulting token in
an `HttpOnly` cookie the engine then verifies exactly like a header token. It
stores no users, holds no passwords and issues no tokens of its own — see
[ADR 010](../../knowledgebase/api-surfaces/decisions/010-donat-does-not-own-identity.md).
That is what [`apps/ui`](../../apps/ui) signs in with.

Open <http://localhost:8080/auth/login> and sign in as `sam@example.com`; the
browser comes back holding a session. `/auth/logout` ends it.

`bootstrap/clients.json` registers two redirect URIs: the engine's own
`/auth/callback`, and `http://localhost:5173/callback` for an application that
runs the flow itself. Add yours there. The provider reads `bootstrap/` only
while initializing an empty database, so editing those files later does nothing
until you recreate the volume with `docker compose down -v`.

> The image is built and pushed only on release tags (`v*`). Before the first
> release exists, build it locally from the repo root instead:
> `docker build -t ghcr.io/donatlabs/donat:latest .`
> (The image needs the `migrate`/`validate` subcommands, so build from a
> revision that includes them.)

## Data model

| Table             | Purpose                                                       |
|-------------------|---------------------------------------------------------------|
| `category`        | Catalogue sections (Dogs, Cats, Reptiles)                      |
| `product`         | A catalogue entry with a `status` of draft/published/archived  |
| `product_variant` | What is actually bought: SKU, price, `active` flag             |
| `inventory_stock` | On-hand and reserved counts per variant                        |
| `customer`        | Shoppers; `customer_id` is the `X-Donat-User-Id` value         |
| `customer_address`| Delivery addresses owned by one customer                       |
| `cart`, `cart_line` | An open basket and its lines                                 |
| `orders`, `order_line` | A placed order and its priced lines                       |
| `payment`, `shipment`, `refund` | The money and fulfilment records              |

Relationships: `product.category`, `category.products`, `product.variants`,
`product_variant.stock`, `cart.lines`, `cart_line.variant`, `orders.customer`,
`customer.orders`, `orders.lines`.

Another 57 relations exist for the declarative domain below — quotes,
allocations, returns, subscriptions, vendor payouts. They are tracked inline in
[`tables.yaml`](metadata/databases/default/tables/tables.yaml) and reachable
only through Commands, never as generic CRUD roots.

## Roles

| Role        | Who                | Can do                                                                 |
|-------------|--------------------|-----------------------------------------------------------------------|
| `anonymous` | unauthenticated    | Browse categories and **published** products with their active variants. No customer, cart or order data. |
| `customer`  | a logged-in shopper| Own profile, addresses, cart and orders; browse the public catalogue.  |
| `staff`     | store employee     | Catalogue and inventory CRUD, read every customer and order.           |

The domain Commands add narrower worker roles — `fulfilment`, `support`,
`payment_worker`, `subscription_worker`, `veterinary_reviewer`, `vendor`,
`groomer` and others. Each is an ordinary explicit role with its own table
permissions; none of them is privileged.

There is **no admin role**: every request runs as one of the roles above,
each scoped by its explicit permissions. `anonymous` is the
`DONAT_GRAPHQL_UNAUTHORIZED_ROLE` — any request with no/role-less auth falls
back to it. The secret `petshop-secret` (see `docker-compose.yml`) marks a
request as *trusted* so it may assert a role via the `X-Donat-Role` header (a
demo stand-in for edge auth); a trusted request must still name a role. In
production, issue JWTs instead of passing roles by hand.

## Try it

All examples below `POST` to `http://localhost:8080/v1/graphql`.

The catalogue and cart-line requests are taken verbatim from the tests beside
the tables ([`public_product_test.yaml`](metadata/databases/default/tables/public_product_test.yaml),
[`public_cart_test.yaml`](metadata/databases/default/tables/public_cart_test.yaml)),
so their shapes are what CI asserts rather than what someone remembered. The others use
the same roles and permissions but are not themselves fixtures.

### Public catalogue (anonymous)

No headers needed. Only published products and active variants come back; the
draft `turtle-heat-lamp` and the inactive heat-lamp variant are filtered out by
the permission.

```bash
curl -s localhost:8080/v1/graphql -H 'content-type: application/json' -d '{
  "query": "{ product(where: {status: {_eq: \"published\"}}, order_by: {slug: asc}) { slug variants(order_by: {sku: asc}) { sku price_minor currency stock { available_quantity } } } }"
}'
```

### Shopper (customer, impersonated as `customer-1`)

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -H 'x-donat-role: customer' \
  -H 'x-donat-user-id: customer-1' \
  -d '{ "query": "{ customer { customer_id name email } orders { id order_status total_minor } }" }'
```

Returns only `customer-1`'s own profile and orders — `customer-2`'s rows are
invisible, and asking for them by id returns an empty list rather than an error.

Add a line to the cart. The upsert edits the existing line instead of creating
a second one, and the cart must be the caller's own and still open — both are
permission predicates, not application code:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -H 'x-donat-role: customer' \
  -H 'x-donat-user-id: customer-1' \
  -d '{ "query": "mutation { insert_cart_line(objects: [{cart_id: 1, variant_id: 1, quantity: 1}], on_conflict: {constraint: cart_line_cart_id_variant_id_key, update_columns: [quantity]}) { returning { cart_id variant_id quantity } } }" }'
```

### Store staff

Staff see every product, including drafts, and own the catalogue:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -H 'x-donat-role: staff' \
  -d '{ "query": "mutation { update_product(where: {slug: {_eq: \"turtle-heat-lamp\"}}, _set: {status: \"published\"}) { affected_rows } }" }'
```

> A request with **no** role — even with the secret — runs as `anonymous`
> (the unauthorized-role fallback); there is no admin role or bypass. To read
> across all customers, ask as `staff`. (If `DONAT_GRAPHQL_UNAUTHORIZED_ROLE`
> were unset, a trusted role-less request would instead be rejected with
> `x-donat-role header is required`.)

## Per-role value validators

A permission answers two different questions, and they are written separately.
`check` and `filter` decide **who may write which row**. A `validate` list
decides **what the value may be**, over the row as written — after presets and
after column defaults, because the gate reads the rows the statement returns
rather than the object that was submitted.

This is the rule a database `CHECK` cannot express, because a `CHECK` binds
every writer at once. `cart_line.quantity` keeps its `> 0` constraint in the
schema, for everyone. The ceiling belongs to one role:

```yaml
# metadata/databases/default/tables/public_cart_line.yaml
insert_permissions:
  - role: customer
    permission:
      check: { cart: { customer_id: { _eq: X-Donat-User-Id } } }
      columns: [cart_id, variant_id, quantity]
      validate:
        - expression: 'quantity <= 20'
          message: a cart line is limited to 20 units
```

A shopper asking for 21 gets that sentence back, with code `validation-failed`,
and nothing is written. A wholesale Command writing the same table is not a
shopper and does not inherit the limit.

### Nulls are declared, never inferred

Expressions are the rule profile from [`rules.yaml`](metadata/rules.yaml), which
refuses to read a nullable value — so `quality_grade > 3` on a nullable column
does not compile, and writing `is_null(quality_grade) || quality_grade > 3`
does not rescue it either: the second arm still reads a nullable value. Say
which one you mean:

```yaml
# public_product_variant.yaml — a null is refused, and named as a null
validate:
  - not_null: quality_grade
    message: quality_grade cannot be null
  - expression: 'quality_grade > 3'
    message: quality_grade must be greater than 3

# public_product.yaml — a null is fine; a value that is there must be usable
validate:
  - expression: 'size(description) >= 20'
    when_present: description
    message: description must be at least 20 characters when present
```

Entries run in document order and the first violated one is what the caller
reads. `not_null` also makes the comparison below it typeable; `when_present`
refines its column inside its own entry only. Forgetting either is a
**deployment** error naming the table, role and entry — `donat validate`
reports it and the engine refuses to serve, rather than failing a request
later.

Four properties hold regardless of how a list is written: a validator passes
only on TRUE, so an unknown value never satisfies one; a permission failure is
reported before any validator; an upsert is held to both the insert and the
update list; and a role that inherits a permission inherits its validators
with it. See
[ADR-032](../../knowledgebase/declarative-saas/decisions/032-permission-validators-declare-presence.md)
for what is deliberately refused instead of enforced.

## Attaching a file

`customer.avatar` is declared as a file column in
[`public_customer.yaml`](metadata/databases/default/tables/public_customer.yaml),
backed by the object store in [`storage.yaml`](metadata/storage.yaml) — MinIO
in this compose, any S3 in production. It carries
no role list: a customer may upload one because the table's update permission
lets it write that column, and may read one back because its select permission
exposes it.

Ask for a URL:

```sh
curl -s localhost:8080/v1/graphql \
  -H "Authorization: Bearer $TOKEN" \
  -H 'X-Donat-Role: customer' \
  -d '{"query":"mutation { donat_request_file_upload(attachment: public_customer_avatar, file_name: \"me.png\", media_type: \"image/png\", size: 1234) { id url method headers { name value } expires_at } }"}'
```

Upload the bytes to the returned `url` with the returned `method` and every
header it returned — the URL is presigned by the store and binds them — then
tell the engine the upload finished, and store the id like any other column
value:

```sh
curl -s -X PUT --data-binary @me.png \
  -H 'Content-Type: image/png' -H "Content-Length: $(stat -c%s me.png)" "<url>"
curl -s -X POST "<complete_url>"

curl -s localhost:8080/v1/graphql \
  -H "Authorization: Bearer $TOKEN" \
  -H 'X-Donat-Role: customer' \
  -d '{"query":"mutation { update_customer(where: {customer_id: {_eq: \"c-1001\"}}, _set: {avatar: \"<id>\"}) { affected_rows } }"}'
```

The update carries a gate in the same statement: an upload that is not this
session's, was already used, expired, or whose bytes never arrived fails the
mutation instead of storing a dangling id.

Reading the column gives an object rather than a bare id. `avatar` is declared
`public: true`, so its URL is stable, unsigned and immutable — a CDN in front of
`/v1/files/public/…` caches it forever, and a subscription on the customer never
sees it change. A private attachment would instead get a short-lived URL the
database signs while producing the row:

```sh
curl -s localhost:8080/v1/graphql \
  -H "Authorization: Bearer $TOKEN" \
  -H 'X-Donat-Role: customer' \
  -d '{"query":"{ customer { name avatar { id file_name size url } } }"}'
```

Clearing the column or deleting the row does not delete the object: a mutation
is database work and performs no file I/O. The background collector reclaims
objects nothing references any more, and uploads nobody ever claimed, on the
schedule in `storage.yaml` (one day by default).

Pointing this at a real S3 bucket changes only `storage.yaml`. Note the
`complete_url` step: it is not a formality. The bytes go straight from the
browser to the store, so the engine asks the store what it actually holds, then
moves the verified object out from under the presigned URL, which cannot be
revoked.

Two more things `storage.yaml` decides here: how many unclaimed uploads one
shopper may hold at a time, and which browser origins may upload directly. Both
are counted or checked by the engine, because neither is visible to a reverse
proxy.

## REST endpoints

The same data is also reachable over Donat **RESTified endpoints**: each route
in [`metadata/rest_endpoints.yaml`](metadata/rest_endpoints.yaml) maps an HTTP
method + URL template to a saved GraphQL operation in
[`metadata/query_collections.yaml`](metadata/query_collections.yaml). They run
through the *same* permission system as GraphQL — no admin bypass — so the rows
you get depend on your role. Path params, query-string keys, and JSON-body keys
bind the operation's GraphQL variables (precedence: path > query > body). A
successful call returns the GraphQL `data` object directly.

| Method & URL                     | Saved query      | Notes                                        |
|----------------------------------|------------------|----------------------------------------------|
| `GET /api/rest/products`         | `Products`       | The published catalogue the role may browse  |
| `GET /api/rest/products/:slug`   | `ProductBySlug`  | One published product by slug                |
| `GET /api/rest/cart`             | `Cart`           | The caller's own cart                        |
| `PUT /api/rest/cart/lines`       | `UpsertCartLine` | The permission-aware cart-line upsert        |
| `GET /api/rest/orders`           | `Orders`         | The caller's own orders                      |
| `GET /api/rest/orders/:id`       | `OrderById`      | One caller-visible order by UUID             |

Browse the catalogue as the public (no headers → `anonymous`):

```bash
curl -s 'localhost:8080/api/rest/products'
```

The permission travels with the route: the same URL as a shopper returns that
shopper's own cart, and as `anonymous` returns nothing to see — the route does
not carry an identity of its own.

```bash
curl -s localhost:8080/api/rest/cart \
  -H "Authorization: Bearer $TOKEN" \
  -H 'x-donat-role: customer' -H 'x-donat-user-id: customer-1'
```

A write goes through the same permissions, presets and validators as the
GraphQL mutation it wraps — the body keys bind the operation's variables:

```bash
curl -s -X PUT localhost:8080/api/rest/cart/lines \
  -H 'content-type: application/json' \
  -H "Authorization: Bearer $TOKEN" \
  -H 'x-donat-role: customer' -H 'x-donat-user-id: customer-1' \
  -d '{"cart_id":1,"variant_id":1,"quantity":2}'
```

An unknown route is `404`; a known route called with the wrong method is `405`.

## MCP

The engine also speaks the **Model Context Protocol** at `POST /mcp` (JSON-RPC
2.0, JSON mode), so an LLM client can read and write the store under a role.
Without `metadata/mcp.yaml` it exposes six generic, table-parameterized tools
— `list_tables`, `describe_table`, `query`, `insert`, `update`, `delete`.
For a remote agent contract, add `mcp.yaml`: it publishes only named saved
queries and explicit table operations. Both modes run as the request's role
through the same permission system; a tool call never bypasses permissions.

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
  -H "Authorization: Bearer $TOKEN" -H 'x-donat-role: staff' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"query",
        "arguments":{"table":"product","columns":["id","slug","status"],
                     "where":{"status":{"_eq":"draft"}},"order_by":{"id":"asc"}}}}'
# result.structuredContent: [{"id":3,"slug":"turtle-heat-lamp","status":"draft"}]
```

Insert as staff (`update`/`delete` take a `where` + `set` the same way). The
write is held to the same validators as every other surface — a `title` under
three characters comes back as `validation-failed` here too:

```bash
curl -s localhost:8080/mcp \
  -H 'content-type: application/json' \
  -H "Authorization: Bearer $TOKEN" -H 'x-donat-role: staff' \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
        "name":"insert",
        "arguments":{"table":"product","objects":[{"category_id":2,"slug":"cat-tunnel","title":"Cat tunnel","status":"draft"}],
                     "returning":["id","slug"]}}}'
# result.structuredContent: {"affected_rows":1,"returning":[{"id":4,"slug":"cat-tunnel"}]}
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

Every process is reachable: exactly one Command declares the `start_process`
effect that creates it, and that Command is the module's public entry point.

| Module | Entry-point Command | Current intent |
| --- | --- | --- |
| `checkout_payment` | `start_checkout` | Immutable pricing/tax/shipping quote snapshots and synchronous provider authorization with ambiguity lookup. |
| `checkout_cancellation` | `cancel_order` | Claim a pending authorization race, prove provider absence or materialize/void the authorization, then release reservations. |
| `authorized_order_cancellation` | `request_authorized_order_cancellation` | Void an authorized payment before releasing reservations and finalizing cancellation. |
| `partial_fulfilment` | `start_order_fulfilment` | Allocate, pack, label, ship, and capture per fulfilment unit. |
| `return_refund` | `start_return` | Support approval, return label, receipt, inspection, refund, exchange, or rejection. |
| `subscription_renewal` | `start_subscription_renewal` | Renewal authorization with dunning timers and a terminal pause. |
| `b2b_order_approval` | `submit_quote` | Quote routing, automatic credit use, and approver/finance waits. |
| `vendor_payout` | `start_vendor_payout` | Create bounded vendor payouts and record synchronous terminal provider outcomes per vendor. |
| `grooming_booking` | `start_grooming_booking` | Reserve a grooming slot and await confirmation, cancellation, or hold expiry. |
| `prescription_review` | `start_prescription_review` | Submit a prescription review and await the recorded decision or expiry. |
| `payment_reconciliation` | `start_payment_reconciliation` | Retrieve provider evidence, reconcile it, and await manual resolution when needed. |

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
Process calls. Command execution already persists these revision-pinned intents
atomically and replays without duplicating them; the Process worker consumes
them once the worker runtime is enabled.

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
