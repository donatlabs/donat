# Go event-trigger SDK: codegen + handler contract — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a Go project handle Donat event triggers via native registered functions — by generating per-table Go structs from the Postgres catalog and shipping a pure-Go SDK with an `Event[T]` envelope and a name-keyed handler registry.

**Architecture:** Two phases. **Phase A (Rust):** a new `donat codegen go` subcommand whose core is a *pure* function `generate_go(catalog, tracked, enums, package) -> String` (insta-snapshot tested), wrapped by a DB-facing runner that introspects via `donat_catalog::introspect`, runs one supplemental `pg_enum` query, filters to metadata-tracked tables, and writes one `donat_gen.go`. **Phase B (Go):** a pure-Go module `sdk/go` (no cgo) with `Event[T]` (decodes the Donat envelope), a `Registry` with `On[T]` / `Dispatch`, and a `CGO_ENABLED=0` build guardrail. Transport is out of scope; `Dispatch(name, rawEnvelope)` is the seam every future transport calls.

**Tech Stack:** Rust (clap, tokio-postgres, insta, the existing `donat_catalog` / `donat_metadata` crates), Go 1.22+ generics, `github.com/shopspring/decimal` (pure Go).

**Spec:** `specs/003-go-event-trigger-sdk-codegen.md`. **Hard constraint:** the Go package must be `go get`-able and compiled into the user's binary with no cgo (CI guardrail `CGO_ENABLED=0 go build ./...`).

---

## File Structure

Phase A — Rust (in `crates/server`):

- Create: `crates/server/src/codegen.rs` — Go codegen: pure `generate_go` + DB-facing `run_codegen` + supplemental enum query. One responsibility: turn catalog+metadata into a Go source string and write it.
- Modify: `crates/server/src/main.rs` — add the `Codegen` subcommand + dispatch.
- Snapshots: `crates/server/src/snapshots/` (insta auto-creates).

Phase B — Go (new top-level `sdk/go`):

- Create: `sdk/go/go.mod` — module `github.com/donat/donat-go`, requires `shopspring/decimal`.
- Create: `sdk/go/donat/event.go` — `Op`, `Event[T]`, `TableRef`, `TriggerRef`, `DeliveryInfo`, `ParseEvent[T]`.
- Create: `sdk/go/donat/registry.go` — `Registry`, `On[T]`, `Dispatch`, `ErrNoHandler`, `Names`.
- Create: `sdk/go/donat/event_test.go`, `sdk/go/donat/registry_test.go`.
- Create: `sdk/go/internal/golden/donat_gen.go` — a checked-in golden codegen output, built by `go build ./...` as the compile guardrail.
- Create: `sdk/go/donat/golden_test.go` — instantiates `Event[golden.TestT1]` to prove generated structs compose with the SDK.

---

## Phase A — Rust codegen

### Task 1: Pure `generate_go` — basic scalars (int4, text, bool)

**Files:**
- Create: `crates/server/src/codegen.rs`
- Modify: `crates/server/src/main.rs` (add `mod codegen;`)

- [ ] **Step 1: Declare the module**

In `crates/server/src/main.rs`, add alongside the other `mod` lines (near `mod migrate;` / `mod events;`):

```rust
mod codegen;
```

- [ ] **Step 2: Write the failing test**

Create `crates/server/src/codegen.rs` with only the test module and a stub:

```rust
//! `donat codegen go`: generate Go row structs from the Postgres catalog.
//!
//! The pure `generate_go` turns a catalog snapshot + the set of tracked
//! tables + an enum-label map into a single gofmt-ready Go source string.
//! It performs no I/O so it is fully snapshot-testable. `run_codegen` wraps
//! it with introspection and file writing.

use std::collections::BTreeMap;

use donat_catalog::{Catalog, ColumnInfo, TableInfo};

/// Map of Postgres enum type name -> ordered labels.
pub type EnumMap = BTreeMap<String, Vec<String>>;

/// Generate Go source for `tracked` tables `(schema, name)` found in `catalog`.
pub fn generate_go(
    catalog: &Catalog,
    tracked: &[(String, String)],
    enums: &EnumMap,
    package: &str,
) -> String {
    todo!("implemented across Tasks 1-6")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, pg_type: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo { name: name.into(), pg_type: pg_type.into(), nullable, has_default: false }
    }

    fn catalog_with(table: TableInfo) -> Catalog {
        let mut tables = BTreeMap::new();
        tables.insert(format!("{}.{}", table.schema, table.name), table);
        Catalog { tables, functions: BTreeMap::new() }
    }

    #[test]
    fn scalars_basic() {
        let t = TableInfo {
            schema: "public".into(),
            name: "test_t1".into(),
            columns: vec![
                col("c1", "int4", false),
                col("c2", "text", false),
                col("ok", "bool", false),
            ],
            primary_key: vec![],
            foreign_keys: vec![],
        };
        let cat = catalog_with(t);
        let out = generate_go(&cat, &[("public".into(), "test_t1".into())], &EnumMap::new(), "donat_gen");
        insta::assert_snapshot!(out);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p donat-server codegen::tests::scalars_basic`
