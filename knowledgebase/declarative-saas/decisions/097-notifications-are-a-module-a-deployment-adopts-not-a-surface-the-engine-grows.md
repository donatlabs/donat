---
type: decision
status: accepted
date: 2026-08-21
features:
  - "[[declarative-saas]]"
  - "[[platform]]"
---

# Notifications are a module a deployment adopts, not a surface the engine grows

## Context

Every application built on this engine rebuilds the same four things: a table of
in-app messages, an opt-out check, a durable send that retries without sending
twice, and a log of what actually went out. The ask was for those to be present
out of the box — a trimmed version of what a dedicated notification service
gives you.

The obvious shape was a new metadata section. `notifications.yaml` declaring
recipients, channels and workflows, compiled into Processes, with the engine
owning the tables in the `donat` schema. It reads well, it is short, and it is
what a product would advertise.

[[platform/research-what-a-platform-needs]] §3 argues against it directly, from
the last time this question came up: recurring billing cost four to five
engineer-days of *engine* work and then turned out to be mostly metadata, and
the leverage was in shipping that metadata once rather than in growing the
engine. Its "what not to do" section is blunter still. Against that, the
counter-argument for a new section is real — a workflow written by hand is
about a hundred lines of YAML — but it is an argument about *verbosity*, and
verbosity is the thing you can measure only after someone has written the
hundred lines.

Checking what the engine could already do settled how much was at stake:
Processes give durability, retries and timers; commands give transactional
multi-writes and an idempotency window; tables and per-role permissions give the
inbox and the preferences; `for_each` gives a bounded sweep; connectors give the
send, with `sendgrid`'s `mail.send` already classified `AtMostOnce` and
therefore reachable. Nothing in the list was missing. What was missing was an
arrangement of it.

## Decision

**Notifications ship as `modules/notifications`: migrations plus ordinary
metadata, adopted by an application with `!include` and `inherited_roles`, and
adding no Rust to the engine.** The terse declaration that compiles into this is
deferred until the module has been used and its boilerplate has been shown to
repeat — the sequencing [[013-petshop-first-executable-requirements]] already
established.

**The module owns an application schema, `notification`, and not `donat.*`.**
The engine's schema is the engine's, and the shape of what is in it is a
compatibility surface; `plans/003` is still a spike away from serving any of it
through permissions. An application schema needs none of that and a deployment
can read it with the tools it already has.

**The recipient is a view the application supplies, and the module ships its
shape.** `notification.recipient` is created by the module's migration as a view
with the right columns and no rows; the application replaces the body with
`create or replace view` over whatever table already holds its users. Postgres
refuses a replacement whose columns differ, so the contract is enforced by the
database at migration time rather than by a convention in a README, and a
deployment that never replaces it is refused at the first send by
`require_found` rather than delivering into a void.

**Every outcome is a row in `notification.delivery`, including the outcomes that
sent nothing, and the digest is a status machine on that same row.**
`suppressed` (opted out), `skipped` (already read in the app), `deferred`
(waiting for the digest), `sending` (claimed by a sweep), `failed` (refused or
unreachable) and `sent` are all recorded by the state that learned them, under
`unique (dispatch_id, channel)`. There is no second table for the sweep's work
list: a claim is `deferred → sending`, which means one place to ask what
happened to a notification and no way for two tables to disagree. A notification that went nowhere is a row, not
an absence, which is the difference between an operator being able to answer
"what happened to this" and having to guess.

**The role that starts the sweep is not the role the sweep runs as.** A command
published over REST is reachable by whatever role gates it, and that role's
*other* commands come with it. `notification_worker`'s commands take a recipient
id as a plain argument, so a scheduler credential holding that role could read
any address or retire any digest unsent. `notification_scheduler` reaches the
entry point and nothing else, and `notification_worker` is never granted to a
token at all.

**Roles are parameterised through `inherited_roles`, and the four of them are
separated by what a credential holding one could do.** An adopting deployment
renames nothing inside the module. `notification_worker`, which every Process
state runs as, has no `select_permissions` anywhere, so no query in that role
reads a row directly; but its commands are ordinary mutations that take a
recipient id as a plain argument, so a token holding it could read any address
through `resolve_channel` or retire any digest unsent. It is therefore a
`run_as` and never a grant, which is why the published sweep got
`notification_scheduler` rather than sharing it. Calling the worker "a role with
no API surface" would have been the comfortable half of the truth.

## Alternatives

