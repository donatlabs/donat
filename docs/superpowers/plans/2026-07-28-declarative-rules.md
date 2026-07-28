# Declarative Rules Implementation Plan

> **For Codex:** Execute every checkbox in order with RED/GREEN evidence and a
> judge ACCEPT after each commit.

**Goal:** Introduce a small, strict, typed CEL-inspired expression language
that can validate command input and make deterministic process decisions
without embedding arbitrary code in YAML.

**Architecture:** A new dependency-free `donat-rules` crate parses source text
into a typed AST, checks declared bindings, and evaluates only closed JSON
contexts. Commands compile the same typed AST to Postgres SQL; process guards
and decision tables evaluate it in Rust. One `rules.yaml` wrapper contains both
`rules` and `decision_tables`; it has no runtime mutation path.

**Tech stack:** Rust, serde, `thiserror`, `serde_yaml`, Postgres SQL renderer,
`proptest` or existing property-test dependency if already present, native
conformance harness.

**Specification:**
[`specs/004-decision-rules.md`](../../../specs/004-decision-rules.md)

## Required interfaces

```rust
// crates/rules/src/lib.rs
pub struct RuleCatalog { /* parsed, typed rules and decision tables */ }

pub struct RuleDefinition {
    pub name: String,
    pub bindings: BTreeMap<String, RuleType>,
    pub expression: String,
}

pub struct RulesMetadata {
    pub rules: Vec<RuleDefinition>,
    pub decision_tables: Vec<DecisionTableDefinition>,
}

pub enum RuleType {
    Bool, String, Int, Decimal, Uuid, Date, Timestamp,
    Enum(Vec<String>), List(Box<RuleType>), Object(BTreeMap<String, RuleType>),
}

pub enum Expr { /* literals, bindings, access, calls, operators */ }

pub fn compile_catalog(
    rules: &[RuleDefinition],
    tables: &[DecisionTableDefinition],
) -> Result<RuleCatalog, RuleError>;

pub fn evaluate_bool(
    rule: &CompiledRule,
    bindings: &RuleBindings,
) -> Result<bool, RuleError>;

pub fn lower_postgres(
    rule: &CompiledRule,
    bindings: &SqlBindings,
) -> Result<String, RuleError>;
```

`RuleType` has no implicit conversion. Nullable values may only participate in
`== null`, `!= null`, or `is_null(value)`. Allowed functions are exactly
`size`, `is_null`, `startsWith`, and `endsWith`; source length is at most 4096
bytes, AST depth at most 64, and a literal list has at most 256 elements.

### Task 1: Add metadata shapes and empty-directory compatibility

**Files:**
- Modify: `Cargo.toml`, `crates/metadata/src/types.rs`,
  `crates/metadata/src/loader.rs`
- Test: `crates/metadata/tests/types_serde.rs`,
  `crates/metadata/tests/load_fixture.rs`,
  `crates/metadata/tests/loader_includes.rs`
- Add fixtures: `crates/metadata/tests/fixtures/rules/`

- [ ] Add a deserialization test for the single `rules.yaml` wrapper, including
  a quoted `"!include child.yaml"` entry in either list. Add a regression test
  that a directory lacking the file loads with empty rule and decision-table
  vectors.
- [ ] RED: run `cargo test -p donat-metadata rules`. Expected: compile failure
  because `Metadata::rules`, `Metadata::decision_tables`, and their YAML types
  do not exist.
- [ ] Define serde-owned `RuleDefinition`, `DecisionTableDefinition`,
  `DecisionRow`, declared binding types, and source-location-free expression
  fields in `types.rs`. Keep source text as `String`; parsing belongs only to
  `donat-rules`.
- [ ] Load the `rules.yaml` wrapper through existing `load_section`, and add an
  empty wrapper/default to every explicit `Metadata` literal, including
  `crates/server/src/main.rs` and conformance builders once compiler errors
  identify them. Do not introduce `decision_tables.yaml`.
- [ ] GREEN: run `cargo test -p donat-metadata` and
  `cargo test -p donat-conformance --lib metadata`. Expected: metadata
  round-trips preserve the new sections and existing fixtures are unchanged.
- [ ] Commit this metadata-only slice and obtain judge ACCEPT.

### Task 2: Parse the deliberately small language

**Files:**
- Add: `crates/rules/Cargo.toml`, `crates/rules/src/lib.rs`,
  `crates/rules/src/lexer.rs`, `crates/rules/src/parser.rs`,
  `crates/rules/tests/parser.rs`
- Modify: root `Cargo.toml`

- [ ] Add parser tests for literals, object/list member access, precedence,
  `&&`, `||`, `!`, comparisons, arithmetic, and each of the four allowed
  functions. Add negative tests for unknown syntax, overly long input, depth
  65, and a 257-item literal list.
- [ ] RED: run `cargo test -p donat-rules --test parser`. Expected: package
  `donat-rules` does not exist.
- [ ] Add `crates/rules` as workspace member, with `serde`, `serde_json`, and
  `thiserror` only. Implement a span-carrying lexer/parser and expose a stable
  error that includes rule name, byte offset, and expectation without leaking
  parser internals into GraphQL errors.
- [ ] Make `parse_expression` reject any function name outside the four-item
  allowlist before type checking.
- [ ] GREEN: run `cargo test -p donat-rules --test parser` and
  `cargo test -p donat-rules`. Expected: every accepted expression produces a
  deterministic AST and every rejected expression carries its source offset.