Expected: FAIL — panics in `todo!`.

- [ ] **Step 4: Implement the minimal generator**

Replace the `generate_go` body and add helpers in `crates/server/src/codegen.rs`:

```rust
/// Tracks which imports the generated file needs.
#[derive(Default)]
struct Needs {
    time: bool,
    decimal: bool,
    json: bool,
}

/// Go type for a Postgres `typname`. Records needed imports in `needs`.
/// `enums` lets enum-typed columns resolve to their generated alias name.
fn map_type(pg_type: &str, enums: &EnumMap, needs: &mut Needs) -> String {
    // Array types: pg `typname` is the element name with a leading underscore.
    if let Some(elem) = pg_type.strip_prefix('_') {
        return format!("[]{}", map_type(elem, enums, needs));
    }
    if enums.contains_key(pg_type) {
        return pascal(pg_type);
    }
    match pg_type {
        "int2" => "int16".into(),
        "int4" => "int32".into(),
        "int8" => "int64".into(),
        "float4" | "float8" => "float64".into(),
        "numeric" => { needs.decimal = true; "decimal.Decimal".into() }
        "text" | "varchar" | "bpchar" | "name" => "string".into(),
        "bool" => "bool".into(),
        "uuid" => "string".into(),
        "timestamptz" | "timestamp" | "date" => { needs.time = true; "time.Time".into() }
        "json" | "jsonb" => { needs.json = true; "json.RawMessage".into() }
        _ => { needs.json = true; "json.RawMessage".into() } // unknown: never fail
    }
}

/// snake_case / lower_case identifier -> PascalCase Go identifier.
fn pascal(s: &str) -> String {
    s.split(|c| c == '_' || c == ' ')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut ch = p.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &ch.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect()
}

/// Go type name for a table. `public` is unprefixed; other schemas are
/// schema-prefixed so cross-schema same-named tables never collide.
fn go_type_name(schema: &str, name: &str) -> String {
    if schema == "public" { pascal(name) } else { format!("{}{}", pascal(schema), pascal(name)) }
}

/// A column's Go type, made a pointer when the column is nullable.
fn field_type(col: &ColumnInfo, enums: &EnumMap, needs: &mut Needs) -> String {
    let base = map_type(&col.pg_type, enums, needs);
    if col.nullable { format!("*{base}") } else { base }
}

pub fn generate_go(
    catalog: &Catalog,
    tracked: &[(String, String)],
    enums: &EnumMap,
    package: &str,
) -> String {
    let mut needs = Needs::default();
    let mut body = String::new();

    for (schema, name) in tracked {
        let Some(table) = catalog.tables.get(&format!("{schema}.{name}")) else {
            eprintln!("warning: tracked table {schema}.{name} not found in catalog; skipped");
            continue;
        };
        body.push_str(&format!("type {} struct {{\n", go_type_name(&table.schema, &table.name)));
        for c in &table.columns {
            body.push_str(&format!(
                "\t{} {} `json:\"{}\"`\n",
                pascal(&c.name),
                field_type(c, enums, &mut needs),
                c.name,
            ));
        }
        body.push_str("}\n\n");
    }

    let mut out = String::new();
    out.push_str("// Code generated by donat codegen go; DO NOT EDIT.\n\n");
    out.push_str(&format!("package {package}\n\n"));

    let mut imports: Vec<&str> = Vec::new();
    if needs.json { imports.push("\"encoding/json\""); }
    if needs.time { imports.push("\"time\""); }
    if needs.decimal { imports.push("\"github.com/shopspring/decimal\""); }
    if !imports.is_empty() {
        out.push_str("import (\n");
        for imp in imports { out.push_str(&format!("\t{imp}\n")); }
        out.push_str(")\n\n");
    }
    out.push_str(&body);
    out
}
```

- [ ] **Step 5: Run the test and accept the snapshot**

Run: `cargo test -p donat-server codegen::tests::scalars_basic`
Expected: FAIL (new snapshot). Then review and accept:
Run: `cargo insta review` — confirm the struct has `C1 int32`, `C2 string`, `Ok bool` with correct json tags and no import block. Accept.
Re-run: `cargo test -p donat-server codegen::tests::scalars_basic` → PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/codegen.rs crates/server/src/main.rs crates/server/src/snapshots
git commit -m "codegen: pure generate_go for basic scalar columns"
```

---

### Task 2: Remaining scalar types (int2/int8/numeric/float/time/uuid/jsonb)

**Files:**
- Modify: `crates/server/src/codegen.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
#[test]
fn scalars_all_types() {
    let t = TableInfo {
        schema: "public".into(),
        name: "wide".into(),
        columns: vec![
            col("a", "int2", false),
            col("b", "int8", false),
            col("amount", "numeric", false),
            col("ratio", "float8", false),
            col("created_at", "timestamptz", false),
            col("uid", "uuid", false),
            col("payload", "jsonb", false),
            col("weird", "tsvector", false), // unknown -> json.RawMessage
        ],
        primary_key: vec![],
        foreign_keys: vec![],
    };
    let cat = catalog_with(t);
    let out = generate_go(&cat, &[("public".into(), "wide".into())], &EnumMap::new(), "donat_gen");
    insta::assert_snapshot!(out);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p donat-server codegen::tests::scalars_all_types`
