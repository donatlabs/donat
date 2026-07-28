# Declarative Rules Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` task-by-task. Every task needs RED/GREEN evidence, one focused commit, and a judge ACCEPT before the next task.

**Goal:** Close every whole-branch review gap in the Spec 004 declarative rules profile before commands consume it.

**Architecture:** Extend the one `rules.yaml` wrapper with finite named type declarations, compile a profile-versioned canonical rule artifact, and keep one typed AST shared by the Rust evaluator and PostgreSQL lowerer. Missing static access is explicit nullable data, not an engine-specific failure. Decision outputs remain typed data until a future consumer statically maps finite values to fixed effects.

**Tech Stack:** Rust workspace, serde/serde_json, `sha2`, Postgres 16, insta, native conformance harness.

## Global Constraints

- Retain exactly one optional deploy-time `rules.yaml` wrapper; never create `decision_tables.yaml` or runtime metadata mutation.
- No admin role, permission bypass, generic process management endpoint, GraphQL/REST/MCP rules endpoint, ambient context, network, database, clock, filesystem, reflection, UDF, or dynamic capability selection.
- Keep `donat-rules` independent from axum, server runtime, and database clients; source ports remain clean-room behavior-only unless the provenance register is updated first.
- Rule source is limited to 4096 UTF-8 bytes, 64 syntactic levels, and 256 literal-list items; expression diagnostics carry `metadata_path`, expression owner/name, and a half-open UTF-8 byte span.
- Rule artifact profile version is `1`; source, canonical AST, decision revision, and redacted typed-input digests use lowercase SHA-256 hex. The canonical bytes use the exact Spec 004 v1 header, tags, length prefixes, map ordering, and scalar normal forms; never hash serde output.
- A static object/list access is nullable in both evaluators. `RuleType::nullable` must normalize an already nullable inner type. `&&`/`||` lower through `CASE`, never raw SQL boolean operators; live SQL tests use typed row-derived divisors, while syntactic literal zero divisors are deploy-time errors.
- SQL identifiers and literals use the existing narrow sqlgen helpers. Every binary SQL expression is parenthesized and no original source/rule name becomes SQL text.
- Existing no-admin, one-statement, fixture exactness, and TDD/conformance rules from `AGENTS.md` apply unchanged.

---

### Task 1: Declare finite object and enum types in the single wrapper

**Files:**
- Modify: `crates/metadata/src/types.rs`, `crates/metadata/tests/types_serde.rs`, `crates/metadata/tests/load_fixture.rs`, `crates/metadata/tests/loader_includes.rs`, `crates/rules/src/lib.rs`, `crates/rules/src/lexer.rs`, `crates/rules/src/parser.rs`, `crates/rules/src/types.rs`, `crates/rules/tests/parser.rs`, `crates/rules/tests/eval.rs`, `crates/server/src/state.rs`, `crates/conformance/src/lib.rs`, `crates/conformance/tests/rules.rs`
- Add: `crates/metadata/tests/fixtures/rules-types-only/version.yaml`, `crates/metadata/tests/fixtures/rules-types-only/databases/databases.yaml`, `crates/metadata/tests/fixtures/rules-types-only/rules.yaml`, `crates/conformance/fixtures/rules/object_enum_valid.yaml`, `crates/conformance/fixtures/rules/type_cycle.yaml`

**Interfaces:**

```rust
pub struct RuleTypeDeclaration {
    pub name: String,
    pub object: Option<BTreeMap<String, String>>,
    pub enum_values: Option<Vec<String>>, // serde rename = "enum"
}

pub struct RulesMetadata {
    pub types: Vec<RuleTypeDeclaration>,
    pub rules: Vec<RuleDefinition>,
    pub decision_tables: Vec<DecisionTableDefinition>,
}

pub enum RuleType {
    // scalar and structural variants omitted
    Object {
        name: String,
        fields: BTreeMap<String, RuleType>,
    },
    Enum { name: String, symbols: Vec<String> },
}

pub enum ExprKind {
    EnumSymbol { enum_name: String, symbol: String },
    // existing expression variants omitted
}

fn resolve_declared_rule_types(
    declarations: &[RuleTypeDeclaration],
    path: &str,
) -> Result<BTreeMap<String, RuleType>, PlanError>;

fn parse_rule_type_ref(
    source: &str,
    declared: &BTreeMap<String, RuleType>,
    path: &str,
) -> Result<RuleType, PlanError>;
```

