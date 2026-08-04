---
type: decision
status: accepted
date: 2026-07-29
features:
  - "[[declarative-saas]]"
  - "[[007-community-connector-factory]]"
---

# Connector boundary policy uses version-independent Rust token atoms

## Context

ADR 010 makes one Python whole-workspace checker the mechanical substitute for
friend-crate visibility around connector construction namespaces, static
failure literals, and host traits. Its first tokenizer used Python
`str.isidentifier()`. Python 3.12.3 uses an older Unicode database than the
local Rust compiler, so compiler-valid aliases using newer XID characters
could evade every alias-based rule. Removing `r#` also made legal raw keyword
identifiers look like Rust grammar, and flat import scanning missed grouped
`use` aliases after a sibling.

The policy needs to recognize protected ASCII names in every compiler-valid
Rust source. It does not need to validate all Rust identifiers.

## Decision

The existing checker tokenizes Rust with a delimiter-based, fail-closed
over-approximation. It uses Rust's explicit `Pattern_White_Space` and ASCII
punctuation boundaries, retains every other maximal scalar run as an
identifier candidate, and preserves whether the candidate used `r#`.
Grammar predicates require a non-raw token; protected-name predicates ignore
raw state.

The checker recursively flattens supported Rust `use` trees into full-path
leaves with visibility and alias data. Nested groups and `self` inherit their
prefix, siblings remain independent, and aliases of descendants under a
restricted construction namespace remain restricted.

The checker remains one offline Python-standard-library program. It gains no
Unicode XID table, Python package, `rustc` subprocess, generated parser, or
second policy mechanism. Rust compilation remains the authority for complete
source validity; conservative rejection of an already-invalid non-XID atom is
acceptable.

## Alternatives

| Option | Why Not |
| --- | --- |
| Vendor Unicode XID tables | They match only one Rust/Unicode version and recreate the bypass when a newer stable compiler accepts additional identifiers. |
| Invoke `rustc`, nightly parsing, or `rustc_private` | Stable `rustc` exposes no suitable supported parser output; fragment compilation adds unrelated errors and process cost; a helper becomes an unstable second checker. |
| Keep Python Unicode classification and pin Python | It couples accepted Rust syntax to the wrong runtime and still drifts from a newer Rust toolchain. |
| Keep flat token scanning | It cannot associate aliases structurally across nested groups, `self`, and sibling branches. |

## Consequences

Every compiler-valid Unicode identifier remains one searchable atom across
Rust Unicode upgrades, and legal raw identifiers no longer become grammar
keywords. A recursive import model closes grouped alias escapes without
building a general Rust parser.

The over-approximation may diagnose an invalid Rust source before `rustc`
does. That is an intentional fail-closed trade-off. Macro expansion and name
resolution remain outside this lexical checker's authority and continue to
require narrow ABI design, existing conservative macro rules, and review.

