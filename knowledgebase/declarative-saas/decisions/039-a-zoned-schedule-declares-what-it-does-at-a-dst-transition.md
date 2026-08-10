---
type: decision
status: accepted
date: 2026-08-09
features:
  - "[[declarative-saas]]"
---

# A zoned schedule declares what it does at a DST transition

## Context

Cron triggers (Spec 001) were UTC-only: the workspace carried `chrono` with
`clock` and nothing else, so `0 9 * * 1-5` meant 09:00 UTC and nothing could
express "09:00 in Berlin". Half of Europe's year that is 10:00 local and the
other half 11:00, which is not a schedule anybody wrote down. Spec 021 §2 adds
the IANA database so a schedule can be a wall-clock schedule.

A wall-clock schedule is only well defined for 8758 of the 8760 hours in a
year. The two exceptions are the transitions: in spring an hour of local time
does not happen, so a schedule naming a time inside it has no instant to fire
at; in autumn an hour happens twice, so it has two. Every scheduler has to
answer both, and most answer them by accident.

`croner`, which we already use, answers them — but by inspecting the schedule's
shape. It classifies a pattern as fixed-time or interval, and gives them
opposite behaviour: a fixed-time job snaps forward out of a gap and takes only
the earlier instant of an overlap, while an interval job silently skips the gap
and fires at both instants of the overlap. So `0 2 * * *` and `0 2 * * *` with
a second field, or `30 2 * * *` and `*/30 2 * * *`, differ in whether a nightly
run happens at all on one night a year — decided by a classification the
metadata never mentions and the operator never sees.

## Decision

A cron trigger may declare `timezone: <IANA name>`, and a trigger that declares
one **must** declare both DST policies:

```yaml
schedule: "0 9 * * 1-5"
timezone: Europe/Berlin
dst:
  skipped_time: fire_after_gap | skip
  repeated_time: fire_at_first | fire_at_second
```

There is no default for either, in the metadata or in the code: `dst` has no
serde default, the loader refuses a `timezone` without it (and a `dst` without
a `timezone`, which nothing would read — ADR-034), and `next_occurrence`
returns `None` rather than choosing for a trigger that reached it without one.
An absent `timezone` is untouched UTC: the same function as before, on the same
path, which is what every deployment that exists today runs.

The spellings say what happens, in runs:

- `fire_after_gap` — one run, at the instant the gap ends. Late by the width of
  the gap, never lost. The answer for work that must happen every day.
- `skip` — no run, and the dropped local time is logged at materialization. The
  answer when a late run is worse than none, or when the next one is along
  shortly anyway. It is explicit, and it is loud; what it must never be is
  silent.
- `fire_at_first` / `fire_at_second` — exactly one run, at the earlier or the
  later of the two instants carrying that local time. `first` keeps the usual
  distance from the runs before it, `second` from the rest of the new day.

To apply these ourselves we drive `croner` over naive wall-clock time — a naive
value read as UTC, where every local time exists exactly once — and resolve the
zone afterwards, so the library's own classification never runs. The search
starts two hours before the local time of `after`, because under
`fire_at_second` an occurrence whose wall-clock time has just passed can still
be in the future; candidates resolving to an instant at or before `after` are
discarded. The gap end is found by bisecting a day either side of the missing
local time for the first instant whose local time has reached it, which needs
no assumption about the width or the shape of the transition.

The tz database version is pinned twice: the crate exactly (`=0.10.4`, so
`cargo update` cannot move it) and `IANA_TZDB_VERSION` (`2025b`), the release it
embeds, which is the value that actually decides when a schedule fires.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep UTC only and tell authors to change the hour twice a year | A metadata edit twice a year, in every deployment, that is only ever noticed by being forgotten. |
| Take `croner`'s zoned behaviour as-is | Two different policies chosen by pattern shape, documented nowhere in the metadata; the operator cannot see which one their schedule got, and a harmless-looking edit to the expression flips it. |
| One `dst` policy covering both cases | They are independent choices. A nightly backup wants `fire_after_gap` and `fire_at_first`; a report that must land inside its own day wants `skip` and `fire_at_second`. |
| Default the policies (say, gap → fire, overlap → first) | The default would be right for one of the two schedules above and wrong for the other, and being wrong is invisible until the one night a year it matters. Spec 021 asks for explicit metadata for exactly this reason. |
| A third overlap policy, `both`, firing at each instant | It is what an interval schedule arguably wants, and it is the one thing the requirement forbids: a schedule that says once a day fires twice. An interval schedule dense enough to care is better expressed in UTC, where the hour is not repeated at all. |
| Resolve the zone from the request or the environment (`TZ`) | A timezone is declared data. The engine has no ambient caller context at materialization time, and taking one from `TZ` makes the schedule depend on the container's locale. |
| Store the zone as a parsed `Tz` in metadata | Metadata round-trips to YAML byte-for-byte with the Donat export; a string plus a load-time check gives the same refusal without a custom serde impl. |
| Validate the zone name in the server instead of the loader | The boot would succeed and the trigger would then be dropped every tick with a warning. Refusing at load is where every other structural check lives. |

## Consequences

`chrono-tz` joins the workspace, in the metadata crate (to refuse an unknown
zone at load) and the server (to resolve occurrences). It carries the whole
database, so the binary — and the wasm plan core through the metadata crate —
grows by roughly its size. Filtering the zone set would make a deployment's
valid zone names depend on a build feature, which is worse than the megabyte.

A zoned schedule costs more per materialization tick than a UTC one: up to
about 120 discarded candidates for a per-minute schedule, because of the
two-hour back-off, each one a cheap in-memory match with no round trip. The
back-off is uniform rather than conditional on the overlap policy, so the
correctness argument does not depend on which policy is declared.

Two things this does not do. `donat.cron_events` still stores the instant, so
the catalog and the webhook envelope stay UTC — the zone decides *when*, and is
not carried into the delivery contract. And the pinned tz version is covered by
a sentinel test only; it is not in a deployment fingerprint, because the engine
has no engine-wide fingerprint to put it in (the ones that exist are per
connector operation and per process revision). Spec 021 asks for one for both
the tz database and the phone metadata (ADR-038); when it is built, both pins
should feed it.

One part of this is superseded by
[[060-a-repeated-span-collapses-a-named-time-not-a-cadence]]: the
`repeated_time` policy is applied to a local time the schedule *names*, not to
every candidate the repeated span made ambiguous, and the gap end is bisected to
the transition instant exactly rather than to a one-second width. Both policies,
their mandatory declaration, and the UTC path are unchanged.

Process timers and report date bucketing, the other two things Spec 021 §2
lists, are not covered here. They are separate paths and will need the same
question answered when they gain a zone; the two enums are theirs to reuse.