- [ ] Add failing serde/loader and `donat validate` fixtures for `OrderStatus`, `CreateOrderLine`, nested `[CreateOrderLine!]!`, a duplicate type, primitive collision, unknown reference, cycle, and a `TypeName::symbol` expression. Add the types-only directory fixture named above and prove loading it produces one type declaration with no rules/tables.
- [ ] In `types_serde.rs`, add a types-only wrapper regression: `RulesMetadata::is_empty()` is false, metadata serialization retains `rules.types`, and serialization omits empty `rules`/`decision_tables` rather than dropping the wrapper. In `loader_includes.rs`, retain the absent-file default regression as empty.
- [ ] Add failing parser/eval tests that preserve `enum_name` in `TypeName::symbol`, accept `OrderStatus::draft`, reject an undeclared symbol, and reject `OrderStatus::draft == OtherStatus::draft` even when both symbol lists include `draft`.
- [ ] Run `cargo test -p donat-metadata --test types_serde --test load_fixture --test loader_includes`, `cargo test -p donat-rules --test parser --test eval`, and `cargo build -p donat-server --bin donat && PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-conformance --test rules`; expect the new valid object/enum fixture and types-only wrapper to fail before implementation.
- [ ] Add `RulesMetadata.types`, make `is_empty` inspect all three collections, require exactly one declaration body, resolve declarations with cycle detection before rules/tables, and adapt named types through `compile_rule_catalog` without a metadata-to-rules crate dependency.
- [ ] Extend the restricted lexer/parser with only `TypeName::symbol`, retain enum name and symbol in the AST and `RuleType::Enum { name, symbols }`, type-check equality nominally, and reject unknown enum values as deploy-time validation errors.
- [ ] Run `cargo test -p donat-metadata --test types_serde --test load_fixture --test loader_includes`, `cargo test -p donat-rules --test parser --test eval`, `cargo test -p donat-server`, rebuild the engine, then run `PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-conformance --test rules`. Commit `feat(rules): resolve declared object and enum types` and obtain judge ACCEPT.

### Task 2: Preserve semantic diagnostic spans through deploy validation

**Files:**
- Modify: `crates/rules/src/types.rs`, `crates/rules/src/eval.rs`, `crates/rules/src/lib.rs`, `crates/rules/tests/eval.rs`, `crates/server/src/state.rs`, `crates/conformance/tests/rules.rs`, `crates/conformance/fixtures/rules/invalid_type_location.yaml`
- Add: `crates/conformance/fixtures/rules/invalid_decision_condition_location.yaml`

**Interfaces:**

```rust
pub struct ExpressionContext {
    pub metadata_path: String,
    pub expression_owner: ExpressionOwner,
}

pub struct RuleDiagnostic {
    pub context: ExpressionContext,
    pub span: Span,
    pub message: String,
}

pub enum ExpressionOwner {
    Rule { name: String },
    DecisionCondition {
        table_name: String,
        row_id: String,
        input_name: String,
    },
}

impl RuleError {
    pub fn diagnostic(&self) -> Option<&RuleDiagnostic>;
}

fn compile_expression_in_context(
    context: &ExpressionContext,
    source: &str,
    bindings: &BTreeMap<String, RuleType>,
) -> Result<CheckedExpr, RuleError>;
```

