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
python3 -m venv tests-system/.venv
tests-system/.venv/bin/pip install -r tests-system/requirements.txt

export PETSHOP_BASE_URL=http://127.0.0.1:8080
export PETSHOP_PROVIDERS_URL=http://127.0.0.1:8099
cd tests-system && .venv/bin/python -m pytest        # or: make petshop-system-tests

tests-system/stack.sh down               # when finished
```

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

See [ADR 033](../knowledgebase/declarative-saas/decisions/033-a-declaration-the-runtime-ignores-is-a-defect.md).
Three of the four were fields that parsed, validated and deployed while doing
nothing — which is indistinguishable from working until something in
production quietly fails.

Two gaps in the example itself are still open, and the suite works around
them in `provision.sql`:

- **A payout cycle cannot be closed.** `vendor_payout_candidate` reports every
  `accepted` vendor order against one cycle id fixed in the view, and
  `vendor_order.status` has no settled state to move to — so a paid candidate
  stays a candidate and the next run collides.
- **Goods cannot be received.** Neither per-location inventory nor the B2B,
  subscription and marketplace reference rows can be created or replenished by
  any role, through any surface.

## Running the suite twice on one stand

`stack.sh provision` between runs, or a fresh stand. It tops the warehouse back
up and takes settled payout candidates out of the cycle — both consequences of
the gaps above, not of the tests. CI raises a fresh stand per run, so it needs
neither.

## Layout

| Path | What it holds |
|---|---|
| `petshop_qa/` | the toolkit: config, tokens, GraphQL/REST/MCP clients, waiting, domain steps |
| `tests/` | one module per part of the store |
| `stack.sh` | raise, provision, drop a stand |
| `provision.sql` | opening stock and reference data |
| `measure-memory.sh`, `measure-latency.sh`, `compare-latency.sh` | what the engine costs to serve this store |
