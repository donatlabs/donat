# Connector Boundary Lexer Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the sole connector boundary checker recognize every
compiler-valid Rust Unicode/raw alias and structurally enforce nested/grouped
`use` trees without changing the connector ABI or runtime.

**Architecture:** First replace Python Unicode classification with
delimiter-based identifier candidates and explicit raw/grammar predicates.
Then flatten the supported Rust `use` grammar into full-path leaves so alias
and re-export policy is structural rather than comma-sensitive. The existing
checker, diagnostics, allowlists, CI entry point, ABI, and runtime remain
single and unchanged in authority.

**Tech Stack:** Python 3.12 standard library, Rust 2024 lexical grammar,
Rust 1.97.1 verification probes, existing Rust workspace and native Postgres
conformance harness.

## Global Constraints

- Baseline is `d678f85`, which contains the approved design
  `docs/superpowers/specs/2026-07-29-connector-boundary-lexer-remediation-design.md`
  and ADR 011.
- Modify the existing
  `scripts/check_connector_processor_boundary.py`; do not create a second
  checker, wrapper checker, parser binary, Python package, Cargo dependency,
  build script, generated Unicode table, or `rustc` runtime dependency.
- The checker must not call `str.isidentifier`, `str.isalpha`,
  `str.isspace`, `unicodedata.category`, or `unicodedata.normalize`.
- Rust whitespace is exactly U+0009 through U+000D, U+0020, U+0085, U+200E,
  U+200F, U+2028, and U+2029. A leading U+FEFF is ignored.
- Every ASCII scalar outside `[A-Za-z0-9_]` is punctuation/delimiter; every
  non-ASCII scalar outside the fixed whitespace set is part of an identifier
  candidate.
- `RustToken` preserves `raw`; grammar matching requires `raw == false`;
  protected-name matching ignores raw state.
- The import parser handles visibility, optional leading `::`, simple paths,
  aliases, globs, arbitrary nested groups, trailing commas, and `self`
  prefix inheritance.
- Existing rule names, diagnostic messages, diagnostic precedence,
  production roots, test roots, private-test policy, lint policy, allocation
  leak policy, and workspace scan order remain stable.
- No Rust ABI source, manifest, lockfile, catalog, processor, server,
  provider, credential, transport, metadata, process, database, GraphQL,
  REST, MCP, or native conformance fixture changes.
- There is no admin role, permission bypass, runtime metadata mutation,
  public connector execution route, logical/workflow node, UI, Node.js,
  JavaScript, WASM, dynamic plugin, or donor runtime.
- Use ordinary independent spec/code reviewers. Do not invoke the Judge
  agent; the user explicitly removed that per-commit workflow.
- Invoke Cargo as `/home/dev/.cargo/bin/cargo` and use a RAM-backed target:

```bash
export CARGO_TARGET_DIR=/dev/shm/donat-connector-boundary-lexer
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export DONAT_BIN="$CARGO_TARGET_DIR/debug/donat"
: "${PG_URL:?the controller must supply the isolated Postgres URL}"
```

## Authoritative Inputs

- `AGENTS.md`
- `knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md`
- `knowledgebase/declarative-saas/decisions/011-version-independent-rust-boundary-lexer.md`
- `specs/007-community-connector-factory.md`
- `docs/superpowers/specs/2026-07-29-connector-abi-remediation-design.md`
- `docs/superpowers/specs/2026-07-29-connector-boundary-lexer-remediation-design.md`
- `docs/superpowers/plans/2026-07-29-community-connector-factory.md`
- `.superpowers/sdd/2026-07-29-connector-abi-remediation/final-fix-rereview.md`
- `scripts/check_connector_processor_boundary.py`

## File and Responsibility Map

```text
scripts/check_connector_processor_boundary.py
    sole tokenizer, use-tree policy parser, deterministic fixtures,
    workspace scanner, and stable diagnostics

.superpowers/sdd/2026-07-29-connector-boundary-lexer-remediation/
    task briefs, RED/GREEN evidence, reviewer reports, and progress ledger

.superpowers/sdd/2026-07-29-connector-abi-remediation/progress.md
    records that the old final-review blocker was resolved by this separate
    approved follow-up rather than a forbidden second old-plan fix wave
```

The implementation keeps the established single-file checker layout. Splitting
it in this bounded remediation would create a second policy-loading path and
is not required to close the defects.

