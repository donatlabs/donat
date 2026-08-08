---
name: donat-schema-and-migrations
description: Use when adding or changing tables, columns, constraints or indexes in a donat application, or when a command needs a database constraint to stay correct under concurrency.
---

# Schema migrations

Versioned SQL applied by `donat migrate` (refinery). This is the **only** thing
that changes the database schema. The serving binary never runs DDL and has no
`run_sql` endpoint, so a schema change that is not a migration cannot happen.

## Naming

```
migrations/V{YYYYMMDDHHMMSS}__{description}.sql
```

For example `V20260613222215__create_widget.sql`. Applied in version order,
tracked in `refinery_schema_history`; re-running is idempotent.

**Use a timestamp, not a counter.** Two branches that each add "the next"
migration both pick the same counter and collide on merge; two timestamps never
do. And two independently versioned sets — the engine's own schema and the
application's — can share one history table, which a counter makes impossible
because both would start at `V1`.

Any integer that fits in `BIGINT` is accepted, so an existing sequential set
keeps working, and moving it onto timestamps is safe: `migrate` carries the
applied history onto the new versions by joining on the migration name. It
refuses to guess when a name does not identify exactly one migration on both
sides.

## Two sets of migrations, one history table

**The image ships the binary and nothing else.** The engine's own `donat.*`
schema — cron state, the event log, command claims, the durable Process
journals and their bounded fan-out — lives in the donat repository's top-level
`migrations/` directory, twelve files, **not inside the container**. Somebody
has to apply it, and that somebody is a `donat migrate` run pointed at those
files.

So a deployment runs `migrate` twice, engine first:

```yaml
migrate:
  image: ghcr.io/donatlabs/donat:${ENGINE_TAG}
  entrypoint: ["/bin/sh", "-c"]
  command:
    - >
      donat migrate --migrations-dir /engine-migrations &&
      donat migrate --migrations-dir /app-migrations
  volumes:
    - ./engine-migrations:/engine-migrations:ro
    - ./migrations:/app-migrations:ro
```

Both sets share one `refinery_schema_history`, which is only possible because
both are versioned by timestamp — two sets of counters would each start at `V1`
and collide. That is the concrete reason for the naming rule above.

### Getting the engine's migrations into a standalone application

`examples/petshop` mounts `../../migrations` because it lives inside the donat
repository. Your application does not, so the files have to arrive some other
way — and this is where the arrangement goes wrong quietly.

**A vendored copy drifts.** Nothing checks that `./engine-migrations` matches
the engine you are running. Pair it with `ENGINE_TAG: ${ENGINE_TAG:-latest}`
and drift is not a risk but a schedule: the image moves on the next pull, the
copy does not, and the failure surfaces as a runtime error in a `donat.*` table
nobody on the team has heard of.

Either:

- **pin the tag to an exact version** — never `latest` — and record in the
  directory's README which version the copy came from, so the pair is reviewed
  together; or
- **fetch the migrations at that same pinned tag** as a build or deploy step,
  so the copy cannot disagree with the binary.

Pinning is the minimum. A floating tag beside a vendored schema is the one
combination to refuse outright.

### If the application has Processes

A third step, after both migrations: a `migrate` that also reads the metadata
and deploys the Process revisions for each source.

```yaml
deploy:
  image: ghcr.io/donatlabs/donat:${ENGINE_TAG}
  command: ["migrate", "--migrations-dir", "/engine-migrations",
            "--metadata-dir", "/metadata", "--source", "default"]
```

Skip it and the engine boots into `revision ... is not deployed as active` and
retries forever. This step reads the metadata, so every connector's
configuration has to resolve here too — an unset credential fails the deploy
rather than the first request that needs it.

## What belongs in a migration

Everything that binds **every** writer:

- tables, columns, types, defaults
- primary and foreign keys
- unique constraints — including the ones a command relies on for correctness
- `CHECK` constraints that are true of the domain, not of one role
- indexes, including the ones the row filters will need

What does **not** belong: anything role-shaped. Table visibility, per-role row
filters, column sets, presets and value validators are metadata. See
`donat-tables-and-permissions`.

## Constraints a command depends on

A command's guard runs inside its transaction, but a guard is not a lock. When
two concurrent callers may both pass the same guard, the database has to be the
one that refuses.

The grooming booking is the worked example: `reserve_grooming_slot` asserts the
hold deadline is valid, then inserts a row carrying `slot_key`. Two concurrent
holds on the same slot both pass the assertion — what stops the second is a
unique constraint on `slot_key`:

```sql
-- migrations/V…__create_grooming_booking.sql
CREATE TABLE public.grooming_booking (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id         text        NOT NULL,
    service_resource_id uuid        NOT NULL,
    slot_key            text        NOT NULL,
    starts_at           timestamptz NOT NULL,
    hold_expires_at     timestamptz NOT NULL,
    status              text        NOT NULL,
    CONSTRAINT grooming_booking_slot_key_key UNIQUE (slot_key)
);
```

Write the constraint the command needs **in the same change** as the command.
A command whose correctness rests on a constraint that is not there is a race
waiting for load.

## Nullability is a decision, not a default

Whether a column is `NOT NULL` is felt far beyond the database: the rule
profile that validators and command guards compile against refuses to read a
nullable value. A nullable column forces every expression touching it to say
what it means about the null — `not_null:` or `when_present:` — and forgetting
is a deploy error.

So `NOT NULL` where the domain says the value is always there. Nullable only
where absence is genuinely meaningful, and then expect the extra line in the
validator. See `donat-validators`.

## `on_conflict` needs a named constraint

GraphQL upserts name the constraint by its database identity:

```graphql
insert_cart_line(
  objects: [{cart_id: 1, variant_id: 1, quantity: 1}],
  on_conflict: {constraint: cart_line_cart_id_variant_id_key, update_columns: [quantity]}
) { returning { cart_id variant_id quantity } }
```

`cart_line_cart_id_variant_id_key` is a real constraint name from the
migration. Name unique constraints deliberately — they become part of the API
the moment a client upserts through them, and renaming one is a breaking change.

## Deploy order

```sh
donat migrate  --migrations-dir migrations    # apply pending DDL
donat validate --metadata-dir metadata        # check metadata against the schema
donat serve
```

When the application has Process metadata, deploy each selected Postgres source
explicitly — a second `migrate` step deploys the Process revisions. A running
instance keeps the revision it started under, so deploying a new one does not
rewrite history mid-flight.

Never reverse `migrate` and `validate`. `validate` checks metadata against the
schema as it actually is; running it first checks against the old one and
passes for the wrong reason.

## Files to read

- [`examples/petshop-rest/migrations/`](https://github.com/donatlabs/donat/tree/main/examples/petshop-rest/migrations)
  — five small, readable files covering category, pet, customer, orders and
  order_item. Start here.
- [`examples/petshop/migrations/`](https://github.com/donatlabs/donat/tree/main/examples/petshop/migrations)
  — the full store, including the constraints its commands rely on.
- [`migrations/README.md`](https://github.com/donatlabs/donat/blob/main/migrations/README.md)
  — the naming convention and the timestamp-versus-counter reasoning.
