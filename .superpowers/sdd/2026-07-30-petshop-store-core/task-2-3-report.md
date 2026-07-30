# Task 2–3 report: quantitative Petshop store core

## Implementation summary

- Replaced the five toy Petshop migrations with seven versioned store-domain
  migrations. They create the required catalogue, inventory, customer, cart,
  order, payment, fulfilment, refund, and read-only operations relations.
- Added `bigserial` internal keys, UUID order/payment public IDs, the required
  monetary, currency, stock, quantity, SKU, cart, and line-total constraints,
  plus seeded published/draft catalogue data, two customers, and a one-unit
  stock case.
- Added the `cart_pricing` and `order_operations` tracked read-only views.
  Neither projects payment-event payloads or provider secrets.
- Replaced toy metadata with explicit classic roles only. Anonymous is
  catalogue-only; customers are scoped by `customer_id = X-Donat-User-Id`;
  staff, fulfilment, support, and `payment_worker` have their declared
  non-admin boundaries. There is no bypass role or runtime management path.
- Replaced Petshop REST operations with products, product-by-slug, cart,
  cart-line upsert, orders, and order-by-ID operations. `PUT /cart/lines`
  invokes the ordinary permission-aware GraphQL upsert.
- Added real example-backed catalog, cart, permissions, REST, MCP, and direct
  SQL constraint cases. The customer-B ownership fixture seeds customer-A rows
  before checking that they remain invisible.
- Added `petshop` to the existing Postgres-reference conformance classifier.
  This narrowly fixes Task 1's newly added integration target omission, found
  by the classifier's own failing test.

## Files changed

- `examples/petshop/migrations/V1__catalog.sql` through
  `V7__fulfilment_refunds_views.sql` replace the five toy migrations.
- `examples/petshop/metadata/databases/default/tables/` now tracks the store
  tables and the two read-only views, replacing `public_pet.yaml` and
  `public_order_item.yaml`.
- `examples/petshop/metadata/query_collections.yaml` and
  `examples/petshop/metadata/rest_endpoints.yaml` define the six store REST
  surfaces.
- `crates/conformance/fixtures/petshop/{catalog,cart,permissions}.yaml` and
  `crates/conformance/tests/petshop.rs` define the public executable contract.
- `crates/conformance/src/lib.rs` classifies the new Petshop integration
  target as Postgres-reference coverage.

## TDD evidence

The tests name the production breaks they catch: returning a draft product or
inactive variant to anonymous callers; adding a duplicate cart line instead
of updating it; exposing another customer's cart/order; exposing restricted
role mutations; bypassing role checks via MCP; and removing each database
constraint. Expected values are literal fixtures and the tests use the real
example metadata, migrations, HTTP server, and Postgres suite database.

### RED

The brief's literal multi-filter Cargo command was attempted first:

```bash
cargo test -p donat-conformance --test petshop catalog cart
```

Cargo rejected it because it accepts one positional test-name filter:

```text
error: unexpected argument 'cart' found
Usage: cargo test [OPTIONS] [TESTNAME] [-- [ARGS]...]
```

The equivalent focused commands were run separately with the required
PostGIS URL and absolute stable toolchain:

```bash
PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres \
RUSTC=/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc \
RUSTDOC=/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustdoc \
/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test \
  -p donat-conformance --test petshop catalog
```

```text
test catalog ... FAILED
message: "field 'product' not found in type: 'query_root'"
```

```bash
... cargo test -p donat-conformance --test petshop cart
```

```text
test cart ... FAILED
message: "field 'insert_cart' not found in type: 'mutation_root'"
```

```bash
... cargo test -p donat-conformance --test petshop permissions
```

```text
test permissions ... FAILED
message: "field 'cart' not found in type: 'query_root'"
```

```bash
... cargo test -p donat-conformance --test petshop store_constraints
```

```text
test store_constraints ... FAILED
relation "cart" does not exist
```

The first full conformance attempt then correctly exposed the Task 1 target
classification omission:

```text
test tests::every_conformance_binary_is_classified ... FAILED
unclassified conformance test binary
left: [..., "petshop", ...]
right: [...]
```

### GREEN

After the migrations and metadata were added, the example suite passed:

```bash
PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres \
RUSTC=/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc \
RUSTDOC=/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustdoc \
/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test \
  -p donat-conformance --test petshop
```

```text
running 4 tests
test store_constraints ... ok
test catalog ... ok
test cart ... ok
test permissions ... ok
test result: ok. 4 passed; 0 failed
```

The classifier fix also passed:

```bash
RUSTC=/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc \
RUSTDOC=/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustdoc \
/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test \
  -p donat-conformance --lib every_conformance_binary_is_classified
```

```text
test tests::every_conformance_binary_is_classified ... ok
test result: ok. 1 passed; 0 failed
```

## Final verification

```bash
RUSTC=/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc \
RUSTDOC=/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustdoc \
/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo build \
  -p donat-server --bin donat
```

Output: `Finished dev profile`.

The rebuilt binary passed the four Petshop integration tests using the
PostGIS server at `127.0.0.1:15433`. `git diff --check` and
`rustfmt --edition 2024 --check crates/conformance/src/lib.rs
crates/conformance/tests/petshop.rs` also passed.

`cargo test -p donat-conformance` was started twice after the classifier fix:
both passed the full 49-test library phase and then progressed through the
initial integration binaries (`actions`, aggregation/introspection, auth,
backend matrix, commands, connectors, cron, enabled APIs, and event triggers)
without failure. The controller requested that this long full run not delay
the commit, so its remaining target output is intentionally not claimed as a
completed final result.

## Self-review

- Public catalogue assertions use an anonymous explicit role and prove that
  only published products, active variants, and non-negative stored computed
  availability are exposed.
- Cart creation and line edits exercise the standard GraphQL insert/upsert
  path, and the REST `PUT` fixture proves that its endpoint is a saved GraphQL
  operation rather than a Petshop handler.
- Customer B is tested after customer A rows exist, avoiding an empty-table
  false positive. The MCP case verifies that another transport cannot reveal
  a customer relation to anonymous callers.
- Direct SQL assertions require Postgres `CHECK` or unique SQLSTATEs, so a
  missing relation cannot falsely satisfy a constraint test.
- Customer metadata does not grant direct write permissions on order, order
  line, stock, reservation, payment, payment event, shipment, or refund;
  the worker is an ordinary explicit role with no bypass.
- Views project only operational/pricing fields. `payment_event.payload` and
  provider fields are absent from both views.

## Concerns

- The complete conformance crate run was intentionally left for the
  controller after it had cleared the library classifier and multiple
  integration targets; the focused Petshop suite and classifier are green.
- The Petshop `customer_id` is a stable text identity in addition to each
  relation's `bigserial` internal key. This is necessary for the required
  literal `customer_id = X-Donat-User-Id` policy and the `customer-1` fixture.
