# Petshop Store Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the toy Petshop CRUD example with a permission-safe
pet-supplies store whose catalog, cart, quantitative inventory, atomic
checkout, order lifecycle, fulfilment, cancellation, and refund behavior are
executable through Donat's existing public surfaces.

**Architecture:** Keep catalog and cart edits on the existing
permission-aware GraphQL/REST/MCP data plane. Use declarative Commands and
Rules only for atomic cross-relation lifecycle changes. Add one bounded
set-based Command capability for multi-line checkout; preserve one Postgres
statement, role permissions, static validation, and in-database JSON assembly.
Payment remains provider-neutral and `pending` in this plan; the separate Mock
Payment Flow plan performs HTTP and feeds outcomes back through the declared
commands.

**Tech Stack:** Rust workspace, Postgres 16, serde YAML, GraphQL, native
conformance harness, `insta`.

## Global Constraints

- The runtime remains one Rust binary plus Postgres; crates are not services.
- There is no UI, admin role, permission bypass, runtime DDL, `run_sql`, plugin
  runtime, JavaScript, WASM, or Petshop-specific Rust handler.
- Ordinary CRUD remains GraphQL/REST/MCP; do not create a second entity or
  query DSL.
- A Command is source-local and compiles to one Postgres statement.
- Customer identity comes from `X-Donat-User-Id`; a customer-supplied owner,
  price, total, lifecycle state, or service role is rejected.
- Money is signed 64-bit minor units plus `USD`; floating-point money is
  forbidden.
- Every engine behavior begins with a failing native conformance case and a
  focused crate-level test.
- Snapshot changes are read before acceptance.
- Do not dispatch the Judge after each commit; the user explicitly replaced
  that gate with a later whole-range code review.
- Medusa, Saleor, and Spree are behavior inventories only in this plan; no
  upstream source or fixture bytes are copied.
- After engine changes, rebuild
  `cargo build -p donat-server --bin donat` before focused conformance.

## File Map

### Example-owned files

- `examples/petshop/migrations/V1__catalog.sql` — category and product schema.
- `examples/petshop/migrations/V2__variants_inventory.sql` — variants and
  quantitative stock.
- `examples/petshop/migrations/V3__customers.sql` — customers and addresses.
- `examples/petshop/migrations/V4__carts.sql` — open carts and cart lines.
- `examples/petshop/migrations/V5__orders.sql` — orders and immutable lines.
- `examples/petshop/migrations/V6__payments_reservations.sql` — reservation,
  payment, and provider-event records.
- `examples/petshop/migrations/V7__fulfilment_refunds_views.sql` — shipment,
  refund, safe pricing/operations views, and deterministic seed data.
- `examples/petshop/metadata/databases/default/tables/*.yaml` — tracked
  relations, relationships, and explicit-role permissions.
- `examples/petshop/metadata/commands.yaml` — lifecycle Commands.
- `examples/petshop/metadata/rules.yaml` — validation and transition Rules.
- `examples/petshop/metadata/query_collections.yaml` — saved public and
  role-scoped operations.
- `examples/petshop/metadata/rest_endpoints.yaml` — REST adapters over saved
  operations.
- `examples/petshop/README.md` — runnable reference walkthrough and reset note.

The existing V1–V5 Petshop migrations are replaced rather than extended. This
is a deliberate breaking reset of an unreleased demo schema; README must
require `docker compose down -v` when moving to this revision. The migration
history presented to a new user must describe the current store directly,
without creating and dropping the obsolete `pet` model on every startup.

### Engine-owned files

- `crates/metadata/src/types.rs`, `crates/metadata/tests/types_serde.rs` —
  bounded `select_many`, `aggregate`, and `update_many` metadata.
- `crates/schema/src/commands.rs`,
  `crates/schema/src/plan_mutation.rs`,
  `crates/schema/tests/commands.rs` — static compilation and role planning.
