---
name: donat-notifications
description: Use when an application needs to tell someone something - an in-app inbox, an email, an opt-out, a digest. There is a shipped module for this; adopt it rather than rebuilding it, and know the five things it asks of you.
---

# Telling someone something

Do not build this. `modules/notifications` ships it: an inbox, per-recipient
opt-out, a durable email send that never sends twice, a delivery log, and a
digest. It is ordinary metadata — the same YAML you write — so you can read all
of it, and you adopt it rather than depending on it.

Read `modules/notifications/README.md` before writing anything. This skill is
the part that is easy to get wrong.

## The five things it asks of you

**1. A recipient binding — two views, not one.** The module ships both with the
right shape and no rows; you replace the bodies:

```sql
create or replace view notification.recipient as
select u.id::text, u.locale, u.timezone from public.app_user u;

create or replace view notification.recipient_address as
select u.id::text as recipient_id, 'email'::text as channel,
       u.email as address, u.email_verified as verified
from public.app_user u where u.email is not null;
```

`recipient.id` must be the value that arrives as `X-Donat-User-Id` — it is
`text`, not `uuid`, because a session variable is a string and applications key
people by all sorts of things. Get it wrong and every recipient reads an empty
feed while `notify` still succeeds; that is the one quiet failure here.

Two views because an address is per channel and a person's language is not.
`create or replace view` refuses a replacement whose columns differ, so a single
view carrying `email` would have frozen the set of channels forever.

**2. Roles — inherit only `notification_user`.** This is the trap:

```yaml
inherited_roles:
  - role_name: customer
    role_set: [customer, notification_user]      # works
```

`inherited_roles` carries **table** permissions and **not command** permissions
(`plans/009-*`, pinned by
`examples/petshop/metadata/inherited_roles_test.yaml`). `notification_user` is permissions on two
tables, so a shopper inherits it and reads their feed. `notification_sender` and
`notification_scheduler` own *commands*, so inheriting them grants nothing you
can call — a token that triggers notifications holds `notification_sender`
itself, and a scheduler holds `notification_scheduler` itself. Never grant
`notification_worker` to anything: it is a `run_as`, and its commands take a
recipient id as a plain argument.

**3. A sender.** `NOTIFICATION_MAIL_BASE_URL` and `NOTIFICATION_MAIL_TOKEN`
pointing at the module's shipped contract, or your own file in place of
`connectors/notification-mail.yaml`. See *Bringing your own sender* below.

**4. A schedule, if you use the digest.** The scheduler reads
`notification_pending_digest` and calls the sweep once per row:

```
POST /api/rest/notifications/digests/flush
{ "request_id": "<fresh uuid>", "recipient_id": "…", "workflow": "…" }
```

A fresh id per tick — the sweep is idempotent on it. A donat `cron_trigger`
cannot supply one (its payload is static), which is why the module ships none.

**5. Three lists in your `rules.yaml`.** A deployment has exactly one and it is
a mapping, so the module's rules join *your* lists. The `types:` line is the one
people forget, and without it `validate` says `unknown connector contract type
NotificationPayload`.

## How to trigger one

```graphql
mutation Notify($p: NotificationPayload) {
  notify(workflow: "order_shipped", recipient_id: …, title: …, body: …,
         payload: $p, request_id: …) { dispatch_id }
}
```

`request_id` **is** the dedupe key, scoped to (recipient, workflow), retained 7
days. Derive it from the thing that happened — the order id, the booking id —
and repeats collapse for free. Do not pass a fresh uuid unless you mean "send
this again".

`payload` is optional and opaque to the module: it is what a template renders
from, bounded at 4 KiB. Use `notify_digested` when the email should be batched;
the bell still rings immediately.

From inside a flow, it is a command state like any other:

```yaml
- id: tell_the_customer
  command:
    name: notify
    run_as: notification_sender
    arguments: { workflow: {literal: order_shipped}, recipient_id: {…}, … }
    next: done
```

## Reading it

```graphql
query { notification_inbox_aggregate(where: {read_at: {_is_null: true}})
        { aggregate { count } } }
```