- [ ] Add failing tests for undeclared name, inaccessible field, nullable misuse, incompatible branches, and non-enum `Type::symbol`; each must carry exact source byte start/end, owner/name, and path `rules.yaml.rules[N].expression`.
- [ ] Add a failing decision-table semantic-condition test for `rows[M].when.<input>` that asserts the path `rules.yaml.decision_tables[N].rows[M].when.<input>`, table/row/input owner fields, and exact condition-expression span. It must not be represented as a rule-only diagnostic.
- [ ] Run `cargo test -p donat-rules --test eval && cargo build -p donat-server --bin donat && PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-conformance --test rules`; expect semantic errors to lack one or more required diagnostic fields before implementation.
- [ ] Have the metadata adapter create one `ExpressionContext` per source site, then pass it to the rules compiler for both a rule body and each decision-row condition. Thread each checked `Expr.span` into type-validation `RuleError` variants without putting raw expression text or bindings in the error. The compiler returns `RuleDiagnostic { context, span, message }`; render its stable location in `compile_rule_catalog`'s `PlanError`.
- [ ] Run `cargo test -p donat-rules --test eval`, `cargo test -p donat-server`, `cargo test -p donat-metadata`, rebuild, and run `PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-conformance --test rules`. Commit `feat(rules): report semantic diagnostic spans` and obtain judge ACCEPT.

### Task 3: Make static object/list access nullable and SQL short-circuit exact

**Files:**
- Modify: `crates/rules/src/eval.rs`, `crates/rules/src/postgres.rs`, `crates/rules/src/types.rs`, `crates/rules/tests/eval.rs`, `crates/rules/tests/postgres.rs`

**Interfaces:**

```rust
fn access_result_type(member_or_item: &RuleType) -> RuleType {
    RuleType::nullable(member_or_item.clone())
}

impl RuleType {
    pub fn nullable(inner: RuleType) -> RuleType {
        match inner {
            RuleType::Nullable(_) => inner,
            other => RuleType::Nullable(Box::new(other)),
        }
    }
}

// SQL shape for bool operands:
// and: CASE WHEN (<left>) THEN (<right>) ELSE FALSE END
// or:  CASE WHEN (<left>) THEN TRUE ELSE (<right>) END
```

- [ ] Add failing evaluator tests for `is_null(lines[3])` on a one-item list; `is_null(customer.nickname)` where `nickname: string` is absent from a declared object; and a nullable list item (`[string]`) so nested nullable wrappers normalize. Assert `RuleType::nullable(RuleType::nullable(T)) == RuleType::nullable(T)` and that an access used in `is_null(customer.nickname) ? "missing" : customer.nickname` is still rejected: there is no flow-sensitive non-null refinement.
- [ ] Add live Postgres differential tests with bindings `numerator: int!` and `denominator: int!` lowered as typed columns of `FROM (VALUES (1::numeric, 0::numeric)) AS input(numerator, denominator)`: `false && (numerator / denominator > 0)` returns false and `true || (numerator / denominator > 0)` returns true without evaluating the divisor. Separately add a deploy-validation test rejecting a syntactic literal-zero divisor such as `false && (1 / 0 > 0)`; never claim `CASE` protects that planner-visible constant.
- [ ] Run `PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-rules --test postgres` and `cargo test -p donat-rules --test eval`; expect raw `AND`/`OR`, literal-divisor acceptance, or Rust invalid access behavior to disagree before implementation.
- [ ] Make static access nullable in the checker and evaluator, decode absent nullable object keys as null, return null for out-of-range literal indexes, normalize access-result wrappers, and reject every non-null-only operation on those access results.
- [ ] Reject syntactic literal divisors that normalize to zero at deploy time. Lower `&&`/`||` with parenthesized `CASE`; retain safe quoted JSON access and ensure it returns the same null representation. Add reviewed SQL snapshots for both operators and nullable access, and assert neither snapshot contains raw ` AND ` or ` OR ` for profile boolean operators.
- [ ] Run `cargo insta pending-snapshots && cargo insta review`, `cargo test -p donat-rules`, `cargo clippy -p donat-rules --all-targets -- -D warnings`, rebuild, and `PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-conformance --test rules`, then full conformance. Commit `fix(rules): align nullable access and short circuit SQL` and obtain judge ACCEPT.

