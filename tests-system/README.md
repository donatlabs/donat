# Petshop system tests

Black-box tests for the checked-in [Petshop example](../examples/petshop),
driven the way an outside tester drives a store: over HTTP, as one of the
store's own roles, against a stand that is already running. Nothing here
imports the engine, and nothing reads the database to decide whether a test
passed.

They are not the conformance contract — that lives in
[`crates/conformance`](../crates/conformance) and stays the source of truth for
Donat compatibility. This suite asks a different question: with the whole thing
deployed, does the store behave like a store?

## Running

```bash
tests-system/stack.sh up                 # database, mock providers, this branch's engine
tests-system/stack.sh up-fast            # the same store with its periods in seconds
python3 -m venv tests-system/.venv
tests-system/.venv/bin/pip install -r tests-system/requirements.txt

cd tests-system && eval "$(./stack.sh env)" && .venv/bin/python -m pytest
                                         # or, from the repo root: make petshop-system-tests

tests-system/stack.sh down-fast && tests-system/stack.sh down    # when finished
```

`stack.sh env` names whichever stands are answering. Without the fast one every
deadline branch skips itself, and a run that tested none of them is green.

`stack.sh` follows the deploy model the example documents — `donat migrate` for
the DDL, a second `migrate` to deploy the durable Process revisions, then the
engine — and builds the engine **from this working tree**, because a published
image would test somebody else's build. Any other stand works too: point
`PETSHOP_BASE_URL` at it.

With no `PETSHOP_BASE_URL` the whole suite skips. A run with no stand must not
look like a passing run, and must not look like a failing store either.

| Variable | Meaning |
|---|---|
| `PETSHOP_BASE_URL` | the store to test; unset means skip |
| `PETSHOP_PROVIDERS_URL` | the mock providers' control plane; unset skips provider-driven cases |
| `PETSHOP_JWT_KEY` | HS256 key the suite signs its tokens with; must match the engine's `DONAT_GRAPHQL_JWT_SECRET` |
| `PETSHOP_SETTLE_TIMEOUT` | how long to wait for a durable Process (default 30s) |
| `PETSHOP_FAST_BASE_URL` | the seconds-scale stand; unset skips every deadline branch |
| `PETSHOP_FAST_PROVIDERS_URL` | its own mock providers, separate so one stand cannot claim the other's scripted answers |

## How the tests talk to the store

**Tokens, not headers.** The store has no admin role, so every request runs as
one explicit role. The suite signs its own HS256 token per actor
(`petshop_qa/auth.py`); a token minted for a customer cannot be replayed as
staff by adding a header, and the engine — not the test — enforces that.

**Providers are steerable.** The five external services the connectors are
declared against are answered by
[`mock-providers/providers.py`](../examples/petshop/mock-providers/providers.py),
which exposes a test-only control plane when `PETSHOP_PROVIDERS_CONTROL=1`. A
test can script a decline, a 5xx or a slow answer, and can read back exactly
what the store sent — the only place a black-box test can watch the money
leave. Default behaviour, with nothing scripted, is the example's own success
fixture.

**Durable work is waited for, never slept through** (`petshop_qa/wait.py`).
No test paces itself past a race; where one used to, the race was a defect and
is now fixed.

## Stand data

Two kinds of data cannot be created through any API surface, by any role:
per-location inventory (`inventory_level`), and the reference rows for the B2B,
subscription and marketplace modules. `provision.sql` states them at stand-up
time, next to the migrations, the way a real deployment loads opening stock —
and `stack.sh provision` tops the warehouse back up on a long-lived stand. That
no role can receive goods into a location is a finding about the example, not a
convenience for the tests.

## What the suite found

Every defect below was found by running the whole store, and each is fixed now
— the cases that pinned them are ordinary tests again. They are recorded here
because they say what this layer of testing is for.