Poll that. Do **not** put every signed-in user on a websocket subscription: the
engine re-executes each subscription once a second and per-user inboxes are
different SQL, so nothing batches. One bell per user is one query per second per
user against a 1000-subscription ceiling.

## Bringing your own sender

One file — `connectors/notification-mail.yaml`. Include yours instead of the
module's; no flow and no command changes. The flow relies on exactly this:

- an instance named `notification_mail`,
- with operations `send_email` **and** `send_digest` (missing the second fails
  `validate`),
- whose inputs are `message_key`, `recipient`, `subject`, `body`, `workflow`,
  `locale`, `payload` — and `pending` for the digest,
- whose `body` **consumes every one of them**.

That last rule is the one that will cost you an hour: an input the body never
mentions is refused as `connector operation input contains an undeclared value`,
with `class=Invariant`, **before the request leaves** — so your relay sees
nothing and the log says only that an activity failed. Everything else is yours:
URL, method, path, auth, timeouts, retries, error map, and the field names on
the wire (`To`, `Subject`, `TextBody`, whatever your provider calls them).

## What will trip you up

- **`notification_worker` has no *table* surface, but its commands are
  mutations.** No query in that role reads a row; a token holding it could read
  any recipient's address through `notification_resolve_channel`. It is a
  `run_as` and never a grant.
- **A suppressed notification is a row, not an absence.** Check
  `notification.delivery`: `suppressed` (opted out), `skipped` (read in the app
  first), `deferred` (waiting for the sweep), `sending` (claimed), `failed`.
- **The module does not render.** Both sends carry text plus `workflow`,
  `locale` and `payload`, and the relay composes. The engine's MJML renderer is
  unreachable from a Process (`plans/004-*`); this is not an oversight.
- **A failed digest send stays `sending`.** The sweep's requeue stage returns it
  to `deferred`, but a send that failed *after* the relay accepted it does not
  come back on its own.
- **Internal commands are `notification_*`.** Only `notify`, `notify_digested`
  and `flush_notification_digests` are short, because those are the ones you
  call. A module must not squat generic names in your namespace — its
  `record_delivery` once collided with a store's own.

## Proving it in your deployment

Adopting is four declarations and a migration; whether they line up is one test
beside the flow that notifies. The shape, from `examples/petshop`:

```yaml
- providers: !include ../testdata/providers.yaml   # the relay's answer
- as: { role: customer, user: customer-1 }
- graphql: 'mutation { ... }'                      # whatever triggers it
- await: { row: notification.inbox }               # the bell, schema-qualified
- await:                                           # then the log, both channels
    sql: |
      select channel, status from notification.delivery
      where workflow = 'your_workflow' order by channel
    expect:
      - { channel: email, status: sent }
      - { channel: in_app, status: sent }
- calls: { path: /v1/email/messages, count: 1, body: { recipient: … } }
```

Two things bite here. **Wait on the delivery log, not on the relay**: the bell
is written before the mail is sent, so a `calls` step that follows the inbox row
finds nothing. And **`await.row` takes the real table name** —
`notification.inbox`, not the GraphQL `notification_inbox`.

The module's own tests are `*_test.yaml` beside its declarations
(`make app-test APP_DIR=modules/notifications`), and
`modules/notifications/examples/deployment` is a worked adoption you can copy:
its own sender, and the escalation turned on.

## Extending it: a new channel

Adding Telegram or SMS is four things, and none of them edits the module:

1. a row in `notification.channel` (your migration),
2. a branch in your `notification.recipient_address` view supplying that
   channel's address,
3. a connector instance and operation for the provider,
4. a send state in a flow, preceded by a `notification_resolve_channel` call
   with the new channel name — **so the opt-out works for it too**.

A channel a recipient cannot silence is the thing this module exists to avoid.

Note the shape of (4): a compiled provider like `telegram` or `sendgrid`
publishes no idempotency key, so its send is `at_most_once` — `retry_on: []`,
`max_attempts: 1`, a mandatory `on_ambiguous` route, and no `idempotency_key`.
Compilation refuses a mismatch in either direction, so you cannot reuse the
email state's shape.
