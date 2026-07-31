---
type: decision
status: accepted
date: 2026-07-31
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# Command effect assignability compares resolved fields, not type spelling

## Context

A Command argument declared `[ReturnRequestLine!]!` keeps its nominal
reference to the declared input object. A Process input declared with the same
type publishes its contract with that object inlined as fields. Both YAML
declarations name the same type, but the two contracts reach the effect
validator in different shapes.

The validator compared `Ref` against `Ref` and `Object` against `Object`, and
rejected every mixed pair. Passing a typed list of declared objects from a
Command into a Process was therefore impossible — the exact case a return
request needs — and the diagnostic read
`[ReturnRequestLine!]! is not assignable to [object!]!`, which names two types
an author cannot tell apart in metadata.

## Decision

When one side of an effect binding is a named reference and the other inlines
an object, the validator resolves the reference against its own named-object
table and compares the resolved fields. A nominal reference and its resolution
describe one contract, so which side happened to keep the name is not a
compatibility question.

Everything else stays exact. Field sets must still match in full, a required
target field still rejects an optional source, scalars still follow the same
widening table, enums still compare nominally, and recursion is still bounded
by the existing visited-pair guard. Only the spelling of an otherwise identical
object contract stops being a rejection reason.

## Alternatives

| Option | Why Not |
| --- | --- |
| Make Process inputs keep the nominal reference | The Process contract is built from resolved value contracts; reintroducing names there would fork the one value-type owner. |
| Make Command arguments inline their objects | Loses the declared type identity that argument validation and the published schema both use. |
| Require authors to restate the object inline in the Process | Two spellings of one type drift apart silently, and the duplicate is what the compiler was meant to check. |
| Compare by name only when both sides are named | Leaves the mixed case rejected, which is the only case that actually occurs. |

## Consequences

A declared input object can now cross the Command-to-Process boundary as a
typed list or field, so a Process input contract can be as rich as the domain
needs instead of being limited to scalars.

Structural equality now admits two objects with the same fields under different
names. That is consistent with how the Process contract already treats them —
it never sees the name — and named enums remain nominal, so the closed
vocabularies that carry meaning are unaffected.
