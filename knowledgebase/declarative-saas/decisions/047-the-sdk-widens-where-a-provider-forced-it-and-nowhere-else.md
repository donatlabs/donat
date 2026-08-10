---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# The SDK widens where a provider forced it, and nowhere else

## Context

Five gaps in the connector SDK were found the only way this kind of gap is
found: by writing a real connector against a real provider's published
documentation and discovering that the declaration could not say what the
provider does. Each one had a concrete provider behind it, and each had been
recorded in a module comment and pinned by a test that asserted the *wrong*
behaviour, so the gap could not quietly become the contract.

* `Operation::decode_response` always parsed JSON, so a documented empty-bodied
  success — SendGrid's `204` list delete and `202` mail send, Typeform's `200`
  response delete — was reported as a `validation` failure.
* No plan expressed "the continuation URI is in the body". SendGrid publishes
  `_metadata.next` and Twilio publishes `next_page_uri`; `TokenInBody` would
  send either back as a query *value*, which both providers reject.
* `PageNumber` started every walk at page 1. Twilio's `page` is zero-indexed, so
  the plan there would have returned the collection without its first page — a
  wrong answer rather than a failure.
* `HttpMethod` had no `HEAD`, so Amazon's `HeadObject` could not be declared.
* A header value could not bind from operation input, so Amazon's `CopyObject`,
  which carries its source in `x-amz-copy-source`, could not be declared either.

## Decision

Each gap is closed as narrowly as the provider evidence justifies, and every
widening keeps the boundary it was tempting to relax.

**A no-content success is declared per status, not per operation.** SendGrid
answers one list delete with `200` and a job identifier *or* `204` and nothing,
so a flag on the operation could not describe it. `no_content_statuses` narrows
statuses the operation already admits — it never adds one — and an empty body at
such a status decodes exactly as `{}` would, so the empty case and the
`{}` case cannot diverge. An operation declaring one may not declare a
*required* output pointer: silence cannot satisfy a required field, and
publishing it as absent would be the null the SDK refuses everywhere else. An
operation that declared no such status still fails on a missing body.

**A body-carried next URI is a destination, and is treated as one.**
`NextUriInBody` resolves the value against the compiled origin and then checks
it, sharing one `resolve_continuation` with `LinkHeader` so the two cannot
drift; a continuation that lands anywhere else is refused with
`connector_pagination_cross_origin` rather than followed. This is deliberately
the opposite of `TokenInBody`, which can only ever spend its value as a query
parameter — the SDK now has one plan for "this is data" and one for "this is a
destination", and a connector picks the one its provider documents.

**The first page number is part of the declaration.** `page_number` keeps its
meaning as the one-based walk Typeform documents, and `page_number_from` takes
the first page explicitly. The page number is still derived from the walk — the
declared first page plus the pages already fetched — never from a provider
value, so a provider still cannot restart or rewind a walk.

**`HEAD` is a read by its method.** A `HEAD` retrieves and returns for exactly
the reason a `GET` does, so `mutates()` is false for it, `Effect::read_only()`
builds, `read_only_documented` is refused as it is on a `GET`, and neither
idempotency class is admitted. The gate did not have to be told about `HEAD`
because it reads evidence rather than a method list (ADR 042).

**A header value may bind from input; a header name may not.**
`header_input(name, input, scalar)` takes the name as declaration material
subject to every rule `static_header` obeys, and binds only the value, from a
declared typed slot, bounded by the header ceiling and validated as a single
header value so it cannot carry a second header line. `aws_s3.object.copy` then
composes its `x-amz-copy-source` from the *configured* bucket and a
percent-encoded source key, and `copy_source` is a reserved input name, so a
caller that tried to supply the whole value is refused rather than silently
overridden and a copy can only ever read from the deployment's own bucket.

`object.copy` is `ProviderIdempotent::NaturalMethod` on Amazon's own words: the
copy happens "in a single atomic action", and a write to a fixed key overwrites
— "If it receives multiple write requests for the same object simultaneously, it
overwrites all but the last object written" — so two identical copies leave one
object at the destination key, exactly as two identical `PUT`s do. Amazon also
documents that "A `200 OK` response can contain either a success or an error",
which means the declared success status is not the whole contract for this one
operation: `S3Instance::decode` reads the body of the 200 and routes an
`<Error><Code>` through the same closed error map a failing status would reach.

## Alternatives

| Option | Why Not |
|--------|---------|
| One `no_content` flag on the operation | Cannot describe SendGrid's list delete, which documents `200` with a job *and* `204` with nothing under one operation id |
| Let a no-content success publish declared required pointers as nulls | That is the silently-dropped required field the declared output contract exists to prevent; refusing the declaration at build time names the defect where it was written |
| Treat a body at a no-content status as a contract violation | A provider that sends a body has told us more, not less. The status narrows the *admitted* case; the normal decode still runs when a body arrives |
| Express the body-carried next URI with `TokenInBody` | It would send SendGrid's own URL back as a `page_token` value, which SendGrid rejects. It is also the wrong shape: the value is a destination, and calling it data would leave the origin check unwritten |
| Let the continuation URI skip the origin check because the provider chose it | The provider body is exactly the input the origin invariant exists for. `LinkHeader` already refuses a cross-origin `next`; a second plan with a weaker rule would be a hole with a different name |
| Change `page_number` to take a first page | Every existing caller would have to be edited to say "1", and the common case would read as configuration. `page_number` is the one-based walk; `page_number_from` is the general one |
| Derive the next page number from the provider's own `page` field | A provider could then restart or rewind the walk, which is unbounded work wearing a page number |
| Give Twilio only one pagination plan | Twilio publishes both, and they are not interchangeable: only the continuation carries the `PageToken` the API needs past the first page, while `page` is client state. Declaring both, named for what each is, describes the provider honestly |
| Bind the header *name* from input as well | That is the generic request node spec 010 §2 refuses. A name from input means a caller choosing `Authorization` |
| Let `object.copy` take the whole `x-amz-copy-source` from input | The value names a bucket, so it is a target. A caller supplying it would be choosing which bucket the copy reads from, which is deploy-time configuration |
| Declare `object.copy` inventory-only because a `200` can carry an error | The `200`-with-error is a decode problem, not a repeat-safety problem, and it is answerable: read the body and classify. Refusing the operation would have thrown away a documented, repeat-safe write over a response-parsing detail |

## Consequences

One clarification belongs here, because this ADR is where Twilio's two plans
were admitted: since
[[058-a-declared-walk-is-the-executors-walk]] the serving executor walks a
declared plan, and for Twilio it walks `pagination` — the body-carried
continuation — because only that one carries the `PageToken` the API needs past
its first page. `page_number_pagination` stays declared and tested, describing
the protocol Twilio also publishes, and nothing selects it.

The SDK's closed sets grew by one pagination plan and one HTTP method, and the
operation declaration grew two constructs. Every one of them is anchored to a
provider sentence in the module that needed it, and each arrived with the test
that fails without it. Three tests that had been written to *record* a gap now
assert the documented behaviour instead; that inversion is the visible evidence
the gap closed, and it is why those tests were written that way.

The `TokenInBody` / `NextUriInBody` pair is the piece most worth remembering. A
future connector author reading a provider's "next" field has to decide which of
the two it is, and the answer is not cosmetic: one of them cannot leave the
origin because it never becomes a URL, and the other cannot leave the origin
because it is checked. Both are safe, and neither is safe by accident.
