# Notifications — a donat domain module

Notifications as declarations, not as a service: an in-app inbox, per-recipient
opt-out, email delivery through a durable Process, and a digest that collapses a
backlog into one message.

This is a **module**, not an engine feature. It adds no Rust and no new metadata
language — it is migrations plus the same YAML any application writes, shipped
once so that every project does not rebuild it. The engine has no notion of a
notification; it has tables, permissions, commands, rules, processes and
connectors, and this directory is those things arranged.

## What it does

| Table | What it is |
|---|---|
| `notification.dispatch` | one row per triggered notification: the workflow, the recipient, what it says, whether its email is batched |
| `notification.inbox` | the in-app message, with `seen_at` / `read_at` / `archived_at` |
| `notification.delivery` | what happened per channel — `sent`, `suppressed`, `skipped`, `deferred`, `sending`, `failed` — including channels never tried |
| `notification.preference` | opt-out per recipient, per workflow, per channel |
| `notification.pending_digest` | a view: which recipients are owed a digest, for the scheduler to page through |
| `notification.channel` | the channels this deployment delivers on; adding one is an insert, not an edit to the module |
| `notification.recipient` | **the application's own binding** — who a person is |
| `notification.recipient_address` | **the application's own binding** — where each channel reaches them |

Four roles are declared, and a deployment reaches them through
`inherited_roles` rather than by editing this module:

- **`notification_user`** — reads their own feed and nobody else's, counts their
  unread with an aggregate, marks a notification seen/read/archived, and owns
  their own preferences. Every permission is filtered by `X-Donat-User-Id`.
- **`notification_sender`** — may trigger a notification (`notify`,
  `notify_digested`) and nothing else.
- **`notification_scheduler`** — may call `flush_notification_digests`, and
  nothing else. This is the role a scheduler's credential holds.
- **`notification_worker`** — the role every Process state runs as. It holds
  command permissions only, so it has no *table* surface: no query in that role
  reads a row directly. **Never grant it to a token.** Its commands are still
  mutations, and they take a recipient id as a plain argument — a credential
  holding this role could read any recipient's address through
  `resolve_channel`, or retire any recipient's digest without sending it. That
  is why the sweep has a role of its own rather than sharing this one.

## What happens when you call `notify`

```
notify (command)              records the dispatch, starts the Process
  └─ notification_delivery (durable Process)
       resolve_in_app  → opted out?  → write the feed row │ record `suppressed`
       resolve_email   → opted out?  → …                  │ record `suppressed`
       route_address   → no address? → …                  │ record `failed`
       route_digest    → batched?    → …                  │ record `deferred`
       wait_before_email             the delay the deployment declared
       check_seen      → already read in the app? → …     │ record `skipped`
       send_email                    the mail relay
       record_email_sent / record_email_failure
```

Every branch ends in a row in `notification.delivery`. Nothing is dropped
silently, and "what happened to this notification" is one query.

## Adopting it

**1. Apply the DDL.** The module's migrations are versioned by timestamp, so
they share one `refinery_schema_history` with the engine's set and your own:

```
donat migrate --migrations-dir modules/notifications/migrations
```

**2. Bind your recipients.** This is the one thing the module cannot ship. It
creates `notification.recipient` as a view with the right *shape* and no rows;
you replace the body with a view over whatever table already holds your users:

```sql
create or replace view notification.recipient as
select u.id::text, u.locale, u.timezone from public.app_user u;

create or replace view notification.recipient_address as
select u.id::text as recipient_id, 'email'::text as channel,
       u.email as address, u.email_verified as verified
from public.app_user u where u.email is not null;
```

Two views, and the split is what lets a channel be added later. A person's own
facts are the same whatever the channel; an address is per channel, and a shape
with `email` in it has nowhere to put a chat id — `create or replace view`
refuses a replacement whose columns differ, so one view would have frozen the
set of channels into the module forever. Adding Telegram is a `union all` branch
in the second view, a row in `notification.channel`, and a send state; none of
it changes this module.

This is the module's one unavoidable piece of deployment DDL, and it is worth
knowing why. The platform *does* ship per-user identity — `idp_users` carries an
address and a verified flag — but it is published as GraphQL **actions**, and
neither a command step nor a Process state can call one: a command reads
relations, a Process request names a connector. So the only address a
declaration can reach is one in a relation, and the engine keeps no user rows of
its own on purpose. `plans/008-*` is the way out; until it lands, this view is
how a module learns who its recipients are.

`create or replace view` is what makes this a contract rather than a convention:
Postgres refuses a replacement whose column names or types differ, so a binding
that does not fit is a failed migration rather than a notification that goes
nowhere. **Until it is replaced there are no recipients**, and every `notify`
refuses with "recipient not found" — loudly, at the first attempt.