| Option | Why Not |
|--------|---------|
| A `notifications.yaml` metadata section compiled into Processes | Weeks of engine work and a new forever-compatible surface, to save verbosity nobody has yet complained about. [[platform/research-what-a-platform-needs]] §3 is the record of the last time this trade was taken the other way. |
| Own the tables in the `donat` schema, like `donat.file_uploads` | Their shape becomes the engine's compatibility surface, and every deployment gets them whether it wants notifications or not. Nothing in the repository tracks a `donat.*` table today, and `plans/003` says why that is still open. |
| Ship the declaration compiled into the binary, as `idp_admin.yaml` does | That precedent holds where the declaration is identical everywhere, which "who may administer the identity provider" is. A deployment's notification workflows are not: the module would have to be configuration-driven to be adopted at all, and configuration is how a module becomes a framework. |
| A `subscribers` table the engine owns, synchronised by the application | A second copy of the user table and a synchronisation problem, in exchange for non-null columns. The view costs a nullability workaround in one place (`plans/005`); the copy would cost correctness in every deployment that let it drift. |
| Deliver the in-app message inside the `notify` command, synchronously | A command cannot branch, so the preference check would have to move somewhere else, and the delivery log would be written from two places. One worker poll (250 ms) is the price of having one answer to "what happened". |
| Two Processes, one per channel | Then "what happened to this notification" is two instances and two journals, and the escalation — *email only if the bell went unread* — cannot be expressed at all. |
| A shipped `cron_triggers.yaml` for the digest sweep | A cron trigger's payload is static, the sweep is idempotent on the caller's request id, and the two together mean a sweep that runs once and then does nothing for a day. Shipping it would look like the feature working. The module publishes the sweep over REST and says plainly that the schedule is the deployment's. |
| Compose the digest's message in a command | There is no string concatenation in the command grammar, deliberately. The relay is given the workflow and the count. |
| Claim a digest group with a read (`select_one` + `require_found`) | It reads like a check and is not one. Under READ COMMITTED two fan-out items for one recipient both see unclaimed rows, both proceed, and the recipient gets two messages. The claim is an `update_many` whose `by` carries `status: deferred` beside the primary key, with `require_each`: the second item blocks on the row locks, re-evaluates that predicate after the first commits, matches nothing, and is rejected. `require_non_empty` on the read covers the other order, where the second item finds the group already empty and would otherwise send a digest of zero. |
| A separate `digest_queue` table as the sweep's work list | It was two tables that could disagree — a row claimed in one and still deferred in the other — and it needed a retirement state, because a command may name a row only by its primary key and the grammar has no batch delete. The delivery log already carries non-null `recipient_id` and `workflow`, so the claim became a status transition on the row that was already there: `deferred` → `sending` → `sent`. |
| Let the sweep discover its own groups and fan out over them | This is the shape the module had first, and it wedges. A `select_many` cannot be bounded — `maximum_rows` is accepted and ignored (`plans/006`) — and a `for_each` whose input exceeds `max_items` fails the whole instance rather than trimming. One notification past the ceiling and every sweep fails identically, forever, having claimed nothing. Enumeration moved to the scheduler, which pages `notification.pending_digest` through a `select_permission` carrying a `limit` — a bound the engine actually enforces. |
| Keep the fan-out and raise `max_items` to its 256 ceiling | It moves the cliff without removing it, and a digest that works until a busy Tuesday is the "almost right" this module is trying not to be. |
| Scope the digest's record by status alone | Two sweeps can hold `sending` rows for one recipient at once — the second claimed notifications that arrived after the first's claim — and a record matching every `sending` row stamps the other sweep's messages with this one's provider id. The claim stamps `claim_id` and everything after it names that id. |
| Leave a failed digest send's rows claimed for an operator | It is the one way this module could quietly lose mail: `sending` is invisible to the next sweep. The send's `on_error` returns its own claim to `deferred`, and the next sweep finds it exactly as it found it the first time. |


## Consequences

A deployment gets an inbox, preferences, a delivery log, an escalation and a
digest for one `donat migrate`, a view it writes, and about a dozen `!include`
lines — and can read every one of those lines, because they are the same YAML it
writes itself. There is no compiler between the declaration and the behaviour,
so there is nothing to learn beyond what the engine already asks of it.

The cost is verbosity, and it is real: the delivery Process is 180 lines and the
digest sweep another 90. That is the number the deferred `notifications.yaml`
would be judged against, and now it exists to be judged.

Two engine gaps surfaced while building it, and both are written up rather than
worked around silently. `local.document` — the MJML renderer the email channel
was designed around — is unreachable from any Process, because a `local.*`
instance publishes no operation contract (`plans/004`). A `for_each`
item cannot assert non-null, so a fan-out can never consume a column that came
from a view (`plans/005`). And a `select_many`'s declared row bound is accepted
and then ignored, which — combined with a fan-out that fails rather than trims —
is what made the first digest design wedge (`plans/006`).

The third one is worth dwelling on, because it changed the design rather than
the workaround. Removing the fan-out to escape it turned out to be the better
shape anyway: a Process with no `for_each` can branch, so "this recipient has no
address any more" and "this group emptied while the scheduler was reading the
list" became routed outcomes instead of command rejections, and the digest send
could go back to promising a non-null recipient. The engine gap forced a
simplification the module should have started from. Neither was closed
here: an engine change belongs in a slice of its own with its own conformance
case, not in the module that happened to find it.
