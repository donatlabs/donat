# Command Literal Database-Scalar Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make command metadata literals fail deterministically at deployment
unless they exactly satisfy the concrete PostgreSQL target column's scalar type,
modifier, and nullability constraints.

**Architecture:** Retain raw `atttypmod` with `ColumnInfo.pg_type` during
PostgreSQL catalog introspection. In the command compiler only, derive a
private `CommandScalarDescriptor` from a concrete target column whenever a
`CommandValue::Literal` is assigned to an object field or primary-key
predicate. The descriptor validates the literal before the existing
`StaticType` assignment check; arguments, items, prior-step values, Rules,
GraphQL schema generation, and the one-statement SQL contract remain unchanged.

**Tech Stack:** Rust, PostgreSQL 16, `tokio-postgres`, `serde_json`, `chrono`,
`uuid`, `insta`, native conformance harness.

## Global Constraints

- Keep `ColumnInfo.pg_type` and add the raw PostgreSQL `atttypmod`; do not
  synthesize a modifier from a GraphQL type or `native_type`.
- Derive `CommandScalarDescriptor` only for a command metadata literal with a
  concrete catalog-column destination; it is private to `crates/schema`.
- Do not add descriptor behavior to `StaticType`, GraphQL scalars, Rules,
  arguments, items, prior-step values, or rule values.
- Reject object and list literals, unknown PostgreSQL literal types, malformed
  modifiers, non-finite floats, and null for non-nullable columns at deploy
  time; never fall back to a static scalar.
- Preserve explicit classic roles, no admin or permission bypass, no runtime
  metadata mutation or DDL, and one Postgres statement per command root.
- Begin each behavior with its named failing test. After every commit, dispatch
  the mandatory judge and continue only after ACCEPT.
- The current Task 2 commit `26f4bde` is unaccepted. This plan repairs that
  range and requires fresh review; it does not claim earlier Task 2 review
  outcomes as approval.

---

## File structure and ownership

| File | Responsibility |
| --- | --- |
| `crates/catalog/src/lib.rs` | Add `ColumnInfo.pg_typmod`, select `a.atttypmod`, and retain the exact PostgreSQL value. |
| `crates/catalog/tests/introspect.rs` | Prove real PostgreSQL introspection retains type names and modifiers. |
| `crates/schema/src/commands.rs` | Define the private descriptor, decode supported modifiers, and validate column-bound literals. |
| `crates/schema/tests/commands.rs` | Freeze command compiler acceptance and rejection boundaries for every descriptor family. |
| `crates/server/src/state.rs`, `crates/server/src/migrate.rs`, `crates/server/tests/migrate.rs` | Prove candidate-engine construction, `validate`, and `migrate --metadata-dir` surface literal diagnostics before serving without writes from `validate`. |
| `crates/schema/tests/multi_source.rs`, `crates/schema/tests/planner.rs`, `crates/server/src/gql.rs`, `crates/server/src/mcp.rs`, `crates/server/src/state.rs`, `crates/sqlgen/tests/pipeline.rs` | Update every checked-in `ColumnInfo` literal with its explicit modifier sentinel or concrete test modifier. |

### Task 1: Retain raw PostgreSQL type modifiers in the catalog

**Files:**
- Modify: `crates/catalog/src/lib.rs`, `crates/catalog/tests/introspect.rs`,
  `crates/schema/tests/commands.rs`, `crates/schema/tests/multi_source.rs`,
  `crates/schema/tests/planner.rs`, `crates/server/src/gql.rs`,
  `crates/server/src/mcp.rs`, `crates/server/src/state.rs`,
  `crates/sqlgen/tests/pipeline.rs`
- Test: `crates/catalog/tests/introspect.rs`

**Interfaces:**
- Produces: `ColumnInfo { name, pg_type, pg_typmod, native_type, nullable,
  has_default }`, where `pg_typmod` is the raw `pg_attribute.atttypmod` value
  for PostgreSQL columns and `-1` for constructors without a PostgreSQL
  modifier.
