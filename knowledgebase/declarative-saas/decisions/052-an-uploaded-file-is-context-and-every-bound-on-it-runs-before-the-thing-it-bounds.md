---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[020-spreadsheet-ingest]]"
---

# An uploaded file is execution context, and every bound on it runs before the thing it bounds

## Context

Spec 020 adds the reverse of spec 019 §5: `local.ingest` reads a spreadsheet or
a CSV that a user uploaded and returns typed rows. It is the first local
capability whose input is not something the deployment computed. Every byte of
it was chosen by whoever uploaded the file, and an `.xlsx` is a zip archive, so
the input is a program's worth of attack surface: a 42-byte member that expands
to 4 GB, a central directory that understates what it holds, a macro part, an
external workbook link, a formula, a serial date that means two different days
depending on a flag elsewhere in the package.

Two things about the existing shape did not fit it. ADR 044 makes a local
operation's executor a `fn(&LocalInvocation) -> Result<LocalProduct, _>` whose
only argument is the JSON input, and ADR 050 then established that a template
travels in a `LocalContext` beside the input rather than inside it. Neither had
to answer where *megabytes a process fetched at runtime* go. And the engine had
no precedent for reading a stored attachment at all: `crates/storage` and ADR
033 are written for files going out — a presigned PUT, a pending row, a claim.

The third thing is that a bound is not a bound unless it runs before the work it
is bounding. "Reject an archive whose expansion ratio is over the ceiling" is
free if it is answered from the central directory and worthless if it is
answered after the member has been expanded, because expanding it was the whole
cost. Spec 020 §3 therefore states the bounds as an *ordered* list, which is a
statement about control flow rather than about limits.

## Decision

**The bytes are execution context, and the input keeps only the file's
identity.** `LocalContext` gains a per-execution map of stored files keyed by
the handle an input names, and the dispatcher resolves `input.source` — a file
identifier — to bytes before the blocking task starts. Everything ADR 050 said
about a template's bytes applies with more force here: bytes inside the input
would be measured by the input ceiling, hashed into the activity's request
fingerprint, retained by the journal, and compared by the determinism probe. The
input ceiling for both ingest operations is 4 KiB, and that number is the
evidence: what an ingest activity carries is a schema name and a file id.

**The five bounds run in the order spec 020 §3 states, and the order is
enforced by which function can even see the bytes.** The stored file's recorded
size is compared in the dispatcher, against the row `donat.file_uploads` already
holds, so a file over the ceiling is refused after one query and no download —
and again in the capability, before the archive is opened. Then
`archive::admit_declared` reads the central directory: entry count, every
entry's declared uncompressed size, the resulting ratio. It is handed no
decompressing reader, so "nothing was decompressed" is a property of the
function rather than a claim about it. Then `admit_active_content` refuses
external links, data connections, embedded objects and macro parts by part name
— no XML, no content, no decompression. Only then does `verify_streamed` expand
every member through a counting reader that stops at the member's own declared
size and at the running total, which is what makes believing the directory safe
in the first place. A parser sees a byte after all four have passed, and it
expands the archive a second time on purpose: by then the real expansion has
been measured and is inside the ceiling.

**There is no schema inference, and the type system decides what a cell
becomes.** A schema is deployment metadata (`ingest.yaml`), pinned into every
process that reads with it by the same `<name>@<hash>` stamp a document template
uses, because editing a column's declared type changes what every import means.
A column the schema does not declare is ignored; a declared column the header
does not carry fails the whole read before a row is parsed. Coercion is over the
metadata type system's own built-in scalars, minus `Html`: a value out of a
stranger's spreadsheet is never markup a renderer may trust, which is the same
rule ADR 050 stated from the writing side. Three coercions are corrections
rather than conveniences — a number in a `String` column is refused rather than
stringified (`00123` is not `123`), a bare number in a `Date` column is a serial
resolved through *the workbook's own* epoch and is refused outright in a CSV
which declares none, and a formula cell yields the value the writer cached or a
typed rejection, never a computation.

## Alternatives

| Option | Why Not |
|--------|---------|
| Put the file's bytes in the activity input, base64 or otherwise | The journal would retain megabytes per attempt, the request fingerprint would be a hash of the upload, and the input ceiling — the bound that says an ingest input is a name and an id — would be measuring a payload instead. It is the alternative ADR 050 already rejected for a template, with a larger payload. |
| Let the capability fetch the file itself | A local capability is `Pure`: no origin, no credential, no network. A reader that opened an HTTP client would be a connector with the declaration missing, which is exactly what ADR 044 refused to build. |
| Give `LocalOperation::execute` a fourth argument for the bytes | Every existing caller and every other capability would carry a parameter that is `None` for all of them, and the "sources" concept would be in the signature of work that has none. The context already exists for things that travel beside the input. |
| Keep the file in a process-global store the capability reads from | The same objection ADR 050 made to a global template set: nothing types the dependency, one execution can see another's file, and a test can install exactly one. |
| Bound the archive by the size of the file alone | A 40 KiB archive can declare 4 GB of members. The stored size bounds the download; only the directory bounds the expansion. |
| Trust the central directory's sizes and skip the streaming pass | The directory is written by the attacker. Believing it is what makes the cheap check possible; verifying it while streaming is what makes believing it safe. Both, in that order, or neither. |
| Check the compression ratio after extracting | Then the bomb has already gone off. The ratio is the one check whose entire value is that it is answered before the cost is paid. |
| Detect macros and external links by scanning the XML | That reads attacker-controlled content to decide whether to read attacker-controlled content. A part name is structural, is known from the directory, and costs nothing. |
| Infer a schema from the header row | Then the uploader chooses the shape of the process's data. Every downstream typecheck would be over a shape nobody declared, and a renamed column would silently become a different import. |
| Let the activity supply the columns it wants | A schema chosen at runtime is a schema supplied at runtime by another name, and `validate` could no longer typecheck an import before it runs. |
| Stringify a numeric cell into a `String` column | This is the corruption, spelled as a convenience: `00123` loses its zeros, `12.50` loses its scale, and a large id becomes an exponent. Refusing is the only answer that cannot silently change a key. |
| Convert a serial date with the 1900 epoch and accept the 1904 case as rare | It is four years and two days of silent error on every row, in files produced by a still-shipping application. The workbook says which epoch it uses; reading it is one flag. |
| Evaluate a formula whose value was not cached | Evaluating is executing the uploader's program. A cached value is a value the *writer* computed, which is the only thing a reader can honestly report. |
| Return every rejected row | A file whose whole purpose is to be rejected would then make the *answer* the denial of service. The count is exact; the list is bounded and carries no cell content, because the content is the uploader's text. |
| Have the capability write the rows it read | An import is a decision — deduplicate, upsert, hold for approval — and it belongs to the process. A reader that wrote would also be a `Pure` operation with a side effect, which is not a class this engine has. |

## Consequences

A deployment gets two readers for one `ingest.yaml`, and gets the ordered
bounds, the active-content refusal, the date-system correctness and the typed
rejection list without writing any of them. The schema pin means an import's
meaning is versioned with the process that performs it.

The cost is a third declaration surface beside `documents.yaml` and `media.yaml`,
and a real authoring cost in the schema: every column has to be declared, with a
type, before a single file can be read. That is the point. The other cost is
that an archive is expanded twice — once by the guard, once by the parser —
which is a factor of two on a bounded quantity, paid to keep the guard
independent of whatever the parser decides to do next.

The context's source map is the first per-execution thing in a structure that
was otherwise per-deployment. It is deliberately narrow: a map the dispatcher
fills, that an executor can read and cannot add to, holding only files the
running activity itself named.