- [ ] Commit this parser slice and obtain judge ACCEPT.

### Task 3: Type-check and evaluate closed bindings

**Files:**
- Modify: `crates/rules/src/lib.rs`
- Add: `crates/rules/src/types.rs`, `crates/rules/src/eval.rs`,
  `crates/rules/tests/eval.rs`

- [ ] Add tests for exact scalar matching, nullable restrictions, missing
  binding rejection, inaccessible object fields, `size` on a list, string
  predicates, short-circuit boolean operators, no implicit `Int`/`Decimal` or
  `Uuid`/`String` conversion, a `first` default row, and zero/multiple unique
  matches. Add tests that an evaluation trace contains only the table revision,
  row ID, per-condition booleans, and input digest, never the raw input or a
  secret value.
- [ ] RED: run `cargo test -p donat-rules --test eval`. Expected: compile
  failure because `compile_catalog` and `evaluate_bool` do not exist.
- [ ] Implement the declared type model and validate every AST node against a
  rule's named bindings. Reject duplicate rule/table names, undeclared names,
  incompatible branches, unnamed decision-map columns, and a `first` decision
  table without a final all-`true` default row. Support only `first` (first
  matching row) and `unique` (exactly one matching row); reject every other
  hit policy.
- [ ] Preserve stable decision row `id` and optional description. Compile each
  declared `test_cases` case and validate its expected output and matched row
  ID during `donat validate`; reject a result field that can select a runtime
  role or permission.
- [ ] Evaluate from a closed `BTreeMap<String, serde_json::Value>` only;
  disallow environment, time, network, database, reflection, and user-defined
  functions. Return typed evaluation errors rather than panicking.
- [ ] GREEN: run `cargo test -p donat-rules --test eval` and
  `cargo test -p donat-rules`. Expected: valid examples return an exact bool
  and invalid contexts fail before evaluation.
- [ ] Commit this evaluator slice and obtain judge ACCEPT.

### Task 4: Lower the shared typed AST to Postgres safely

**Files:**
- Add: `crates/rules/src/postgres.rs`, `crates/rules/tests/postgres.rs`
- Modify: `crates/rules/src/lib.rs`, `crates/sqlgen/Cargo.toml`,
  `crates/sqlgen/src/lib.rs`

- [ ] Add SQL snapshot tests for scalar comparisons, null handling, nested
  object/list access supplied as typed JSON, and each allowed function. Add
  rejection tests for a binding that has no typed SQL expression.
- [ ] RED: run `cargo test -p donat-rules --test postgres` and
  `cargo test -p donat-sqlgen rules`. Expected: no Postgres lowering API.
- [ ] Implement `lower_postgres` using sqlgen's existing identifier and
  literal-escaping rules through a narrow public helper; do not interpolate
  untrusted identifiers or raw expression text. Render parentheses explicitly
  for every binary expression.
- [ ] Introduce a SQL binding representation that distinguishes a typed SQL
  expression from a literal. A rule name is resolved before lowering and never
  becomes SQL text.
- [ ] Add a property test that generates supported closed contexts, compares
  Rust evaluation to `SELECT <lowered expression>` in Postgres, and stores the
  minimized counterexample when they differ.
- [ ] GREEN: run `cargo test -p donat-rules --test postgres`,
  `cargo test -p donat-sqlgen`, and the focused Postgres differential test.
  Review snapshots with `cargo insta review`.
- [ ] Commit this lowering slice and obtain judge ACCEPT.

### Task 5: Validate rules at deploy time and expose no runtime mutation

**Files:**
- Modify: `crates/server/Cargo.toml`, `crates/server/src/migrate.rs`,
  `crates/server/src/state.rs`, `crates/conformance/src/lib.rs`
- Add: `crates/conformance/tests/rules.rs`,
  `crates/conformance/fixtures/rules/`

- [ ] Add native fixtures for duplicate names, invalid source/type errors,
  missing `first` decision-table default, zero or multiple `unique` matches,
  failing declared decision test case, and an ordinary metadata directory with
  no rules. Add a test that `donat validate --metadata-dir` exits non-zero and
  names the failing metadata path.
- [ ] RED: run
  `cargo build -p donat-server --bin donat && cargo test -p donat-conformance --test rules`.
  Expected: validation accepts malformed declarative-rule metadata.
- [ ] Compile a `RuleCatalog` during candidate engine construction after
  metadata load. Have `migrate::check_consistency` collect its validation
  errors, and have serving startup reject invalid metadata before listening.
- [ ] Extend `ConformanceBuilder::write_metadata_dir` to serialize both new
  YAML files only when non-empty. Do not add any HTTP endpoint that changes
  rules or decision tables.
- [ ] GREEN: rebuild the binary and run the focused conformance test, then
  `cargo test -p donat-server` and `cargo test -p donat-metadata`.
- [ ] Commit this deploy-validation slice and obtain judge ACCEPT.

### Task 6: Preserve the contract before commands consume rules

**Files:**
- Modify only for test-proven defects in the files above.

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy -p donat-rules -p donat-metadata -p donat-server --all-targets -- -D warnings`.
- [ ] Run `cargo test -p donat-rules`, `cargo test -p donat-metadata`, and
  `cargo test -p donat-server`.
- [ ] Rebuild with `cargo build -p donat-server --bin donat` and run
  `cargo test -p donat-conformance --test rules`.
- [ ] Review all snapshots and obtain a final judge ACCEPT for this plan before
  beginning the commands plan.
