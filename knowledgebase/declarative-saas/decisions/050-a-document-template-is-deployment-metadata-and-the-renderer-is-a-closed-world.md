---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[019-document-rendering]]"
---

# A document template is deployment metadata, and the renderer that reads it is a closed world

## Context

Spec 019 adds the four things every client project rebuilds: an invoice, a
transactional email, an export, a calendar invitation. All four have the same
shape — a template plus typed data — and the template is the part that decides
what the engine executes.

That is the whole difficulty. Typst's template language can import packages and
read files; MJML can include a URL; a spreadsheet cell beginning `=` is code in
whoever opens it. A renderer given a template from a request is a code-execution
endpoint with a friendly name, and one given a template from disk still reaches
the filesystem and the network at render time unless something stops it.

Spec 018 §3 also admits these operations as `Pure` only while "same input bytes
produce byte-identical output", and ADR 044 turned that into a registration
condition: `LocalOperationBuilder::build` renders the declared probe twice and
compares bytes. Every one of the four backends breaks that on its own — Typst
reads the clock for `datetime.today()` and discovers system fonts, `icalendar`
fills `DTSTAMP` from the wall clock, and `rust_xlsxwriter` writes a creation
time into `docProps/core.xml`.

ADR 044 left one thing unanswered for its first consumer: the executor is a
`fn(&LocalInvocation) -> Result<LocalProduct, _>` whose only argument is the
input, and a template is neither compiled in nor input.

## Decision

**A template lives in the metadata directory and reaches the renderer as
execution context, never as input.** `documents.yaml` declares a name, a kind, a
source path, the files it may resolve, its typed inputs, and its bounds. The
loader reads the source and every declared include into a frozen map keyed by
paths rooted at the template's own directory, and hashes the whole set. A
`LocalContext` carries that set beside the input on every execution; input names
a template and can add nothing to it. `LocalContext::builtin()` holds probe
templates compiled into this binary, so ADR 044's double render stays a property
of the binary rather than of whatever a deployment declared.

**The PDF world is closed by construction, and the same door is shut twice.**
`ClosedWorld` implements all seven `typst::World` methods over that frozen map,
the two `include_bytes!` font families, and a `today` taken from declared input.
It holds no filesystem handle, no package storage, no HTTP client, and no
environment lookup — not disabled, absent. Before that, at load, a Typst source
is parsed and every package import and every path literal that does not resolve
inside the template's own set is refused, so a template that would have hit the
world's boundary never becomes a template. The lexical check catches what a
template writes; the world catches what a template computes. Determinism's three
remaining sources are declared input: the document id, the timestamp, and — by
never asking the system — the fonts.

**The two properties that are security properties are enforced where the value
is, not where it came from.** A spreadsheet text cell whose value begins `=`,
`+`, `-`, `@`, or a leading control character is written as a string cell with a
`'` prefix: the cell type is what makes it inert in the file, and the prefix is
what survives a copy-paste out of the recipient's application. An email
interpolation is HTML-escaped unless its dotted path is one the template's
declared input types marked `Html` — resolved from the type system at load and
frozen onto the template, so the decision is made by a declaration and never by
the value's own contents.

**The pin rides on the process, not on a new dependency edge.** The loader
stamps `<template>@<sha256>` onto every `local.document` request activity from
the template it selects. The field is serialize-only, so an operator cannot
write one, and the process definition fingerprint is already taken over the
serialized process — editing a template file therefore changes the revision of
every process that renders with it, with no change to the process compiler and
no new entry in the dependency closure.

## Alternatives

| Option | Why Not |
|--------|---------|
| Let the activity input carry the template source | The renderer becomes a code-execution endpoint reachable by whoever can start a process. Spec 019 §2 forbids it, and no sandbox makes an arbitrary caller-supplied Typst program safe to run in the engine. |
| Let the input select a template by path | A path is a filesystem query, and a filesystem query has a traversal answer. A name resolves in a map the deployment fixed. |
| Put the template set in a process-global `OnceLock` | It reads like context and behaves like configuration: nothing types the dependency, a second deployment in one process is impossible, and a test can install exactly one set for the life of the binary. |
| Keep `LocalOperation::execute(input, ceiling, stop)` and pass templates inside the input JSON | The bytes of a template would then be inside the value the bounds measure, the journal retains, and the determinism probe hashes — and "input selects a template only by name" would be a convention rather than a signature. |
| Use `typst-as-lib` and configure it | The sandbox *is* the feature. Its closure would then be a set of options that a future minor release can default differently, and a reviewer would be checking a configuration rather than reading seven method bodies that reach nothing. |
| Rely on the `World` alone, and let a package import fail at render | Spec 019 §7 asks for load-time failure, and it is right to: a deployment that ships a template with `@preview/...` in it should not start, rather than fail on the first invoice of the month. |
| Rely on the load-time scan alone | It is lexical. A path a template computes — `read("/" + sys.inputs.x)` — is invisible to it, and the world is the only thing that can answer that. |
| Use the compiler's bundled font set (`typst-assets`) | It is 17 faces under four licenses, including one GPL-with-font-exception, and the crate exposes them only as one all-or-nothing iterator. Two OFL families we choose are a licensing story that fits in one notice. |
| Let a renderer fall back to system fonts for a missing glyph | Then the base image decides what an invoice looks like, and an image upgrade silently re-renders every document. A missing glyph is reported in the typed warnings instead, and renders as the font's own notdef. |
| Let `today()` read the clock and accept "near enough" determinism | The double render at registration compares bytes; near enough is a different byte string. A process that wants today's date has one — its own workflow time. |
| Sanitize a formula-leading cell by stripping the character | It changes the operator's data to defend the recipient's application. The value is preserved and made inert instead. |
| Escape an email field by inspecting the value for markup | That is the injection, spelled as a defence: the attacker controls the value and therefore the inspection's answer. The declared type is the only input the attacker does not control. |
| Add templates to `ProcessDependencyClosure` | It needs a new method on `ProcessDependencyCatalog` and a change in every implementation, to reach a fingerprint the serialized process already feeds. The stamp is smaller and provably equivalent. |
| Let a process choose its template at runtime from a computed value | Choosing among declared templates sounds harmless, but it moves the "which program runs" decision out of the deployment, and the metadata typecheck against the bound data stops being possible at `validate`. |

## Consequences

A deployment gets four renderers for one `documents.yaml` and one file per
template. Every one of them is `Pure`, so the process owns retries and a
re-render is free; every one is bounded by all five of spec 018's bounds, with
the template able to narrow the page count, the deadline, and the output size
but never widen them.

The cost is a second declaration surface — a template is metadata, so a template
change is a deploy — and a repository that is 2.8 MB larger for the eight font
files. Both are deliberate: the deploy is what makes the pin meaningful, and the
fonts are what make the bytes reproducible.

The escaping and formula rules move a decision into the metadata type system.
A field that renders as markup has to be declared `Html`, which is a real
authoring cost, and it is the point: the declaration is the only part of the
path an attacker supplying the value does not control.

Two renderers now require a `document_timestamp` in their input that has nothing
to do with the document's content. That is unusual and it is the honest shape:
the alternative is a clock inside a function that claims to be one of its input.
