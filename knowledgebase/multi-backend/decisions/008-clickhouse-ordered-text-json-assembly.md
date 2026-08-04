---
type: decision
status: accepted
date: 2026-07-12
features:
  - "[[multi-backend]]"
---

# ClickHouse assembles ordered JSON text in the database

## Context

Donat preserves GraphQL selection order in response objects. ClickHouse 25.8's
experimental `JSON` type canonicalizes object keys when text is cast to JSON.
For example, casting `{"id":4,"bid_price":260}` produces
`{"bid_price":260,"id":4}`. The values remain semantically equivalent JSON,
but the result violates the exact GraphQL response contract.

The ClickHouse dialect previously concatenated fields in selection order and
then cast every row to `JSON`; `groupArray` also cast each row to `JSON`. Those
casts discarded the order before the HTTP executor received the result.

## Decision

ClickHouse query SQL keeps assembled objects and arrays as ordered JSON text.
SQLgen serializes scalar leaves with `toJSONString`, `json_object` concatenates
serialized keys and values, and `json_array_agg` uses `groupArray` plus
`arrayStringConcat`. Ordered arrays continue to sort `(ordinal, row_text)`
tuples before concatenation. Serialized nullable values remain
`Nullable(String)` until `coalesce(..., 'null')`, and column aggregates use
ClickHouse's `OrNull` combinator so empty sets match SQL/GraphQL null semantics.

The runtime still executes one ClickHouse statement with `FORMAT
TabSeparatedRaw` and parses its single JSON-text result. No response fields or
rows are reordered in Rust.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep `CAST(... AS JSON)` | ClickHouse canonicalizes keys and loses GraphQL selection order. |
| Reorder decoded objects in Rust | It moves response assembly out of the database and requires backend-specific post-processing. |
| Accept a ClickHouse known difference | Selection order is an exact shared API contract, not an optional capability. |
| Return arrays of native `JSON` rows | Each row has already lost field order before array aggregation. |

## Consequences

ClickHouse preserves exact field and row order while retaining database-side
assembly and the one-statement invariant. Scalar and aggregate expressions
must be serialized before entering object assembly; nested object and array
expressions are already JSON text and are embedded without double encoding.
New ClickHouse scalar families therefore need an explicit `toJSONString`
serialization decision.