**3. Include the metadata.** Add the module's files to your own aggregators.
`!include` resolves relative to the file doing the including, so the module can
stay where it is — write the path from *your* metadata directory to this one:

```yaml
# databases/databases.yaml → tables:
- "!include <path-to>/modules/notifications/metadata/databases/default/tables/notification_inbox.yaml"
# …and the other four table files, plus commands.yaml, flows.yaml, rules.yaml,
# connectors.yaml, query_collections.yaml and rest_endpoints.yaml.
```

**4. Inherit the roles.** Do not rename them inside the module — a renamed
module is a forked module. Point your own roles at them instead:

```yaml
inherited_roles:
  - role_name: customer
    role_set: [customer, notification_user]
```

**Inheritance works for `notification_user` and for nothing else here, and the
reason is worth knowing before you copy the pattern.** `inherited_roles` carries
table permissions and *not* command permissions — verified, and written up in
`plans/009-*`. `notification_user` is permissions on two tables, so a shopper
inherits it and reads their feed. `notification_sender` and
`notification_scheduler` own *commands*, so inheriting them grants nothing you
can call: a token that must trigger a notification holds `notification_sender`
itself, and a scheduler holds `notification_scheduler` itself. Both are machine
roles issued to a credential, not hats a person wears, so that is the shape you
want anyway.

`notification_worker` appears nowhere on purpose: it is a `run_as` and never a
grant.

**5. Configure the relay.** `NOTIFICATION_MAIL_BASE_URL` and
`NOTIFICATION_MAIL_TOKEN`. See *Sending* below.

**6. Validate.** This catches a deployment that skipped step 1 — and, being
honest about it, *not* step 2: the shipped stub view satisfies the tracked-table
check by construction, which is the whole reason it is shipped. An unreplaced
binding surfaces at the first `notify`, as "recipient not found", and not
before:

```
donat validate --metadata-dir <your metadata dir>
# inconsistency: tracked table "notification.inbox" does not exist in the database
```

The module's own directory loads and validates on its own too, which is what
keeps it honest:

```
DONAT_GRAPHQL_DATABASE_URL=… donat validate --metadata-dir modules/notifications/metadata
```

## Using it

```graphql
mutation Trigger {
  notify(
    workflow: "order_shipped"
    recipient_id: "…"
    title: "Your order shipped"
    body: "It is on its way."
    request_id: "…"
  ) { dispatch_id }
}

query Bell {
  notification_inbox_aggregate(where: { read_at: { _is_null: true } }) {
    aggregate { count }
  }
}

query Feed {
  notification_inbox(order_by: { created_at: desc }, limit: 20) {
    id title body url created_at read_at
  }
}

mutation MarkRead($id: uuid!, $now: timestamptz!) {
  update_notification_inbox(where: { id: { _eq: $id } }, _set: { read_at: $now }) {
    affected_rows
  }
}

# Opting out is an upsert, not an insert: `(recipient_id, workflow, channel)` is
# the key, so a recipient who opts out, back in, and out again would otherwise
# hit it the second time.
mutation OptOut {
  insert_notification_preference(
    objects: [{ workflow: "order_shipped", channel: "email", enabled: false }]
    on_conflict: {
      constraint: preference_pkey
      update_columns: [enabled]
    }
  ) { affected_rows }
}
```

`recipient_id` on a preference is a preset, not an argument — it is not in the
role's insert column list at all, so a request that names someone else's id is
refused by the schema rather than corrected quietly.

### Deduplication

`request_id` is the dedupe key. The same id for the same recipient and workflow
within the retention (7 days) returns the first result and writes nothing
further. A caller that wants "at most one `order_shipped` per order" derives the
id from the order.

The window is per *command*, not per workflow, because a command's
`idempotency.retention` is one declaration. A deployment that needs two windows
ships a second command with the other retention.

### The bell: poll the count, do not subscribe by default

`notification_inbox_aggregate` is a query, and polling it every few seconds is
the intended shape. A GraphQL **subscription** on the feed works, but the engine
serves subscriptions by re-executing them once per second, and because session
variables are substituted when the query is planned, two recipients' inboxes are
different SQL and cannot share a poll. One subscribed bell per signed-in user is
one query per second per user, against a process-wide ceiling
(`DONAT_GRAPHQL_MAX_ACTIVE_SUBSCRIPTIONS`, default 1000). Subscribe on a small
stand; poll on a large one.

### Sending

The module ships an outbound contract, not a provider. `notification_mail` is a
plain JSON POST carrying an `Idempotency-Key`:

```
POST {NOTIFICATION_MAIL_BASE_URL}/v1/email/messages
{ "message_key": …, "recipient": …, "subject": …, "body": … }
→ { "message_id": …, "status": … }

POST {NOTIFICATION_MAIL_BASE_URL}/v1/email/digests
{ "message_key": …, "recipient": …, "workflow": …, "pending": 3 }
```

