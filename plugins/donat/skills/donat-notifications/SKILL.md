---
name: donat-notifications
description: Use when an application needs to tell someone something - an in-app inbox, an email, an opt-out, a digest. There is a shipped module for this; adopt it rather than rebuilding it, and know the four things it asks of you.
---

# Telling someone something

Do not build this. `modules/notifications` ships it: an inbox, per-recipient
opt-out, a durable email send that never sends twice, a delivery log, and a
digest. It is ordinary metadata — the same YAML you write — so you can read all
of it, and you adopt it rather than depending on it.

Read `modules/notifications/README.md` before writing anything. This skill is
the part that is easy to get wrong.

## The four things it asks of you

**1. A recipient binding.** The module ships `notification.recipient` as a view
with the right shape and no rows. You replace the body:

```sql
create or replace view notification.recipient as
select u.id, u.email, u.email_verified, u.locale, u.timezone
from public.app_user u;
```

`u.id` must be the value that arrives as `X-Donat-User-Id`, because that is what
the inbox permission filters on. Get this wrong and every recipient reads an
empty feed while `notify` still succeeds — the one failure here that is quiet.

`create or replace view` refuses a shape that does not match, so a wrong column
list is a failed migration rather than a runtime surprise.

**2. Inherited roles, not renamed ones.** Never edit a role name inside the
module:

```yaml
inherited_roles:
  - role_name: customer
    role_set: [customer, notification_user]
  - role_name: app_backend
    role_set: [app_backend, notification_sender]
```

**3. A relay.** `NOTIFICATION_MAIL_BASE_URL` and `NOTIFICATION_MAIL_TOKEN`
pointing at something that accepts `POST /v1/email/messages` with an
`Idempotency-Key`. Sending through SendGrid/SES/Twilio instead is a different
state shape — the README has it, and it is `at_most_once` with a mandatory
`on_ambiguous`, which is a weaker guarantee you have to accept deliberately.

**4. A schedule, if you use the digest.** The sweep is `POST
/api/rest/notifications/digests/flush` and **the caller must send a fresh uuid
per tick**. A donat `cron_trigger` cannot: its payload is static, the sweep is
idempotent on that id, and it would run once and then do nothing for a day.

## How to trigger one

From a command, as an effect, which is how everything else starts a Process:

```yaml
effects:
  - start_process: …          # your own flow
```

…or just call the mutation as `notification_sender`:

```graphql
mutation { notify(workflow: "order_shipped", recipient_id: …,
                  title: …, body: …, request_id: …) { dispatch_id } }
```

`request_id` **is** the dedupe key, scoped to (recipient, workflow), retained 7
days. Derive it from the thing that happened — the order id, the invoice id —
and repeats collapse for free. Do not pass a fresh uuid unless you mean "send
this again".

Use `notify_digested` when the email should be batched. The bell still rings
immediately; only the mail waits.

## Reading it

```graphql
query { notification_inbox_aggregate(where: {read_at: {_is_null: true}})
        { aggregate { count } } }
```

Poll that. Do **not** put every signed-in user on a websocket subscription: the
engine re-executes each subscription once a second and per-user inboxes are
different SQL, so nothing batches. One bell per user is one query per second per
user against a 1000-subscription ceiling.

## What will trip you up

- **`notification_worker` has no API surface.** It holds command permissions
  only. If you find yourself wanting to query as it, you want a different role.
- **A suppressed notification is a row, not an absence.** Check
  `notification.delivery` before concluding something was dropped: `suppressed`,
  `skipped`, `deferred` and `failed` all mean different things.
- **The module does not render.** Both sends carry text and the relay composes
  the message. The engine's MJML renderer is unreachable from a Process
  (`plans/004-*`); this is not an oversight in the module.
- **A failed digest send stays claimed** and the next sweep does not retry it.
  Requeuing is one `update` and nothing does it for you.

## Extending it

Adding a channel is a channel row in `connectors.yaml` and a send state in
`flows/notification-delivery.yaml` — plus a `resolve_channel` call with the new
channel name, so the opt-out works for it too. Do not add a channel without the
opt-out branch; a channel a recipient cannot silence is the thing this module
exists to avoid.
