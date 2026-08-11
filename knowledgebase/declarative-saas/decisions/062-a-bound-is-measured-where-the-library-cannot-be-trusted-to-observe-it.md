---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[018-local-capabilities]]"
  - "[[019-document-rendering]]"
  - "[[020-spreadsheet-ingest]]"
---

# A bound is measured where the library cannot be trusted to observe it

## Context

ADR 044 admits `Pure` on a double render and states two operational promises
beside it: an execution ends at its declared `cpu_deadline`, and a draining
replica gets a bounded, cooperative stop rather than an abandoned thread. ADR
052 states a third, about ordering: a bound that runs after the work it bounds
is not a bound. All three were implemented as calls the capability makes —
`LocalInvocation::checkpoint`, `reserve`, `charge_units` — which is correct
exactly as far as the capability is the one doing the work.

A review of the local capabilities found four places where it is not, and where
the bound had quietly become a description of somebody else's library:

* `calamine`'s `Range` is dense. `Range::from_sparse` allocates one slot per
  cell of the bounding box of the populated cells, so an `.xlsx` holding `A1`
  and `XFD1048576` — under a kilobyte of XML, and nothing the archive bounds of
  ADR 052 can see, because uncompressed it is still under a kilobyte — asks for
  16384 × 1048576 slots. That is 512 GiB, an allocation failure, and an aborted
  process, and it happened *before* the row and column ceilings were read.
* `calamine`'s serial-date conversion has no range guard: it takes
  `.floor() as u64`, which saturates a non-finite value to `u64::MAX`, and then
  adds the epoch offset. A date-styled cell containing `1e400` therefore
  panicked in debug and produced a wrapped, truncated year in release.
* `typst::compile` observes nothing. Measured on a forty-second compile, it
  touches its `World` exactly once — one `library`, one `main`, one `source`,
  one `book`, one `font`. There is no callback, no injectable `Sink`, and no
  tracked method a `checkpoint` could live in.
* `icalendar` escapes only the properties it classifies as `TEXT`, and it does
  not escape `\r` even there. `ORGANIZER` and `ATTENDEE` are `CAL-ADDRESS`,
  `RRULE` is `RECUR`, and a `TZID` is a parameter — all written through
  verbatim, so a declared value carrying a CRLF writes a property nobody
  declared into a delivered `.ics`.

## Decision

**Where a library's shape is the cost, the shape is answered before the library
is asked.** `spreadsheet.read` no longer materializes a `Range` at all: it
streams the sheet through `worksheet_cells_reader` and holds what it reads
sparsely, so a sheet's *extent* costs a comparison rather than an allocation.
The extent is checked twice, in the order ADR 052 already argued for the central
directory — the sheet's own `dimension` record first, against the absolute
ceilings, because believing it is free and it refuses a grid-sized bounding box
for nothing; then every cell that actually arrives, against the schema's
narrower ones, because the record was written by whoever wrote the file. The
declared record is deliberately checked against the *absolute* ceilings rather
than the schema's: a writer's used range legitimately runs past the rows a
schema expects, and an early refusal that fires on honest files would be a
defect of its own.

**Where a library will compute a value out of range, the range is ours and it
is written down once.** `schema::is_representable_serial` is the single
statement of which Excel serials name a date, and both callers — a bare number
in a date column, and a cell the workbook itself typed as a date — refuse the
same set. An unrepresentable serial is a typed row rejection (`cell_not_a_date`)
rather than a panic or a year nobody can explain.

**Where a library cannot observe a bound, somebody outside it does.**
`pdf.render` compiles on a thread of its own and waits on a channel, polling
the deadline and the `StopSignal` every 25 ms. This is a smaller promise than
ADR 044's "the work observes the signal and ends", and it is deliberately the
smaller one: what is left running is a thread holding a `ClosedWorld` and
nothing else — no runtime handle, no connection, no file, no signal it could be
needed to answer — and its product is dropped. What becomes true in exchange is
the promise that matters to a Process: an activity never outlives the
`start_to_close` it declared, and a replica never waits out a template to drain.
ADR 044's objection to abandoning work was about a *tokio blocking task* left
running against a runtime that is going away; this thread depends on no runtime
and ends on its own.

