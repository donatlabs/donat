---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A `PUT` against a fixed identity is the method half, and a version pin belongs to the operation

## Context

Spec 028's forms half added four connectors — `jotform`, `surveymonkey`,
`cal_com`, `acuity` — and between them they publish five mutating operations.
Four of the five are the same shape the effect gate has met many times: a
provider documents *what* an endpoint does and says nothing at all about what a
second identical request does. Two of those four are deletes
(`jotform.submission.delete`, `surveymonkey.response.delete`), one is a cancel
over `POST` (`cal_com.booking.cancel`), and the classification for all three is
the one `trello.card.delete` already carries.

The fifth is different, and it is why this ADR exists.
`acuity.appointment.cancel` is `PUT /api/v1/appointments/{id}/cancel` — a `PUT`
against a fixed resource identity, which is precisely the *method*
[[042-the-effect-gate-admits-evidence-not-methods]] and spec 010 §7 name for
`ProviderIdempotent::NaturalMethod`. Nothing about the request shape is wrong.
What is missing is the sentence. Acuity's reference publishes exactly one
statement adjacent to repetition — "Once canceled, appointments will have a
`noShow` attribute. This attribute may be updated, but it isn't possible to
un-cancel the appointment." — and that sentence is about a *different*
operation. It says the state is terminal. It does not say a second cancel is
absorbed, and it does not say whether the cancellation e-mail and SMS Acuity
sends by default are sent again.

The same batch's other half met the same shape independently — Clockify
publishes `PUT /workspaces/{ws}/time-entries/{id}` against one fixed identity and
says nothing at all about a repeat — and `providers/INVENTORY.md` now names it as
a fifth near-miss shape beside the four
[[067-a-retention-with-an-escape-clause-is-not-a-minimum-retention]] and
[[080-a-deduplication-that-lapses-when-the-incident-is-resolved-is-not-a-retention]]
already record. Acuity is the harder of the two, and that is why it needs the
decision written down: Clockify is silent, while Acuity publishes a
repetition-adjacent sentence about a *neighbouring* operation that reads like a
repeat statement to anyone who does not check which operation it is about. A
reviewer reaching for the class here finds the method obviously right and a
sentence that seems to finish the argument, which is exactly the moment the gate
is easiest to widen by accident.

Cal.com brought a second, unrelated problem. `cal-api-version` is a **required**
header on every v2 endpoint — "If you omit it, requests will return a 404" — and
its correct value is published *per endpoint*: `2026-05-01` on the booking
collection, `2026-02-25` on the booking read, create and cancel, `2024-06-14` on
the event types. Cal.com's own agent guide contradicts all three with a blanket
"Always include `cal-api-version: 2024-08-13`". Every connector in this
workspace that pins an API version so far pins one value for the whole
connector — an `Api-Version` constant, a `/v3` path prefix — so there was no
precedent for a version that is a property of an operation.

## Decision

**`ProviderIdempotent::NaturalMethod` is refused for a `PUT` or `DELETE` against
a fixed identity when the provider publishes no repeat statement, and the
refusal is recorded as such.** The method is a precondition, never the evidence.
`acuity.appointment.cancel` is declared, typed, tested and `InventoryOnly`, and
its recorded reason names the method it satisfies, the sentence that is missing,
and the near-miss sentence a reviewer will otherwise mistake for one. ADR 063's
`AtMostOnce` is refused for the same operation on its own bar: the class is
admitted on a **recorded consequence** of a second send, and "the provider does
not say" is the absence of one rather than one. `INVENTORY.md` records the case
under its own heading so the next reviewer meets the argument rather than
re-deriving it.

The same rule disposes of the other three silent writes without further
argument, and the register now names four in one half:
`jotform.submission.delete`, `surveymonkey.response.delete`,
`cal_com.booking.cancel`, `acuity.appointment.cancel`.

**A provider-required API version is declared as a static header of the
operation, not of the connector, whenever the provider publishes it per
endpoint.** `cal_com` declares three different `cal-api-version` values across
six operations, each taken from the OpenAPI parameter description of the
endpoint that owns it. This is
[[073-a-retention-is-read-from-the-reference-that-owns-the-operation]]'s rule
applied to a second per-endpoint fact: the reference that owns the operation
wins, and a provider's general guide does not override its own endpoint
reference. `cal_com_version_header_is_per_operation` holds each of the three
shut, and also holds shut the thing that would make the pin worthless — no
operation publishes a version input, so no Process can move a request onto an
endpoint version this connector was not written against.

## Alternatives

| Option | Why Not |
|--------|---------|
| Admit `acuity.appointment.cancel` as `NaturalMethod` on the method plus "it isn't possible to un-cancel" | The sentence is about un-cancelling, not about cancelling twice, and the class promises the activity worker that a resend is absorbed. Reading a terminal-state note as an absorption promise is inventing the provider's contract, which is what ADR 042 exists to stop. |
| Admit it as `AtMostOnce` with a guessed consequence such as "a second cancellation e-mail" | The consequence sentence is what an operator accepts when they write `at_most_once: true`. A guessed one is worse than no class: it is a promise nobody made, presented as documentation. Acuity's `noEmail` parameter proves an e-mail is normally sent — it does not prove a *second* one is. |
| Add a fifth effect class for "documented terminal state, repeat unstated" | Spec 010 §15 already names one open class (a repeat-safe write over a method HTTP does not define repeat-safety for) and this is not it. A class per shade of provider silence is a taxonomy, not a gate. |
| Pin one `cal-api-version` for the whole `cal_com` connector | Three of its six operations would send a value their own reference does not name, and Cal.com answers a wrong or missing version with `404` — a `permanent` failure a Process would read as "the booking does not exist". A wrong answer, not a failure. |
| Follow the agent guide's blanket `2024-08-13` because it is more recent prose | It contradicts the machine-readable description of every endpoint declared here. ADR 073 already settled which one wins for a per-endpoint fact. |
| Make the version a declared input so a deployment can move it | An input that changes which contract the provider applies would let a Process reach a response shape the declared output pointers were never written against. The version is declaration material for the same reason the origin is. |

## Consequences

Four provider writes across four connectors are declared and unreachable, and
one of them will look wrong to a reviewer who checks the method and stops. The
module's recorded reason and `INVENTORY.md`'s own section both exist to make
that reader finish the check; `acuity_effects_are_classified` asserts the method
is `PUT` *and* the class is `InventoryOnly` in the same case, so the two facts
cannot drift apart quietly.

Two writes are reachable: `cal_com.booking.create` and
`acuity.appointment.create`, both `AtMostOnce`, both carrying the consequence
their provider's own documentation supports. Those are the two operations spec
028 §2 predicted a Process would actually drive; the cancel half of that
prediction is not available, and a deployment that needs it has to reconcile
through the booking read instead.

`cal_com` is the first connector whose API version differs per operation. The
cost is that adding an operation to it means reading that endpoint's own version
rather than copying the one above, and the test that would catch a copy is
already written.
