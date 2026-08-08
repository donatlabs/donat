# wasm-core Phase 1 — Catalog Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the pure catalog types compile to `wasm32` by extracting them into a new wasm-safe `donat-catalog-types` crate, so `donat-schema` builds for `wasm32-unknown-unknown` — without changing any behavior of the standalone server.

**Architecture:** Today `donat-catalog` mixes pure serde data types (`Catalog`, `TableInfo`, `ColumnInfo`, `ForeignKey`, `FunctionInfo`, `FunctionArg`) with I/O introspection drivers (`tokio-postgres`, `rusqlite/bundled`, `mysql`) that cannot cross-compile to wasm. We use **shape A** from Spec 004: move the six types + their inherent `impl`s into a new `donat-catalog-types` crate (deps: `serde` only). `donat-catalog` keeps the drivers and re-exports the types so its own driver code and existing dependents (`donat-server`) are unchanged. `donat-schema` switches its dependency to `donat-catalog-types`, which removes the wasm-incompatible drivers from its graph.

**Tech Stack:** Rust workspace (cargo), `serde`, wasm target `wasm32-unknown-unknown`, insta snapshot tests, the native conformance harness.

**Scope:** This is Phase 1 of Spec 004 (`specs/004-wasm-core-host-split.md`) only — the low-risk, Rust-only catalog split. Phases 2–9 (wasm-core crate + PlanV1, the Go host execution layer, event hooks, per-SDK conformance) are separate plans written after Phase 1 lands green.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/catalog-types/Cargo.toml` (new) | Manifest for the pure-types crate; depends on `serde` only — wasm-clean |
| `crates/catalog-types/src/lib.rs` (new) | The six serde structs + their inherent `impl`s (`Catalog`, `FunctionInfo`, `FunctionArg`, `TableInfo`, `ColumnInfo`, `ForeignKey`), moved verbatim |
| `Cargo.toml` (workspace, modify) | Add `crates/catalog-types` to `members` |
| `crates/catalog/Cargo.toml` (modify) | Add `donat-catalog-types` path dependency |
| `crates/catalog/src/lib.rs` (modify) | Delete the moved struct/impl definitions; add `pub use donat_catalog_types::*;`; keep the SQL consts + the three `*introspect` driver fns + the `sqlite_tests` module |
| `crates/schema/Cargo.toml` (modify) | Replace `donat-catalog` dependency with `donat-catalog-types` |
| `crates/schema/src/plan.rs` (modify) | Switch 5 `donat_catalog::` references to `donat_catalog_types::` |
| `crates/schema/src/introspection.rs` (modify) | Switch 1 `donat_catalog::TableInfo` reference to `donat_catalog_types::TableInfo` |

Note: `donat-sqlgen` references `donat-catalog` only under `[dev-dependencies]` (line 15) and is unaffected — the re-export keeps it compiling. `donat-server` keeps depending on `donat-catalog` for introspection and is unchanged.

---

### Task 1: Create the `donat-catalog-types` crate

**Files:**
- Create: `crates/catalog-types/Cargo.toml`
- Create: `crates/catalog-types/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create the crate manifest**

Create `crates/catalog-types/Cargo.toml`:

```toml
[package]
name = "donat-catalog-types"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Pure serde catalog snapshot types (no I/O); wasm-safe"

[dependencies]
serde = { workspace = true }
```

- [ ] **Step 2: Create the types module (moved verbatim from `crates/catalog/src/lib.rs:8-85`)**

Create `crates/catalog-types/src/lib.rs`:

```rust
//! Pure catalog snapshot types.
//!
//! The serde data shapes produced by introspection and consumed by the
//! planner. No I/O, no drivers — this crate compiles to `wasm32` so the
//! wasm-core can deserialize a [`Catalog`] snapshot. The introspection
//! drivers live in `donat-catalog`, which re-exports these types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    /// Keyed by "schema.table".
    pub tables: BTreeMap<String, TableInfo>,
    /// Keyed by "schema.function". Overloads are not supported.
    pub functions: BTreeMap<String, FunctionInfo>,
}

impl Catalog {
    pub fn table(&self, schema: &str, name: &str) -> Option<&TableInfo> {
        self.tables.get(&format!("{schema}.{name}"))
    }

    pub fn function(&self, schema: &str, name: &str) -> Option<&FunctionInfo> {
        self.functions.get(&format!("{schema}.{name}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionInfo {
    pub schema: String,
    pub name: String,
    pub args: Vec<FunctionArg>,
    /// If the function returns (setof) a known table's row type.
    pub returns_table: Option<(String, String)>,
    pub returns_set: bool,
    /// Scalar return type name when not returning a table row.
    pub returns_scalar: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionArg {
    pub name: Option<String>,
    /// The argument has a DEFAULT and may be omitted from calls.
    #[serde(default)]
    pub has_default: bool,
    pub pg_type: String,
    /// Set when the argument type is the row type of a known table.
    pub composite_of: Option<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Vec<String>,
    pub foreign_keys: Vec<ForeignKey>,
}

impl TableInfo {
    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.name == name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    /// Postgres type name as reported by pg_catalog (e.g. `int4`, `text`).
    pub pg_type: String,
    pub nullable: bool,
    pub has_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub constraint_name: String,
    /// Local column -> referenced column.
    pub column_mapping: BTreeMap<String, String>,
    pub referenced_schema: String,
    pub referenced_table: String,
}
```

