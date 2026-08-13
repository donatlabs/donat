---
name: donat-tables-and-permissions
description: Use when declaring a table to donat, granting a role access, writing a row filter or check, or debugging why rows are invisible or a write is refused.
---

# Tables and permissions

A table in the database is invisible until it is declared here. Once it is
declared with a permission, the GraphQL, REST and MCP surfaces for it exist —
you do not write resolvers, endpoints or tools.

One file per table, `databases/default/tables/public_<table>.yaml`, listed in
`tables.yaml`.

## The shape of a table file

```yaml
table:
  name: cart_line
  schema: public

object_relationships:            # many-to-one, via a local foreign key
  - name: cart
    using:
      foreign_key_constraint_on: cart_id
  - name: variant
    using:
      foreign_key_constraint_on: variant_id

array_relationships:             # one-to-many, via a foreign key on the other side
  - name: orders
    using:
      foreign_key_constraint_on:
        column: customer_id
        table:
          name: orders
          schema: public

select_permissions:   [...]
insert_permissions:   [...]
update_permissions:   [...]
delete_permissions:   [...]
```

Relationships are not decoration. They are the vocabulary permissions are
written in — a cart line is the caller's because *its cart* belongs to the
caller, and that sentence is only expressible if `cart` is a declared
relationship.

## filter versus check

This is the distinction to get right first.

| Key | Question | Applies to |
|---|---|---|
| `filter` | **Which existing rows may this role touch?** | select, update, delete |
| `check` | **Is the row it is writing allowed to exist?** | insert, update |

An update needs both, and they are usually different: `filter` says which row
may be edited, `check` says what it may be edited *into*. A cart line may be
edited while its cart is open (`filter`), and only into a variant that is
active and published (`check`).

```yaml
update_permissions:
  - role: customer
    permission:
      columns: [variant_id, quantity]
      filter:
        _and:
          - cart: { customer_id: { _eq: X-Donat-User-Id } }
          - cart: { status: { _eq: cart_open } }
      check:
        _and:
          - cart: { customer_id: { _eq: X-Donat-User-Id } }
          - cart: { status: { _eq: cart_open } }
          - variant: { active: { _eq: true } }
          - variant: { product: { status: { _eq: published } } }
```

`filter: {}` means every row. That is a real grant — write it only where you
mean it, as `public_customer.yaml` does for the `support` role.

## Session variables

A session variable is written where a literal would go, spelled as its header
name in canonical form:

```yaml
filter:
  customer_id:
    _eq: X-Donat-User-Id
```

`X-Donat-User-Id` is the caller's identity, established by the JWT or asserted
by the authentication hook. There is no other way to reach the caller from a
permission, and no way for a client to influence it.

Predicates traverse relationships to any depth: `cart: { customer_id: ... }`,
`variant: { product: { status: ... } }`. The engine folds them into the same
statement — a nested predicate is not a second query.

## Predicates over an unrelated table: `_exists`

When the rule depends on a table that has no foreign key to this one, use
`_exists` with an explicit table and a `_where`:

```yaml
check:
  _exists:
    _table: user
    _where:
      id: X-Donat-User-Id
      is_admin: true
```

Inside `_where`, `["$", "<column>"]` refers back to a column of the row being
checked, so the subquery can be correlated to the row rather than merely
existential:

```yaml
filter:
  _exists:
    _table: entitlement
    _where:
      _and:
        - customer_id: { _eq: X-Donat-User-Id }
        - product_id:  { _ceq: ["$", "product_id"] }
```

This covers the "validate against a custom select" case: the predicate reaches
any table in the source, correlated to the current row, and stays inside one
statement.

## Columns and presets

`columns` is the mask — the list of columns this role may read (select) or
write (insert/update). `columns: "*"` grants all of them and inherits new ones
automatically, which is convenient and is also how a column added tomorrow
leaks. Prefer an explicit list on anything sensitive.

`set` presets a column server-side and removes it from the caller's control:

```yaml
insert_permissions:
  - role: customer
    permission:
      check:
        _and:
          - customer_id: { _eq: X-Donat-User-Id }
          - status:      { _eq: cart_open }
      set:
        customer_id: X-Donat-User-Id
      columns: []
```

`columns: []` with a preset is the strongest shape available: the caller
supplies nothing, the engine writes the identity, and forging another
customer's cart is not expressible in the API rather than merely refused.

`allow_aggregations: true` on a select permission exposes the `_aggregate`
root. It is off by default because a count over rows you cannot read is still
an information channel.

## Value rules belong in `validate`, not here

`filter` and `check` decide *who may write which row*. What the value may
be — a ceiling, a length, a grade — is a separate list with its own per-entry
error message. See `donat-validators`.

## Roles

Roles are not declared centrally; a role exists because some permission names
it. That makes `grep -rn 'role: <name>' metadata/` the authoritative answer to
"what can this role do", and it is worth running before adding a role rather
than after.

The petshop's shape is worth copying:

| Role | Who | Can do |
|---|---|---|
| `anonymous` | unauthenticated | browse published products and active variants |
| `customer` | a logged-in shopper | own profile, addresses, cart and orders |
| `staff` | store employee | catalogue and inventory CRUD, read every order |

plus narrow worker roles — `fulfilment`, `payment_worker`, `groomer`,
`veterinary_reviewer` — that commands and processes run as. Each is an ordinary
role with its own explicit permissions. **None of them is privileged, and there
is no admin role to fall back on.**

`DONAT_GRAPHQL_UNAUTHORIZED_ROLE` names the role an unauthenticated request
runs as. If it is unset, such a request is rejected with
`x-donat-role header is required`.

## Checklist for a new table

1. Migration exists, with the constraints and indexes the filters will need.
2. Table file created and added to `tables.yaml`.
3. Relationships declared — including the ones only a permission will use.
4. For each role: `select` filter, `insert` check plus column mask, `update`
   filter *and* check, `delete` filter. Absent means denied; write only what
   the role needs.
5. Value rules as `validate` entries, each with its own message.
6. `donat validate --metadata-dir metadata` green against the migrated schema.
7. A test that the *wrong* role sees nothing — a permission is only proven by
   the request it refuses.

## Files to read

- [`examples/petshop/metadata/databases/default/tables/public_cart_line.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/databases/default/tables/public_cart_line.yaml) —
  the full four-permission shape with relationship-traversing predicates
- [`examples/petshop/metadata/databases/default/tables/public_cart.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/databases/default/tables/public_cart.yaml) —
  preset plus `columns: []`
- [`examples/petshop/metadata/databases/default/tables/public_customer.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/databases/default/tables/public_customer.yaml) —
  column masks, a `filter: {}` grant, and a file column
- [`examples/petshop-rest/metadata/databases/default/tables/`](https://github.com/donatlabs/donat/tree/main/examples/petshop-rest/metadata/databases/default/tables) — five small
  tables, the easiest starting point
