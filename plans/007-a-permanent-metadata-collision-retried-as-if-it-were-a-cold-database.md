# A permanent metadata collision, retried as if it were a cold database

**Effort: S. Status: TODO — found while adopting `modules/notifications` into `examples/petshop`.**

## What is wrong

Two tracked tables whose GraphQL base names collide make the engine retry
forever instead of refusing. The store tracked `public.notification_delivery`
and the module tracked `notification.delivery`; both render as
`notification_delivery`, because a non-public schema's base name is
`<schema>_<name>` (`crates/schema/src/naming.rs:19`).

What the operator sees, once a second, until they give up:

```
WARN donat: database not ready, retrying attempt=13
     error=incompatible type collision for 'notification_delivery'
```

The engine never binds a listener and never exits non-zero. In the conformance
harness this presented as a 100-second startup timeout with a single line of log
and no error — the retry warning only appears at `debug`, and the message says
the *database* is not ready when the database is perfectly healthy and the
metadata is wrong.

## Why it matters

Three separate things are wrong with that behaviour, in increasing order:

1. **The message names the wrong subject.** "Database not ready" is what a cold
   Postgres looks like. An operator reads it and checks their database.
2. **A permanent error is retried.** No number of retries resolves a name
   collision between two tracked tables. The retry loop is right for a
   connection that has not come up yet and wrong for a metadata defect, and
   `sync_sources` does not distinguish them.
3. **`donat validate` does not catch it.** The petshop metadata with both tables
   tracked passes `validate` cleanly — verified — and then the engine refuses to
   serve it. That is the gap that matters most: the deploy gate is supposed to
   be where a metadata mistake is caught, and this one gets through it and
   becomes a hang at boot.

## What closing it needs

- **Classify the failure.** `sync_sources` should separate "cannot reach the
  source yet" from "this metadata cannot be compiled against this source". The
  first retries; the second is fatal and should exit non-zero with the message
  it already has.
- **Move the check into `validate`.** Schema compilation already computes the
  base names; a duplicate across tracked tables is decidable at
  `check_consistency_inner` time (`crates/server/src/validate.rs:103-180`) with
  no database round trip beyond the introspection it already does.
- **Say what collided.** The current message names the type but not the two
  tables that produced it. `tracked tables "public.notification_delivery" and
  "notification.delivery" both generate the GraphQL type "notification_delivery"`
  is the message an operator can act on.

## Note for module authors

The rule this exposed is worth stating wherever modules are documented: a module
that tracks `<its schema>.<table>` owns the `<its_schema>_*` GraphQL namespace,
and an application with a `public.<its_schema>_something` of its own has to
rename it or leave it untracked. `examples/petshop` renamed
`public.notification_delivery` to `public.provider_notification_receipt` for
exactly this reason (`examples/petshop/migrations/V20260823090100__*`).
