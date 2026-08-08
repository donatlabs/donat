---
name: donat-validators
description: Use when a value rule in donat must bind one role rather than every writer, when different conditions need different error messages, or when a validator will not compile at deploy time.
---

# Per-role value validators

A permission answers two different questions, written separately.

- `check` and `filter` decide **who may write which row**.
- A `validate` list decides **what the value may be**, over the row as written.

`validate` runs after presets and after column defaults, because the gate reads
the rows the statement returns rather than the object that was submitted. A
preset-filled column is therefore validated too.

## When to reach for it

The test is whether the rule binds **every** writer or **one role**.

`cart_line.quantity > 0` is true of a shopper, a wholesale command and a data
fix, so it is a database `CHECK` and stays in the migration. `quantity <= 20`
is a shopper's basket ceiling — a wholesale command writing the same table is
not a shopper putting 500 bags of kibble in a basket and must not inherit the
limit. That is what a validator is for, and it is the rule a `CHECK` cannot
express.

```yaml
# metadata/databases/default/tables/public_cart_line.yaml
insert_permissions:
  - role: customer
    permission:
      check: { cart: { customer_id: { _eq: X-Donat-User-Id } } }
      columns: [cart_id, variant_id, quantity]
      validate:
        - expression: 'quantity <= 20'
          message: a cart line is limited to 20 units
```

A shopper asking for 21 gets that sentence back with code `validation-failed`,
and nothing is written.

## A list, with one message per condition

Entries run in **document order**, and the first violated entry is what the
caller reads. This is how one condition produces one error and another
condition a different error:

```yaml
validate:
  - not_null: quality_grade
    message: quality_grade cannot be null
  - expression: 'quality_grade > 3'
    message: quality_grade must be greater than 3
```

Order the list so the most specific diagnosis comes first. A caller who sent a
null should be told about the null, not about a comparison that could never
have been true.

## Nulls are declared, never inferred

Expressions compile against the rule profile from `rules.yaml`, which **refuses
to read a nullable value**. So `quality_grade > 3` on a nullable column does
not compile — and writing `is_null(quality_grade) || quality_grade > 3` does
not rescue it either, because the second arm still reads a nullable value.
There is no flow-sensitive refinement; the profile will not find your guard for
you. Say which one you mean:

```yaml
# a null is refused, and named as a null
validate:
  - not_null: quality_grade
    message: quality_grade cannot be null
  - expression: 'quality_grade > 3'
    message: quality_grade must be greater than 3

# a null is fine; a value that is there must be usable
validate:
  - expression: 'size(description) >= 20'
    when_present: description
    message: description must be at least 20 characters when present
```

- `not_null:` is an entry of its own. It fails with its own message, **and** it
  makes every comparison below it typeable.
- `when_present:` refines its column **inside its own entry only**. It does not
  carry to the next entry.

Forgetting either is a **deployment** error naming the table, role and entry.
`donat validate` reports it and the engine refuses to serve, rather than
failing a request later. If a validator will not compile, the fix is to declare
the presence you meant — never to loosen the column to make the expression pass.

If the column is `NOT NULL` in the schema, none of this applies: the expression
is total by construction and needs no ceremony.

## Four properties that always hold

1. **A validator passes only on TRUE.** An unknown value never satisfies one,
   so a null is refused even where nothing says so. `not_null` adds that the
   metadata *says* so — it names the real cause in the message.
2. **A permission failure is reported before any validator.** A caller who may
   not write the row never learns which value would have been rejected.
3. **An upsert is held to both lists** — the insert list and the update list.
4. **A role that inherits a permission inherits its validators with it.**

## Where the error surfaces

The contract is fixed and is part of conformance. For an insert the error
carries code `validation-failed` and a path pointing at the arguments of the
operation, e.g.
`$.selectionSet.insert_product_variant.args.objects`, with the entry's
`message` verbatim. Do not invent a different shape; write the message you want
the caller to read.

## What is deliberately not offered

Cross-row conditions, cross-table lookups and "validate against a subquery" are
not part of a `validate` entry — the type environment is the table's own
columns and nothing else. Those rules belong in `check`, where a predicate may
traverse relationships and use `_exists` with a correlated `_ceq` to reach any
table in the source. See `donat-tables-and-permissions`.

`delete_permissions` has a `filter` and no `validate`, which follows: a delete
writes no value.

## Files to read

- [`examples/petshop/metadata/databases/default/tables/public_cart_line.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/databases/default/tables/public_cart_line.yaml)
- [`examples/petshop/metadata/databases/default/tables/public_product_variant.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/databases/default/tables/public_product_variant.yaml)
  — the `not_null` + comparison pair, with the reasoning in comments
- [`crates/conformance/fixtures/petshop/validation.yaml`](https://github.com/donatlabs/donat/blob/main/crates/conformance/fixtures/petshop/validation.yaml) — the exact error bodies
- [`knowledgebase/declarative-saas/decisions/032-permission-validators-declare-presence.md`](https://github.com/donatlabs/donat/blob/main/knowledgebase/declarative-saas/decisions/032-permission-validators-declare-presence.md)
  — why refinement is refused rather than inferred
