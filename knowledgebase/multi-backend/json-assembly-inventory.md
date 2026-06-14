---
type: design
status: draft
date: 2026-06-13
features:
  - "[[multi-backend]]"
---

# JSON-assembly inventory + Dialect interface sketch (the Phase-2 crux)

The one-statement-in-DB invariant assembles the whole response as JSON inside
the database. In Postgres `sqlgen` this is ~8 woven shapes built from
`json_build_object` / `json_agg`. They are NOT isolated helpers — they are
context-specific subquery fragments. This note inventories them (grounded in
the current code) so the `Dialect` JSON-assembly interface can be designed
against reality, not guessed.

## Inventory (Postgres, `crates/sqlgen/src/lib.rs`)

| # | Shape | Current Postgres rendering | Abstract op |
|---|---|---|---|
| 1 | Row → object | `json_build_object('k', v, …)` (`row_json`, lib.rs:333/554) | `object(pairs)` |
| 2 | Rows → array | `(SELECT coalesce(json_agg(e.j), '[]'::json) FROM (SELECT <row> AS j <tail>) AS e)` (lib.rs:232) | `array_subquery(row, tail)` |
| 3 | Single row | `(SELECT <row> <tail> LIMIT 1)` (lib.rs:228) | `single_subquery(row, tail)` |
| 4 | Relay connection | object wrapping `coalesce(json_agg(json_build_object('cursor',c,'node',n)), '[]'::json)` (lib.rs:206) | compose 1+2 |
| 5 | Aggregate root | `(SELECT json_build_object(<pairs>) FROM (SELECT * <tail>) AS oa)` (lib.rs:283) | `object` over a materialized row set |
| 6 | Aggregate fields | nested `json_build_object('alias', COUNT(..)/op(expr), …)` (lib.rs:327/333) | `object` of agg exprs |
| 7 | `__typename` | `to_json(<lit>::text)` (lib.rs:275) | `to_json_text(expr)` |
| 8 | Nodes array | `coalesce(json_agg(<row>), '[]'::json)` (lib.rs:272) | `array_agg(row)` |

## Why this is the multi-backend crux

The leaf ops (`object`, `array_agg`, `to_json_text`) map cleanly across most
SQL backends **except SQL Server**:

| op | Postgres | SQLite (json1) | MySQL 8 | SQL Server |
|---|---|---|---|---|
| `object(pairs)` | `json_build_object(…)` | `json_object(…)` | `JSON_OBJECT(…)` | **no row-object function** |
| `array_agg(row)` | `coalesce(json_agg(x),'[]'::json)` | `coalesce(json_group_array(x),'[]')` | `COALESCE(JSON_ARRAYAGG(x), JSON_ARRAY())` | `… FOR JSON PATH` |
| `to_json_text(x)` | `to_json(x::text)` | `json_quote(x)` | `JSON_QUOTE(x)` | string in `FOR JSON` |

SQL Server has **no `json_build_object` equivalent**: a row becomes JSON only
via `(SELECT … FOR JSON PATH, WITHOUT_ARRAY_WRAPPER)`, and an array via
`(SELECT … FOR JSON PATH)`. That is a *structural* difference — it restructures
shapes #1–#5, not just the leaf function name.

## Interface decision

Expose BOTH levels on the `Dialect`:

```rust
trait JsonAssembly {
    // leaf ops — most backends implement just these
    fn json_object(&self, pairs: &[(String /*key*/, String /*expr*/)]) -> String;
    fn json_array_agg(&self, row_expr: &str, order_by: Option<&str>) -> String; // empty -> []
    fn to_json_text(&self, expr: &str) -> String;

    // shape hooks — the assembler calls these for the subquery shapes #2/#3/#5;
    // PG/SQLite/MySQL get a default impl built from the leaf ops above, SQL
    // Server overrides them with FOR JSON PATH.
    fn array_subquery(&self, row_expr: &str, tail: &str) -> String { /* default: leaf */ }
    fn single_subquery(&self, row_expr: &str, tail: &str) -> String { /* default */ }
}
```

The `sqlgen` assembler is rewritten to call these instead of inlining
`json_build_object`/`json_agg`. Postgres' impl reproduces today's output
byte-for-byte (snapshot-gated, like Phases 1a/1b).

## Sequencing (updates the implementation plan)

1. **Design the interface against PG + SQLite first** (both fit the leaf model).
   SQLite validates that the leaf ops + default shape hooks are sufficient.
2. **SQL Server (Phase 4) drives the shape-hook overrides** — do not finalize
   the shape-hook signatures until building the MSSQL backend, because its
   `FOR JSON` restructuring is what they exist for. Until then the shape hooks
   have only the default (leaf-based) impl.
3. This is why the JSON-assembly delegation was deliberately NOT done as a
   blind Phase-1b leaf refactor: its correct shape is defined by the second and
   fourth backends, not by Postgres alone.