- `crates/ir/src/lib.rs` — SQL-free set-valued Command IR.
- `crates/sqlgen/src/lib.rs`, `crates/sqlgen/tests/commands.rs`,
  `crates/sqlgen/tests/snapshots/` — one-statement relational-batch SQL.
- `specs/003-declarative-domain-commands.md` — normative bounded-batch
  amendment.
- `knowledgebase/declarative-saas/decisions/014-command-relational-batches.md`
  — reason the extension is safe and required.

### Acceptance files

- `crates/conformance/src/lib.rs` — reusable SQL-directory application helper.
- `crates/conformance/tests/petshop.rs` — example-backed product and
  concurrency cases.
- `crates/conformance/fixtures/petshop/catalog.yaml`
- `crates/conformance/fixtures/petshop/cart.yaml`
- `crates/conformance/fixtures/petshop/checkout.yaml`
- `crates/conformance/fixtures/petshop/lifecycle.yaml`

---

### Task 1: Make the checked-in Petshop example an executable fixture

**Files:**

- Modify: `crates/conformance/src/lib.rs`
- Create: `crates/conformance/tests/petshop.rs`
- Create: `crates/conformance/fixtures/petshop/catalog.yaml`
- Modify: `knowledgebase/declarative-saas/reference-porting-register.md`

**Interfaces:**

- Produces:

```rust
pub fn apply_sql_migration_dir(database_url: &str, dir: &Path) -> anyhow::Result<()>;
```

The helper sorts `V<number>__*.sql` by numeric version, rejects a duplicate
version, and executes each complete file with `postgres::Client::batch_execute`.
It never parses or executes a path supplied by an HTTP request.

- [ ] **Step 1: Write the failing helper tests**

Add unit tests beside the conformance helper using a temporary directory:

```rust
#[test]
fn migration_files_are_sorted_by_numeric_version() {
    // V2 must run before V10 even when lexical order differs.
}

#[test]
fn duplicate_migration_versions_are_rejected() {
    // V2__a.sql plus V2__b.sql returns an error containing "duplicate version 2".
}
```

- [ ] **Step 2: Run the focused RED test**

Run:

```bash
cargo test -p donat-conformance migration_files_are_sorted
```

Expected: FAIL because `apply_sql_migration_dir` does not exist.

- [ ] **Step 3: Implement the migration-directory helper**

Accept only regular `.sql` files matching
`V([1-9][0-9]*)__([A-Za-z0-9_]+).sql`. Reject malformed names rather than
silently skipping them. Apply files on one newly created suite database before
the first engine request.

- [ ] **Step 4: Add the initial example-backed catalog test**

`petshop.rs` must:

```rust
let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop");
let metadata = donat_metadata::load_metadata_dir(&root.join("metadata")).unwrap();
let running = Suite::new("petshop_catalog")
    .initial_metadata(metadata)
    .admin_secret("petshop-secret")
    .start();
apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
running.apply("petshop/catalog.yaml", "/v1/graphql");
```

The catalog fixture initially describes the current example and passes before
the schema rewrite. This proves later cases exercise the checked-in example,
not a second copied metadata fixture.

- [ ] **Step 5: Record behavior-only upstream references**

