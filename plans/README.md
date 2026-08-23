# Advisor plans

Design specs produced by a direction review of the platform, written against
`768f89c`. These describe work that has **not** been decided on — each one
carries the fork or the question that has to be settled before code is worth
writing. They are deliberately separate from `specs/`, which documents
capabilities that shipped.

| # | Spec | Effort | Status | Depends on |
|---|---|---|---|---|
| [001](001-subscription-multiplexing.md) | Multiplex live-query subscriptions | M or L, by fork | TODO — needs a decision | sqlgen parameterisation, if Fork B |
| [002](002-json-schema-for-metadata.md) | Publish a JSON Schema for the metadata | M | TODO | — |
| [003](003-the-journal-served-through-its-own-rules.md) | Serve the Process journal through the engine's own permissions | S–M | TODO — spike first | — |
| [004](004-a-local-capability-no-process-can-name.md) | Publish local capability contracts so a Process can name one | M | TODO — one fork to settle | — |
| [005](005-a-fan-out-item-cannot-assert-what-it-is.md) | `require_non_null` on a fan-out item, so a view and a `for_each` can be used together | S | TODO | — |
| [006](006-a-declared-row-bound-that-nothing-enforces.md) | Enforce `select_many`'s declared row bound, and decide whether a bound refuses or truncates | S–M | TODO — one design question | — |
| [007](007-a-permanent-metadata-collision-retried-as-if-it-were-a-cold-database.md) | A tracked-table name collision hangs boot instead of failing `validate` | S | **DONE** | — |
| [008](008-the-identity-we-ship-is-unreachable-from-a-declaration.md) | Publish the identity provider as a connector, so a Process can read a person's address | M | TODO | — |
| [009](009-an-inherited-role-grants-the-tables-and-not-the-commands.md) | `inherited_roles` carries table permissions and drops command ones | S–M | TODO — one fork to settle | — |

## Suggested order

**002 first.** It is self-contained, it has no fork to resolve, and it is the
one a new user meets in their first hour. Nothing else depends on it, so it
can go in parallel with anything.

**003 second**, starting with the spike it names — whether `donat.*` is
introspectable from metadata. If the spike is clean, most of the remaining
work is a metadata fragment and a worked example, not engine code.

**001 last, and only after answering one question**: is the sqlgen
parameterisation refactor in scope this cycle? Fork B is the real feature and
depends on it; Fork A is cheap but solves the case nobody complained about and
would make the metric look solved while the workload that motivated it is
untouched. If the answer is no, the honest move is to lower the advertised
subscription limit to one the poll budget can serve, and revisit.

## Considered and not written up

**Declarative schema management** — generating migrations from a declared
desired schema, the way Supabase's pg-delta, Atlas and Prisma Migrate now do.
It is the most interesting gap: everything in donat is declarative except the
schema itself, which is hand-written versioned SQL that `donat validate` then
checks the metadata against. It is not written up because it is a product
inside the product, and only half the input exists — `crates/catalog` gives
introspection, but there is no differ. A half-built diff engine is worse than
honest hand-written SQL. Worth revisiting as a deliberate, resourced decision,
not as an improvement item.

**MCP plus durable execution as a position** — the field is converging on
durable execution as the reliability layer under AI agents, and donat already
has both halves over one permission model: an agent calls a tool, the tool
starts a process, the process survives the agent's death and does not charge
twice. Not written up because it is not a design problem — the code exists.
It needs a worked example and a paragraph on the landing page, which is
writing, not engineering.

**Per-role value validators are invisible** — `validate` lists with per-entry
messages are implemented (ADR `declarative-saas/032`), documented in
`examples/petshop/README.md`, and absent from the main README, which is the
page someone comparing against Hasura reads. A documentation fix, tracked here
only so it is not rediscovered as a missing feature a third time.