---

### Task 1: Make tokenization Unicode-version independent and raw-aware

**Files:**

- Modify: `scripts/check_connector_processor_boundary.py:47-236`
- Modify: `scripts/check_connector_processor_boundary.py:239-455`
- Modify: `scripts/check_connector_processor_boundary.py:821-1230`
- Test: embedded deterministic fixtures in
  `scripts/check_connector_processor_boundary.py:1248-2227`

**Interfaces:**

- Consumes: `blank_rust_noncode(source: str) -> str`.
- Produces:

```python
RUST_PATTERN_WHITESPACE: frozenset[str]
UNICODE_VERSION_GAP_IDENTIFIER: str

@dataclass(frozen=True)
class RustToken:
    value: str
    offset: int
    identifier: bool
    raw: bool = False

def rust_identifier_atom_character(character: str) -> bool: ...
def rust_name(token: RustToken, name: str) -> bool: ...
def rust_keyword(token: RustToken, keyword: str) -> bool: ...
def rust_tokens(source: str) -> list[RustToken]: ...
```

- `rust_name` is used for protected symbols and members.
- `rust_keyword` is used for Rust grammar.
- Task 2 consumes this exact token/raw contract.

- [ ] **Step 1: Add the deterministic lexer RED/GREEN fixtures**

Define a readable version-gap constant without asking Python's Unicode
database:

```python
UNICODE_VERSION_GAP_IDENTIFIER = "\U00016100"
RUST_PATTERN_WHITESPACE_CASES = (
    ("next_line", "\u0085"),
    ("left_to_right_mark", "\u200e"),
    ("right_to_left_mark", "\u200f"),
    ("line_separator", "\u2028"),
    ("paragraph_separator", "\u2029"),
)
```

Add one forbidden simple alias fixture for each protected family:

```python
Fixture(
    "crates/server/src/connectors/future_unicode_static_error.rs",
    f"use donat_connector_abi::StaticErrorCode as "
    f"{UNICODE_VERSION_GAP_IDENTIFIER};",
    "static-literal-alias: StaticErrorCode::literal cannot be reached "
    "through an alias outside STATIC_LITERAL_ROOTS",
)
Fixture(
    "crates/server/src/connectors/future_unicode_static_message.rs",
    f"use donat_connector_abi::StaticSafeMessage as "
    f"{UNICODE_VERSION_GAP_IDENTIFIER};",
    "static-literal-alias: StaticSafeMessage::literal cannot be reached "
    "through an alias outside STATIC_LITERAL_ROOTS",
)
Fixture(
    "crates/server/src/connectors/future_unicode_host_namespace.rs",
    f"use donat_connector_abi::host_construction as "
    f"{UNICODE_VERSION_GAP_IDENTIFIER};",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
Fixture(
    "crates/connector-catalog/src/future_unicode_catalog_namespace.rs",
    f"use donat_connector_abi::catalog_construction as "
    f"{UNICODE_VERSION_GAP_IDENTIFIER};",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
Fixture(
    "crates/connector-processors/src/future_unicode_connector_io.rs",
    f"use donat_connector_abi::ConnectorIo as "
    f"{UNICODE_VERSION_GAP_IDENTIFIER};",
    "host-trait-alias: ConnectorIo cannot be aliased outside approved "
    "host implementation roots",
)
Fixture(
    "crates/connector-processors/src/future_unicode_processor_control.rs",
    f"use donat_connector_abi::ProcessorControl as "
    f"{UNICODE_VERSION_GAP_IDENTIFIER};",
    "host-trait-alias: ProcessorControl cannot be aliased outside approved "
    "host implementation roots",
)
```

Add these legal raw-keyword controls:

```python
Fixture(
    "crates/server/src/connectors/raw_use_field_decoy.rs",
    "struct Decoy { pub r#use: donat_connector_abi::StaticErrorCode }",
    None,
)
Fixture(
    "crates/server/src/connectors/raw_type_binding_decoy.rs",
    "fn decoy() { let r#type = "
    "None::<donat_connector_abi::StaticErrorCode>; }",
    None,
)
Fixture(
    "crates/server/src/connectors/raw_use_alias.rs",
    "use donat_connector_abi::StaticErrorCode as r#use;",
    "static-literal-alias: StaticErrorCode::literal cannot be reached "
    "through an alias outside STATIC_LITERAL_ROOTS",
)
Fixture(
    "crates/server/src/connectors/raw_protected_literal.rs",
    'const CODE: r#StaticErrorCode = '
    'r#StaticErrorCode::r#literal("connector_failed");',
    "static-literal-producer: static failure literals are restricted to "
    "approved roots",
)
```