Add Medusa `5b732d40ee78e4c9973fdb1e0ac247b319611f51`,
Saleor `8e6164e3d12327496660f91f836d5c3222d8d2b6`, and Spree
`b839535c9e634d61196b5ab341cd2b1ec062526c` as behavior-only entries. State
that no upstream bytes are copied and name `crates/conformance/tests/petshop.rs`
as the independently authored destination.

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo test -p donat-conformance --lib migration
cargo test -p donat-conformance --test petshop catalog
```

Commit:

```bash
git add crates/conformance knowledgebase/declarative-saas/reference-porting-register.md
git commit -m "test(petshop): execute the checked-in example"
```

---

### Task 2: Replace the toy schema with a quantitative store model

**Files:**

- Delete: `examples/petshop/migrations/V1__create_category.sql`
- Delete: `examples/petshop/migrations/V2__create_pet.sql`
- Delete: `examples/petshop/migrations/V3__create_customer.sql`
- Delete: `examples/petshop/migrations/V4__create_orders.sql`
- Delete: `examples/petshop/migrations/V5__create_order_item.sql`
- Create: `examples/petshop/migrations/V1__catalog.sql`
- Create: `examples/petshop/migrations/V2__variants_inventory.sql`
- Create: `examples/petshop/migrations/V3__customers.sql`
- Create: `examples/petshop/migrations/V4__carts.sql`
- Create: `examples/petshop/migrations/V5__orders.sql`
- Create: `examples/petshop/migrations/V6__payments_reservations.sql`
- Create: `examples/petshop/migrations/V7__fulfilment_refunds_views.sql`
- Modify: `crates/conformance/fixtures/petshop/catalog.yaml`
- Create: `crates/conformance/fixtures/petshop/cart.yaml`

**Interfaces:**

- Produces the exact relation names from the design:
  `category`, `product`, `product_variant`, `inventory_stock`, `customer`,
  `customer_address`, `cart`, `cart_line`, `orders`, `order_line`,
  `inventory_reservation`, `payment`, `payment_event`, `shipment`, `refund`.

- [ ] **Step 1: Expand catalog and cart fixtures first**

Freeze these externally visible cases:

```graphql
query PublicCatalog {
  product(where: {status: {_eq: "published"}}, order_by: {slug: asc}) {
    slug
    variants(order_by: {sku: asc}) {
      sku
      price_minor
      currency
      stock {
        available_quantity
      }
    }
  }
}
```

The public result contains only published products, active variants, and
non-negative computed availability. The cart fixture creates one cart for
customer `customer-1`, inserts the same variant twice with `on_conflict`, and
expects one line with the new quantity.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p donat-conformance --test petshop catalog cart
```

Expected: FAIL because the current example exposes `pet` and has no cart.

- [ ] **Step 3: Write the replacement migrations**

Use `bigserial` internal keys, UUID public order/payment IDs, and these
non-negotiable constraints:

```sql
CHECK (price_minor >= 0),
CHECK (currency ~ '^[A-Z]{3}$'),
CHECK (on_hand >= 0),
CHECK (reserved >= 0),
CHECK (reserved <= on_hand),
CHECK (quantity > 0),
UNIQUE (sku)
```

`inventory_stock.available_quantity` is a stored generated column:

```sql
available_quantity integer
  GENERATED ALWAYS AS (on_hand - reserved) STORED
```

Use a partial unique index for one open cart per customer:

```sql
CREATE UNIQUE INDEX cart_one_open_per_customer
ON cart(customer_id) WHERE status = 'open';
```

Use integer generated or checked totals:

```sql
line_total_minor bigint NOT NULL
  CHECK (line_total_minor = unit_price_minor * quantity)
```

Seed at least three products, four variants, one inactive variant, two
customers, and stock where one SKU has exactly one available unit for the
concurrency case.

- [ ] **Step 4: Add pricing and operations views**

Create tracked read-only views:

```sql
cart_pricing
  (cart_id, customer_id, variant_id, sku, title, quantity,
   unit_price_minor, currency, line_total_minor, available_quantity)

order_operations
  (order_id, customer_id, order_status, payment_status,
   fulfilment_status, total_minor, currency)
```

The views contain no secret or connector payload.

- [ ] **Step 5: Verify database constraints directly**