### Task 4: Persist canonical profile artifacts and immutable trace revisions

**Files:**
- Modify: root `Cargo.toml`, `Cargo.lock`, `crates/rules/Cargo.toml`, `crates/rules/src/lib.rs`, `crates/rules/src/types.rs`, `crates/rules/src/eval.rs`, `crates/rules/tests/eval.rs`, `crates/server/src/state.rs`, `crates/conformance/tests/rules.rs`
- Add: `crates/rules/tests/canonical.rs`

**Interfaces:**

```rust
pub const PROFILE_VERSION: u16 = 1;
pub const MAGIC: [u8; 22] = *b"DONAT-RULES-CANONICAL\0";

pub struct RuleArtifact {
    pub profile_version: u16,
    pub original_source: String,
    pub canonical_ast_sha256: String,
    pub source_sha256: String,
}

pub struct DefinitionRevision(pub String);

pub enum CanonicalRoot {
    TypedRuleAst = 0x01,
    DecisionDefinition = 0x02,
    DecodedTypedInput = 0x03,
}

pub fn canonical_bytes(root: CanonicalRoot, value: &CanonicalValue) -> Vec<u8>;
```

**Canonical v1 codec (normative implementation contract):** `MAGIC` is exactly
the 22 bytes `44 4f 4e 41 54 2d 52 55 4c 45 53 2d 43 41 4e 4f 4e 49 43 41 4c 00`,
rendered as `b"DONAT-RULES-CANONICAL\0"`. The root byte stream is exactly
`MAGIC || U16BE(1) || RECORD`, where `U16BE(1)` is `00 01` and `RECORD` begins
with `01` typed rule, `02` decision, or `03` decoded input. `U32` is unsigned
big-endian 32-bit; `S` is `U32(utf8_byte_length) || UTF-8 bytes`; `L<X>` is
`U32(count)` followed by supplied-order items; `M<X>` is `U32(count)` then
`S(key) || X(value)` entries sorted by unsigned lexicographic UTF-8 key bytes;
and `B` is one byte (`00` false, `01` true). Every count/length is `U32`; only
`M` sorts.

| Tag | `T` payload |
| --- | --- |
| `10` bool, `11` string, `12` int, `13` decimal, `14` uuid, `15` date, `16` timestamp | none |
| `17` enum | `S(enum_name) || L<S>(symbols)` in declaration order |
| `18` list | `T(item)` |
| `19` object | `S(object_declaration_name) || M<T>(field_name -> field type)` |
| `1a` nullable | `T(inner)` after normalizing nested nullable wrappers |

Every AST node is `E = tag || payload || T(resolved_result)`: `20` literal,
`21` name/`S(name)`, `22` enum symbol/`S(enum_name) || S(symbol)`, `23` list
literal/`L<E>`, `24` member/`E(receiver) || S(field)`, `25` literal index/
`E(receiver) || U32(index)`, `26` call/function tag plus ordered `L<E>`, `27`
unary/operator tag plus operand, `28` binary/operator tag plus left/right, and
`29` conditional/condition-true-false. Literal tags are `00` null/no payload,
`01` bool/`B`, `02` int/`S(minimal_signed_base10)`, `03` decimal/`D`, and `04`
string/`S`. Function tags are `00` size, `01` is_null, `02` startsWith, `03`
endsWith; unary tags are `00` not and `01` negate; binary tags are `00` or,
`01` and, `02` equal, `03` not_equal, `04` less_than, `05`
less_than_or_equal, `06` greater_than, `07` greater_than_or_equal, `08` add,
`09` subtract, `0a` multiply, `0b` divide. The resolved result `T` is emitted
for every node, while spans/descriptions/YAML map order are never emitted.

