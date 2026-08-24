# The identity we ship is unreachable from a declaration

**Effort: M. Status: TODO — found while adopting `modules/notifications`.**

## What is wrong

The platform ships per-user identity. `crates/server/src/idp_admin.yaml` publishes
`IdpUser` with `id`, `email`, `email_verified`, `given_name`, `language` — the
whole of what anything addressed to a person needs — and a deployment gets it by
naming one role, with no metadata of its own (`idp_admin.rs:1-24`).

Nothing declarative can read it.

- It is published as **actions** (`idp_admin.yaml:256`, `kind: synchronous`), and
  an action is a GraphQL field backed by a webhook.
- A **Process state** is one of seven kinds — command, request, when, wait,
  for_each, output, fail (`crates/metadata/src/types.rs:1432-1441`). A request
  names a *connector*. None of them names an action.
- A **command step** reads relations: `select_one` / `select_many` over a table
  or a view. There is no step that calls an action.

So the only thing a declaration can read a person's address from is a relation
in the database — and the engine deliberately keeps no user rows: there is no
`create table … user` anywhere in `migrations/`, because "who can get in" is the
provider's question and the engine stores no users.

## Why it matters

`modules/notifications` has to ask every adopting deployment to write a view:

```sql
create or replace view notification.recipient as
select c.customer_id as id, c.email, … from public.customer c;
```

`examples/petshop` can, because it happens to keep `public.customer`. A
deployment whose users live only in the identity provider — which is the shape
the platform ships and recommends — has nothing to bind to. It would have to
build a users table and keep it in sync with the provider, which is exactly the
duplication the "engine stores no users" rule exists to avoid.

That is the gap in one sentence: **we ship identity and we ship notifications,
and the second cannot see the first.**

It is the same shape as `plans/004` (a `local.*` capability no Process can name)
and `plans/006` (a bound nothing enforces): a capability that exists, and a
declaration surface that cannot reach it.

## What closing it needs

**Publish the identity provider as a connector, not only as actions.** A
compiled connector module — the way `sendgrid` and `twilio` are compiled — with
a read-only `user.get` operation keyed by the subject id, and a `user.list` if
paging is wanted. Connectors are already reachable from a Process `request`, so
this needs no grammar change and no new state kind:

```yaml
- id: resolve_recipient
  request:
    connector: identity            # configured where DONAT_OIDC already is
    operation: user.get
    input: { id: { input: recipient_id } }
    next: …
```

Its effect class is `ReadOnly`, so it is freely retryable and needs none of the
at-most-once machinery. The credential is the one `idp_admin` already resolves.

Then `modules/notifications` ships a delivery path that reads the address from
the provider, and the recipient view becomes the *override* for deployments that
keep their own users table rather than the requirement it is today. Adoption
goes from "one migration you must write" to "nothing, unless you want to".

## The alternatives, and why they are worse

| Option | Why not |
|--------|---------|
| A command step that calls an action | Actions are synchronous HTTP behind a GraphQL field. Putting one inside a command means an outbound call inside the single statement a command compiles to, which is the invariant the whole command design rests on. |
| Cache the provider's users into a `donat.*` table | The engine then stores users, which is the rule it is built around, and every deployment inherits a sync problem and a staleness window on someone's email address. |
| Let the module read the address from the caller's session claims | A notification is addressed to someone who is usually not the caller. |
| Leave it: every deployment writes the view | Workable and honest, and it is what ships today. It is also the reason a deployment cannot adopt notifications without first deciding to keep a users table — a decision the platform otherwise tells it not to make. |