- Consumed by: Task 2's private command descriptor.

- [ ] **Step 1: Add the real-PostgreSQL RED test for type modifiers.**

  Extend `introspection_retains_table_view_and_materialized_view_relation_kinds`
  or add a sibling async test that creates one uniquely named table with these
  columns: `smallint`, `integer`, `bigint`, `numeric(5,2)`, `varchar(3)`,
  `char(2)`, `timestamp(3)`, and `timestamptz(6)`. Assert exact catalog facts:

  ```rust
  assert_eq!(table.column("amount").unwrap().pg_type, "numeric");
  assert_eq!(table.column("amount").unwrap().pg_typmod, ((5 << 16) | 2) + 4);
  assert_eq!(table.column("code").unwrap().pg_typmod, 3 + 4);
  assert_eq!(table.column("created_at").unwrap().pg_typmod, 3);
  assert_eq!(table.column("received_at").unwrap().pg_typmod, 6);
  assert_eq!(table.column("id").unwrap().pg_typmod, -1);
  ```

  Drop the table in the test cleanup after the assertions.

- [ ] **Step 2: Run the RED catalog test.**

  Run: `cargo test -p donat-catalog --test introspect type_modifiers -- --nocapture`

  Expected: FAIL because `ColumnInfo` has no `pg_typmod` field and
  `COLUMNS_SQL` does not select `a.atttypmod`.

- [ ] **Step 3: Add the catalog field and retain the raw value.**

  In `ColumnInfo`, add `pub pg_typmod: i32` with serde compatibility for
  legacy serialized test data. Add `a.atttypmod` to `COLUMNS_SQL`, read it as
  `i32` while building the PostgreSQL `ColumnInfo`, and preserve `-1` exactly
  rather than replacing it with `None` or a normalized length. Update every
  listed `ColumnInfo` literal explicitly: use `-1` for an unmodified type and
  the intended raw value when a test exercises a modifier.

  ```rust
  pub struct ColumnInfo {
      pub name: String,
      pub pg_type: String,
      pub pg_typmod: i32,
      pub native_type: Option<String>,
      pub nullable: bool,
      pub has_default: bool,
  }
  ```

- [ ] **Step 4: Run the focused catalog and compile checks.**

  Run: `cargo test -p donat-catalog --test introspect type_modifiers && cargo test -p donat-catalog && cargo test --workspace --no-run`

  Expected: PASS. The real PostgreSQL test observes raw `atttypmod`; every
  struct literal names a modifier instead of relying on an implicit default.

- [ ] **Step 5: Review and commit the catalog slice.**

  Run: `git diff --check && git diff -- crates/catalog/src/lib.rs crates/catalog/tests/introspect.rs crates/schema/tests/commands.rs crates/schema/tests/multi_source.rs crates/schema/tests/planner.rs crates/server/src/gql.rs crates/server/src/mcp.rs crates/server/src/state.rs crates/sqlgen/tests/pipeline.rs`

  Confirm the diff contains no command runtime behavior, DDL, role change, or
  GraphQL type change. Then commit exactly this task:

  ```bash
  git add crates/catalog/src/lib.rs crates/catalog/tests/introspect.rs \
    crates/schema/tests/commands.rs crates/schema/tests/multi_source.rs \
    crates/schema/tests/planner.rs crates/server/src/gql.rs \
    crates/server/src/mcp.rs crates/server/src/state.rs \
    crates/sqlgen/tests/pipeline.rs
  git commit -m "feat(catalog): retain postgres type modifiers"
  ```

- [ ] **Step 6: Dispatch and satisfy the mandatory Task 1 judge gate.**

  Dispatch the judge with the Task 1 commit SHA, the raw `atttypmod` contract,
  the exact focused command above, and the requirement that all `ColumnInfo`
  literals are explicit. Continue to Task 2 only after ACCEPT. If rejected,
  add a failing regression test first, repair the Task 1 range, amend the
  commit, and obtain a new ACCEPT.