`Q = T || V` encodes a typed value. `V` tags are `30` null/no payload, `31`
bool/`B`, `32` string/`S`, `33` int/`S(minimal_signed_base10)`, `34` decimal/
`D`, `35` uuid/`S(lowercase_hyphenated_uuid)`, `36` date/
`S(ISO-8601_calendar_date)`, `37` timestamp/`S(UTC_RFC3339_timestamp)`, `38`
enum/`S(enum_name) || S(symbol)`, `39` list/ordered `L<Q>`, and `3a`
object/lexical `M<Q>`. Null requires nullable `T`. `D` is sign byte (`00`
non-negative, `01` negative), `S(significant_digits)`, `U32(scale)`, with
fractional trailing zeroes removed and zero as `00 || S("0") || U32(0)`.

Declarations are `40 || S(enum_name) || L<S>(symbols)` and
`41 || S(object_name) || M<T>(fields)`. After `MAGIC || U16BE(1) || 01`, encode
`S(rule_name) || M<T>(bindings) || T(result) || E(expression)`. After
`MAGIC || U16BE(1) || 02`, encode
`S(table_name) || L<declaration>(types) || M<T>(inputs) ||
M<T>(outputs) || policy` (`00` first, `01` unique) `|| L<row>(rows) ||
L<case>(tests)`; row is `S(id) || M<E>(conditions) || M<Q>(output)`, case is
`S(name) || M<Q>(input) || M<Q>(expected_output) || S(matched_row_id)`. After
`MAGIC || U16BE(1) || 03`, encode `M<Q>(bindings)`. Declaration, row, and test
lists retain
declaration order; every keyed schema/field/condition/output uses `M`.

`source_sha256` is SHA-256 of raw UTF-8 source only; AST, decision, and input
hashes are SHA-256 of their complete framed records. Never use serde output.

**Required fixed vectors in `crates/rules/tests/canonical.rs`:**

| Test | Canonical record | Exact lower-case hex | SHA-256 |
| --- | --- | --- | --- |
| `canonical_literal_true_rule_vector` | rule `literal`, no bindings, bool result, `true` literal | `444f4e41542d52554c45532d43414e4f4e4943414c00000101000000076c69746572616c000000001020010110` | `acfb7be4c3c31baa5f72433eabedc47d721ad2f72d7d70aa73a7f368b0445d34` |
| `canonical_nullable_index_rule_vector` | rule `nullable_index`, non-null `items: list<string>` binding, bool result, `is_null(items[0])`; the index expression has result type `nullable<string>` and the enclosing function has result type `bool` | `444f4e41542d52554c45532d43414e4f4e4943414c000001010000000e6e756c6c61626c655f696e64657800000001000000056974656d731811102601000000012521000000056974656d731811000000001a1110` | `113eae1a24d9dcc45ec7e3f26b07cab1ac0b5e09f44725c980ccf54a2571fe75` |
| `canonical_size_function_rule_vector` | rule `size_items`, `items: [string]`, int result, `size(items)` | `444f4e41542d52554c45532d43414e4f4e4943414c000001010000000a73697a655f6974656d7300000001000000056974656d7318111226000000000121000000056974656d73181112` | `c9419c2368337338f014257d0ff3d286240a1373e073c46e3d6d096aca9f01cd` |
| `canonical_decision_declaration_vector` | first table `approval`, `amount: int` -> `route: string`, one `default` true row outputting `manual`, no types/tests | `444f4e41542d52554c45532d43414e4f4e4943414c0000010200000008617070726f76616c000000000000000100000006616d6f756e74120000000100000005726f7574651100000000010000000764656661756c740000000100000006616d6f756e74200101100000000100000005726f7574651132000000066d616e75616c00000000` | `4b43abdf0d8c689127993ba14614011cb5399e895a43f990c96166e8afa4ee32` |