In `petshop.rs`, attempt negative quantity, `reserved > on_hand`, duplicate
SKU, and non-three-letter currency through SQL setup helpers. Assert each
fails before exposing metadata.

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo test -p donat-conformance --test petshop catalog cart
```

Commit:

```bash
git add examples/petshop/migrations crates/conformance
git commit -m "feat(petshop): add store domain schema"
```

---

### Task 3: Apply explicit-role permissions and reuse CRUD for carts

**Files:**

- Replace: `examples/petshop/metadata/databases/default/tables/tables.yaml`
- Delete: `examples/petshop/metadata/databases/default/tables/public_pet.yaml`
- Delete: `examples/petshop/metadata/databases/default/tables/public_order_item.yaml`
- Create/Modify:
  `examples/petshop/metadata/databases/default/tables/public_*.yaml`
- Modify: `examples/petshop/metadata/query_collections.yaml`
- Modify: `examples/petshop/metadata/rest_endpoints.yaml`
- Modify: `crates/conformance/fixtures/petshop/catalog.yaml`
- Modify: `crates/conformance/fixtures/petshop/cart.yaml`

**Interfaces:**

- `anonymous`: published catalog reads only.
- `customer`: published catalog plus own customer/address/cart/order reads;
  write access only to own profile/address/open cart and cart-line
  `variant_id`/`quantity`.
- `staff`: catalog management and safe operational reads.
- `fulfilment`: paid order/shipment reads; no catalog/customer write.
- `support`: customer/order/payment/refund reads and declared commands only.
- `payment_worker`: payment/reservation/order columns needed by declared
  commands only; it is an explicit deploy-time service role, never inferred as
  admin.

- [ ] **Step 1: Add permission-negative fixture cases**

Freeze exact GraphQL validation errors for:

- anonymous querying customer/cart/order;
- customer B querying customer A's cart/order;
- customer writing `price_minor`, `unit_price_minor`, `total_minor`,
  `reserved`, or any lifecycle state;
- staff invoking fulfilment/support-only mutations;
- role-less trusted request omitting `X-Donat-Role`.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p donat-conformance --test petshop permissions
```

Expected: FAIL because new metadata is absent.

- [ ] **Step 3: Write relation metadata**

Every owner filter follows an explicit relationship to:

```yaml
customer_id:
  _eq: X-Donat-User-Id
```

Cart-line insert/update checks follow `cart.customer_id`. Variant insert check
follows `variant.product.status = published` and `variant.active = true`.
Customers receive no direct insert/update permission on order, order line,
stock, reservation, payment, event, shipment, or refund relations.

- [ ] **Step 4: Replace REST saved operations**

Expose:

- `GET /api/rest/products`
- `GET /api/rest/products/:slug`
- `GET /api/rest/cart`
- `PUT /api/rest/cart/lines`
- `GET /api/rest/orders`
- `GET /api/rest/orders/:id`

Each endpoint points to a named operation in the `petshop` query collection.
`PUT cart/lines` is the same GraphQL upsert used directly; it is not a custom
handler.

- [ ] **Step 5: Verify GraphQL, REST, and MCP permissions**

Run:

```bash
cargo test -p donat-conformance --test petshop permissions
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop catalog cart
```

- [ ] **Step 6: Commit**

```bash
git add examples/petshop/metadata crates/conformance
git commit -m "feat(petshop): enforce store role boundaries"
```

---

### Task 4: Specify the bounded relational Command batch

**Files:**

- Modify: `specs/003-declarative-domain-commands.md`
- Create:
  `knowledgebase/declarative-saas/decisions/014-command-relational-batches.md`
- Modify: `crates/metadata/src/types.rs`
- Modify: `crates/metadata/tests/types_serde.rs`
- Modify: `crates/metadata/tests/load_fixture.rs`
- Create: `crates/metadata/tests/fixtures/commands/relational_batch.yaml`

**Interfaces:**

Add these closed forms:

```rust
pub enum CommandStepOperation {
    // existing variants
    SelectMany { select_many: SelectManyCommandStep },
    Aggregate { aggregate: AggregateCommandStep },
    UpdateMany { update_many: UpdateManyCommandStep },
}

pub struct SelectManyCommandStep {
    pub table: QualifiedTable,
    pub by: BTreeMap<String, CommandValue>,
    pub order_by: Vec<String>,
    pub returning: Vec<String>,
    pub require_non_empty: bool,
}

pub struct UpdateManyCommandStep {
    pub table: QualifiedTable,
    pub for_each: CommandValue,
    pub by: BTreeMap<String, CommandValue>,
    pub set: BTreeMap<String, CommandValue>,
    pub check: Option<CommandRuleBinding>,
    pub returning: Vec<String>,
    pub require_each: bool,
}

pub struct AggregateCommandStep {
    pub from: CommandValue,
    pub values: BTreeMap<String, CommandAggregate>,
}

pub enum CommandAggregate {
    Count { count: CountCommandAggregate },
    Sum { sum: ColumnCommandAggregate },
    Min { min: ColumnCommandAggregate },
    Max { max: ColumnCommandAggregate },
    CountDistinct { count_distinct: ColumnCommandAggregate },
}

pub struct CountCommandAggregate {}

pub struct ColumnCommandAggregate {
    pub column: String,
}

pub enum CommandValue {
    // existing variants
    CurrentColumn { column: String },
}
```

`select_many.by` is a non-empty equality map over catalog columns and scalar
values. `order_by` is mandatory. SQL execution rejects duplicate complete
`order_by` tuples, making the observed row order total even for a view without
a catalog primary key. `aggregate.from` accepts only a prior `select_many`
row set, returns exactly one row, and has no `group_by`. `update_many.for_each`
accepts only a prior `select_many` row set. `update_many.by` must contain every
target primary-key column, sourced from the current item. `current_column` is
valid only inside that step's `set` or `check`.

- [ ] **Step 1: Write exhaustive serde tests**

Cover canonical YAML, quoted `!include`, unknown keys, empty `by`, missing
`order_by`, duplicate order column, unsupported aggregate, aggregate over a
scalar step, `current_column` outside `update_many`, and a forward step
reference.

Canonical example:

```yaml
- name: priced_lines
  select_many:
    table: { schema: public, name: cart_pricing }
    by:
      cart_id: { step: cart, column: id }
    order_by: [variant_id]
    returning:
      [variant_id, sku, title, quantity, unit_price_minor, currency, line_total_minor]
    require_non_empty: true
- name: totals
  aggregate:
    from: { step: priced_lines }
    values:
      line_count: { count: {} }
      subtotal_minor: { sum: { column: line_total_minor } }
      currency_count: { count_distinct: { column: currency } }
      currency: { min: { column: currency } }
- name: reserve_stock
  update_many:
    table: { schema: public, name: inventory_stock }
    for_each: { step: priced_lines }
    by:
      variant_id: { item: variant_id }
    set:
      reserved:
        rule: add_int
        with:
          left: { current_column: reserved }
          right: { item: quantity }
    check:
      rule: can_reserve
      with:
        on_hand: { current_column: on_hand }
        reserved: { current_column: reserved }
        requested: { item: quantity }
    returning: [variant_id, reserved]
    require_each: true
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p donat-metadata relational_batch
```

Expected: FAIL because both variants are unknown.

- [ ] **Step 3: Write the Spec 003 amendment and ADR**

The amendment must state:

- read targets may be tracked tables or views; update targets remain ordinary
  Postgres tables with a primary key;
- underlying select/update permissions are mandatory;
- there is no free-form predicate, SQL, identifier template, function call,
  join declaration, loop, or dynamic relation;
- aggregate input is a prior row set and its exact operations are `sum`,
  `count`, `min`, `max`, and `count_distinct`; there is no grouping, filter,
  window, or user expression;
- `require_each` compares distinct input keys, input count, and affected count;
- `select_many` rejects a duplicate complete `order_by` tuple before later
  steps consume the row set;
- duplicate input primary keys are rejected before DML;
- row sets cannot cross sources or escape the Command statement;
- result lists preserve declared total order.

- [ ] **Step 4: Implement metadata-only types**

Add serde types without catalog or SQL behavior. Preserve the existing
external tag style and `deny_unknown_fields`.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p donat-metadata
```

Commit:

```bash
git add specs/003-declarative-domain-commands.md \
  knowledgebase/declarative-saas/decisions/014-command-relational-batches.md \
  crates/metadata