### Task 2: Validate column-bound command literals with a private descriptor

**Files:**
- Modify: `crates/schema/src/commands.rs`, `crates/schema/tests/commands.rs`,
  `crates/server/src/state.rs`, `crates/server/src/migrate.rs`,
  `crates/server/tests/migrate.rs`
- Test: `crates/schema/tests/commands.rs`, `crates/server/tests/migrate.rs`

**Interfaces:**
- Consumes: Task 1's `ColumnInfo.pg_type`, `ColumnInfo.pg_typmod`, and
  `ColumnInfo.nullable`.
- Produces: private `CommandScalarDescriptor::from_column(&ColumnInfo) ->
  Result<CommandScalarDescriptor, PlanError>` and
  `validate_command_literal(&serde_json::Value, &CommandScalarDescriptor,
  path) -> Result<(), PlanError>`.
- Boundary: call the descriptor only from
  `validate_value_against_column` when `value` is `CommandValue::Literal`;
  leave `value_type`, `StaticType`, GraphQL schema construction, and Rules
  binding validation responsible for every other value form.

- [ ] **Step 1: Write the full command-compiler RED matrix.**

  Extend the catalog fixture helper so a test can construct columns with a
  specific `pg_type`, `pg_typmod`, and `nullable` value. Add table-driven tests
  that bind `literal` values to a concrete object column and assert a
  deployment `validation-failed` result with the command step path. Cover all
  of these exact cases:

  ```rust
  // Every width accepts both JSON numbers and strings at both inclusive
  // boundaries, then rejects one value below and above the exact range.
  ("int2", -1, json!(-32768), true), ("int2", -1, json!(32767), true),
  ("int2", -1, json!("-32768"), true), ("int2", -1, json!("32767"), true),
  ("int2", -1, json!(-32769), false), ("int2", -1, json!("32768"), false),
  ("int4", -1, json!(-2147483648_i64), true),
  ("int4", -1, json!(2147483647_i64), true),
  ("int4", -1, json!("-2147483648"), true),
  ("int4", -1, json!("2147483647"), true),
  ("int4", -1, json!(-2147483649_i64), false),
  ("int4", -1, json!("2147483648"), false),
  ("int8", -1, json!(-9223372036854775808_i64), true),
  ("int8", -1, json!(9223372036854775807_i64), true),
  ("int8", -1, json!("-9223372036854775808"), true),
  ("int8", -1, json!("9223372036854775807"), true),
  ("int8", -1, json!(9223372036854775808_u64), false),
  ("int8", -1, json!("-9223372036854775809"), false),
  ```

  Also cover: boolean JSON values and quoted-boolean rejection; finite
  `float4`/`float8`, quoted `NaN`, quoted infinity, and a `float4` overflow;
  canonical UUID and invalid UUID; real and invalid dates; local timestamp and
  offset timestamp grammar; timestamp and timestamptz modifiers `-1`, `0`,
  `3`, and `6` with an over-precise fractional value rejected; `numeric(5,2)`
  boundaries before and after rounding; unconstrained numeric; a valid
  negative-scale numeric modifier; malformed numeric precision/scale; string
  limits for `varchar(3)`, `bpchar(2)`, `name`, `text`, and `citext`; Unicode
  character counting for `varchar`; `literal: null` accepted only for a
  nullable column; object/list literal rejection; unsupported `jsonb`, enum,
  array, domain, and extension types; and a supported type with an invalid
  modifier.

- [ ] **Step 2: Run the RED descriptor matrix.**

  Run: `cargo test -p donat-schema --test commands command_literal -- --nocapture`

  Expected: FAIL because the existing `StaticType` path accepts values that
  exceed concrete database ranges, loses modifiers, and accepts unknown
  database scalar names through its string fallback.