For every `RUST_PATTERN_WHITESPACE_CASES` row, generate a fixture shaped as:

```python
separator = value
source = (
    f"use{separator}donat_connector_abi::StaticErrorCode"
    f"{separator}as{separator}{UNICODE_VERSION_GAP_IDENTIFIER};"
)
```

Each expects the existing `static-literal-alias` diagnostic. Add one leading
U+FEFF control, one invalid-atom mutation, and one non-code control exactly as:

```python
Fixture(
    "crates/server/src/connectors/leading_bom_alias.rs",
    "\ufeffuse donat_connector_abi::StaticErrorCode as Alias;",
    "static-literal-alias: StaticErrorCode::literal cannot be reached "
    "through an alias outside STATIC_LITERAL_ROOTS",
)
Fixture(
    "crates/server/src/connectors/invalid_unicode_atom_alias.rs",
    "use donat_connector_abi::StaticErrorCode as 💥;",
    "static-literal-alias: StaticErrorCode::literal cannot be reached "
    "through an alias outside STATIC_LITERAL_ROOTS",
)
Fixture(
    "crates/server/src/connectors/unicode_noncode_decoys.rs",
    "// host_construction StaticErrorCode ConnectorIo \U00016100\n"
    "/* outer catalog_construction /* nested StaticSafeMessage */ "
    "ProcessorControl */\n"
    'const NORMAL: &str = "host_construction StaticErrorCode";\n'
    'const RAW: &str = r###"catalog_construction ConnectorIo"###;\n'
    "const CHARACTER: char = '𖄀';\n"
    "fn lifetimes<'host_construction>(value: "
    "&'host_construction str) -> &'host_construction str { value }",
    None,
)
```

The emoji fixture documents conservative rejection of invalid Rust; Task 3
separately proves that `rustc` rejects it.

- [ ] **Step 2: Run the checker self-test and capture RED**

Run:

```bash
python3 scripts/check_connector_processor_boundary.py --self-test
```

Expected: non-zero. The six U+16100 aliases produce “expected … got []”;
`raw_use_field_decoy.rs` produces the old
`static-literal-reexport`; `raw_type_binding_decoy.rs` produces the old
`static-literal-type-alias`. Record the exact output in the Task 1 SDD report.
Do not alter an expected diagnostic to make RED green.

- [ ] **Step 3: Replace Python Unicode classification with fixed separators**

Delete `rust_identifier_start` and `rust_identifier_continue`. Add:

```python
RUST_PATTERN_WHITESPACE = frozenset(
    "\u0009\u000a\u000b\u000c\u000d\u0020"
    "\u0085\u200e\u200f\u2028\u2029"
)


def rust_identifier_atom_character(character: str) -> bool:
    if character in RUST_PATTERN_WHITESPACE:
        return False
    if ord(character) >= 0x80:
        return True
    return (
        "A" <= character <= "Z"
        or "a" <= character <= "z"
        or "0" <= character <= "9"
        or character == "_"
    )


def rust_name(token: RustToken, name: str) -> bool:
    return token.identifier and token.value == name


def rust_keyword(token: RustToken, keyword: str) -> bool:
    return rust_name(token, keyword) and not token.raw
```

Make `rust_tokens`:

1. skip a U+FEFF only at offset zero;
2. skip only `RUST_PATTERN_WHITESPACE`;
3. coalesce `::`;
4. recognize `r#` plus a following identifier candidate before ordinary
   punctuation and emit `raw=True`;
5. consume every ordinary maximal identifier-candidate run with `raw=False`;
6. emit all remaining scalars as non-identifier punctuation tokens.

Use the dataclass default so existing punctuation construction remains
explicitly non-raw. Do not use a Python Unicode classification call.

- [ ] **Step 4: Make every grammar consumer raw-aware**

Replace bare grammar comparisons with `rust_keyword` in:

- `rust_use_statements` (`use`, `pub`);
- `rust_use_aliases` (`as`);
- `rust_type_aliases` (`type`);
- `rust_keyword_before`;
- `rust_named_item_before`;
- `exported_cfg_test_module`;
- `rust_impl_trait_references` (`impl`, `for`);
- `private_cfg_test_ranges`; and
- static macro/wrapper checks for `macro_rules`.