git commit -m "feat(commands): declare bounded relational batches"
```

---

### Task 5: Compile relational batches to typed SQL-free IR

**Files:**

- Modify: `crates/ir/src/lib.rs`
- Modify: `crates/schema/src/commands.rs`
- Modify: `crates/schema/src/plan_mutation.rs`
- Modify: `crates/schema/src/introspection.rs`
- Modify: `crates/schema/tests/commands.rs`

**Interfaces:**

```rust
pub enum CommandStepIr {
    // existing variants
    SelectMany {
        cte: String,
        table: QualifiedTable,
        equality: Vec<CommandAssignment>,
        order_by: Vec<CommandColumn>,
        returning: Vec<CommandColumn>,
        require_non_empty: bool,
    },
    Aggregate {
        cte: String,
        input_cte: String,
        values: Vec<CommandAggregateIr>,
    },
    UpdateMany {
        cte: String,
        table: QualifiedTable,
        input_cte: String,
        primary_key: Vec<CommandAssignment>,
        assignments: Vec<CommandAssignment>,
        check: Option<LoweredCommandRule>,
        returning: Vec<CommandColumn>,
        require_each: bool,
    },
}

pub enum CommandExecutionValue {
    // existing variants
    CurrentColumn { column: CommandColumn },
    StepRows { cte: String, columns: Vec<CommandColumn> },
}
```

- [ ] **Step 1: Add static validation tests**

Reject:

- unauthorized read/update target;
- `select_many` without a non-empty equality key;
- non-total or non-column order;
- aggregate over anything except a prior row set;
- `sum` on a non-numeric column or `min`/`max` on an unsupported type;
- `update_many` input not produced by `select_many`;
- incomplete target primary key;
- duplicate input key;
- current-column type mismatch;
- rule result not assignable to the target column;
- view or non-Postgres update target;
- result reference that treats a row set as a scalar row.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p donat-schema --test commands relational_batch
```

Expected: FAIL because IR and compiler variants are absent.

- [ ] **Step 3: Implement static compilation**

Resolve every relation, column, permission, Rules artifact, value type, and
cardinality during candidate-engine construction. The runtime IR contains no
raw metadata name lookup and no session access beyond already resolved
permission predicates.

- [ ] **Step 4: Generate exact GraphQL output types**

A `select_many` or `update_many` result field is a non-null list of the
declared row object. An `aggregate` result is one non-null generated object.
Each exposes only declared outputs; an unauthorized column cannot enter
introspection even if it is not selected by a client.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p donat-schema --test commands
cargo test -p donat-ir
cargo test --workspace --no-run
```

Commit:

```bash
git add crates/ir crates/schema
git commit -m "feat(commands): compile relational batch IR"
```

---

### Task 6: Render relational batches as one guarded Postgres statement

**Files:**

- Modify: `crates/sqlgen/src/lib.rs`
- Modify: `crates/sqlgen/tests/commands.rs`
- Create/Modify: `crates/sqlgen/tests/snapshots/commands__*.snap`
- Modify: `crates/server/src/gql.rs` only if result decoding needs a new
  explicit row-set arm.

**Interfaces:**

The rendered shape is:

```sql
WITH priced_lines AS MATERIALIZED (
  SELECT ...
  FROM public.cart_pricing AS row
  WHERE row.cart_id = ...
  ORDER BY row.variant_id
),
reserve_stock AS (
  UPDATE public.inventory_stock AS target
  SET reserved = target.reserved + input.quantity
  FROM priced_lines AS input
  WHERE target.variant_id = input.variant_id
    AND target.on_hand - target.reserved >= input.quantity
  RETURNING ...
),
reserve_stock_gate AS MATERIALIZED (
  SELECT donat.raise_graphql_error(...)
  WHERE (SELECT count(*) FROM reserve_stock)
     <> (SELECT count(*) FROM priced_lines)
)
...
SELECT json_build_object(...)
```

The actual renderer uses typed Rules lowering and identifier quoting; the SQL
above is a structural expectation, not a string template in metadata.

- [ ] **Step 1: Write reviewed insta cases**

Add snapshots for:

- ordered `select_many`;
- row-set `sum/count/min/max/count_distinct`;
- guarded arithmetic `update_many`;
- duplicate input-key rejection;
- zero-row `require_non_empty`;
- partial affected-row rollback;
- permission predicate on both selected and updated rows;
- idempotent replay returning the same row-set result.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test -p donat-sqlgen --test commands relational_batch
```

