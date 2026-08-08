# 003 — Serve the Process journal through the engine's own permissions

**Written against:** `768f89c`
**Kind:** design spec
**Effort:** S–M

## The gap

`donat process inspect --source <s> --instance <uuid>` answers a question you
can only ask once you already know the answer's subject. The question an
operator actually has at 03:00 is *which* instances are stuck — and today the
only way to ask it is `psql` against `donat.process_instances`.

That is a real gap, and it is not the one the CLI was built to close: ADR
`declarative-saas/002` permits `inspect` and `verify-history` by name and
forbids everything mutating, so the CLI is complete as specified. What is
missing is the listing question, and the ADR does not forbid it — it delegates
it: "operators use deployment-owned observability for the internal journal."

Meanwhile the durable-execution field treats exactly this as the thing worth
choosing a product for. Temporal's pitch is its Web UI: inspect a workflow's
history, find where it failed, step through it.

## The idea

Do not build an admin surface. **Declare the journal as ordinary tables with
ordinary per-role `select_permissions`, and let the existing engine serve it.**

The `donat.*` tables already exist in the migrated database. A deployment that
wants operator visibility adds table metadata for them and a role — say
`operator` — with select permissions and a row filter. From that moment the
journal is queryable over GraphQL, REST and MCP, with the engine's own
permission checks, its own error contract, its own limits, and no new code
path to audit.

The properties this buys are unusually good:

- **It cannot be a permission bypass**, because it is not a bypass — it is a
  permission. The blocking rule in `CLAUDE.md` is satisfied by construction
  rather than by review.
- **It is read-only** because `select_permissions` are read-only; nothing needs
  to enforce that separately.
- **It composes with what exists.** Relay pagination, filters, aggregates and
  the MCP tools all work on it the day it is declared, including "show me
  every instance in state X older than an hour" — which is the actual question.
- **An operator dashboard is a GraphQL client**, not a feature of the engine.

## What the work actually is

Mostly not code:

1. A shipped metadata fragment describing the `donat.process_*` tables —
   columns, relationships between instance, events, activity jobs and
   transition logs — that a deployment can `!include` rather than write.
2. A worked `operator` role in `examples/petshop`, so there is one correct
   example of what to expose and what to withhold. This is the part that needs
   judgement: `input_json` and `state_json` carry business payloads, and an
   operator role that can read every process's state has read access to a great
   deal of the domain by the back door. The example must show the column mask
   and say why.
3. Documentation placing it beside `process inspect`: the CLI for one instance
   on a machine with database credentials, the declared role for a dashboard.

Possibly one engine change, to be confirmed rather than assumed: the `donat`
schema's tables must be introspectable and referenceable from metadata like
any other schema. Verify with a spike before planning further — if something
excludes the `donat` schema from introspection, that is the one piece of real
code in this item.

## Trade-offs

**Against:** it puts the engine's internal journal into a deployment's public
API surface, where a mistake in the row filter is a data leak of the domain,
not of engine internals. That risk is the reason the example matters more than
the fragment.

**Also against:** the journal's shape becomes something deployments depend on,
which makes future migrations of `donat.*` a compatibility question. Today
those tables are private. This should be decided deliberately — perhaps by
shipping *views* with a stable shape rather than exposing the tables directly,
which costs one migration and buys the freedom to change the underlying
columns later.

That view question is the main design decision in this item, and it should be
settled before anything is written.

## Verification

- A conformance case: an `operator` role lists instances by state and gets
  exactly the rows its filter allows; a role without the permission gets the
  ordinary access-denied contract, unchanged.
- A case proving a non-operator role cannot reach the journal at all, so the
  fragment cannot be added accidentally.
- `examples/petshop` still raises and passes its system tests with the role
  declared.

## Escape hatch

If the spike finds that serving `donat.*` requires the planner to treat that
schema specially, **stop and report**. The value here is entirely that it
needs no new mechanism; a special case in the planner would mean this idea is
not what it appeared to be, and the item should be re-priced as ordinary
feature work.
