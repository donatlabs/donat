---
type: decision
status: accepted
date: 2026-08-09
features:
  - "[[declarative-saas]]"
---

# The connector failure type has one definition, and query keys travel literally

## Context

Migrating the existing `http` and `stripe` connectors onto the SDK
([[037-connectors-are-written-by-hand-against-provider-documentation]]) exposed
two places where the server and the SDK disagreed about the same contract.

The first was the failure itself. `crates/server/src/connectors/mod.rs` defined
`ConnectorErrorClass` and a `ConnectorFailure` whose `safe_message` was a
`String`; the SDK defined its own pair with a `&'static str` message and a
`Retry-After` clamped at 86,400 seconds. Two definitions of one closed contract
is a defect regardless of which is better: a Process declares `retry_on`
against these names, a connector returns one of them, and the journal records
one of them, so they have to be the same type.

The second was on the wire. `http.rs` percent-encoded query *keys* as well as
values, so an operation declaring `api-version` sent `api%2Dversion=…`. The SDK
validates the key when the operation is built and then emits it as written.

## Decision

`donat-server` re-exports the SDK's `ConnectorErrorClass` and
`ConnectorFailure` and deletes its own, exactly as it already re-exports
transport. The `&'static str` message is the reason to prefer the SDK's type
rather than a cost of adopting it: a message borrowed from a provider response
does not typecheck, so "no provider text is ever forwarded" is enforced by the
compiler instead of by review. Nothing in the server needed a message built at
runtime — every message it produces is a literal written in this workspace, and
the redacted diagnostic that carries provider *facts* (status, retry-after,
correlation IDs) is assembled from typed fields. The clamp comes with the type,
so a provider can no longer propose an eleven-day retry delay to the durable
worker.

A declared query key travels to the provider exactly as declared. RFC 3986
makes `%2D` and `-` equivalent only after normalization, and query-string
parsers do not normalize: a provider matching on `api-version` saw an unknown
parameter and a missing required one. The key is validated at build time
against a closed character set, which is what makes emitting it literally safe;
values remain percent-encoded per value.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep both failure types and convert at the boundary | A conversion is where the `String` message would come back, and it puts the redaction guarantee back into review |
| Give the SDK type public fields so existing call sites compile unchanged | The clamp and the correlation-ID truncation are enforced in the constructors; public fields make them advisory |
| Keep encoding query keys | It is the more conservative-looking option but it is the one that changes the wire away from what the provider documents |
| Percent-encode keys only when they contain a reserved character | The key is already restricted to a safe character set at build time, so the branch would never fire on a valid declaration |

## Consequences

Every call site reads the failure through accessors rather than fields, which
is a mechanical change across the process worker and its tests; no assertion
changed meaning. A single type also means a future eighth-class or diagnostic
change lands once.

The query-key change is a deliberate, tested wire change rather than a snapshot
drift: an operation declaring a key containing `-`, `.`, `_`, or `[]` now sends
the key the provider documents. No deployment could have depended on the old
form, because no provider documents a percent-encoded parameter name. A key
outside the validated character set is now refused at startup instead of being
escaped into something a provider cannot match.
