---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[021-value-semantics]]"
---

# A recurrence rule is bounded before it is expanded, and it meets a DST transition with the answer cron already gave

## Context

Spec 021 §3 adds `local.recurrence`: `expand`, `next` and `validate` over RFC
5545, all `Pure`. Subscriptions, bookings, shifts and lesson schedules all need
recurrence, and almost nobody implements it correctly by hand, so the expansion
is `rrule`'s. Two things are ours, and both are the reason this is more than a
wrapper.

The first is boundedness. `FREQ=SECONDLY` with neither `UNTIL` nor `COUNT` is a
rule that never ends. Every library answers it the same way — by iterating until
somebody stops it — and every wrapper that "bounds" it does so by truncating
after N results, which turns an unanswerable question into a wrong answer. Spec
021 §3 says instead that such a rule "is rejected at declaration time, not
discovered at expansion time", and that "every expansion is bounded by both a
window and a maximum occurrence count".

The second is the interaction with ADR 039. A daily occurrence at 02:30 local
has no instant on the spring transition day and two in autumn. `rrule` answers
both, silently and by construction: `add_time_to_date` first asks for the single
instant carrying a local time, and when there is none — which happens in *both*
cases, because `chrono` reports an ambiguous time as not-single — it falls back
to local midnight plus the wall-clock offset as a duration. In spring that lands
at 03:22:10 for an 02:22:10 rule (the gap's width, added to the time rather than
to the gap's end); in autumn it lands on the first instant. Two policies, chosen
by arithmetic, mentioned in no declaration — which is exactly the complaint ADR
039 made about `croner` classifying a schedule by its shape.

`Pure` adds a third constraint that turned out to be sharper than it looks. The
same library reads a `DTSTART` without a zone, and an `UNTIL` without a `Z`, in
`Tz::LOCAL` — the *machine's* timezone. An operation whose answer depends on the
container's `TZ` is not deterministic, and ADR 044 admits `Pure` on a double
render rather than on a promise.

## Decision

**Boundedness is arithmetic over the declaration, and it is the capability's own
unit count.** Before any occurrence exists, the worst case a rule can produce
between its start and the end of its window is computed from the rule's parts:
the period of its `FREQ` times its `INTERVAL` gives a step, the `BY*` parts that
*expand* the period (RFC 5545 §3.3.10's expand/limit split) give a per-period
multiplier, `BYSETPOS` tightens it, and `COUNT` and `UNTIL` cap it. That number
is what `LocalOperation::units` returns, so spec 018 §4's unit ceiling refuses an
unbounded rule *before the executor is entered* — the same gate, with the same
`validation` class, that refuses an oversized input. A policy's own
`max_occurrences` refuses it a second time, tighter, inside the run. `validate`
exists to be the operation a deployment calls before it stores a rule a user
wrote, and it is deliberately outside the unit gate, because an operation whose
job is to explain a refusal must not be refused before it can.

Every estimate rounds up, and the rounding is tested: `FREQ=WEEKLY;BYDAY=MO,WE,FR`
over a year is admitted at 318 and produces 156. A ceiling that can be walked
past is not a ceiling, so an under-count would be a defect and an over-count is
only a policy that has to be one number more generous.

Boundedness is therefore a property of the *pair* — a rule and a policy — not of
`FREQ`. `FREQ=DAILY` forever is a schedule under a policy admitting 400
occurrences over a year, and is refused under one admitting 64. That is the
honest reading of "bounded by both a window and a maximum occurrence count": the
declaration says how much answer this deployment will ever produce, and a rule
either fits or is refused where it is declared.

**The DST answer is ADR 039's, in ADR 039's words.** A recurrence policy declares
`timezone` plus `dst: { skipped_time, repeated_time }`, with the same four
spellings and the same meanings; the metadata type *is* `CronDstPolicy`, so
there is one declaration to learn and one place the semantics are written down.
The loader refuses a `timezone` without `dst` and a `dst` without a `timezone`,
which are ADR 039's two refusals verbatim. To apply them we do what
`crates/server/src/cron.rs` does with `croner`: drive the expansion over naive
wall-clock time — a naive value read as UTC, where every local time exists
exactly once — and resolve the zone afterwards ourselves, so the library's own
classification never runs. The gap end is found by the same bisection, for the
same reason.

Two consequences of doing it ourselves are visible in the answer. `UNTIL` is an
instant by RFC and the iteration is over wall clock, so the rule carries the
*local image* of its `UNTIL` and the instant bound is applied again after
resolution; without the rewrite the sequence would be cut early by the zone's
offset. And under `fire_after_gap` two wall-clock times inside one gap resolve to
the same instant — a declaration that says once must not happen twice, so the
second is reported in `skipped` beside the ones `skip` declined. Nothing is ever
dropped in silence, which is the half of ADR 039 that is not about which instant
wins.

**Nothing ambient may reach the answer.** The rule text is the `RRULE` property
value alone: a `DTSTART`, a property prefix, a second line, and a floating
`UNTIL` are all refused, because each is a door to `Tz::LOCAL`. The start is a
wall-clock time with no offset, the window and `next`'s `after` are declared
instants, and the current time is read nowhere — which is what makes `Pure`
true here rather than merely claimed.

## Alternatives

| Option | Why Not |
|--------|---------|
| Bound an expansion by truncating after N occurrences | It answers an unanswerable question with a plausible-looking list. A booking screen showing 200 of 31 million occurrences is worse than an error, because nobody looks for the error. |
| Refuse a list of "pathological" frequencies (`SECONDLY`, `MINUTELY`) | A blocklist is wrong in both directions: `FREQ=SECONDLY;COUNT=10` is a perfectly bounded rule, and `FREQ=DAILY;BYHOUR=0..23;BYMINUTE=0..59` is 1440 a day with neither name on the list. The arithmetic answers both without a list. |
| Discover the bound by iterating with a cap and reporting whether the cap was hit | That is the discovery spec 021 asks us to replace. It also costs the work before it detects it, and the deadline — not the count — becomes the real bound. |
| Compute the bound over the window only, ignoring the distance from the rule's start | `rrule` has no seek: an occurrence before the window still costs a step, so a bound that ignores the run-up is not a bound on the work. Measuring from the start is what makes the number mean "occurrences this expansion will walk". |
| Let the rule text carry its own `DTSTART` | It is the shortest path to `Tz::LOCAL`: a `DTSTART` without a zone is read in the machine's timezone, so the same input would expand differently on two replicas and `Pure` would be a lie the double render cannot catch (both renders read the same wrong clock). |
| Accept a floating `UNTIL` and read it in the policy's zone | It looks helpful and it is a second spelling for one instant. RFC 5545 §3.3.10 already requires `UNTIL` in UTC once `DTSTART` is zoned, so the refusal costs an author nothing and closes the same ambient door. |
| Take `rrule`'s DST behaviour as-is | It is one undeclared policy for the gap ("midnight plus the offset", which is neither of ADR 039's spellings) and another for the overlap ("the first instant"), decided in a helper function no metadata mentions. ADR 039 refused exactly this from `croner`; accepting it from a second library would mean a deployment's cron trigger and its recurrence rule answer the same night differently. |
| Invent a recurrence-specific DST vocabulary | Two names for one question, and a deployment that has answered it for its cron triggers would have to answer it again in a different dialect. The type is literally shared: a policy declares `CronDstPolicy`. |
| Expand directly in the zone and post-process the results | The library's resolution has already happened by then, and its output cannot be un-resolved: a 03:22:10 instant does not say whether it came from a gap or was always there. The wall-clock drive keeps the decision where the declaration is. |
| Put the DST enums in one crate and share them | `donat-metadata` and `donat-connectors` do not depend on each other by design (ADR 044), so the two enums are declared twice and mapped in the serving binary — the same seam `CodeTemplateSpec` and `IngestSchemaSpec` already cross. |
| Declare the rules themselves in `recurrence.yaml` | A recurrence rule is usually a user's: the customer picks "every second Tuesday". What must not be the user's is the zone, the DST answer, and the ceilings, so those are the declaration and the rule is data — checked against the declaration on the way in. |
| Let a process compute which policy it expands under | Then the run chooses its own DST answer and its own ceiling, one expansion at a time. A literal name from the deployment's declarations keeps `validate` able to typecheck it, exactly as ADR 050 and 051 require for templates and code origins. |
| Return a `truncated: true` flag instead of the invariant failure | The admission proves the expansion fits, so a truncation would mean the estimator and the expander disagree. That is a defect in this module, and a flag would let it ship as a slightly short answer. |

## Consequences

`rrule 0.14` joins the workspace, pinned exactly and with `default-features =
false` (no `cli-tool` binary, no deprecated `EXRULE`), and `chrono`/`chrono-tz`
join `donat-connectors` — the same tz database, at the same pinned release, that
decides when a zoned cron trigger fires. A deployment gets recurrence for one
`recurrence.yaml` section and pays for it in one declaration per policy rather
than one per rule.

The cost is a real authoring burden in one place: a policy has to state a
ceiling, and a rule that does not fit is refused rather than trimmed. That is
the intended pressure — it makes "how much answer can this produce" a question
somebody answers at deploy time instead of one a customer discovers.

Two duplications are deliberate and worth naming. The gap-end bisection exists
in `crates/server/src/cron.rs` and again here, because the crates do not depend
on each other; if a third caller appears it should become a shared crate rather
than a third copy. And the worst-case estimator is a second model of RFC 5545's
expansion rules beside `rrule`'s own — kept sound by rounding up everywhere and
by a test that expands fourteen rule shapes for real and compares the produced
count against the number each was admitted on.

What this does not do: `EXRULE`, `RDATE`, `EXDATE` and `VTIMEZONE` are out — a
set is a different declaration from a rule, and the ones that carry their own
zone definition carry a second tz database with them. Process timers and report
date bucketing, the other two things spec 021 §2 named, still do not take a
zone; when they do, the two enums are theirs to reuse, which is now the third
place that sentence applies.