- [ ] **Step 3: Implement the private descriptor and exact parsing rules.**

  Add a non-public descriptor in `crates/schema/src/commands.rs`; do not add a
  public export or change `StaticType`.

  ```rust
  enum CommandScalarDescriptor {
      Bool,
      SignedInteger { minimum: i128, maximum: i128 },
      Float32,
      Float64,
      Numeric { precision: Option<u16>, scale: Option<i16> },
      Uuid,
      Date,
      Timestamp { with_time_zone: bool, fractional_precision: u8 },
      Text { maximum_characters: Option<usize>, maximum_bytes: Option<usize> },
  }
  ```

  Decode `numeric` modifiers by subtracting `4`, taking the high 16 bits as
  precision, and sign-extending the low 11 bits as scale. Reject impossible
  precision/scale combinations before inspecting the literal. Parse integer
  strings with the exact integral grammar and an `i128` intermediate; parse
  floats into the target width and require `is_finite`; accept numeric strings
  and JSON-number spellings only when they match the declared decimal grammar,
  then apply PostgreSQL numeric rounding before checking the post-rounding
  precision. For `varchar` and `bpchar`, derive the character maximum from
  `atttypmod - 4`; for `name`, use the PostgreSQL 16 63-byte maximum. Reject a
  descriptor for every unrecognized type or invalid modifier.

  Branch before `value_type` only for `CommandValue::Literal` in
  `validate_value_against_column`. A valid literal still returns the existing
  `column_type(column)` for downstream static assignment; non-literal forms
  take the existing code unchanged. Reject a null literal unless
  `column.nullable` is true, and retain the descriptor's concrete SQL type for
  later typed-null lowering.

- [ ] **Step 4: Add Engine, validate, and migrate integration RED tests.**

  In the existing `crates/server/src/state.rs` test module, construct a valid
  published engine, retain its `old_compiled` snapshot, and attempt to publish
  a candidate whose Postgres catalog has an `int8` command target with literal
  `"9223372036854775808"`. Follow the existing failed-candidate test pattern:

  ```rust
  let old_compiled = engine.compiled.as_ref().expect("compiled snapshot").clone();
  let state = state(engine);
  let error = state
      .publish_candidate(Engine::compiled(invalid_metadata, catalogs, runtimes, true))
      .await
      .expect_err("out-of-range command literal rejects candidate");
  assert_eq!(error.path, "commands[0].steps[0]");
  assert!(error.message.contains("int8"));
  let current = state.engine_snapshot().await;
  assert!(Arc::ptr_eq(
      &old_compiled,
      current.compiled.as_ref().expect("unchanged compiled snapshot"),
  ));
  ```

  In `crates/server/tests/migrate.rs`, add a real-Postgres
  `check_consistency_rejects_out_of_range_int8_command_literal` test with the
  same literal and assert that the returned problem contains both
  `commands[0].steps[0]` and `int8`. Add a nullable `varchar(3)` success case
  and a `jsonb` literal rejection. The CLI-level `validate` and
  `migrate --metadata-dir` paths consume this check; assert the validation
  path reports diagnostics and creates no database object before accepting a
  command definition.

- [ ] **Step 5: Run the integration RED tests.**

  Run: `cargo test -p donat-server --test migrate command_literal -- --nocapture`

  Expected: FAIL until candidate-engine and migration validation consume the
  corrected command compiler diagnostics for concrete literal descriptors.

- [ ] **Step 6: Wire deployment paths without adding runtime behavior.**

  Ensure `crates/server/src/state.rs` builds the candidate command catalog
  using the Task 1 catalog snapshot, and ensure `migrate::check_consistency`
  returns the same compiler diagnostic. Do not query catalog metadata from a
  GraphQL request, create a migration, mutate metadata, use an admin role, or
  add a Rust response-assembly path. Keep the descriptor private and
  deploy-time only.