- [ ] Add failing tests with the four exact vector names and constants above, plus a decoded-input vector; assert map-order invariance for bindings, object fields, table inputs/outputs, row `when`, and row output maps.
- [ ] Add failing semantic-change tests showing the affected decision revision changes for a type declaration, declaration-order row swap, condition change, and output change; show decoded-input digest changes for a typed value change. Include an enum-name-only difference with overlapping symbols. Descriptions, expression spans, and map insertion order must not change canonical bytes.
- [ ] Add failing tests proving every compiled rule retains profile `1`, original source, deterministic lowercase SHA-256 source/canonical hashes, and a same-name changed decision table yields a different revision and trace. Add a trace test that a deterministic canonical decoded input has a 64-character SHA-256 digest and no raw input.
- [ ] Run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-rules --test canonical canonical_` and `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-rules --test eval`; expect missing artifacts/name-derived revision/non-canonical digest behavior before implementation.
- [ ] Add only `sha2`; implement the exact codec above without serializing a Rust map or `serde_json::Value`. Hash raw source separately and make `DecisionTrace` consume the derived revision and typed-input digest.
- [ ] Surface profile/hash/revision in deploy-time diagnostics without adding an endpoint or exposing source in request errors. Run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-rules --test canonical --test eval`, `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-server`, rebuild with `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo build -p donat-server --bin donat`, then run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres DONAT_BIN=/home/dev/.cache/donat-runtime-target/debug/donat cargo test -p donat-conformance --test rules`. Commit `feat(rules): retain canonical profile artifacts` and obtain judge ACCEPT.

### Task 5: Expose typed value evaluation and lowering

**Files:**
- Modify: `crates/rules/src/lib.rs`, `crates/rules/src/eval.rs`, `crates/rules/src/postgres.rs`, `crates/rules/src/types.rs`, `crates/rules/tests/eval.rs`, `crates/rules/tests/postgres.rs`

**Interfaces:**

```rust
pub struct EvaluatedRuleValue {
    pub type_: RuleType,
    pub value: serde_json::Value,
}

pub struct LoweredRuleValue {
    pub sql: String,
    pub type_: RuleType,
}

pub fn evaluate_value(
    rule: &CompiledRule,
    bindings: &RuleBindings,
) -> Result<EvaluatedRuleValue, RuleError>;

pub fn lower_postgres_value(
    rule: &CompiledRule,
    bindings: &SqlBindings,
) -> Result<LoweredRuleValue, RuleError>;
```

- [ ] Add failing `evaluate_value_returns_typed_values` in `crates/rules/tests/eval.rs` for string, decimal, enum, nullable object/list-derived values; add `evaluate_bool_rejects_non_bool_rule` there. Add `lower_postgres_value_matches_live_postgres` in `crates/rules/tests/postgres.rs` for the same values and `lower_postgres_rejects_non_bool_rule` there.
- [ ] Run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-rules --test eval evaluate_value_returns_typed_values`, `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-rules --test eval evaluate_bool_rejects_non_bool_rule`, `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-rules --test postgres lower_postgres_value_matches_live_postgres`, and `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-rules --test postgres lower_postgres_rejects_non_bool_rule`; expect compilation without a typed value API before implementation.
- [ ] Factor the closed evaluator/lowerer to return a typed value first; boolean wrappers require exactly `bool` and retain existing guard behavior/errors.
- [ ] Extend the bounded differential generator to compare canonical JSON value plus `RuleType` for bool, string, int, decimal, enum, nullable access, and conditional expressions.
- [ ] Run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-rules --test eval`, `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-rules --test postgres`, `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo insta pending-snapshots`, then `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo insta review`. Run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo clippy -p donat-rules --test eval --test postgres -- -D warnings`; rebuild with `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo build -p donat-server --bin donat`; then run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres DONAT_BIN=/home/dev/.cache/donat-runtime-target/debug/donat cargo test -p donat-conformance --test rules`. Commit `feat(rules): evaluate and lower typed values` and obtain judge ACCEPT.

### Task 6: Replace decision field-name policy with typed static-consumer safety

**Files:**
- Modify: `crates/rules/src/types.rs`, `crates/rules/src/eval.rs`, `crates/rules/tests/eval.rs`, `specs/003-declarative-domain-commands.md`, `specs/005-durable-processes.md`, `docs/superpowers/plans/2026-07-28-declarative-commands.md`, `docs/superpowers/plans/2026-07-28-declarative-processes.md`

**Interfaces:**

```rust
pub struct DecisionOutputField {
    pub name: String,
    pub type_: RuleType,
}