| What was wrong | Where it was fixed |
|---|---|
| A payment authorization went out with **no `Idempotency-Key`**: the header was bound only from the legacy `http.idempotency` field, never from the operation's provider-idempotent effect, so a provider could not deduplicate a replay | `crates/server/src/connectors/http.rs` |
| A declared `error_map` had **no runtime effect**: a 500 the operation mapped to `http_5xx` was classified `Permanent`, so the flow's three declared attempts became one call | `crates/server/src/connectors/http.rs` |
| A receipt recorded **before** the return Process reached its wait was dropped; the wait declared `persist_before_match` and nothing read it, so the shopper was never refunded while every Command answered `200` | `crates/server/src/processes/{definition,transition,signal}.rs` |
| A Command that violated a unique index inside a transition was **retried forever**, and the shared consumer stopped advancing **every** Process in the deployment until the conflict was cleared by hand | `crates/server/src/processes/{command,transition}.rs` |
| An idempotency scope reading a step result compiled to SQL that named a CTE it never defined, so the Command answered `data-exception` and a reconciliation needing a person could never be closed | `crates/sqlgen/src/lib.rs`, `crates/schema/src/commands.rs` ([ADR 035](../knowledgebase/declarative-saas/decisions/035-an-idempotency-scope-may-read-a-lookup-never-a-write.md)) |
| A declared `as: string` cast was **never applied**, so a numeric identifier reached a connector as a number and broke the very input contract the cast was written to satisfy — the whole refund-reconciliation branch could not start | `crates/server/src/processes/value.rs` |
| A return nobody approved ended its Process without writing anything: the shopper's request read `requested` for ever, with no Process left to decide it | `examples/petshop/metadata` (`expire_return`) |
| Both B2B waits were the only ones in the store without `persist_before_match`, so an approver who answered at once had their decision dropped and the purchase escalated as if nobody had answered | `examples/petshop/metadata/flows/b2b-order-approval.yaml` |
| The schema published nine filter operators while the engine accepted `_like`/`_ilike` too, so a generated client, an IDE or an agent reading the schema concluded the catalogue could not be searched | `crates/schema/src/introspection.rs` |
| The schema hid the regex and JSON-key filters the engine accepts, and a composed schema refused to be built at all once a filter's shape depended on the backend | `crates/schema/src/{introspection,multi_source}.rs` |
| A selection set of more than **50 fields** hit Postgres's `json_build_object` argument ceiling and came back as `data-exception` — a wide table or a dashboard query passes fifty fields without trying | `crates/backend/src/dialect.rs` |
| One Process instance that could not make progress stopped **every** Process in the deployment: the consumer applied one transition at a time, always the oldest, and a failing or slow one stayed the oldest | `crates/server/src/processes/{runtime,transition,command}.rs` ([ADR 036](../knowledgebase/declarative-saas/decisions/036-the-transition-queue-is-a-work-queue-not-a-line.md)) |
| A shopper could write their own `payment` row directly — `status: captured` without paying, or a new `amount_minor`. The role held `columns: "*"` insert and update on the money table, when only its own commands ever needed to touch it | `examples/petshop/metadata/databases/default/tables/public_payment.yaml` |

See [ADR 034](../knowledgebase/declarative-saas/decisions/034-a-declaration-the-runtime-ignores-is-a-defect.md).
Most of them were declarations that parsed, validated and deployed while doing
nothing — a header binding, an error map, a wait's persistence, a scalar cast.
That is indistinguishable from working until something in production quietly
fails, and it is the failure mode this layer of testing exists to catch.

Two gaps in the example itself are still open, and the suite works around
them in `provision.sql`:

- **A payout cycle cannot be closed.** `vendor_payout_candidate` reports every
  `accepted` vendor order against one cycle id fixed in the view, and
  `vendor_order.status` has no settled state to move to — so a paid candidate
  stays a candidate and the next run collides.
- **Goods cannot be received.** Neither per-location inventory nor the B2B,
  subscription and marketplace reference rows can be created or replenished by
  any role, through any surface.

## Coverage of the store's flows

The suite reaches **165 of the 169 states** the eleven Petshop flows declare.
The four it does not reach are defensive defaults that no caller can steer the
store into — each is unreachable by construction, not untested by omission:

| State | Why nothing can reach it |
|---|---|
| `b2b_order_approval.reject_unroutable_quote` | The `b2b_approval_route` decision table ends in a catch-all row (`when: true`), so a quote always routes to `automatic` or `finance_review`. |
| `b2b_order_approval.route_rejected` | Only `reject_unroutable_quote` leads here. |
| `grooming_booking.unexpected_hold_transition` | `BookingOutcome` declares `no_show` and `expired` beyond the two the flow routes, but `record_no_show` only applies to a **confirmed** booking — by then the wait is over. Nothing can deliver either outcome into a held one. |
| `prescription_review.unexpected_reviewer_decision` | `ApprovalDecision` declares `escalated`, and both commands that signal a review decision send a literal `approved` or `rejected`. |

Re-measure after a full run with `.venv/bin/python coverage.py`, which reads
both stands' Process journals and lists whatever a run did not reach. A number
that drops means a branch stopped being exercised.

## Running the suite twice on one stand

`stack.sh provision` between runs, or a fresh stand. It tops the warehouse back
up and takes settled payout candidates out of the cycle — both consequences of
the gaps above, not of the tests. CI raises a fresh stand per run, so it needs
neither.

A fresh stand means stopping **both** stands: they share one Postgres, and
whichever is stopped last takes the databases with it. Stop only one and the
next `up` continues on the store the previous run left behind — which reads as
the suite failing when it is the stand that is stale.

## Layout

| Path | What it holds |
|---|---|
| `petshop_qa/` | the toolkit: config, tokens, GraphQL/REST/MCP clients, waiting, domain steps |
| `tests/` | one module per part of the store |
| `stack.sh` | raise, provision, drop a stand |
| `provision.sql` | opening stock and reference data |
| `coverage.py` | which declared flow states a run actually walked |
| `tests/test_performance.py` | what the store costs to use, as ratios rather than milliseconds |
| `tests/test_live_updates.py` | the fourth door: a subscription held open over a websocket |
| `tests/test_rest_surface.py` | every declared RESTified endpoint, its parameters and its walls |
| `tests/test_attacks.py`, `tests/test_file_attacks.py` | somebody hostile in front of the store, and in front of its bytes |
| `measure-memory.sh`, `measure-latency.sh`, `compare-latency.sh` | what the engine costs to serve this store |
