---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A repeated span collapses a named time, not a cadence

## Context

[[039-a-zoned-schedule-declares-what-it-does-at-a-dst-transition]] made both DST
answers explicit metadata and applied them itself, over naive wall-clock
candidates, so `croner`'s own shape-based classification never runs. Two things
about the way it applied them were wrong in a way only a review of the code
found, because the tests it shipped with picked the one wall-clock time that
hides both.

The gap end was bisected to a **one-second** width and the upper bound returned.
That answer is up to a second past the transition, and *which* fraction of a
second depends on the candidate's own local time, because the bisection window
is centred on it. `30 2 * * *` in Berlin happens to land on the transition
exactly; `0 2 * * *` lands 0.219726562s late and `59 2 * * *` 0.424804687s late.
The materializer's idempotence is the `(trigger_name, scheduled_time)` unique
key, so two candidates inside one gap that resolve to two different instants are
two rows: `* * * * *` with `fire_after_gap` in a two-hour-gap zone
(`Antarctica/Troll`) delivered the same logical occurrence again and again, one
delivery per swallowed minute, and every zoned schedule wrote a
fractional-second `scheduled_time` into the catalog.

The repeated-time policy was applied to every candidate whose local time is
ambiguous. For a schedule that names one time a day that is exactly right. For
`* * * * *` in Berlin under `fire_at_first` it means every local time in the
repeated hour resolves to its *first* instant, all of which are at or before the
run that already happened — so the loop walked past the whole span and the job
ran last at `2026-10-25T00:59:00Z` and next at `02:00:00Z`. A per-minute job
blacked out for 61 minutes, and for 121 in `Antarctica/Troll`. `fire_at_second`
has the same hole at the other end of the span.

## Decision

**A gap ends at one exact instant, and the bisection runs to it.** The search is
over `DateTime`'s own resolution — a one-nanosecond width, about fifty offset
lookups for a two-day window — so what comes back is the transition instant
itself rather than a value near it. Every local time the gap swallowed therefore
produces the identical instant, which is what makes ADR 039's "one run, at the
instant the gap ends" a property of the delivery rather than of the wording: the
unique key collapses the candidates because they are equal, not because
something de-duplicated them. The same exact bisection finds the start of a
repeated span.

**The repeated-time policy applies to a named local time, and a schedule that
matches the repeated span more than once has not named one.** ADR 039 says the
policy is about "a *named local time* occurring twice"; it is now applied to
exactly that case. When a candidate is ambiguous, the span the transition
repeated is computed exactly, and the schedule is asked how many wall-clock
times it matches inside it. One (or none but this one) is a named time: the
declared `fire_at_first` / `fire_at_second` picks its instant and the other pass
is not a run, unchanged. Two or more is a cadence: there is no ambiguity for the
policy to resolve, both passes are runs, and the schedule keeps its interval
across the span. `0 * * * *` is the first case and still runs once; `* * * * *`
and `*/2 * * * *` are the second and no longer stop for the width of the span.

Deciding this on *what the schedule matches in that zone's actual span* is
deliberately not what ADR 039 refused. What it refused was `croner` classifying
a **pattern's shape** — `0 2 * * *` and `*/30 2 * * *` getting opposite gap
behaviour from a distinction the metadata never mentions. This rule reads the
same matches the engine is about to fire, against the same transition, and it
changes nothing for a schedule that names a time: both declared policies keep
their exact meaning, which is why every ADR 039 test still passes unchanged.

One consequence of admitting both passes had to be fixed with it: candidates are
enumerated in wall-clock order, and inside a repeated span that is not instant
order — the second pass of 02:00 comes after the first pass of 02:01. The search
now keeps the earliest run found rather than returning the first one it sees,
and stops as soon as a candidate's earliest possible instant has reached it.

## Alternatives

| Option | Why Not |
|--------|---------|
| Round the one-second bisection result to the second | The transition is not guaranteed to be the second the rounding picks, and it would still be arithmetic standing in for the answer. Bisecting to the nanosecond costs about thirty more comparisons and *is* the answer |
| De-duplicate deliveries in the materializer instead (one row per gap, by trigger and date) | A second idempotence rule beside the unique key, applying to one case, which the delivery loop would have to keep in step with the resolver. Equal instants need no rule |
| Add ADR 039's rejected `both` policy and let the metadata choose | It puts the question to the operator twice: they already declared what a repeated time does, and the second declaration would be about a distinction ("is my schedule a cadence?") the engine can read off the schedule |
| Keep ADR 039 literal — a per-minute job blacking out for an hour is the declared policy working | The policy is stated in ADR 039 as being about a named local time occurring twice. A schedule that matches all sixty minutes of the repeated hour names none of them, and the outcome is a blackout no operator declared |
| Classify by the expression's shape, as `croner` does | Exactly what ADR 039 refused: a harmless-looking edit to the expression flips the behaviour, invisibly |
| Ask the schedule for its period once, instead of per span | The answer depends on the span (a 30-minute Lord Howe span and a two-hour Troll span disagree about the same schedule), and the span is the thing the policy is about |

## Consequences

`fire_after_gap` now delivers one webhook per gap in every zone and at every
cadence, and `scheduled_time` is a whole second again. A per-minute schedule in
a zone keeps firing every minute through both DST transitions, in both repeated
policies. A schedule that names a time is untouched: same instants, same single
run, same reported skips.

A zoned schedule costs more per materialization tick during a transition. Each
ambiguous candidate now resolves its span (about fifty offset lookups) and asks
the matcher for two occurrences inside it, and the search visits one candidate
past the answer to prove it is the earliest. For a per-minute schedule that is a
few milliseconds per tick, for the two hours a year the span exists, and nothing
in it touches the database or the network. Caching the decision per span would
remove almost all of it and was left out: one more piece of state in a function
whose correctness is the point, for a cost that only exists twice a year.

This supersedes ADR 039 in one place only — that the `repeated_time` policy
applies to *every* ambiguous candidate. Everything else it decided stands: both
policies stay mandatory with no default anywhere, the gap-and-overlap answers
stay metadata rather than pattern shape, the UTC path stays untouched, and the tz
database stays pinned twice over.