**Where a library's escaping is by value type, the refusal is by value.**
`calendar.render` gates every value it writes — template-supplied and
process-supplied alike, values and parameters alike — through one function that
refuses control characters. Not the three properties the library happens to
leave raw today: the classification is the library's and can change, while "a
calendar value never contains a line break" is ours and cannot.

The same reading applied to `email.render`'s `{{#each}}`, which is the one form
in that grammar whose output is the product of a process's list and a template's
body. It reached a `checkpoint` only where it found a `{{`, so a body of static
markup — what a repeat is usually for — ran the whole list with nothing charged.
It now charges the buffer's growth per iteration, and the total charged for one
render is exactly the length of what it produced.

## Alternatives

| Option | Why Not |
|--------|---------|
| Check the row and column ceilings just after `worksheet_range` returns | That is the order the defect is made of. The allocation is the cost; a ceiling read afterwards is a description of a process that already aborted. |
| Keep the dense `Range` and only check the declared `dimension` record | The record is written by whoever wrote the file. It can be absent, and it can understate — and the default ceilings still admit a 1,024 × 1,000,000 bounding box, which is 32 GB of dense slots for a file that is inside every declared bound. |
| Trust the declared `dimension` record against the schema's own ceilings | A writer's used range includes formatted-but-empty cells and legitimately runs past the rows a schema imports. A cheap check that refuses honest files is a worse defect than the one it prevents; the streaming check is the one that has to be narrow. |
| Convert the date-styled cell through `from_serial` instead of adding a guard | It would lose the workbook's own typing: a date-styled cell would become a bare number, and a number is an admissible value for an `Int`, `Float` or `Decimal` column. Refusing a bad date must not turn a date into a number. |
| Report an out-of-range serial as a failed file | One bad cell is a bad row, not a hostile file — the same reading spec 020 already applies to a cell over its byte ceiling. |
| Bound `typst` by making the `World` methods checkpoint | Measured: one call to each over a forty-second compile. It is not a slow path, it is no path. |
| Accept that a PDF render is unbounded, and document it | Then `start_to_close` is a number a Process declares and the runtime ignores, and a rolling deployment's grace period is decided by the worst template anybody wrote. A declaration the runtime ignores is the defect ADR 034 is about. |
| Bound `typst` by shrinking what a template may contain | The pathological shape is two nested loops, each inside `typst`'s own 10,000-iteration ceiling. There is no lexical property to refuse, and ADR 050's load-time checks are about what a template may *reach*, not how long it may take. |
| Escape control characters in the calendar rather than refusing them | An escape has to be correct per value type — `\n` in a `TEXT` is `\n`, in a `CAL-ADDRESS` it is nothing legal at all — so escaping means reimplementing the library's classification and keeping it in agreement. A refusal is correct for every type, and no field this operation writes has a legitimate use for a control character. |
| Gate only the three properties the library leaves unescaped | It pins our correctness to a table inside a dependency. The next release that reclassifies a property, or the next property this operation writes, silently reopens it. |
| Charge the interpolated output once, after `interpolate` returns | That is where it was charged, and it is why eighty megabytes could exist under an eight-megabyte ceiling. A ceiling consulted after the allocation reports the breach; it does not bound it. |

## Consequences

Four bounds that were descriptions become measurements, and each is asserted the
way spec 022 already asserted the image decoder's: with a counting allocator and
a peak, because "the extent is checked before a range is materialized" and "a
repeat is charged as it repeats" are statements about memory that a refusal code
cannot distinguish from a refusal issued too late.

The costs are real and worth naming. A spreadsheet's cells are now held in a
`HashMap` rather than a `Vec`, which is more memory per cell for a dense sheet
and much less for a sparse one — and it is charged per cell, which the dense
range never was. A PDF render spawns a thread, and a render that outruns its
deadline leaves that thread finishing work nobody will read; the CPU is spent
either way, and what changed is that the activity no longer waits for it.
`LocalArtifact` grew a second claim field, so every producing capability now
reads `claim_session_key` from its input the way it already read `claim_role` —
a pending upload is claimed on `session_role` *and* `session_key` with
`IS NOT DISTINCT FROM`, so a row recorded with a hard-coded `NULL` was one no
identified session could ever claim.

The general rule this leaves behind: when a bound's cost lives inside a
dependency, the bound belongs on our side of the call. The three shapes that
keeps taking are the ones above — answer the size before the allocation, state
the range in one place both callers read, and put the observer outside the thing
that cannot observe.