- [ ] **Step 7: Run the complete Task 2 GREEN suite.**

  Run: `cargo test -p donat-schema --test commands && cargo test -p donat-server --test migrate && cargo test -p donat-metadata && cargo test --workspace --no-run`

  Expected: PASS. Verify every new descriptor-family case is a deployment
  diagnostic, valid boundary literals compile, and no argument/item/prior-step
  or Rule test gained descriptor-specific behavior.

- [ ] **Step 8: Review and commit the repaired Task 2 range.**

  Run: `git diff --check && git diff -- crates/schema/src/commands.rs crates/schema/tests/commands.rs crates/server/src/state.rs crates/server/src/migrate.rs crates/server/tests/migrate.rs`

  Inspect the diff for a private descriptor, exact rejection of unknown types,
  no fallback to a static scalar, no GraphQL or Rules type-system change, no
  DDL, and no role bypass. Amend the unaccepted Task 2 range or make a clearly
  linked corrective commit, then record the relationship in the commit body:

  ```bash
  git add crates/schema/src/commands.rs crates/schema/tests/commands.rs \
    crates/server/src/state.rs crates/server/src/migrate.rs \
    crates/server/tests/migrate.rs
  git commit -m "fix(commands): validate literals against postgres columns"
  ```

- [ ] **Step 9: Dispatch and satisfy the mandatory Task 2 judge gate.**

  Dispatch the judge with the Task 1 and Task 2 commit range, this recovery
  plan, the exact command test matrix, and the migrate test command. State
  that `26f4bde` was unaccepted and request review of the corrected complete
  range, not only the latest diff. On rejection, first add the missing failing
  boundary regression, repair the range, amend or add the linked corrective
  commit, rerun Steps 7-8, and obtain ACCEPT.

### Task 3: Strict verification and whole-range recovery review

**Files:**
- Modify: only a file required by a newly observed failing regression, with its
  focused test in the same commit.
- Test: catalog introspection, command compiler, migration validation, build,
  conformance, snapshots, and repository formatting checks.

**Interfaces:**
- Consumes: the accepted Task 1 catalog field and Task 2 private descriptor.
- Produces: fresh evidence that the repaired command validation range preserves
  deployment-only behavior, explicit-role authorization, and the command
  one-statement invariant.

- [ ] **Step 1: Run strict static verification.**

  Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --exclude donat-conformance`

  Expected: PASS. If the repository has a documented pre-existing strict-clippy
  blocker, record the exact unchanged diagnostics separately and run the
  strict command for the touched catalog, schema, and server targets; do not
  hide a new warning.

- [ ] **Step 2: Rebuild and run conformance from the corrected binary.**

  Run: `cargo build -p donat-server --bin donat && cargo test -p donat-conformance --test commands && make conformance`

  Expected: PASS after the binary rebuild. Confirm no command fixture needs an
  admin header and no command validation error becomes a request-time GraphQL
  error.

- [ ] **Step 3: Review generated artifacts and operational boundaries.**

  Run: `cargo insta review && git diff --check && git log --oneline 26f4bde^..HEAD`

  Review every snapshot diff. Inspect the complete recovery range for exactly
  these facts: raw `atttypmod` originates in PostgreSQL introspection; every
  accepted literal has a concrete supported descriptor; unsupported types and
  modifiers fail at deployment; typed null respects column nullability; no
  `StaticType`, GraphQL, or Rules expansion occurred; `validate` and serving
  do not issue DDL; commands still require an explicit role; and no change
  creates more than one SQL statement per command root.

- [ ] **Step 4: Dispatch the final whole-range judge review.**

  Send the judge the full range from `26f4bde^` through the recovery commits,
  the ADR, the specification change, this plan, and the verification output.
  Require a fresh ACCEPT that explicitly treats the original Task 2 commit as
  superseded by its corrected range. If the review finds a defect, add its
  failing test, fix only the identified range, repeat all affected verification
  commands, commit, and obtain another ACCEPT before completion.