- [ ] **Step 3: Register the crate in the workspace**

Modify `Cargo.toml` (workspace root) `members` to add the new crate after `crates/catalog`:

```toml
members = [
    "crates/metadata",
    "crates/catalog",
    "crates/catalog-types",
    "crates/schema",
    "crates/ir",
    "crates/sqlgen",
    "crates/backend",
    "crates/server",
    "crates/conformance",
]
```

- [ ] **Step 4: Verify it compiles natively and to wasm**

Run: `cargo build -p donat-catalog-types && cargo build -p donat-catalog-types --target wasm32-unknown-unknown`
Expected: both `Finished` with no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/catalog-types/Cargo.toml crates/catalog-types/src/lib.rs Cargo.toml Cargo.lock
git commit -m "catalog: extract pure snapshot types into wasm-safe donat-catalog-types"
```

---

### Task 2: Re-export the types from `donat-catalog` and delete the duplicates

**Files:**
- Modify: `crates/catalog/Cargo.toml`
- Modify: `crates/catalog/src/lib.rs:8-85`

- [ ] **Step 1: Add the path dependency**

Modify `crates/catalog/Cargo.toml` `[dependencies]` to add the new crate (keep the existing driver deps):

```toml
[dependencies]
donat-catalog-types = { path = "../catalog-types" }
serde = { workspace = true }
tokio-postgres = { workspace = true }
rusqlite = { version = "0.32", features = ["bundled"] }
mysql = { version = "25", default-features = false, features = ["minimal-rust"] }
```

- [ ] **Step 2: Replace the moved definitions with a re-export**

In `crates/catalog/src/lib.rs`, delete the six struct definitions and the two `impl` blocks (the current lines 13-85: `pub struct Catalog` through the end of `pub struct ForeignKey`), and replace the `use` header (current lines 8-11) so the file begins:

```rust
//! Postgres introspection (milestone M1).
//!
//! The single place that knows how to read `pg_catalog`. Produces a
//! [`Catalog`] snapshot — tables, columns with their SQL types and
//! nullability, primary keys and foreign keys — which the planner combines
//! with metadata. Nothing downstream talks to `pg_catalog` directly.
//!
//! The snapshot types live in `donat-catalog-types` (wasm-safe) and are
//! re-exported here so existing `donat_catalog::Catalog` paths keep working.

use std::collections::BTreeMap;

use tokio_postgres::Client;

