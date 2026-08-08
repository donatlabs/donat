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
