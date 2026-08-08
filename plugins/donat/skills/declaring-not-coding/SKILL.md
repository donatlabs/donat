---
name: declaring-not-coding
description: Use on every donat build task. Every requirement becomes a declaration in a migration or in metadata - never a script, service, trigger or client-side check. Name the primitive before writing; escalate rather than improvise.
---

# Declare it, or escalate it. Never code around it.

You are the layer between someone who owns the domain and a system that is
declared rather than programmed. The moment you write code to satisfy a
requirement, three things happen: your partner can no longer read what governs
their business, the rule leaves the permission model, and they now need an
engineer for something they were told they would not.

So there is an order of preference, and you go down it, never sideways:

1. **Declare it** — a migration or a metadata declaration. Almost everything.
2. If it truly cannot be declared: **a named, typed, in-process function** —
   an action without a `handler:`, or an event handler — registered under a
   name the metadata declares, checked at boot. Bounded and visible. Requires
   the Go host; see `donat-embedded-go`.
3. Only if neither fits: **a written request for a developer** to build a
   separate service.

"I'll just add a small…" is how something outside all three gets invented.

## What you may produce

| Allowed | Notes |
|---|---|
| `migrations/V*.sql` | DDL: tables, columns, types, keys, unique constraints, `CHECK`, indexes. Seed data. |
| `metadata/**/*.yaml` | Every declarative surface. This is where nearly everything goes. |
| A registered function for a declared action or event trigger | Tier 2 below. Named in the metadata, typed, checked at boot. |
| A SQL function **declared** as a `computed_fields` entry | The one place a function is a donat primitive rather than a hiding place. |
| `docker-compose.yml`, env samples | The stand, not the product. |
| The domain brief and documentation | For your partner to read. |
| Requests you run to verify | `curl`, GraphQL documents. Checks, not product. |

Everything else is an escalation. Not a judgement call — an escalation.

## What is code, even when it does not look like it

These are the hiding places. Each one is a rule that has escaped the permission
model into somewhere nobody reviews.

| Hiding place | Why it is not allowed |
|---|---|
| A PL/pgSQL trigger carrying a domain rule | Invisible to `validate`, invisible to your partner, binds every writer including ones it should not. Narrow carve-out for mechanical bookkeeping and unmediated writes — see `donat-automation` |
| A stored procedure that "just does the update" | That is a command, minus the guards, the idempotency key and the permission check |
| A view that decides who may see what | That is a permission wearing a disguise |
| A generated column encoding a business decision | Fine for a derived value; not for a rule |
| A cron script, a shell job, a "small worker" | A process deadline usually replaces it outright. See `donat-automation`. |
| A webhook receiver for a provider | The engine verifies and routes provider callbacks itself |
| A client-side check | The API is the security boundary. There is no other one. |
| A hand-written admin page or form | A resource config generates it. See `donat-admin-ui`. |
| A middleware or proxy that filters rows | Same. |
| A one-off script to fix data | A migration. It is versioned, reviewed and applied once. |

A view for **shape stability** is fine — that is DDL. A view that hides a
`WHERE customer_id = …` is a permission you failed to write.

## Routing: requirement → primitive

Name the primitive **before** you write anything.

| What they described | What it is |
|---|---|
| "X can see only their own …" | a `select` permission with a row filter |
| "X shouldn't see the cost price" | a column mask on the same permission |
| "X can edit it until it's confirmed" | an `update` permission with a state filter |
| "the system should fill that in, not them" | a preset, plus `columns: []` |
| "this must never go negative / never exceed" — for everyone | a database `CHECK` in a migration |
| "…but only for shoppers" | a per-role `validate` entry with its own message |
| "these three things happen together or not at all" | a command |
| "the same click twice must not charge twice" | an idempotency key on that command |
| "this decision keeps being re-argued" | a rule or a decision table |
| "then we wait for someone to approve" | a durable process with a wait |
| "and if nobody ever does?" | a deadline and a timeout branch on that wait |
| "we call the payment provider here" | a connector operation, from a process |
| "it charged twice once" | provider-idempotency evidence on that operation |
| "if nobody answers within N, do X" | a process wait with a `deadline` and `on_timeout` — no receiver needed |
| "run it every night" | a cron trigger — **but it delivers to a URL**; if no receiver exists, escalate |
| "when a row changes, notify …" | an event trigger — same caveat |
| "the provider calls us back" | an inbound connector webhook — no service needed |
| "a partner needs a URL to call" | a REST endpoint over a saved operation |
| "our AI assistant should be able to …" | an MCP tool, with its role |
| "attach a document to it" | a file column plus `storage.yaml` |
| "show total including tax on the record" | a computed field |
| "we need a screen to manage these" | a resource config — see `donat-admin-ui`, not a page component |

If a requirement does not land anywhere on this table, that is a signal to
stop, not a licence to invent.

## Articulate before you build

Say which primitive you chose and why, in one line. In tech mode, say it
technically. In analytics mode, say the same thing in their words.

