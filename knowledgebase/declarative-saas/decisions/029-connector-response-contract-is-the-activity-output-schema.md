---
type: decision
status: accepted
date: 2026-07-31
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A connector response contract is the activity's output schema

## Context

A connector operation declares its response as named fields with types, each
bound to a JSON pointer into the provider body. The extractor copied a field
into the activity output only when the pointer resolved: a declared optional
field the provider omitted simply did not appear, and a declared non-null field
the provider sent as an explicit `null` passed as satisfied.

Downstream, a Process reads those fields by name — `{ item: failure_code }`,
`{ state: request_labels, field: tracking_number }`. A key that exists only
when the provider felt like sending it makes an optional field unreadable
exactly when it is absent, which is the case the declaration exists to
describe. In the Petshop fulfilment module this bound the carrier's optional
`failure_code`: a successful label omitted it, the binding failed as "for_each
item field `failure_code` is absent", and because that failure happens while
*preparing* a transition rather than while running one, the durable runtime
retried it forever. The Process wedged with every activity job reported
`succeeded`.

## Decision

The declared response is the schema of the activity's output, not a filter over
whatever the provider happened to send. Every declared field appears in the
output. A declared optional field the provider omitted is published as an
explicit `null`. A declared non-null field is not satisfied by an explicit
`null` any more than by an absent pointer; both are contract violations
classified as `validation`.

This keeps the shape a Process binds against equal to the shape the metadata
declares, independent of the provider's response variance.

## Alternatives

| Option | Why Not |
|--------|---------|
| Make `{ item: field }` yield null for any absent key | Weakens every binding, including typo detection on fields the contract never declared. The absence should be impossible, not tolerated. |
| Require providers to send every optional field | Not ours to require; "optional" is precisely the field a provider may omit. |
| Leave the required/explicit-null hole open | A non-null declaration that a `null` satisfies is not a contract. |

## Consequences

Optional response fields are always readable, so a Process can branch on their
absence. Providers that send `null` for a declared non-null field now fail the
activity as a validation error rather than propagating a null into a contract
that forbids it — visible as a behaviour change for any operation whose
provider did that. The output of an operation with declared response pointers
grows by its absent optional fields, which is bounded by the declaration.