pub use donat_catalog_types::{
    Catalog, ColumnInfo, ForeignKey, FunctionArg, FunctionInfo, TableInfo,
};
```

Leave everything from `const COLUMNS_SQL` (current line 87) onward unchanged — the SQL consts, `introspect`, `sqlite_type_to_pg`, `sqlite_introspect`, `mysql_introspect`, `mysql_type_to_pg`, and the `sqlite_tests` module all keep referring to the now re-exported types. `serde` is still a dependency because the driver code and tests may use it; `BTreeMap` is still used by the driver code.

- [ ] **Step 3: Verify the host crate builds and its tests pass**

Run: `cargo build -p donat-catalog && cargo test -p donat-catalog`
Expected: `Finished`; the `sqlite_tests` module passes (the `sqlite_introspect` round-trip still compiles against the re-exported types).

- [ ] **Step 4: Commit**

```bash
git add crates/catalog/Cargo.toml crates/catalog/src/lib.rs Cargo.lock
git commit -m "catalog: re-export snapshot types from donat-catalog-types, keep drivers"
```

---

### Task 3: Point `donat-schema` at `donat-catalog-types` and build it to wasm

**Files:**
- Modify: `crates/schema/Cargo.toml:10`
- Modify: `crates/schema/src/plan.rs:5,188,607,663,874`
- Modify: `crates/schema/src/introspection.rs:557`

- [ ] **Step 1: Swap the dependency**

In `crates/schema/Cargo.toml`, replace the `donat-catalog` line with:

```toml
donat-catalog-types = { path = "../catalog-types" }
```

(Leave the other dependencies — `donat-metadata`, `donat-ir`, `graphql-parser`, `serde`, `serde_json`, `thiserror`, `base64` — unchanged.)

- [ ] **Step 2: Update the import in `plan.rs`**

In `crates/schema/src/plan.rs`, change line 5 from:

```rust
use donat_catalog::{Catalog, TableInfo};
```

to:

```rust
use donat_catalog_types::{Catalog, TableInfo};
```

- [ ] **Step 3: Update the remaining qualified paths in `plan.rs`**

Still in `crates/schema/src/plan.rs`, replace the three remaining `donat_catalog::` qualifiers with `donat_catalog_types::`:

- line 188: `Option<&'a donat_catalog::ColumnInfo>` → `Option<&'a donat_catalog_types::ColumnInfo>`
- line 607: `finfo: &donat_catalog::FunctionInfo,` → `finfo: &donat_catalog_types::FunctionInfo,`
- line 663: `) -> Option<&'a donat_catalog::FunctionInfo> {` → `) -> Option<&'a donat_catalog_types::FunctionInfo> {`
- line 874: `finfo: &donat_catalog::FunctionInfo,` → `finfo: &donat_catalog_types::FunctionInfo,`

(Quick check after editing: `grep -rn "donat_catalog::" crates/schema/src/` must return nothing.)

- [ ] **Step 4: Update the reference in `introspection.rs`**

In `crates/schema/src/introspection.rs`, change line 557 from:

```rust
    info: &donat_catalog::TableInfo,
```

to:

```rust
    info: &donat_catalog_types::TableInfo,
```

- [ ] **Step 5: Verify schema builds natively and its snapshots are unchanged**

Run: `cargo test -p donat-schema`
Expected: PASS — the existing `donat-schema` insta snapshots are unchanged, proving the split is behavior-preserving. If any snapshot diff appears, STOP and review with `cargo insta review` (an unexplained diff is a bug, per the project rule); the split must not change any snapshot.

- [ ] **Step 6: Verify schema now compiles to wasm (the Phase 1 win)**

Run: `cargo build -p donat-schema --target wasm32-unknown-unknown`
Expected: `Finished` — previously this failed on `getrandom`/`tokio`/`libsqlite3-sys`/`mysql` pulled via `donat-catalog`; with the dependency swapped those drivers are gone from schema's graph.

- [ ] **Step 7: Commit**

```bash
git add crates/schema/Cargo.toml crates/schema/src/plan.rs crates/schema/src/introspection.rs Cargo.lock
git commit -m "schema: depend on donat-catalog-types so donat-schema compiles to wasm32"
```

---

### Task 4: Full regression — server unchanged, tests + conformance green

**Files:** none (verification only).

- [ ] **Step 1: Workspace unit + snapshot tests**

Run: `make test`
Expected: all crates green, including `donat-catalog` (`sqlite_tests`), `donat-schema`, `donat-sqlgen` snapshots — no diffs.

- [ ] **Step 2: Server binary still builds (regression guard)**

Run: `cargo build -p donat-server --bin donat`
Expected: `Finished` — `donat-server` still depends on `donat-catalog` for introspection and is byte-for-byte behavior-unchanged.

- [ ] **Step 3: Conformance suite green**

Run: `make conformance`
Expected: the full suite passes (the engine is unchanged; this proves the refactor is invisible to the HTTP surface).

- [ ] **Step 4: Final wasm acceptance check**

Run: `cargo build -p donat-schema --target wasm32-unknown-unknown && cargo build -p donat-catalog-types --target wasm32-unknown-unknown`
Expected: both `Finished`. This is the Phase 1 acceptance criterion from Spec 004.

- [ ] **Step 5: Dispatch the mandatory judge review**

Per the project BLOCKING rule, after the Task 1–3 commits dispatch the judge agent (REVIEW TASK: catalog split, Spec 004 Phase 1). Continue only after ACCEPT; on REJECT, fix and re-verify. Constrain the judge to make no git state changes (no checkout/switch/stash).

---

## Self-Review

**Spec coverage (Spec 004 Phase 1 / task #1):**
- "Extract pure types (shape A or B)" → Tasks 1–3 (shape A chosen). ✓
- "`cargo build -p donat-schema --target wasm32-unknown-unknown` Finishes" → Task 3 Step 6, Task 4 Step 4. ✓
- "`make test` + `make conformance` stay green (server unchanged)" → Task 4 Steps 1–3. ✓
- "introspection stays host-side; server keeps depending on donat-catalog" → Task 2 (drivers retained), Task 4 Step 2. ✓
- Acceptance criterion "`make test && cargo build -p donat-server --bin donat && make conformance` green at the phase boundary" → Task 4. ✓

**Placeholder scan:** No TBD/TODO/"handle edge cases"; every code step shows the exact file content or the exact line edit. ✓

**Type consistency:** The six types and two `impl`s defined in Task 1 are the exact names referenced in Tasks 2–3 (`Catalog`, `ColumnInfo`, `ForeignKey`, `FunctionArg`, `FunctionInfo`, `TableInfo`). The re-export list in Task 2 matches them. The schema edits in Task 3 reference only `Catalog`, `TableInfo`, `ColumnInfo`, `FunctionInfo` — all in the moved set. ✓

**Out of scope (deferred to later plans):** the `donat-wasm-core` crate, the PlanV1 contract, the Go host execution layer, event hooks, and per-SDK conformance (Spec 004 Phases 2–9).
