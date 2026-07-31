# Connector boundary lexer remediation design

Date: 2026-07-29

Baseline: `c59daac` (`fix(connectors): close final ABI review gaps`)

Approval: the user instructed execution to continue after the delimiter-based
Option A below was recommended as a new bounded remediation task.

## Context

The safe connector ABI intentionally delegates friend-crate policy to the one
whole-workspace checker at
`scripts/check_connector_processor_boundary.py`. That checker is therefore a
load-bearing part of the construction boundary: downstream catalog work must
not begin while compiler-valid Rust can evade it or while legal Rust produces
false diagnostics.

The final ABI remediation review found three related defects at the baseline:

1. Python 3.12.3 classifies identifiers with Unicode 15.0, while the local
   Rust 1.97.1 compiler accepts newer Unicode identifier characters. A
   compiler-valid U+16100 alias evades namespace, static-literal, and host-trait
   alias rules.
2. `rust_tokens` removes `r#` and records no raw-identifier state. Legal
   `r#use` and `r#type` identifiers are then mistaken for Rust grammar
   keywords and produce false re-export/type-alias diagnostics.
3. `rust_use_aliases` scans a flat token slice and stops at the first sibling
   comma. A valid tree such as
   `host_construction::{transport_response, self as host_api}` can therefore
   evade the alias rule inside an otherwise approved producer root.

The current simple nested `host_construction::{self as alias}` form happens to
be diagnosed. That does not establish structural coverage because inserting an
earlier sibling changes the result.

The [Rust identifier grammar][rust-identifiers] is versioned with Unicode,
while Rust whitespace is the small explicit `Pattern_White_Space` set and Rust
[punctuation][rust-tokens] is an explicit ASCII set. The checker needs to
recognize protected ASCII names and Rust grammar; it does not need to decide
whether every other atom is a valid Rust identifier.

[rust-identifiers]: https://doc.rust-lang.org/stable/reference/identifiers.html
[rust-whitespace]: https://doc.rust-lang.org/reference/whitespace.html
[rust-tokens]: https://doc.rust-lang.org/reference/tokens.html

## Decision

Replace Python Unicode identifier classification with a delimiter-based,
fail-closed tokenization contract and replace flat import alias scanning with a
small recursive `use`-tree parser.

The lexer deliberately over-approximates identifier atoms. Every
compiler-valid Rust identifier remains one atom even when a later Rust release
adopts a newer Unicode version. A non-XID atom may also be retained as an
identifier candidate, but that source is rejected by `rustc`; this can only
make the boundary checker reject already-invalid Rust, never permit
compiler-valid Rust to evade an alias rule.

Raw state is preserved independently from normalized spelling. Grammar
matching requires a non-raw token; protected-name matching ignores raw state.
The recursive `use` parser resolves nested prefixes, groups, `self`, aliases,
globs, and visibility into flattened leaves before policy evaluation.

No ABI, processor, catalog, server, credential, provider, transport, process,
database, metadata, or public API behavior changes.

## Alternatives

| Option | Decision |
| --- | --- |
| Delimiter-based over-approximation plus recursive `use` parsing | **Selected.** It is deterministic, offline, standard-library-only, independent of Python and Rust Unicode-version drift, and sufficient for the protected ASCII names. |
| Vendor XID start/continue tables | Rejected. The table is exact for only one selected Unicode/Rust version and recreates the bypass when a floating stable compiler accepts a later version. It also adds generated third-party data, provenance, and update work without improving the selected fail-closed property. |
| Invoke `rustc`, a nightly parser, or `rustc_private` | Rejected. Stable `rustc` has no supported source-token/AST output for this use, fragment compilation introduces unrelated resolution/type failures, per-fixture processes are slow, and a helper would become a second unstable checker/dependency. |

## Lexical contract

### Non-code precedence

The existing non-code pass remains the first stage. Line comments, nested
block comments, ordinary strings, byte strings, C strings, raw strings, raw
byte strings, raw C strings, character literals, and byte-character literals
do not emit searchable tokens. Raw-string recognition occurs before
raw-identifier recognition. Lifetimes and labels are not swallowed as
character literals.

The remediation does not add a Rust parser or macro expander. It retains the
existing conservative macro-token rules. Protected spellings in comments and
literals remain decoys, not references.

### Fixed separators

Tokenization must not call `str.isidentifier`, `str.isalpha`,
`unicodedata.category`, `unicodedata.normalize`, or `str.isspace`.

Rust whitespace is the exact stable set from the
[Rust Reference][rust-whitespace]:

```text
U+0009 U+000A U+000B U+000C U+000D U+0020
U+0085 U+200E U+200F U+2028 U+2029
```

The optional leading U+FEFF byte-order mark is ignored because Rust removes it
before tokenization.

Every ASCII scalar other than `[A-Za-z0-9_]` is a delimiter/punctuation token.
`::` is coalesced because existing path rules consume it; other punctuation
may remain one-character tokens. Every non-ASCII scalar outside the fixed
whitespace set belongs to an identifier candidate atom.

An identifier candidate is the maximal run of:

- ASCII letters, digits, or underscore; and
- non-ASCII scalars outside the fixed whitespace set.

This is intentionally not an XID validator. The compiler remains the authority
for complete Rust lexical validity.

### Raw identifiers and token predicates

`RustToken` retains at least:

```text
value       spelling without an optional r# prefix
offset      character offset in the checker input
identifier  whether this is an identifier candidate
raw         whether the source token used r#
```

Two predicates have distinct semantics:

```text
is_name(token, "StaticErrorCode")
    token is an identifier candidate and token.value matches;
    raw state does not change the protected identity.

is_keyword(token, "use")
    token is an identifier candidate, token.value matches, and token.raw is
    false.
```

All Rust grammar recognition uses `is_keyword`, including `use`, `as`, `type`,
`impl`, `for`, `fn`, `trait`, `mod`, `pub`, `macro_rules`, and `self`.
Protected namespace, static type, host trait, and `literal` member matching
uses `is_name`.

Consequences:

- `r#use`, `r#as`, `r#type`, `r#impl`, and similar legal names cannot become
  grammar tokens;
- `as r#use` still has a valid raw alias destination;
- `r#StaticErrorCode::r#literal` still names protected API;
- aliases beginning with Unicode characters unknown to the running Python
  release remain one atom.

## Recursive `use`-tree contract

The parser is deliberately limited to import declarations. It is not a
general Rust parser.

For every non-raw `use`, it recognizes:

- optional `pub`, `pub(crate)`, `pub(super)`, and `pub(in path)` visibility;
- an optional leading `::`;
- simple path segments;
- `as <identifier candidate>` and `as _`;
- `*`;
- nested `{ ... }` groups with arbitrary depth and trailing commas; and
- `self`, resolved to the inherited prefix.

It emits flattened leaves:

```text
UseLeaf {
    full_path: tuple of path atoms,
    alias: optional alias atom or "_",
    public: bool,
    start: source offset,
}
```

For example:

```rust
use donat_connector_abi::host_construction::{
    transport_response,
    self as host_api,
};
```

produces one descendant leaf for `transport_response` and one leaf whose path
is `donat_connector_abi::host_construction` and whose alias is `host_api`.
An alias in a sibling branch never attaches to the protected branch.

Policy evaluation is structural:

- a public leaf whose path contains a protected namespace, static type, or
  host trait is a re-export;
- an aliased leaf is forbidden when its target or an ancestor in its full path
  is protected;
- descendants of `host_construction` and `catalog_construction` remain
  protected, so member aliases cannot forward their constructors;
- direct unaliased imports retain the existing producer/test-root policy;
- existing diagnostic rule names and precedence remain stable.

If a statement containing a protected name cannot be parsed as a valid
supported use tree, policy evaluation falls back conservatively without a new
diagnostic family: a public protected statement uses the existing re-export
rule, a protected statement containing a non-raw `as` uses the existing alias
rule, and an otherwise direct protected reference reaches its existing
producer rule. An unrelated unsupported/malformed token tree does not create a
workspace-wide parse diagnostic. Compiler-valid use trees in the supported
Rust 2024 grammar must parse.

## Scope and ownership

The implementation modifies the existing checker and its deterministic
fixtures. It may update the authoritative connector spec/plan wording and the
SDD ledger. It must not:

- create a second checker, wrapper checker, generated Unicode table, Python
  package, Cargo dependency, build script, or `rustc` subprocess dependency;
- change production allowlist roots or test-root authority;
- weaken any existing namespace, static-literal, host-trait, allocation-leak,
  lint, private-test, or no-OS rule;
- change any Rust ABI type, field, constructor, trait, manifest, lockfile, or
  runtime behavior;
- add provider integrations, logical/workflow nodes, UI, Node.js, JavaScript,
  WASM, dynamic plugins, admin behavior, or a public connector execution
  surface.

## TDD acceptance

The implementation begins by adding deterministic fixtures that fail against
the baseline:

1. a Rust-accepted post-Python-15 Unicode alias for each of
   `host_construction`, `catalog_construction`, `StaticErrorCode`,
   `StaticSafeMessage`, `ConnectorIo`, and `ProcessorControl`;
2. sibling-before-`self as ...` grouped aliases for both restricted
   namespaces;
3. nested descendant aliases and a sibling control proving that an unrelated
   alias does not attach to a protected leaf;
4. legal `r#use` and `r#type` decoys that currently produce false
   diagnostics;
5. raw-keyword alias destinations that remain forbidden aliases;
6. protected spellings separated by U+0085, U+200E, U+200F, U+2028, and
   U+2029;
7. comments, nested comments, strings, raw-string families, characters,
   lifetimes, and labels containing protected and future-Unicode spellings;
8. approved production/test roots and unapproved test paths;
9. a deliberately non-XID Unicode alias showing the documented conservative
   rejection of invalid Rust.

At GREEN:

- the self-test is deterministic on two runs;
- the real workspace scan is empty;
- the checker passes with `rustc` absent from `PATH`;
- a standalone local `rustc` probe accepts the chosen future-Unicode,
  Pattern-White-Space, raw-keyword, and grouped-use valid examples;
- an invalid-atom probe is rejected by `rustc`;
- all existing ABI unit/integration/doctests, no-OS closure, dependency-tree,
  strict Clippy, format, connector conformance, and full native conformance
  gates remain green.

## Completion and downstream gate

An independent reviewer must inspect the complete remediation diff and the
fresh verification evidence. Only after the review has no Critical or
Important finding may the old ABI remediation ledger reference this follow-up
as resolved and Community Connector Factory Task 3 begin.