The key is what makes the send *provider-idempotent*: a retry after a timeout
reaches the same key, the relay absorbs it, and the recipient gets one message.
Point it at a small relay of your own, or at any provider that accepts this
shape.

### Plugging in your own sender

The swap point is one file: `connectors/notification-mail.yaml`. Include your own
instead of the module's and nothing else changes — no flow is edited, no command
is touched, the module is not forked.

What the flow relies on, and all it relies on:

| It must be | Because |
|---|---|
| an instance named `notification_mail` | the two send states name it |
| with operations `send_email` and `send_digest` | one per state; a sender that misses the second fails `validate` |
| whose inputs are `message_key`, `recipient`, `subject`, `body`, `workflow`, `locale`, `payload` (and `pending` for the digest) | these are the names the states bind |
| whose `body` **consumes every one of them** | an input the body never mentions is "an undeclared value" and the send is refused before it leaves |

Everything else is yours: the URL, the method, the path, the auth headers, the
timeouts, the retry set, the error map, the bounds — and the field names on the
wire. Map ours to whatever your provider calls them:

```yaml
- name: notification_mail
  module: http
  config:
    base_url: { value_from_env: NOTIFICATION_MAIL_BASE_URL }
    headers:
      - { name: Authorization, value_from_env: NOTIFICATION_MAIL_TOKEN }
  operations:
    - name: send_email
      method: POST
      path: /api/v3/messages/send        # yours
      input_contract:
        message_key: string!
        recipient: string!
        subject: string!
        body: string!
        workflow: string!
        locale: string
        payload: NotificationPayload
      body:                              # your provider's spelling
        To:             { input: recipient }
        Subject:        { input: subject }
        TextBody:       { input: body }
        Tag:            { input: workflow }
        Language:       { input: locale }
        Data:           { input: payload }
        IdempotencyKey: { input: message_key }
      response:                          # and its answer
        provider_message_id: { json_pointer: /Id, type: string!, max_bytes: 256 }
        normalized_status:   { json_pointer: /Result, type: string!, max_bytes: 64 }
      # …plus the effect, bounds, error_map, retry and redaction the shipped
      # file shows; copy them and adjust.
```

`examples/deployment` is exactly this, as a stand you can run: a different path,
different field names and a different response shape, asserting the notification
still arrives and the provider's own id is what gets recorded. Its
`connectors/own-mail.yaml` is derived from the shipped file the way yours will
be — copy it rather than this excerpt.

**Sending through SendGrid, SES or Twilio instead** means using the compiled
connector for that provider — and those publish no idempotency key, so the send
becomes `at_most_once` and the state changes shape:

```yaml
  - id: send_email
    request:
      connector: sendgrid            # a compiled module, not `module: http`
      operation: mail.send
      at_most_once: true             # required: the class demands the opt-in
      on_ambiguous: record_email_unknown   # and a route for an unknown outcome
      retry:
        retry_on: []                 # must be empty
        max_attempts: 1              # must be exactly one
      # and no `idempotency_key`: its safety is the send that is never repeated
```

That is a different bargain and it should look like one: a worker lost
mid-send leaves an outcome nobody knows, which is why `on_ambiguous` is
mandatory (`knowledgebase/declarative-saas/decisions/063-*`).

### Templating

The relay renders; the module carries. Every single send posts:

```json
{
  "message_key": "…", "recipient": "…",
  "subject": "Your order shipped", "body": "It is on its way.",
  "workflow": "order_shipped",
  "locale": "en",
  "payload": { "order": "A-1", "tracking": "TRK-9" }
}
```

`subject` and `body` are the caller's own words and are always there, so a relay
that templates nothing forwards them and is done. The other three are what make
a template possible: `workflow` chooses one, `locale` chooses its language — read
from your own recipient binding — and `payload` is the data it renders from.

`payload` is optional and **opaque to the module**: it is declared as
`NotificationPayload`, a bounded JSON document (4 KiB, depth 8, 128 nodes),
because it crosses a Process journal and a provider request and both deserve a
ceiling that was declared rather than discovered. Nothing here reads inside it.

```graphql
mutation Notify($p: NotificationPayload) {
  notify(workflow: "order_shipped", recipient_id: "…",
         title: "Your order shipped", body: "It is on its way.",
         payload: $p, request_id: "…") { dispatch_id }
}
```

The digest send carries `workflow`, `locale` and `pending` for the same reason.

When `plans/004` lands, a render state goes in front of the send, the template
becomes deployment metadata pinned into the Process revision, and the contract
gains `html` and `text` — the relay then delivers what the engine composed
instead of composing it itself.

### The digest

