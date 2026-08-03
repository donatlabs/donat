---
type: decision
status: accepted
date: 2026-08-02
features:
  - "[[api-surfaces]]"
---

# A wide selection is an ordinary query

## Context

Every JSON object in a response was assembled with one `json_build_object`
call. Postgres allows a function 100 arguments, and a key/value pair spends
two, so a selection set of more than fifty fields failed — the caller received
`data-exception` carrying Postgres's own words about argument counts.

Fifty fields on one object is not an unusual query. A wide table read in full,
a dashboard that asks for everything it renders, an export: all of them pass
fifty without anybody trying. The limit applied at every level, so a nested
relationship of fifty-one fields failed exactly like a root one.

## Decision

The Postgres dialect keeps `json_build_object` for objects of fifty pairs or
fewer — every existing snapshot, and the hot path, are byte-identical — and
writes anything wider out as text, in query order:

```sql
('{' || '"f0":' || coalesce(to_json(_t0.c0)::text, 'null') || ',' || … || '}')::json
```

Concatenation rather than an aggregate over a `VALUES` list. The row set was
tried first and is wrong: it is a query level of its own, and an aggregate
selection's values (`count(*)`, `sum(...)`) belong to the level above it, so
Postgres refuses them there — `aggregate functions are not allowed in VALUES`.
`||` leaves every value where it already was. It also has no argument ceiling
and preserves order exactly, which `jsonb` merging would not: `jsonb` sorts
keys by length and bytes, and a response whose field order does not follow the
query is a different answer. `to_json` keeps each value's type and nesting, and
`coalesce` keeps one NULL from swallowing the whole concatenation.

SQLite has the same ceiling for the same reason — `SQLITE_MAX_FUNCTION_ARG` is
127 by default — and no ordered object aggregate whose result would survive a
round trip through a row set with its JSON subtype intact. There the object is
built in chunks instead: one `json_object` of fifty pairs, then chained
`json_insert` calls adding fifty more each. Keys land in insertion order, and a
nested value built by a json function stays JSON rather than being quoted into
a string.

MySQL and ClickHouse need nothing: they do not call a function at all, they
concatenate the object's text. Both were measured accepting three thousand
`concat` arguments — a thousand fields — so the limit that bit Postgres and
SQLite does not exist there.

## Alternatives

| Option | Why Not |
|--------|---------|
| Chunk on Postgres too, with `jsonb_insert` | Postgres has an ordered object aggregate that keeps types; SQLite does not, which is why the two answers differ. |
| Chunk into several `json_build_object` calls and merge with `\|\|` | `\|\|` exists only for `jsonb`, which discards field order. |
| An ordered aggregate over a `VALUES` list | Opens a query level of its own, which an aggregate selection's values may not cross. |
| Build every object with `json_object_agg` | Pays a `VALUES` scan and an aggregate for every row of every query, to fix the objects that are rare. |
| Refuse wide selections with a clear error | The query is legitimate and the data is there; naming a limit is not answering. |
| `json_object(text[], text[])` | Takes and returns text, so every value would lose its type. |

## Consequences

A selection set may be as wide as the schema allows, at every level, and comes
back in query order. Wide objects cost one `VALUES` scan and an aggregate;
narrow ones cost exactly what they did. The threshold is a property of
Postgres, not of the API, so it is stated once in the dialect and nowhere else.