Expected: FAIL because SQLgen has no variants.

- [ ] **Step 3: Implement minimal rendering**

Use materialized CTE gates so every later DML depends on every earlier check.
Do not perform row iteration or JSON assembly in Rust. Do not hold a database
transaction across any external I/O.

- [ ] **Step 4: Review snapshots**

Run:

```bash
cargo insta review
```

Read every identifier, permission predicate, affected-count gate, idempotency
claim, and final JSON shape. Reject any snapshot containing user-provided SQL
or a second top-level statement.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p donat-sqlgen --test commands
cargo test -p donat-server command
```

Commit:

```bash
git add crates/sqlgen crates/server/src/gql.rs
git commit -m "feat(commands): execute relational batches atomically"
```

---

### Task 7: Implement atomic Petshop checkout through YAML

**Files:**

- Create: `examples/petshop/metadata/commands.yaml`
- Create: `examples/petshop/metadata/rules.yaml`
- Create: `crates/conformance/fixtures/petshop/checkout.yaml`
- Modify: `crates/conformance/tests/petshop.rs`
- Modify: Petshop table permissions required by the command's fixed customer
  role.

**Interfaces:**

`begin_checkout(cart_id: bigint!, request_id: uuid!): BeginCheckoutResult!`
returns `order_id`, `payment_id`, `subtotal_minor`, `total_minor`, `currency`,
and frozen ordered lines.

- [ ] **Step 1: Add checkout RED cases**

Freeze exact behavior:

1. empty cart rejects and creates no rows;
2. inactive/unpublished variant rejects;
3. server price wins over every client-visible stale cart value;
4. one-stock race allows exactly one customer;
5. exact idempotent replay returns the same order/payment;
6. changed cart under the same request key conflicts;
7. failed second line rolls back the first reservation;
8. customer cannot directly write order/payment/reservation state.

The concurrency case uses two threads and a barrier to issue
`begin_checkout` simultaneously, then asserts one successful GraphQL response,
one deterministic stock error, one order, and `reserved = 1`.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop checkout
```

Expected: FAIL because `commands.yaml` and relational batches are absent.

- [ ] **Step 3: Declare checkout Rules**

At minimum:

```yaml
rules:
  - name: add_int
    parameters: { left: int!, right: int! }
    result: int!
    expression: left + right
  - name: can_reserve
    parameters: { on_hand: int!, reserved: int!, requested: int! }
    result: bool!
    expression: requested > 0 && on_hand - reserved >= requested
  - name: same_currency
    parameters: { expected: string!, actual: string! }
    result: bool!
    expression: expected == actual
```

The command selects the owned open cart, selects ordered rows from
`cart_pricing`, aggregates `sum(line_total_minor)`,
`count_distinct(currency)`, and `min(currency)`, asserts exactly one currency,
reserves every stock row, inserts one order and its line snapshots, inserts
reservation rows and one pending payment, marks the cart converted, and
returns the frozen result.

- [ ] **Step 4: Declare command idempotency**

```yaml
idempotency:
  key: { argument: request_id }
  scope:
    - { session_variable: x-donat-user-id }
    - { argument: cart_id }
  retention: 30d
```