`notify_digested` rings the bell immediately and records the email as
`deferred`. A **sweep** then collapses one recipient's backlog for one workflow
into one message, moving its rows `deferred` → `sending` → `sent`. The claim is
that first transition, and it is what makes several notifications one message:
everything after it names the rows by the `claim_id` the claim stamped on them,
so a notification deferred while the relay was being called is not recorded as
sent by a message it was never in.

A sweep handles **one group**, and the scheduler enumerates the groups. That is
deliberate: a Process that discovered its own work would fan out over an
unbounded read, and a fan-out that overflows fails the whole instance rather
than trimming. Enumeration happens where a bound is a permission instead:

```graphql
query Pending {                       # as notification_scheduler
  notification_pending_digest { recipient_id workflow pending }
}
```

```
POST /api/rest/notifications/digests/flush     # once per row above
{ "request_id": "<a fresh uuid>", "recipient_id": "…", "workflow": "…" }
```

Call both as `notification_scheduler`. Run them on whatever schedule you already
have. **The caller must supply a fresh id per tick** — the sweep is idempotent
on `(request_id, recipient_id, workflow)`, so a scheduler sending a constant id
would sweep a group once and then do nothing for a day. That is why the module
ships no `cron_triggers.yaml`: a donat cron trigger's payload is static, so it
cannot supply one. A CronJob running `uuidgen` can, and so can any webhook
receiver.

Sweeping a group with nothing pending is harmless — the instance finds no
deferred rows and ends without sending. A group whose recipient has lost their
address is recorded `failed` with `mail_no_address` rather than posted with a
null recipient.

How long an email waits behind the bell is a decision table,
`notification_email_delay`, shipped as "not at all". A deployment that wants
Novu's escalation — mail only if they did not look within five minutes —
changes one row, and the table's own `test_cases` are what stop that edit from
being silent.

## What it does not do

- **It does not render.** There is no template in this module and no templating
  language. The engine has an MJML renderer built for exactly this — a declared
  template, typed inputs, escaping decided by the declaration rather than by the
  value — and it is unreachable from a Process
  (`plans/004-a-local-capability-no-process-can-name.md`). Until that is closed,
  rendering belongs to whatever you point `NOTIFICATION_MAIL_BASE_URL` at, and
  the module's job is to hand it everything a template needs: see *Templating*
  below.
- **No SMS, push or chat.** The engine has connectors for all of them; adding
  one here is a channel and a send state, and nobody has asked yet.
- **A digest is per workflow, not per person.** Someone owed notifications from
  three workflows gets three messages, because the group the claim collapses is
  `(recipient, workflow)`. Merging them would mean one message whose contents
  cross workflows, which is a product decision the module does not make for you.
- **The scheduler enumerates.** The module publishes the pending list and the
  per-group sweep; running them in a loop is the deployment's, and so is
  deciding how many groups one tick may take.
- **No localisation** beyond a `locale` column the binding exposes and nothing
  yet reads.
- **No bell in the panel's chrome.** `apps/ui` gets an Inbox screen and a
  Preferences screen; the unread badge in the header is not built.
- **Nothing decides what a notification *says*.** Titles and bodies come from
  the caller; the module carries them, it does not author them.

## Screens

`apps/ui` renders both recipient-facing tables for a stand that says it has
them:

```json
{ "role": "customer", "users": { "table": "customer" }, "notifications": true }
```

The declaration duplicates the module's permissions deliberately — introspecting
them would rebuild the admin API this engine deleted
(`knowledgebase/platform/decisions/001-*`).

## Tests

The module's tests are declarations beside the thing they test — the same rule
an application follows, because that is what this is. `donat.test.yaml` stands
the module up on its own: its migrations, a recipient binding from
`testdata/migrations` (the one thing a module cannot ship), its metadata, and a
stub playing the mail relay. Then every `*_test.yaml` under `metadata/` runs
against a fresh database and engine of its own.

```
make app-test APP_DIR=modules/notifications
```

`examples/deployment` is the second stand: a deployment that adopts this module
by `!include`, brings its own sender and turns the email escalation on. Those
are the two seams the module promises to whoever adopts it, so they are tested
rather than described.

```
make app-test APP_DIR=modules/notifications/examples/deployment
```

Both run in CI as part of the conformance crate
(`crates/conformance/tests/notifications.rs` is their cargo entry and asserts
that no test file goes unrun), so a permission that stops naming the session
variable, or a flow that stops compiling, fails here rather than in whichever
project shipped it.

```
cargo test -p donat-conformance --test notifications
```

What is *not* tested here is what an adopting deployment owes: its binding, its
roles, its schedule. `examples/petshop` is that side of it — including the one
thing the module cannot tell you itself, that `inherited_roles` carries its
table permissions and not its command ones
(`examples/petshop/metadata/inherited_roles_test.yaml`).
