---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# The document renderer's advisories are ignored by name, with a reachability argument

## Context

Spec 019 put a Typst document renderer inside the serving binary, and Typst
brings a large tree with it: a bibliography engine (`hayagriva`), a CSL style
parser (`citationberg`), syntax highlighting (`syntect`), font shaping
(`rustybuzz`, `ttf-parser`) and a PDF writer (`krilla`). Adding it took the
workspace from zero advisories to eight — two vulnerabilities and six
unmaintained warnings — and `cargo audit --deny warnings` is a CI gate, so the
branch was red.

None of the eight is ours. Every one is transitive under `typst` and under
nothing else; all eight crates were absent from `main`'s lockfile. The two
vulnerabilities are the same crate, `quick-xml` 0.38.4, reached through
`hayagriva -> citationberg`. Our own direct users of `quick-xml` — `calamine`
for spreadsheet ingest and `phonenumber` — are already on the fixed 0.41.0.

The obvious answer, upgrade, is not available. `citationberg` 0.7.0,
`hayagriva` 0.10.1 and `typst` 0.15.1 are each the newest published release,
and `citationberg` still requires `quick-xml` 0.38. There is no resolution of
this tree that clears the advisory, and forcing one through `[patch]` would
mean compiling a dependency against an API two minor versions ahead of what it
was written for.

## Decision

The eight advisories are ignored **by name** in `.cargo/audit.toml`, each with
the reason it is excused, and the `--deny warnings` gate stays exactly as
strict as it was.

They are listed individually rather than by switching off the `unmaintained`
class, because the value of the gate is catching the *next* one. A blanket
`informational_warnings` exclusion would have silently absorbed every future
unmaintained crate anywhere in the workspace; a list of six ids absorbs six.

The two vulnerabilities are excused on reachability, not on severity. Both are
denial-of-service shaped — quadratic time on duplicate attribute names,
unbounded allocation on namespace declarations — so both need attacker-supplied
XML, and the renderer has no way to be given any. Its Typst world serves one
map and only one, `template.files()`, the deployment's own frozen template set:
no filesystem handle, no package storage, no HTTP client
([[050-a-document-template-is-deployment-metadata-and-the-renderer-is-a-closed-world]]).
Request data arrives as `sys.inputs`, a `Dict`, which is not a file and cannot
name one, and a `bibliography(..)` call is refused at load unless its argument
is a string literal already in that frozen set. The worst outcome reachable is
a deployment slowing its own renderer down with a bibliography file it checked
in itself — which is a deployment-time mistake, not an attack.

An entry stays only while all three of its conditions hold: no upgrade exists,
the reachability argument is still true, and the dependency is still pulled.
The list is reviewed whenever the renderer's dependencies are bumped, and an
entry that has become removable is removed rather than left to accumulate.

## Alternatives

| Option | Why Not |
|--------|---------|
| Upgrade the dependency | There is nothing to upgrade to. Every crate on the path is already at its newest published version, and the fix is not released downstream. |
| `[patch.crates-io]` onto quick-xml 0.41 | Compiles `citationberg` against an API it was not written for. Trades a documented, unreachable DoS for a real chance of miscompilation or silent misparsing in a renderer we ship. |
| Drop the Typst renderer | Deletes a delivered spec 019 capability to clear advisories in code no untrusted input reaches. Wildly out of proportion. |
| Disable the `unmaintained` class wholesale | Cheapest to write and the worst outcome: it would swallow every future unmaintained crate in the workspace, including ones in code that *does* take untrusted input. |
| Drop `--deny warnings` | Same failure, one level broader. The gate's strictness is the point. |

## Consequences

The gate is green again and still catches everything it caught before: a new
advisory, in any crate, including a new one under `typst`, still fails CI. What
we pay is a file that must be re-examined rather than trusted — six ignores can
quietly become permanent, and the mitigation is only the review discipline
written into the config's header comment and into this decision.

We also accept a standing dependency on a reachability argument being *kept*
true. If the renderer ever gains a second file source — a template uploaded at
runtime, a bibliography fetched per request — the argument collapses and both
`quick-xml` entries must come out the same day. That is the specific thing to
check before widening the renderer's world, and it is why the closed world is a
decision of its own rather than an implementation detail.
