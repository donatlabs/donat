# A fan-out item cannot assert what it is

**Effort: S. Status: TODO — found while building `modules/notifications`.**

## What is wrong

`ProcessValue` gives two of its variants a way to say "this is not null":

```rust
Input  { input: String, as_: Option<String>, require_non_null: bool },
State  { state: String, field: String, project: …, as_: …, require_non_null: bool },
Item   { item: String,  as_: Option<String> },          // <- no such field
```

(`crates/metadata/src/types.rs:1762-1785`.)

So a value a Process reads from its own input or from an earlier state can be
narrowed to non-null and bound to a `string!` contract field; the same value
read from a `for_each` item cannot. A fan-out can only ever pass a nullable
column to a nullable contract field.

## Why it bites

Every column of a Postgres **view** is nullable — the catalog reports
`is_nullable = YES` for all of them, because a view carries no `not null`. Views
are how an application binds its own tables to a module's contract
(`modules/notifications` binds `notification.recipient` that way, and
`plans/003` proposes the same shape for the Process journal). Put those two
together and the rule is:

> A `for_each` can never consume a value that came from a view.

That is a large statement for a missing struct field. It forced two things in
the notification module:

- The digest sweep fans out over the delivery log's own rows, whose columns are
  declared `not null`, rather than over the grouped view that would have been
  the natural read — so grouping happens by claiming rather than by reading.
- The digest send's `recipient` is declared nullable in the connector contract,
  with a `route_address` state upstream guaranteeing it is present. The
  guarantee is real and it is enforced in the flow, but it is enforced somewhere
  a reader of the contract cannot see.

## What closing it needs

Add `require_non_null` to `ProcessValue::Item`, and honour it where the other
two are honoured. The compiler already narrows a nullable to its inner type for
`Input` and `State`; the item path resolves against the fan-out's item contract
(`compile_for_each_state`, `crates/processes/src/lib.rs:3181-3272`) and needs the
same narrowing plus the same runtime refusal when the value is actually null.

The runtime half is what makes it more than a type-system edit: a null arriving
at a `require_non_null` item has to fail *that item*, so it lands in
`failed_items` and the rest of the fan-out continues — which is the behaviour a
fan-out already has for a failing activity, so there is a shape to copy.

## What it unblocks

The notification module's digest sweep reads its groups from a view and passes a
non-null address to a `string!` contract, and the two workarounds above go away.
More generally it removes an unstated rule that a deployment discovers only by
hitting it: that a view and a fan-out cannot be used together.
