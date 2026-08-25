---
type: research
status: draft
date: 2026-08-08
---

# What donat needs to be a platform rather than an engine

Written after a week in which the engine was used to build something real — a
control plane with clients, plans, subscriptions and payments — by an agent
carrying the `plugins/donat` skills. Every gap below was found that way, by
something being built, not by reading the code. That is the only reason to
trust the list: none of it is speculative.

## The decided boundaries this respects

Three are settled and none of what follows relitigates them.

- **No admin role**, and no runtime configuration surface.
- **donat does not own identity** — `api-surfaces/010`. Tokens are verified,
  never issued.
- **No runtime plugin loading** — `declarative-saas/010`. Everything a
  deployment can do is compiled in or declared.

They are the reason the engine is worth building a platform on. Anything that
requires breaking one is out of scope by construction.

## What actually broke, in order of discovery

| What surfaced | Where it went |
|---|---|
| Inbound webhooks are a Stripe feature, not a connector capability | spec 009 |
| A verified event cannot begin work, only continue it | spec 009 §C |
| The engine's own migrations were not in the image | PR #27 |
| The interview never asked about create and delete | plugin |
| "Admin panel" collides with "no admin role" | plugin — it is *the platform* |
| Repository content drifted into the room's language | plugin |
| Form-encoded request bodies are unexpressible outbound | spec 009 §9, unclaimed |

Two of seven were engine defects. Five were the format or the guidance being
wrong about how the thing is used. That ratio is the finding.

## 1. The agent is the product surface

Every gap above was found by an agent building with the format, and most of
them were found *fast* — within one session each. That is not an accident of
this week; it is what adoption will look like. Nobody reads a metadata
reference and then writes a control plane. They ask for a control plane.

Consequences worth taking seriously:

**Learnability by a model is a design constraint, not documentation.** A key
whose meaning depends on a paragraph elsewhere costs every future user. The
`validate` list already gets this right: presence is declared rather than
inferred, so the failure is at deploy time with the table, role and entry
named. That property is what makes it teachable in six lines.

**A JSON Schema for the metadata is now higher-value than it looked.** It was
scoped once as editor ergonomics. Its real value is that an agent proposing
metadata can be told it is wrong *before* a deploy, in the loop where it is
still cheap. The `!include` tag remains the open question and is worth solving
rather than working around.

**The skills are versioned product.** They are shipped from this repository,
installed by users, and they encode decisions — the layer rule, the escalation
tiers, the standing defaults. They deserve the same review as an API, because
that is what they are.

## 2. Tenancy: decide it, do not let it drift

There is no first-class tenant anywhere — verified across
`crates/metadata`, `crates/schema` and the knowledgebase. A tenant is a session
variable and a row filter, which is elegant and, for the read path, complete.

What is hand-rolled every time: **provisioning** a tenant (the rows that must
exist before it can be used), **per-tenant limits**, and the operator's
cross-tenant view, which is a second role with a wider filter and is easy to
get subtly wrong.

Two honest options.

- **Stay minimal and say so.** A tenant is a session variable; provisioning is
  a command; the operator is a role. Write the ADR, ship the worked example,
  and the absence stops being a question every new user asks.
- **Ship it as declarable.** A `tenant:` block naming the session variable and
  the provisioning command, with the operator view derived rather than
  hand-written.

The first is probably right and is cheap. The failure mode is doing neither,
which is where it stands now: every deployment invents it, and the inventions
differ in ways that only show up as a leak.

**Resolved (2026-08-18), and closer to the second option than this note
expected.** See
[[declarative-saas/decisions/097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]].

**And the half that stayed hand-rolled (2026-08-20).** Isolation became
declarable; ownership inside one tenant did not, and cannot — the same table
needs opposite answers for different roles, so there is no single predicate to
inject. What this note says about tenancy — "every deployment invents it, and
the inventions differ in ways that only show up as a leak" — turned out to
apply to ownership too, and the Petshop example was carrying exactly that leak.
[[declarative-saas/decisions/099-an-unbounded-permission-says-so]] closes it the
only way left: not by moving the rule, but by making its absence something an
author writes and a reviewer reads.
A tenant is declared once in `tenancy.yaml` and applied by the compiler rather
than written into each permission, because the repetition *is* the problem this
section describes: absence cannot be reviewed, so a missing filter has to become
a boot failure rather than a leak. The operator's cross-tenant view is still not
a wider filter — it is out of scope until it can be an audited role.

## 3. Domain modules, not engine features

Recurring billing cost four to five engineer-days of *engine* work, and most
of what a subscription needs after that is metadata: the states, the grace
period, the dunning branch, the cancellation that respects a paid-through date.

That metadata is the same in every product that sells a subscription. Shipping
it once — as an `!include`-able module with its own migrations, commands, rules
and process — is a different kind of leverage from adding engine features, and
it is available today with no new mechanism.

Candidates, in the order a platform meets them: subscription billing, an audit
trail, soft-deletion with restore, approval workflows.

The risk is real and worth naming: a module that is almost right is worse than
none, because it is harder to leave than to adopt. Each needs the same
treatment `examples/petshop` got — running, conformance-covered, and honest
about what it does not do.

## 4. The deploy story is still three steps and a snapshot

`migrate` (engine) → `migrate` (application) → `migrate --metadata-dir`
(process revisions) → `validate` → serve, plus `dump-core-config` for the
embedded host, plus `--check` in CI so the snapshot cannot rot unnoticed.

Every step exists for a reason and PR #27 removed the worst of the coordination
problem. But an operator who gets the order wrong gets an error naming a
`donat.*` table they have never heard of, and a snapshot that goes stale stays
valid-looking. This is the surface where a platform is judged before anyone
sees a feature.

Worth exploring: a single `donat deploy` that runs the sequence in the correct
order against a directory, refusing rather than guessing when something is
absent. Not a new capability — a name for the one that exists.

## 5. The journal, served through its own rules

Still unbuilt, and a platform operator needs it at 03:00: *which* instances are
stuck. `process inspect` answers only about an instance you can already name.

The shape is unchanged from when it was first written up: declare the
`donat.process_*` tables as ordinary tables with ordinary per-role
`select_permissions`. It cannot be a bypass, because it is a permission. Relay
pagination, filters and the MCP tools work the day it is declared.

The one design decision is still views versus tables — publishing the tables
makes their shape a compatibility surface. A spike first: is the `donat` schema
introspectable from metadata at all?

## What not to do

**Do not add an admin surface for any of this.** The reason the journal item is
attractive is precisely that it needs no new mechanism.

**Do not build declarative schema management.** Everything is declarative
except the schema, which is the most interesting-looking gap in the product.
It is a product inside the product, and only half the input exists —
`crates/catalog` introspects, and there is no differ. A half-built diff engine
is worse than honest hand-written SQL.

**Do not chase provider breadth before the general mechanism lands.** Spec 009
is the prerequisite. Adding a second hand-written provider module before it
doubles the thing that has to be generalised.

## Suggested order

1. **Spec 009** — it is the only item blocking a real product, and every
   further integration is cheaper after it.
2. **The tenancy ADR** — an afternoon, and it stops a recurring question from
   being answered differently each time.
3. **JSON Schema for the metadata** — reprice it upward; the audience is agents
   more than editors.
4. **`donat deploy`** — naming the sequence that already exists.
5. **The journal through its own permissions** — after its spike.

Subscription billing as a shipped module sits behind spec 009 and is the first
place to test whether domain modules are leverage or a liability.