Expected: FAIL (no snapshot yet) — the mapping from Task 1 already handles these, so this test only locks the snapshot.

- [ ] **Step 3: Accept the snapshot**

Run: `cargo insta review` — verify: `A int16`, `B int64`, `Amount decimal.Decimal`, `Ratio float64`, `CreatedAt time.Time`, `Uid string`, `Payload json.RawMessage`, `Weird json.RawMessage`, and an import block containing `encoding/json`, `time`, `github.com/shopspring/decimal`. Accept.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p donat-server codegen::tests::scalars_all_types`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/codegen.rs crates/server/src/snapshots
git commit -m "codegen: snapshot full scalar type mapping"
```

---

### Task 3: Nullable columns → pointers

**Files:**
- Modify: `crates/server/src/codegen.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
#[test]
fn nullable_columns_become_pointers() {
    let t = TableInfo {
        schema: "public".into(),
        name: "nul".into(),
        columns: vec![
            col("id", "int4", false),
            col("note", "text", true),
            col("amount", "numeric", true),
        ],
        primary_key: vec![],
        foreign_keys: vec![],
    };
    let cat = catalog_with(t);
    let out = generate_go(&cat, &[("public".into(), "nul".into())], &EnumMap::new(), "donat_gen");
    insta::assert_snapshot!(out);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p donat-server codegen::tests::nullable_columns_become_pointers`
Expected: FAIL (no snapshot). The pointer logic from Task 1 (`field_type`) already handles this; this test locks it.

- [ ] **Step 3: Accept the snapshot**

Run: `cargo insta review` — verify `Id int32`, `Note *string`, `Amount *decimal.Decimal`. Accept.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p donat-server codegen::tests::nullable_columns_become_pointers`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/codegen.rs crates/server/src/snapshots
git commit -m "codegen: nullable columns map to pointer types"
```

---

### Task 4: Array columns → Go slices

**Files:**
- Modify: `crates/server/src/codegen.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
#[test]
fn array_columns_become_slices() {
    let t = TableInfo {
        schema: "public".into(),
        name: "arr".into(),
        columns: vec![
            col("ids", "_int4", false),     // int4[] -> []int32
            col("tags", "_text", true),     // nullable text[] -> *[]string
        ],
        primary_key: vec![],
        foreign_keys: vec![],
    };
    let cat = catalog_with(t);
    let out = generate_go(&cat, &[("public".into(), "arr".into())], &EnumMap::new(), "donat_gen");
    insta::assert_snapshot!(out);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p donat-server codegen::tests::array_columns_become_slices`
Expected: FAIL (no snapshot). The `strip_prefix('_')` branch from Task 1 already produces slices; this locks the snapshot.

- [ ] **Step 3: Accept the snapshot**

Run: `cargo insta review` — verify `Ids []int32` and `Tags *[]string` (nullable wraps the whole slice in a pointer). Accept.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p donat-server codegen::tests::array_columns_become_slices`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/codegen.rs crates/server/src/snapshots
git commit -m "codegen: array columns map to Go slices"
```

---

### Task 5: Enum types → typed alias + constants

**Files:**
- Modify: `crates/server/src/codegen.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
#[test]
fn enum_types_emit_alias_and_consts() {
    let t = TableInfo {
        schema: "public".into(),
        name: "task".into(),
        columns: vec![
            col("id", "int4", false),
            col("status", "task_status", false),   // enum
            col("prev", "task_status", true),       // nullable enum -> pointer
        ],
        primary_key: vec![],
        foreign_keys: vec![],
    };
    let cat = catalog_with(t);
    let mut enums = EnumMap::new();
    enums.insert("task_status".into(), vec!["open".into(), "in_progress".into(), "done".into()]);
    let out = generate_go(&cat, &[("public".into(), "task".into())], &enums, "donat_gen");
    insta::assert_snapshot!(out);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p donat-server codegen::tests::enum_types_emit_alias_and_consts`
