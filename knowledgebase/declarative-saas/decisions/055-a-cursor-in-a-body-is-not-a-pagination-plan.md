---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A cursor in a body is not a pagination plan, and a credential can be the whole header

## Context

Batch E (spec 016) is six product-SaaS connectors — `slack`, `linear`, `notion`,
`intercom`, `hubspot`, `jira` — and three of its providers do something the SDK's
closed sets had no shape for. Each one was found the way
[[047-the-sdk-widens-where-a-provider-forced-it-and-nowhere-else]] describes:
by writing a connector against the provider's own published documentation and
discovering that the declaration could not say what the provider does.

**A continuation that lives in the request body.** `Pagination`'s six plans spend
a continuation as a query parameter (`Cursor`, `OffsetLimit`, `PageNumber`,
`TokenInBody`) or follow it as a URL (`LinkHeader`, `NextUriInBody`). Four
operations in this batch have one that is neither: Linear's `after` is a GraphQL
variable, Notion's `start_cursor` is a body field of `POST /v1/search` and of the
data-source query, Intercom's search takes `pagination.starting_after` inside its
JSON body, and HubSpot's CRM search takes `after` in the body while its own `GET`
collections take the same name in the query. Sending any of them as a query value
is not "close enough" — the provider ignores it and answers page one forever,
which is an unbounded walk over one page.

**A credential with no scheme in front of it.** Linear publishes both forms side
by side: an OAuth access token as `Authorization: Bearer <ACCESS_TOKEN>`, and a
personal API key as `Authorization: <API_KEY>`. `AuthPlan::bearer` prepends a
scheme the provider does not accept there, and `AuthPlan::api_key_header` refuses
the `Authorization` name on purpose. Neither could describe what reaches the
wire, and spec 010 §6 is explicit that a connector module may not invent a plan
of its own.

**A `200` that is the normal spelling of failure.** Slack answers a rejected
request with `200 OK` and `{"ok": false, "error": "channel_not_found"}`; the HTTP
status carries no information at all. Linear answers with a `200` — or a `400`
for a rate limit — and reports what happened in a GraphQL `errors` array,
documenting that a query "can partially succeed with a 200 HTTP status, returning
some data while including errors for failed fields".

## Decision

**A body-carried cursor is a declared input, and the SDK gains no seventh
plan.** The four operations that have one declare the cursor as a normal input
slot, publish the provider's own next-cursor field as a declared output, and bind
their page size from a *literal* so a caller cannot widen it. One call is then
one page, bounded by the declaration's page size and by the SDK's response
ceiling, and the walk itself belongs to the Process that reads
`has_next_page`/`next_cursor` and calls again. The alternative — a
`CursorInBody` plan that merges a value into a rendered JSON body — would give
the SDK its first plan that rewrites a request body, and a body is the one part
of a request a connector composes from a template rather than from parameters.
The cost is real and is named below.

**One new auth plan, `AuthPlan::authorization_credential`**, which sends the
credential as the entire `Authorization` header value with no scheme, no prefix,
and no separator. It is the narrowest possible widening: it adds no field, no
configuration, and no new place a credential can travel — the header is the one
`Bearer` and `Basic` already use, the value still goes through `header_value`,
and the applied header is still marked sensitive. Adding it in the SDK rather
than in the Linear module is what spec 010 §6 requires and what keeps the plan
set enumerable.

**A `2xx` that carries a provider failure is a failure, and the module owns the
guard.** `slack::decode` and `linear::decode` sit between the status check and
the declared output pointers: Slack's `ok` must be present and `true`, and
Linear's `errors` must be absent or empty, before anything reads a pointer. Both
route the failing body through the module's own ordered error map, which is keyed
on the provider's machine-readable code precisely because the status is not
informative — Slack's `error` string and Linear's `extensions.type`. A body that
carries neither the success envelope nor a failure is an `invariant` failure
rather than a guess. The serving seam that calls these is one `BodyGatedRuntime`
in `crates/server/src/connectors/provider/modules.rs`, which is
`DeclaredProvider` with exactly one thing moved: the question "is this response a
success" is the module's rather than the status code's.