No argument accepts `customer_id`, price, total, order status, payment status,
or stock quantity.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo test -p donat-metadata
cargo test -p donat-schema --test commands
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop checkout
```

Commit:

```bash
git add examples/petshop/metadata crates/conformance
git commit -m "feat(petshop): add atomic idempotent checkout"
```

---

### Task 8: Add payment outcome, fulfilment, cancellation, and refund states

**Files:**

- Modify: `examples/petshop/metadata/commands.yaml`
- Modify: `examples/petshop/metadata/rules.yaml`
- Modify: Petshop table permission YAML
- Create: `crates/conformance/fixtures/petshop/lifecycle.yaml`
- Modify: `crates/conformance/tests/petshop.rs`

**Interfaces:**

Commands:

```graphql
record_payment_outcome(payment_id: uuid!, event_id: String!, outcome: PaymentOutcome!, provider_reference: String!): PaymentOutcomeResult!
expire_checkout(payment_id: uuid!, deadline_key: uuid!): ExpireCheckoutResult!
mark_order_packed(order_id: uuid!, request_id: uuid!): OrderStateResult!
mark_order_shipped(order_id: uuid!, tracking_number: String!, request_id: uuid!): OrderStateResult!
cancel_order(order_id: uuid!, request_id: uuid!): CancelOrderResult!
complete_refund(refund_id: uuid!, event_id: String!, request_id: uuid!): RefundResult!
```

- [ ] **Step 1: Add lifecycle RED cases**

Cover:

- only a paid order can be packed;
- duplicate paid outcome/event is a replay, not a second stock transition;
- failed/expired payment releases reservation once;
- unfulfilled cancellation releases stock;
- paid cancellation creates a refund request rather than pretending remote
  compensation succeeded;
- shipped quantity cannot be restored by cancellation;
- refund total cannot exceed captured amount;
- customer cannot invoke worker/support/fulfilment commands;
- support cannot mutate raw payment rows through CRUD.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop lifecycle
```

- [ ] **Step 3: Declare finite transition Rules**

Use enums declared in `rules.yaml`; do not pass command names or roles through
rule output. Each command has a fixed target and uses a boolean transition
rule over current state.

- [ ] **Step 4: Implement lifecycle Commands in YAML**

Every externally repeatable command declares idempotency. `payment_event` has
`UNIQUE(provider, event_id)`. Payment success moves reservations from
`active` to `committed`, decrements `on_hand` and `reserved` exactly once, and
moves the order to `paid`. Expiry/failure decrements only `reserved`.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop lifecycle
cargo test -p donat-conformance --test commands
```

Commit:

```bash
git add examples/petshop/metadata crates/conformance
git commit -m "feat(petshop): enforce the order lifecycle"
```

---

### Task 9: Finish public surfaces and reference documentation

**Files:**

- Modify: `examples/petshop/metadata/query_collections.yaml`
- Modify: `examples/petshop/metadata/rest_endpoints.yaml`
- Modify: `examples/petshop/README.md`
- Modify: root `README.md`
- Modify: `crates/conformance/tests/petshop.rs`

**Interfaces:**

The README contains runnable GraphQL and REST calls for catalog, cart,
checkout, payment-outcome simulation, fulfilment, cancellation, and refund.
It states that payment HTTP is added by the next plan.

- [ ] **Step 1: Add transport-parity RED cases**

Call the same saved operations over GraphQL and REST and the same role through
MCP. Assert identical owner filtering and totals. Do not compare wrapper
transport bytes where existing contracts intentionally differ.

- [ ] **Step 2: Update saved operations and documentation**

Remove every obsolete `pet` example. Document the explicit reset:

```bash
docker compose down -v
docker compose up --build
```

Explain that generic MCP CRUD remains available only where table permissions
allow it and that lifecycle behavior is exposed as task-level commands.

- [ ] **Step 3: Run focused verification**

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop
cargo test -p donat-conformance --test commands
cargo test -p donat-conformance --test rules
```

- [ ] **Step 4: Run broad verification**

```bash
cargo test -p donat-metadata
cargo test -p donat-schema
cargo test -p donat-sqlgen
cargo test -p donat-conformance
```

If Postgres-dependent tests cannot run, stop and report the exact missing
dependency; do not mark the plan complete.

- [ ] **Step 5: Commit**

```bash
git add examples/petshop README.md crates/conformance
git commit -m "docs(petshop): publish the reference store"
```