Expected: FAIL — column maps to `TaskStatus` (via Task 1's enum branch) but no alias/consts are emitted yet, so the snapshot is missing the enum declaration. (Will fail at snapshot creation; we still need to emit the alias block.)

- [ ] **Step 3: Emit enum declarations**

In `generate_go`, before the per-table loop builds `body`, prepend enum declarations. Add this just after `let mut body = String::new();`:

```rust
    // Enum type aliases + label constants, deterministic by type name.
    for (enum_name, labels) in enums {
        let go_name = pascal(enum_name);
        body.push_str(&format!("type {go_name} string\n\n"));
        body.push_str("const (\n");
        for label in labels {
            body.push_str(&format!(
                "\t{}{} {} = \"{}\"\n",
                go_name, pascal(label), go_name, label,
            ));
        }
        body.push_str(")\n\n");
    }
```

(The column type already resolves to `pascal(enum_name)` via `map_type`'s `enums.contains_key` branch, and nullable still wraps it in a pointer via `field_type`.)

- [ ] **Step 4: Accept the snapshot**

Run: `cargo test -p donat-server codegen::tests::enum_types_emit_alias_and_consts`
Then: `cargo insta review` — verify:
```go
type TaskStatus string

const (
	TaskStatusOpen TaskStatus = "open"
	TaskStatusInProgress TaskStatus = "in_progress"
	TaskStatusDone TaskStatus = "done"
)

type Task struct {
	Id int32 `json:"id"`
	Status TaskStatus `json:"status"`
	Prev *TaskStatus `json:"prev"`
}
```
Accept.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p donat-server codegen::tests::enum_types_emit_alias_and_consts`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/codegen.rs crates/server/src/snapshots
git commit -m "codegen: pg enums emit typed alias + label constants"
```

---

### Task 6: Multi-schema naming + multi-table ordering

**Files:**
- Modify: `crates/server/src/codegen.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
#[test]
fn multi_schema_tables_disambiguated() {
    let mut tables = BTreeMap::new();
    for (schema, name) in [("public", "user"), ("auth", "user")] {
        tables.insert(
            format!("{schema}.{name}"),
            TableInfo {
                schema: schema.into(),
                name: name.into(),
                columns: vec![col("id", "int4", false)],
                primary_key: vec![],
                foreign_keys: vec![],
            },
        );
    }
    let cat = Catalog { tables, functions: BTreeMap::new() };
    let tracked = vec![("public".into(), "user".into()), ("auth".into(), "user".into())];
    let out = generate_go(&cat, &tracked, &EnumMap::new(), "donat_gen");
    insta::assert_snapshot!(out);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p donat-server codegen::tests::multi_schema_tables_disambiguated`
Expected: FAIL (no snapshot). `go_type_name` from Task 1 already yields `User` and `AuthUser`; this locks the behavior.

- [ ] **Step 3: Accept the snapshot**

Run: `cargo insta review` — verify two structs: `type User struct` (public, unprefixed) and `type AuthUser struct` (schema-prefixed), in tracked order. Accept.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p donat-server codegen::tests::multi_schema_tables_disambiguated`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/codegen.rs crates/server/src/snapshots
git commit -m "codegen: schema-prefix non-public table type names"
```

---

### Task 7: DB-facing `run_codegen` + the `codegen go` subcommand

**Files:**
- Modify: `crates/server/src/codegen.rs`
- Modify: `crates/server/src/main.rs:64-103` (Command enum + arg struct), `:129-157` (dispatch)

- [ ] **Step 1: Add the supplemental enum query + runner to `codegen.rs`**

Append to `crates/server/src/codegen.rs` (outside `mod tests`):

```rust
use std::path::Path;

use anyhow::{Context, Result};
use tokio_postgres::NoTls;

/// SQL: every enum type's labels in sort order, user schemas only.
const ENUMS_SQL: &str = "\
SELECT t.typname, e.enumlabel \
FROM pg_type t \
JOIN pg_enum e ON e.enumtypid = t.oid \
JOIN pg_namespace n ON t.typnamespace = n.oid \
WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') \
ORDER BY t.typname, e.enumsortorder";

async fn fetch_enums(client: &tokio_postgres::Client) -> Result<EnumMap> {
    let rows = client.query(ENUMS_SQL, &[]).await.context("querying pg_enum")?;
    let mut map = EnumMap::new();
    for row in rows {
        let typname: String = row.get(0);
        let label: String = row.get(1);
        map.entry(typname).or_default().push(label);
    }
    Ok(map)
}

/// Collect tracked `(schema, name)` pairs from the metadata directory.
/// `QualifiedTable::Name` defaults to the `public` schema.
fn tracked_tables(metadata: &donat_metadata::Metadata) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for source in &metadata.sources {
        for t in &source.tables {
            out.push((t.table.schema().to_string(), t.table.name().to_string()));
        }
    }
    out
}

/// Introspect the database, generate Go, and write `<out_dir>/donat_gen.go`.
pub async fn run_codegen(
    database_url: &str,
    metadata_dir: &Path,
    out_dir: &Path,
    package: &str,
) -> Result<()> {
    let metadata = donat_metadata::load_metadata_dir(metadata_dir)
        .with_context(|| format!("loading metadata from {}", metadata_dir.display()))?;
    let tracked = tracked_tables(&metadata);

    let (client, conn) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connecting to database for codegen")?;
    let conn = tokio::spawn(async move { conn.await });
    let catalog = donat_catalog::introspect(&client)
        .await
        .context("introspecting database")?;
    let enums = fetch_enums(&client).await?;
    conn.abort();

    let source = generate_go(&catalog, &tracked, &enums, package);
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("creating out dir {}", out_dir.display()))?;
    let path = out_dir.join("donat_gen.go");
    std::fs::write(&path, source).with_context(|| format!("writing {}", path.display()))?;
    tracing::info!(path = %path.display(), tables = tracked.len(), "Go types generated");
    Ok(())
}
```

Confirm the top of the file already has `use donat_catalog::{Catalog, ColumnInfo, TableInfo};` (from Task 1); `ColumnInfo`/`TableInfo` may now be flagged unused outside tests — if the compiler warns, change that line to `use donat_catalog::Catalog;` and add `#[allow(unused_imports)] use donat_catalog::{ColumnInfo, TableInfo};` inside `mod tests` instead. (Adjust only if the build warns.)

- [ ] **Step 2: Add the subcommand to `main.rs`**

In the `Command` enum (`crates/server/src/main.rs:64-72`), add a variant:

```rust
    /// Generate Go row structs from the catalog for the embedded SDK.
    Codegen(CodegenArgs),
```

After the `ValidateArgs` struct (`crates/server/src/main.rs:~98-103`), add:

```rust
#[derive(clap::Args, Debug)]
struct CodegenArgs {
    /// `go` is the only target today.
    #[arg(value_parser = ["go"])]
    target: String,
    /// Metadata directory (defaults to --metadata-dir).
    #[arg(long)]
    metadata_dir: Option<PathBuf>,
    /// Output directory for the generated file.
    #[arg(long, default_value = "gen")]
    out: PathBuf,
    /// Go package name for the generated file.
    #[arg(long, default_value = "donat_gen")]
    package: String,
}
```

In the dispatch `match` (`crates/server/src/main.rs:129-157`), add a new arm before the final `_ => {}`:

```rust
        Some(Command::Codegen(c)) => {
            let dir = c
                .metadata_dir
                .clone()
                .or_else(|| args.metadata_dir.clone())
                .ok_or_else(|| anyhow::anyhow!("codegen needs --metadata-dir"))?;
            codegen::run_codegen(&database_url, &dir, &c.out, &c.package).await?;
            return Ok(());
        }
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build -p donat-server --bin donat`
Expected: builds clean. If `ColumnInfo`/`TableInfo` unused-import warnings appear, apply the adjustment noted in Step 1.

- [ ] **Step 4: Manual smoke test against a fixture DB**

Run (requires Postgres at `PG_URL`; reuse the conformance default):
```bash
DB="postgresql://postgres:postgres@127.0.0.1:15432/postgres"
./target/debug/donat migrate --migrations-dir migrations --database-url "$DB"
./target/debug/donat codegen go --metadata-dir crates/conformance/fixtures --out /tmp/donat-gen --database-url "$DB" || true
sed -n '1,40p' /tmp/donat-gen/donat_gen.go
```
Expected: a `donat_gen.go` with the `DO NOT EDIT` header, `package donat_gen`, and structs for tracked tables. (Exact tables depend on the chosen `--metadata-dir`; this is a smoke check that the pipeline runs and writes a file, not an assertion.)

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/codegen.rs crates/server/src/main.rs
git commit -m "codegen: donat codegen go subcommand (introspect + enums + write)"
```

- [ ] **Step 6: Judge review (mandatory per CLAUDE.md)**

Dispatch the judge agent on the Phase A commits before starting Phase B:
```
Agent(subagent_type="judge", run_in_background=true,
  prompt="REVIEW TASK: Phase A of specs/003 — `donat codegen go` subcommand and the pure generate_go function in crates/server/src/codegen.rs. Verify: pg->Go type mapping matches the spec table (numeric->decimal.Decimal, arrays via leading-underscore typname, nullable->pointer, enum alias+consts), no admin surface added, snapshots are sensible. Commits since the Phase A start.")
```
Continue only after ACCEPT; on REJECT, fix first.

---

## Phase B — Go SDK (pure Go, no cgo)

### Task 8: Scaffold the module + envelope value types

**Files:**
- Create: `sdk/go/go.mod`
- Create: `sdk/go/donat/event.go`

- [ ] **Step 1: Create the module**

Create `sdk/go/go.mod`:

```
module github.com/donat/donat-go

go 1.22

require github.com/shopspring/decimal v1.4.0
```

- [ ] **Step 2: Write the envelope types (no decoder yet)**

Create `sdk/go/donat/event.go`:

```go
// Package donat is the pure-Go SDK for handling Donat event triggers in a Go
// process. It has no cgo dependency: `go get` and compile into your binary.
package donat

import "time"

// Op is the row operation that produced an event.
type Op string

const (
	OpInsert Op = "INSERT"
	OpUpdate Op = "UPDATE"
	OpDelete Op = "DELETE"
)

// TableRef identifies the table an event came from.
type TableRef struct {
	Schema string `json:"schema"`
	Name   string `json:"name"`
}

// TriggerRef identifies the firing trigger.
type TriggerRef struct {
	Name string `json:"name"`
}

// DeliveryInfo carries retry bookkeeping from the delivery layer.
type DeliveryInfo struct {
	CurrentRetry int `json:"current_retry"`
	MaxRetries   int `json:"max_retries"`
}

// Event is a decoded Donat event-trigger payload. T is a generated row struct.
// Old is nil on INSERT; New is nil on DELETE.
type Event[T any] struct {
	ID        string
	CreatedAt time.Time
	Table     TableRef
	Trigger   TriggerRef
	Op        Op
	Old       *T
	New       *T
	Session   map[string]string
	Delivery  DeliveryInfo
}
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cd sdk/go && go mod tidy && go build ./...`
Expected: builds clean (downloads `shopspring/decimal`).

- [ ] **Step 4: Commit**

```bash
git add sdk/go/go.mod sdk/go/go.sum sdk/go/donat/event.go
git commit -m "sdk(go): scaffold module + event envelope value types"
```

---

### Task 9: `ParseEvent[T]` — decode the Donat envelope

**Files:**
- Modify: `sdk/go/donat/event.go`
- Create: `sdk/go/donat/event_test.go`

- [ ] **Step 1: Write the failing test**

Create `sdk/go/donat/event_test.go` (envelope literals taken from `crates/conformance/tests/event_triggers.rs`):

```go
package donat

import "testing"

type row struct {
	C1 int32   `json:"c1"`
	C2 string  `json:"c2"`
}

const insertEnvelope = `{
  "id": "ec1c2e0a-0000-0000-0000-000000000000",
  "created_at": "2026-06-13T10:00:00.000000+00:00",
  "table": { "schema": "hge_tests", "name": "test_t1" },
  "trigger": { "name": "t1_all" },
  "event": {
    "op": "INSERT",
    "data": { "old": null, "new": { "c1": 1, "c2": "hello" } },
    "session_variables": null
  },
  "delivery_info": { "current_retry": 0, "max_retries": 0 }
}`

const deleteEnvelope = `{
  "id": "ec1c2e0a-0000-0000-0000-000000000001",
  "created_at": "2026-06-13T10:00:00.000000+00:00",
  "table": { "schema": "hge_tests", "name": "test_t1" },
  "trigger": { "name": "t1_all" },
  "event": {
    "op": "DELETE",
    "data": { "old": { "c1": 1, "c2": "world" }, "new": null },
    "session_variables": null
  },
  "delivery_info": { "current_retry": 0, "max_retries": 0 }
}`

func TestParseEventInsert(t *testing.T) {
	ev, err := ParseEvent[row]([]byte(insertEnvelope))
	if err != nil {
		t.Fatalf("ParseEvent: %v", err)
	}
	if ev.Op != OpInsert {
		t.Errorf("Op = %q, want INSERT", ev.Op)
	}
	if ev.Old != nil {
		t.Errorf("Old = %+v, want nil on INSERT", ev.Old)
	}
	if ev.New == nil || ev.New.C1 != 1 || ev.New.C2 != "hello" {
		t.Errorf("New = %+v, want {1 hello}", ev.New)
	}
	if ev.Table.Schema != "hge_tests" || ev.Trigger.Name != "t1_all" {
		t.Errorf("table/trigger = %+v/%+v", ev.Table, ev.Trigger)
	}
}

func TestParseEventDelete(t *testing.T) {
	ev, err := ParseEvent[row]([]byte(deleteEnvelope))
	if err != nil {
		t.Fatalf("ParseEvent: %v", err)
	}
	if ev.Op != OpDelete {
		t.Errorf("Op = %q, want DELETE", ev.Op)
	}
	if ev.New != nil {
		t.Errorf("New = %+v, want nil on DELETE", ev.New)
	}
	if ev.Old == nil || ev.Old.C2 != "world" {
		t.Errorf("Old = %+v, want {1 world}", ev.Old)
	}
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd sdk/go && go test ./donat/`
Expected: FAIL — `undefined: ParseEvent`.

- [ ] **Step 3: Implement `ParseEvent[T]`**

Append to `sdk/go/donat/event.go`:

```go
import (
	"encoding/json"
)

// wireEnvelope mirrors the on-the-wire Donat envelope (nested under "event").
type wireEnvelope[T any] struct {
	ID        string     `json:"id"`
	CreatedAt time.Time  `json:"created_at"`
	Table     TableRef   `json:"table"`
	Trigger   TriggerRef `json:"trigger"`
	Event     struct {
		Op   Op `json:"op"`
		Data struct {
			Old *T `json:"old"`
			New *T `json:"new"`
		} `json:"data"`
		SessionVariables map[string]string `json:"session_variables"`
	} `json:"event"`
	DeliveryInfo DeliveryInfo `json:"delivery_info"`
}

// ParseEvent decodes a raw Donat event-trigger envelope into Event[T].
func ParseEvent[T any](raw []byte) (Event[T], error) {
	var w wireEnvelope[T]
	if err := json.Unmarshal(raw, &w); err != nil {
		return Event[T]{}, err
	}
	return Event[T]{
		ID:        w.ID,
		CreatedAt: w.CreatedAt,
		Table:     w.Table,
		Trigger:   w.Trigger,
		Op:        w.Event.Op,
		Old:       w.Event.Data.Old,
		New:       w.Event.Data.New,
		Session:   w.Event.SessionVariables,
		Delivery:  w.DeliveryInfo,
	}, nil
}
```

Merge the new `import "encoding/json"` with the existing `import "time"` into one block:

```go
import (
	"encoding/json"
	"time"
)
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd sdk/go && go test ./donat/`
Expected: PASS (both tests).

- [ ] **Step 5: Add a numeric-precision round-trip test**

Add to `sdk/go/donat/event_test.go`:

```go
import "github.com/shopspring/decimal"

type money struct {
	Amount decimal.Decimal `json:"amount"`
}

func TestParseEventNumericPrecision(t *testing.T) {
	const env = `{
      "id": "x", "created_at": "2026-06-13T10:00:00.000000+00:00",
      "table": {"schema":"public","name":"acct"}, "trigger": {"name":"t"},
      "event": {"op":"INSERT","data":{"old":null,"new":{"amount":12345678901234.56789}},"session_variables":null},
      "delivery_info": {"current_retry":0,"max_retries":0}
    }`
	ev, err := ParseEvent[money]([]byte(env))
	if err != nil {
		t.Fatalf("ParseEvent: %v", err)
	}
	if got := ev.New.Amount.String(); got != "12345678901234.56789" {
		t.Errorf("Amount = %s, want 12345678901234.56789 (no precision loss)", got)
	}
}
```

Move the two `import` additions (`encoding/json` is not needed in the test; `decimal` is) into a single import block at the top of `event_test.go`:

```go
import (
	"testing"

	"github.com/shopspring/decimal"
)
```

- [ ] **Step 6: Run to verify it passes**

Run: `cd sdk/go && go test ./donat/`
Expected: PASS — proves `numeric` decodes losslessly via `decimal.Decimal`.

- [ ] **Step 7: Commit**

```bash
git add sdk/go/donat/event.go sdk/go/donat/event_test.go
git commit -m "sdk(go): ParseEvent[T] decodes the Donat envelope (old/new, numeric precision)"
```

---

### Task 10: Registry — `On[T]`, `Dispatch`, `ErrNoHandler`, `Names`

**Files:**
- Create: `sdk/go/donat/registry.go`
- Create: `sdk/go/donat/registry_test.go`

- [ ] **Step 1: Write the failing test**

Create `sdk/go/donat/registry_test.go`:

```go
package donat

import (
	"context"
	"errors"
	"testing"
)

func TestDispatchRoutesToTypedHandler(t *testing.T) {
	r := NewRegistry()
	var gotName string
	On(r, "t1_all", func(_ context.Context, ev Event[row]) error {
		gotName = ev.New.C2
		return nil
	})
	if err := r.Dispatch(context.Background(), "t1_all", []byte(insertEnvelope)); err != nil {
		t.Fatalf("Dispatch: %v", err)
	}
	if gotName != "hello" {
		t.Errorf("handler saw C2=%q, want hello", gotName)
	}
}

func TestDispatchUnknownTrigger(t *testing.T) {
	r := NewRegistry()
	err := r.Dispatch(context.Background(), "nope", []byte(insertEnvelope))
	if !errors.Is(err, ErrNoHandler) {
		t.Errorf("err = %v, want ErrNoHandler", err)
	}
}

func TestNamesListsRegistered(t *testing.T) {
	r := NewRegistry()
	On(r, "b", func(_ context.Context, _ Event[row]) error { return nil })
	On(r, "a", func(_ context.Context, _ Event[row]) error { return nil })
	names := r.Names()
	if len(names) != 2 || names[0] != "a" || names[1] != "b" {
		t.Errorf("Names() = %v, want sorted [a b]", names)
	}
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd sdk/go && go test ./donat/`
Expected: FAIL — `undefined: NewRegistry`, `On`, `ErrNoHandler`.

- [ ] **Step 3: Implement the registry**

Create `sdk/go/donat/registry.go`:

```go
package donat

import (
	"context"
	"errors"
	"sort"
	"sync"
)

// ErrNoHandler is returned by Dispatch when no handler is registered for a
// trigger name. Transports decide whether that is fatal.
var ErrNoHandler = errors.New("donat: no handler registered for trigger")

// Registry maps trigger names to typed handlers. It is transport-agnostic:
// any transport (webhook receiver, pull loop, in-process) calls Dispatch with
// the raw envelope. Handlers may be invoked concurrently and must be
// concurrent-safe.
type Registry struct {
	mu       sync.RWMutex
	handlers map[string]func(context.Context, []byte) error
}

// NewRegistry returns an empty Registry.
func NewRegistry() *Registry {
	return &Registry{handlers: make(map[string]func(context.Context, []byte) error)}
}

// On registers a typed handler for a trigger name. T is a generated row
// struct. Re-registering a name overwrites the previous handler.
func On[T any](r *Registry, triggerName string, h func(context.Context, Event[T]) error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.handlers[triggerName] = func(ctx context.Context, raw []byte) error {
		ev, err := ParseEvent[T](raw)
		if err != nil {
			return err
		}
		return h(ctx, ev)
	}
}

// Dispatch decodes and routes a raw envelope to the handler for triggerName.
// Returns ErrNoHandler if none is registered.
func (r *Registry) Dispatch(ctx context.Context, triggerName string, rawEnvelope []byte) error {
	r.mu.RLock()
	h, ok := r.handlers[triggerName]
	r.mu.RUnlock()
	if !ok {
		return ErrNoHandler
	}
	return h(ctx, rawEnvelope)
}

// Names returns the registered trigger names, sorted. A boot check can assert
// every YAML event_triggers[].name has a handler.
func (r *Registry) Names() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	names := make([]string, 0, len(r.handlers))
	for n := range r.handlers {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd sdk/go && go test ./donat/`
Expected: PASS (all registry + event tests).

- [ ] **Step 5: Commit**

```bash
git add sdk/go/donat/registry.go sdk/go/donat/registry_test.go
git commit -m "sdk(go): name-keyed handler registry with Dispatch seam"
```

---

### Task 11: cgo-free guardrail + golden generated-package compile check

**Files:**
- Create: `sdk/go/internal/golden/donat_gen.go`
- Create: `sdk/go/donat/golden_test.go`

- [ ] **Step 1: Add a checked-in golden generated file**

Create `sdk/go/internal/golden/donat_gen.go` (hand-written to match `generate_go` output exactly — this is the contract the generator must keep producing):

```go
// Code generated by donat codegen go; DO NOT EDIT.

package golden

import (
	"encoding/json"
	"time"

	"github.com/shopspring/decimal"
)

type OrderStatus string

const (
	OrderStatusOpen OrderStatus = "open"
	OrderStatusPaid OrderStatus = "paid"
)

type TestT1 struct {
	C1        int32           `json:"c1"`
	C2        *string         `json:"c2"`
	Amount    decimal.Decimal `json:"amount"`
	Tags      []string        `json:"tags"`
	Status    OrderStatus     `json:"status"`
	CreatedAt time.Time       `json:"created_at"`
	Payload   json.RawMessage `json:"payload"`
}
```

- [ ] **Step 2: Write the failing compile/compose test**

Create `sdk/go/donat/golden_test.go`:

```go
package donat_test

import (
	"context"
	"testing"

	"github.com/donat/donat-go/donat"
	"github.com/donat/donat-go/internal/golden"
)

// Proves a generated struct composes with the SDK generics end to end.
func TestGeneratedStructComposesWithSDK(t *testing.T) {
	r := donat.NewRegistry()
	donat.On(r, "on_t1", func(_ context.Context, ev donat.Event[golden.TestT1]) error {
		_ = ev.New // type-checks: Event[golden.TestT1]
		return nil
	})
	if got := r.Names(); len(got) != 1 || got[0] != "on_t1" {
		t.Errorf("Names() = %v, want [on_t1]", got)
	}
}
```

- [ ] **Step 3: Run to verify it builds and passes**

Run: `cd sdk/go && go test ./...`
Expected: PASS — the golden package compiles against the SDK and the test type-checks `Event[golden.TestT1]`.

- [ ] **Step 4: Verify the cgo-free constraint (the hard requirement)**

Run: `cd sdk/go && CGO_ENABLED=0 go build ./...`
Expected: builds clean with cgo disabled — proves the package (SDK + generated output + decimal dep) is natively loadable and statically buildable.

- [ ] **Step 5: Commit**

```bash
git add sdk/go/internal/golden/donat_gen.go sdk/go/donat/golden_test.go
git commit -m "sdk(go): golden generated package + CGO_ENABLED=0 build guardrail"
```

- [ ] **Step 6: Judge review (mandatory per CLAUDE.md)**

Dispatch the judge agent on the Phase B commits:
```
Agent(subagent_type="judge", run_in_background=true,
  prompt="REVIEW TASK: Phase B of specs/003 — pure-Go SDK in sdk/go. Verify: Event[T] decodes the Donat envelope shape from crates/server/src/events.rs (old/new nil semantics, session, delivery_info), Registry.Dispatch is transport-agnostic with ErrNoHandler, no cgo anywhere, CGO_ENABLED=0 build passes, numeric uses decimal.Decimal losslessly. Phase B commits.")
```
Continue only after ACCEPT.

---

## Self-Review (completed during planning)

- **Spec coverage:** §1 codegen → Tasks 1–7; pg→Go mapping table → Tasks 1–5; multi-schema naming → Task 6; §2 `Event[T]` → Tasks 8–9; registry + `Dispatch` + `Names` → Task 10; packaging constraint (cgo-free) → Task 11 Step 4; testing section → tests in every task. **Forward-compat (pre/post hooks)** is a spec design note with no task — intentional (out of scope this plan); the registry shape in Task 10 is what enables it.
- **Deferred-by-design (no task, correct):** transport (webhook receiver / pull / in-process), sync hooks, custom functions — all explicitly out of scope in the spec.
- **Type consistency:** `generate_go(catalog, tracked, enums, package)`, `EnumMap`, `map_type`/`field_type`/`pascal`/`go_type_name`, `run_codegen` used consistently across Tasks 1–7. Go: `Event[T]`, `ParseEvent[T]`, `NewRegistry`/`On[T]`/`Dispatch`/`Names`/`ErrNoHandler` consistent across Tasks 8–11. The golden file in Task 11 matches the mapping rules from Tasks 1–5.
- **Known acceptable nuance:** `timestamptz` → `time.Time` relies on Postgres `to_jsonb` emitting RFC3339; documented in the spec. Session variables may be nil until Spec 002's GUC limitation is resolved.
```