That third decision is the same one
[[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]
records for Google, reached independently by another batch in the same week, and
the agreement is worth noting rather than merging: the two batches found the same
shape in six providers across two vendors, which is evidence that "the status was
2xx" is not a portable definition of success. What differs is degree. Google's
`2xx`-with-failure is an edge (`freeBusy` errors, `incompleteSearch`); Slack's is
the *normal* case, so for Slack the guard is not a safeguard but the contract.

## Alternatives

| Option | Why Not |
|--------|---------|
| Add a `CursorInBody` pagination plan that merges the cursor into the rendered body | It would be the SDK's first plan that rewrites a request body, and for Linear it would have to reach *inside* the `variables` object of a GraphQL document — a plan editing a provider's own query payload. The four providers spell the slot four different ways (`after`, `start_cursor`, `pagination.starting_after`, `after`), so the plan would carry a JSON pointer into a body, which is a template a declaration already owns |
| Send the body cursor as a query parameter with `TokenInBody` | Every one of the four providers ignores it. The walk would silently re-fetch page one until it hit a budget, which is a wrong answer wearing a bound |
| Give Linear `AuthPlan::bearer` and accept the extra `Bearer ` | It fails authentication. Linear documents the two forms for two different credential kinds, and sending the wrong one is not a style difference |
| Let the Linear module apply its own header | Spec 010 §6: "A connector module cannot define its own auth plan; adding one is an SDK change with its own tests." A module that formatted an `Authorization` header would be the one place the closed set stopped being closed |
| Widen `AuthPlan::api_key_header` to admit `Authorization` | The refusal exists so a module cannot re-spell `Bearer` or `Basic` badly. A named plan says which of the three a connector means, and a reviewer can see it |
| Put the `2xx` body guard in the SDK, as a declared success predicate on the operation | The predicates are not the same shape: Slack reads a boolean at a fixed key, Linear reads whether an array is non-empty, Google reads two different nested shapes. A predicate general enough for all of them is a small expression language in the declaration, which is the workflow-in-a-connector spec 010 §2 refuses |
| Report Slack's `ok: false` as a success and let the output contract fail | It would report `validation` — "the provider's response did not satisfy the declared contract" — for what is actually `channel_not_found` or `ratelimited`, so a Process would neither retry a rate limit nor see the real reason. Worse, an operation with no required output pointer would report *success* |

## Consequences

The SDK's closed sets grew by exactly one auth plan and by nothing else. The
pagination set did not grow at all, and the price is visible: four operations in
this batch publish a cursor a Process has to carry itself, and a Process that
forgets to loop gets one page and no error. Each of those four says so in its
module documentation and each is covered by a
`<name>_cursor_is_opaque_and_bounded` case that proves the cursor is echoed
verbatim, is never parsed or constructed here, and cannot change the page size.
If a later batch finds a fifth and a sixth, the plan is worth reopening — the
decision here is that four is not yet enough to justify a plan that writes into a
request body.

The body guard has one cost worth naming. `slack::decode` and `linear::decode`
are module code on the response path, which is exactly what a declaration-driven
connector exists to avoid, and a third provider with the same problem is a third
copy of the same eight lines. They are as small as they can be — parse, ask the
provider's own question, hand the answer to the shared error map — and the
`BodyGatedRuntime` seam means the *serving* half is written once. The rule that
matters holds structurally: the module's `decode` is the only path to an output,
so there is no spelling in which a provider failure is reported as an activity
success.

One further consequence is a gap this batch chose to record rather than close.
Linear's rate limit is published in two places with two spellings — `ratelimited`
at `extensions.type`, which its own SDK classifies on, and `RATELIMITED` at
`extensions.code` on its rate-limiting page — and an `ErrorMap` reads one
pointer. The map reads `extensions.type` and declares both spellings there; a
`400` that carries the code *only* at `extensions.code` classifies by its status
as `validation`. That is not retried, which is the same safe direction the GitHub
connector takes for its ambiguous `403`, and it is asserted by a test rather than
left to be discovered.