impl CompiledDecisionTable {
    pub fn output_field(&self, name: &str) -> Option<&DecisionOutputField>;
}
```

- [ ] Add failing `decision_output_names_are_typed_data` and `rules_expose_no_dynamic_capability_selection_api` tests in `crates/rules/tests/eval.rs`. The first compiles `role_label`, `permission_count`, and `connector_reference`; the second proves the public Rules API has only typed output lookup/evaluation and no role, permission, command, connector, or connector-operation selector.
- [ ] Run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-rules --test eval decision_output_names_are_typed_data` and `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-rules --test eval rules_expose_no_dynamic_capability_selection_api`; expect the first to fail on substring-based `ForbiddenDecisionOutput` rejection before implementation.
- [ ] Remove name heuristics and retain only typed output schema. Document that Commands/Processes accept a decision value only into an exactly typed data destination or map a declared enum at deploy time to fixed action/state targets; they never bind a generic output to role/permission/command/connector identifiers.
- [ ] Prove heuristic removal with `! rg -n -e 'ForbiddenDecisionOutput' -e 'contains\(.*role' -e 'contains\(.*permission' -e 'contains\(.*connector' crates/rules/src`. Prove the static-consumer wording with `rg -n -F 'exactly typed data destination' specs/003-declarative-domain-commands.md specs/005-durable-processes.md docs/superpowers/plans/2026-07-28-declarative-commands.md docs/superpowers/plans/2026-07-28-declarative-processes.md` and `rg -n -F 'never bind a generic output to role/permission/command/connector identifiers' specs/003-declarative-domain-commands.md specs/005-durable-processes.md docs/superpowers/plans/2026-07-28-declarative-commands.md docs/superpowers/plans/2026-07-28-declarative-processes.md`.
- [ ] Run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo test -p donat-rules --test eval`, `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo insta pending-snapshots`, then `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo insta review`. Run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo clippy -p donat-rules --test eval -- -D warnings`; rebuild with `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target cargo build -p donat-server --bin donat`; then run `CARGO_TARGET_DIR=/home/dev/.cache/donat-runtime-target PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres DONAT_BIN=/home/dev/.cache/donat-runtime-target/debug/donat cargo test -p donat-conformance --test rules`. Commit `fix(rules): make decision outputs typed data` and obtain judge ACCEPT.

### Task 7: Prove the repaired contract and hand off to Commands

**Files:**
- Modify only for defects proven by this task in files above.

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy -p donat-rules -p donat-metadata -p donat-server --all-targets -- -D warnings`; if an unrelated baseline diagnostic remains, capture its path/commit and do not suppress it.
- [ ] Run `PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres cargo test -p donat-rules`, `cargo test -p donat-metadata`, and `cargo test -p donat-server`.
- [ ] Rebuild `donat`, run `PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres DONAT_BIN=/home/dev/.cache/donat-runtime-target/debug/donat cargo test -p donat-conformance --test rules`, then full conformance against the isolated PostGIS.
- [ ] Review all snapshots with `cargo insta pending-snapshots` and `cargo insta review` when non-empty.
- [ ] Request a whole-branch reviewer for the remediation range. Do not begin the Commands plan until it returns ACCEPT.