Keep target/member checks raw-insensitive with `rust_name`, including:

- both restricted namespaces;
- `StaticErrorCode`, `StaticSafeMessage`, and `literal`;
- `ConnectorIo` and `ProcessorControl`;
- alias destinations, which remain identifiers even when raw.

For fixed token sequences such as `#[cfg(test)] mod`, compare `mod`/`pub`
with `rust_keyword`, punctuation by value, and the attribute/meta names
`cfg`/`test` with `rust_name`. Do not change the accepted private-test ranges.

- [ ] **Step 5: Audit for raw-unaware grammar comparisons**

Run:

```bash
rg -n \
  'identifier.*value == "(use|as|type|impl|for|fn|trait|mod|pub|macro_rules|self)"|value == "(use|as|type|impl|for|fn|trait|mod|pub|macro_rules|self)"' \
  scripts/check_connector_processor_boundary.py
```

Expected: no production grammar comparison bypasses `rust_keyword`.
Fixture source strings may contain the searched spellings.

- [ ] **Step 6: Run Task 1 GREEN**

Run:

```bash
python3 scripts/check_connector_processor_boundary.py --self-test
python3 scripts/check_connector_processor_boundary.py
git diff --check
```

Expected: all three commands exit zero with no output. Existing diagnostics
and all pre-existing fixtures remain unchanged.

- [ ] **Step 7: Commit Task 1**

```bash
git add scripts/check_connector_processor_boundary.py
git commit -m "fix(connectors): make boundary tokens Unicode-version independent"
```

The controller dispatches an ordinary spec/code reviewer for this commit
before Task 2. No Judge agent is used.

---

### Task 2: Parse grouped and nested Rust `use` trees structurally

**Files:**

- Modify: `scripts/check_connector_processor_boundary.py:31-47`
- Modify: `scripts/check_connector_processor_boundary.py:239-305`
- Modify: `scripts/check_connector_processor_boundary.py:821-1133`
- Test: embedded deterministic fixtures in
  `scripts/check_connector_processor_boundary.py:1248-1930`

**Interfaces:**

- Consumes: Task 1 `RustToken`, `rust_name`, `rust_keyword`, and
  `rust_tokens`.
- Produces:

```python
@dataclass(frozen=True)
class RustUseLeaf:
    start: int
    path: tuple[str, ...]
    alias: str | None
    public: bool


@dataclass(frozen=True)
class RustUse:
    start: int
    tokens: tuple[RustToken, ...]
    public: bool
    leaves: tuple[RustUseLeaf, ...]
    parsed: bool


def rust_use_leaves(
    tokens: tuple[RustToken, ...],
    public: bool,
) -> tuple[RustUseLeaf, ...] | None: ...
```

- `path` stores normalized token values; raw state does not change protected
  identity.
- `alias is not None` is sufficient for policy; its raw spelling remains a
  valid alias destination but never grammar.
- `parsed=False` retains the statement tokens for conservative fallback.

- [ ] **Step 1: Add grouped-use RED and structural controls**

Add these two load-bearing sibling-before-`self` mutations in the otherwise
approved producer roots:

```python
Fixture(
    "crates/server/src/connectors/grouped_host_self_alias.rs",
    "use donat_connector_abi::host_construction::{"
    "transport_response, self as host_api};",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
Fixture(
    "crates/connector-catalog/src/grouped_catalog_self_alias.rs",
    "use donat_connector_abi::catalog_construction::{"
    "static_error_code, self as catalog_api};",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
```

Add descendant alias mutations, also inside approved roots:

```python
Fixture(
    "crates/server/src/connectors/grouped_host_member_alias.rs",
    "use donat_connector_abi::host_construction::{"
    "authorized_correlations, transport_response as make};",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
Fixture(
    "crates/connector-catalog/src/nested_catalog_member_alias.rs",
    "use donat_connector_abi::{catalog_construction::{"
    "static_safe_message, static_error_code as make_code}, CapabilityId};",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
```

Add these controls:

```python
Fixture(
    "crates/server/src/connectors/unrelated_sibling_alias.rs",
    "use donat_connector_abi::{host_construction, "
    "harmless::{Thing as Alias}};",
    None,
)
Fixture(
    "crates/server/src/connectors/grouped_raw_keyword_alias.rs",
    "use donat_connector_abi::host_construction::{"
    "transport_response, self as r#use};",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
Fixture(
    "crates/server/src/connectors/grouped_unaliased_self.rs",
    "use donat_connector_abi::host_construction::{"
    "transport_response, self};",
    None,
)
```

Add these exact syntax/consumer fixtures:

```python
Fixture(
    "crates/server/src/connectors/leading_global_reexport.rs",
    "pub(crate) use ::donat_connector_abi::StaticErrorCode;",
    "static-literal-reexport: StaticErrorCode::literal cannot be re-exported "
    "outside STATIC_LITERAL_ROOTS",
)
Fixture(
    "crates/server/src/connectors/restricted_visibility_reexport.rs",
    "pub(in crate) use donat_connector_abi::StaticSafeMessage;",
    "static-literal-reexport: StaticSafeMessage::literal cannot be "
    "re-exported outside STATIC_LITERAL_ROOTS",
)
Fixture(
    "crates/server/src/connectors/nested_host_self_alias.rs",
    "use outer::{donat_connector_abi::{host_construction::{"
    "transport_response, self as host_api,},},};",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
Fixture(
    "crates/server/src/connectors/host_as_underscore.rs",
    "use donat_connector_abi::host_construction as _;",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
Fixture(
    "crates/server/src/connectors/host_empty_group.rs",
    "use donat_connector_abi::host_construction::{};",
    None,
)
Fixture(
    "crates/server/src/connectors/host_glob.rs",
    "use donat_connector_abi::host_construction::*;",
    None,
)
Fixture(
    "crates/server/src/connectors/grouped_static_types.rs",
    "use donat_connector_abi::{CapabilityId, "
    "StaticErrorCode as ErrorCode, StaticSafeMessage as SafeMessage};",
    "static-literal-alias: StaticErrorCode::literal cannot be reached "
    "through an alias outside STATIC_LITERAL_ROOTS",
)
Fixture(
    "crates/connector-processors/src/grouped_host_traits.rs",
    "use donat_connector_abi::{TypedBindings, "
    "ConnectorIo as Io, ProcessorControl as Control};",
    "host-trait-alias: ConnectorIo cannot be aliased outside approved host "
    "implementation roots",
)
Fixture(
    "crates/server/src/connectors/protected_macro_use_fallback.rs",
    "macro_rules! import_host { ($alias:ident) => { "
    "use donat_connector_abi::host_construction as $alias; }; }",
    "restricted-namespace-alias: restricted construction namespaces "
    "cannot be aliased",
)
Fixture(
    "crates/server/src/connectors/grouped_noncode_decoys.rs",
    "use donat_connector_abi::{host_construction, harmless};\n"
    "// harmless::{Thing as host_construction}\n"
    'const TEXT: &str = r#"{StaticErrorCode as Alias}"#;',
    None,
)
```

The grouped static/trait fixtures preserve the current first-protected-name
diagnostic order. The empty group and glob are approved-root controls; they
must not create a new alias finding.

Use the existing diagnostic families and preserve re-export-before-alias
precedence.

- [ ] **Step 2: Run the checker self-test and capture RED**

Run:

```bash
python3 scripts/check_connector_processor_boundary.py --self-test
```

Expected: non-zero. At minimum the two sibling-before-`self` and two
descendant-alias fixtures report “expected … got []”. The unrelated sibling
and unaliased approved-root controls remain silent.

- [ ] **Step 3: Implement the recursive leaf parser**

Implement a small cursor-based parser over the tokens between `use` and `;`.
Its recursive function carries an inherited path:

```python
def parse_tree(
    cursor: int,
    inherited: tuple[str, ...],
) -> tuple[list[RustUseLeaf], int] | None:
    # Skip one leading "::".
    # Accumulate identifier-candidate path segments separated by "::".
    # A non-raw "self" leaf resolves to `inherited`.
    # "{...}" recursively parses comma-separated sibling trees with the
    # accumulated path as their inherited prefix.
    # "*" emits a leaf under the accumulated path.
    # A non-raw "as" consumes one identifier candidate or "_".
    # Return None on an unsupported/incomplete structure.
```

The complete parser must implement these exact outcomes:

```text
host_construction::{self as host}
    -> path (..., "host_construction"), alias "host"

host_construction::{first, second as make}
    -> separate paths (..., "host_construction", "first") and
       (..., "host_construction", "second"); only the second has an alias

{host_construction, harmless::{Thing as Alias}}
    -> the protected leaf has no alias; the harmless leaf has Alias

outer::{host_construction::{first, self as host}}
    -> inherited prefixes survive arbitrary group depth
```

`rust_use_statements` sets `public` only from a non-raw `pub` visibility,
calls `rust_use_leaves(tokens, public)`, and stores `parsed=False` when parsing returns
`None`.

- [ ] **Step 4: Migrate use-policy queries to structural leaves**

Implement:

```python
def rust_use_mentions(statement: RustUse, name: str) -> bool:
    if statement.parsed:
        return any(name in leaf.path for leaf in statement.leaves)
    return any(rust_name(token, name) for token in statement.tokens)


def rust_use_aliases(statement: RustUse, name: str) -> bool:
    if statement.parsed:
        return any(
            leaf.alias is not None and name in leaf.path
            for leaf in statement.leaves
        )
    mentioned = rust_use_mentions(statement, name)
    has_alias = any(rust_keyword(token, "as") for token in statement.tokens)
    return mentioned and has_alias
```

Keep `statement.public` as the re-export discriminator and retain current
policy order:

1. re-export;
2. alias;
3. type/macro/wrapper/direct producer policy.

Do not loosen direct reference checks after parsing. A descendant alias under
either restricted namespace is an alias of protected authority because the
leaf path contains that namespace.

- [ ] **Step 5: Run focused structural probes**

Run an in-memory table against `scan_source` for:

```text
host_construction::{first, self as host}
host_construction::{first, second as make}
outer::{host_construction::{first, self as host}}
{host_construction, harmless::{Thing as Alias}}
host_construction::{first, self as r#use}
```

Expected: the first, second, third, and fifth rows return exactly
`restricted-namespace-alias`; the sibling control returns no diagnostic in
the approved host root.

- [ ] **Step 6: Run Task 2 GREEN**

Run:

```bash
python3 scripts/check_connector_processor_boundary.py --self-test
python3 scripts/check_connector_processor_boundary.py
git diff --check
```

Expected: all commands exit zero with no output and Task 1 fixtures remain
green.

- [ ] **Step 7: Commit Task 2**

```bash
git add scripts/check_connector_processor_boundary.py
git commit -m "fix(connectors): parse grouped boundary use trees"
```

The controller dispatches an ordinary spec/code reviewer for the complete
Task 2 diff before verification. No Judge agent is used.

---

### Task 3: Prove compiler compatibility and close the follow-up remediation

**Files:**

- Create:
  `.superpowers/sdd/2026-07-29-connector-boundary-lexer-remediation/verification.md`
- Modify:
  `.superpowers/sdd/2026-07-29-connector-boundary-lexer-remediation/progress.md`
- Modify:
  `.superpowers/sdd/2026-07-29-connector-abi-remediation/progress.md`

**Interfaces:**

- Consumes: accepted Task 1 and Task 2 commits.
- Produces: fresh deterministic/compiler/workspace/conformance evidence and an
  explicit resolution link from the old frozen remediation ledger.
- Does not change production or test code.

- [ ] **Step 1: Prove deterministic standalone checker behavior**

Run:

```bash
verification_dir="$(mktemp -d)"
python3 scripts/check_connector_processor_boundary.py --self-test \
  >"$verification_dir/first.txt"
python3 scripts/check_connector_processor_boundary.py --self-test \
  >"$verification_dir/second.txt"
cmp "$verification_dir/first.txt" "$verification_dir/second.txt"
PATH=/usr/bin:/bin /usr/bin/python3 \
  scripts/check_connector_processor_boundary.py --self-test
python3 scripts/check_connector_processor_boundary.py
```

Expected: every command exits zero; both output files are empty and identical.
The reduced `PATH` contains no Cargo or Rust compiler, proving the checker has
no compiler runtime dependency.

- [ ] **Step 2: Compile the valid Rust lexical probe**

Generate the exact valid source on standard input and compile it:

```bash
python3 - <<'PY' | /home/dev/.cargo/bin/rustc \
  --edition 2024 \
  --crate-type lib \
  --emit metadata \
  -o /dev/shm/donat-boundary-valid-probe.rmeta \
  -
import sys

source = """\
#![allow(unused, uncommon_codepoints, non_upper_case_globals)]
mod donat_connector_abi {
    pub struct StaticErrorCode;
    pub mod host_construction {
        pub fn transport_response() {}
    }
}
use\u0085donat_connector_abi::StaticErrorCode\u200eas\u200f𖄀Code;
use donat_connector_abi::host_construction::{
    transport_response,
    self as 𖄀Host,
};
pub\u2028fn r#use(_: 𖄀Code) {
    transport_response();
    𖄀Host::transport_response();
}
pub\u2029const r#type: usize = core::mem::size_of::<𖄀Code>();
"""
sys.stdout.write(source)
PY
```

Expected: exit zero. The source contains all five non-ASCII
`Pattern_White_Space` separators, U+16100, legal raw-keyword names, a
sibling-before-`self` group, and a trailing comma. The local `allow` exists
only in the stdin probe and does not weaken repository lints.

- [ ] **Step 3: Prove the documented invalid-atom trade-off**

Run:

```bash
if printf '%s\n' \
  'mod abi { pub struct StaticErrorCode; }' \
  'use abi::StaticErrorCode as 💥;' \
  | /home/dev/.cargo/bin/rustc \
      --edition 2024 \
      --crate-type lib \
      --emit metadata \
      -o /dev/shm/donat-boundary-invalid-probe.rmeta \
      -; then
  exit 1
fi
```

Expected: the guarded command succeeds only when `rustc` exits non-zero
because the alias is not a Rust identifier. The checker's embedded
invalid-atom fixture conservatively emits the existing alias diagnostic.

- [ ] **Step 4: Run ABI and closure verification**

Run:

```bash
/home/dev/.cargo/bin/cargo test -p donat-connector-abi \
  --no-default-features --offline --locked
/home/dev/.cargo/bin/cargo test -p donat-value-contract \
  --no-default-features --offline --locked
/home/dev/.cargo/bin/cargo check -p donat-connector-abi \
  --target thumbv7em-none-eabihf \
  --no-default-features --offline --locked
/home/dev/.cargo/bin/cargo tree -p donat-connector-abi \
  --target all --edges normal,build \
  --no-default-features --offline --locked
/home/dev/.cargo/bin/cargo clippy -p donat-connector-abi \
  --all-targets --no-default-features --offline --locked -- \
  -D warnings -D clippy::result_large_err
/home/dev/.cargo/bin/cargo fmt --all -- --check
```

Expected: all commands exit zero. The dependency tree contains only
`donat-connector-abi` and the local `donat-value-contract` dependency.

- [ ] **Step 5: Build a fresh server and run native conformance**

Run:

```bash
/home/dev/.cargo/bin/cargo build -p donat-server --bin donat \
  --offline --locked
test "$DONAT_BIN" = "$CARGO_TARGET_DIR/debug/donat"
test -x "$DONAT_BIN"
/home/dev/.cargo/bin/cargo test -p donat-conformance \
  --test connectors --offline --locked
/home/dev/.cargo/bin/cargo test -p donat-conformance \
  --offline --locked
```

Expected: connector conformance is 4/4 and full native conformance is 261/261
or the current strictly greater count with no failure. The harness uses the
fresh `$DONAT_BIN` and supplied isolated `$PG_URL`.

- [ ] **Step 6: Record exact evidence and resolve the old ledger**

Write `verification.md` with:

- exact HEAD;
- Python Unicode version and Rust version;
- Task 1/Task 2 commit IDs;
- every command, exit status, and test count;
- valid/invalid probe results;
- reviewer report paths and verdicts;
- explicit statement that no ABI/runtime/manifest/lockfile changed.

Update the new progress ledger to `complete`. Update the old ABI remediation
ledger's final blocker to:

```text
Resolved by the separately approved connector-boundary-lexer remediation
at <final commit>; the old plan received no second final fix wave.
```

- [ ] **Step 7: Confirm the final source commit and dispatch final review**

The `.superpowers/sdd/` tree is intentionally gitignored; retain its reports
as execution evidence and do not force-add them. Run:

```bash
git status --short
git log -3 --oneline
```

Expected: the source worktree is clean at the accepted Task 2 commit. Dispatch
one final ordinary independent reviewer over `d678f85..HEAD`. If it reports a
Critical or Important finding, open a new bounded remediation task rather than
silently extending this plan.