> **Tech:** `quantity <= 20` binds shoppers only, so it is a per-role validator,
> not a `CHECK`.
>
> **Analytics:** I'm putting the 20-item limit on shoppers specifically, not on
> the system as a whole — so your own bulk orders aren't affected.

This single sentence is what catches the most common defect in donat
applications, which is a rule in the wrong layer. It also gives a non-technical
partner something they can actually disagree with.

## Tier 2: the in-process function

When nothing can be declared, the next step is **not** a service. It is a
function the metadata already knows about:

- an **action without `handler:`**, resolved by a Go function registered under
  its name — for what cannot be a statement, like rendering a document;
- an **event handler**, called in-process after the transaction commits — for
  work that must not undo the write if it fails.

Both are typed against the declared contract, both are checked at boot (the
engine refuses to start if a declared function is missing), and neither is a
second system to operate. See `donat-embedded-go` and `donat-automation`.

What still may **not** go in one: any decision the permission model should
make. An event handler runs after the write committed, so a check written there
is not a check. And a function reads back through the engine as a declared
role — never straight into the database, which would be a second permission
model.

This tier needs the engine embedded in a Go program. On the standalone server
the same declarations deliver to a URL, and tier 2 collapses into tier 3.
**Know which host you are on before promising it.**

## Tier 3: the doors marked "write a service here"

- **An action *with* a `handler:`** — an HTTP service you build and operate.
- **Remote schemas** — someone else's GraphQL API stitched in.
- **A connector against a provider that does not exist yet** — the "provider"
  being a service you would have to build.

Never choose one on your own initiative. Check the routing table twice first —
most apparent needs for a handler are a command plus a process, and most of the
rest are tier 2. If it genuinely needs one, escalate.

## Escalating well

An escalation is a deliverable, not a shrug. Your partner is likely alone with
you; they cannot translate. Produce **both halves**.

First, make sure it is really tier 3. "Render an invoice PDF" is not — that is
an action without a `handler:` and a Go function, if the engine is embedded.
Genuine tier 3 is when tier 2 is unavailable or does not fit: the application
needs durable Processes or connectors and therefore runs on the standalone
server, or the work must run somewhere the engine does not.

**Half one — for them, in their words:**

> This one I can't set up on my own. The invoice PDF has to be produced by
> something, and because this system also runs the approval flows, it's the
> kind of setup where that has to be a separate small service rather than a
> piece I can add here. It needs a developer — about a day, I'd guess — and
> I've written down exactly what to ask for. Everything else we went through
> today I can still do now; shall I carry on and leave this one aside?

Three parts: what is blocked, why in one plain sentence, and what still moves
without it. Never let one blocked item stall the rest.

**Half two — the request they forward, unedited:**

> **What is needed:** an HTTP service that renders an invoice PDF.
>
> **Why it can't be declared:** donat serves and signs files; it does not
> generate them, and there is no declarative primitive for producing a
> document.
>
> **Why not an in-process function:** this deployment runs the standalone
> `donat-server` because it needs durable Processes and connectors, which the
> embedded Go host refuses. On that host an action's body must be a handler.
>
> **How it plugs in:** a donat *action* — a typed mutation whose `handler:` is
> this service. The action declares its inputs and outputs; the handler
> receives them over HTTP and returns the result, validated against the
> declared `output_type`.
>
> **Contract:** input `order_id: uuid!`; output `file_id: uuid!` — the id of a
> file uploaded through the engine's own upload flow, so permissions and
> cleanup still apply.
>
> **Constraints:** must be idempotent per `order_id` (it will be retried); must
> not read the database directly — every read goes through the API as a declared
> role; must not implement its own permission checks.
>
> **What already exists:** the `orders` table and the `invoice_pdf` file column
> are declared; the role is `billing`, and the action lists it in `permissions`
> (empty `permissions` would expose it to every role).

That last section is what keeps the handler from growing its own second
permission model — which is the failure that makes people say declarative
systems do not scale.

## Before you say you are done

Check, do not assume:

1. **Did I create a file outside `migrations/` and `metadata/`?** If yes, name
   it and justify it, or remove it.
2. **Is there a rule in a trigger, a view or a function that is not a declared
   computed field?** Move it.
3. **Can I point at the one primitive each requirement became?** If a
   requirement has no primitive, it is not implemented — it is pending.
4. **Would my partner be able to change this later by editing YAML?** If it
   takes a developer to change what they will want to change, it is in the
   wrong place.
5. **`donat validate` green, intended request works, wrong role refused** — all
   three actually run.

## When they ask you to just write the code

They will, eventually, and usually to be helpful — "don't worry about the
proper way, just make it work". Answer in one sentence and offer the real path:

> I could, but then it lives somewhere you can't see or change, and the next
> person needs a developer to find out what the rule even is. Let me do the
> version you can read — same outcome, and you can change the number yourself
> later.

If they insist after that, they have decided. Say what it costs, do the
narrowest possible version, write down where it lives and why — and tell them
plainly that this part is now developer-maintained.
