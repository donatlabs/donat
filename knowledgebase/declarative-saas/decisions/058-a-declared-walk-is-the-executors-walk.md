---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A declared walk is the executor's walk, and every page pays the same tolls

## Context

Twenty-two of the twenty-eight hand-written connectors declare a pagination
plan. Each plan is unit-tested against the SDK's provider stub, each is
documented in its module's header, and
[[047-the-sdk-widens-where-a-provider-forced-it-and-nowhere-else]] widened the
closed plan set twice to describe two providers accurately.

The serving executor ran none of them. `crates/server/src/connectors/provider.rs`
contained no reference to pagination at all: it rendered one request, sent it,
and decoded one response. `Pagination::collect` — the walk, the shared budget,
the origin check on a provider-offered continuation — had no caller outside
tests. A deployment that enabled `github.issue.list` got GitHub's first hundred
issues and no indication that there were more, and the catalog snapshot that
same deployment published said `maximum_calls: 1`, `maximum_pages: 1`,
`maximum_items: 1` with a code comment explaining that "the provider executor
paginates nothing". Every layer agreed, and every layer was describing something
other than what the modules declared.

This is exactly the defect
[[034-a-declaration-the-runtime-ignores-is-a-defect]] names, at programme scale:
a declaration that parses, validates, deploys, is tested, is documented — and
does nothing.

Wiring it exposed two further defects that only exist once a walk is real.
`AuthPlan::aws_sigv4` signs the canonical query string, and a continuation *is*
a different query, so a walk that authenticated once before the first page would
have sent every continuation with the wrong signature — `SignatureDoesNotMatch`,
which the AWS error maps classify `authentication`, which nothing retries. And
`Pagination::collect` answered every non-2xx page with one built-in
`permanent` failure of its own invention, discarding `Retry-After`: a `429` on
page three of a listing was a permanent activity failure, while the identical
`429` on page one was a retryable `http_429`. Its own doc comment recorded that
gap.

## Decision

**The declaration is the executor's instruction.** `ProviderRuntime` gains
`pagination`, `pagination_budget`, and `admit_page`. An operation whose module
declares a plan is walked; an operation whose module declares none is exactly
one request. The plan lookup is a *required* constructor argument of every
runtime — `build_declared_instance`, `BodyGatedRuntime`, `GoogleRuntime`,
`MicrosoftRuntime` — so a connector added later cannot acquire a walk by
forgetting one: it has to name `<module>::pagination` or the explicit
`no_pagination`. The connectors that deliberately have no plan
([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]: Linear's body cursor,
Sentry's never-exhausted `Link`, Telegram's long-poll offset) name the latter,
and a test asserts they still send one request.

**One walk is one budget.** The four ceilings come from the module when it
declares its own (AWS SES) and from `PaginationBudget::default_ceilings`
otherwise — 16 calls, 16 pages, 5,000 items, and an aggregate ceiling equal to
the transport's own 1 MiB body ceiling. The attempt's deadline is bound onto
that budget rather than restarted per page. Any ceiling failing fails the
attempt, and the pages already collected go with it: a truncated aggregate is
indistinguishable downstream from a complete one. The same budget is what the
operation's catalog snapshot now publishes, so the bounds a deployment can read
are the bounds its executor spends
([[049-a-connector-publishes-the-declaration-it-was-admitted-on]]).

**A page is classified by the operation, and authenticated as itself.**
`Pagination::collect` no longer decides what a failing page means: it asks the
caller, once per page, and the executor answers with the operation's own
`ErrorMap` — or, for a provider that reports failure inside a `2xx`, with the
module's own body gate ([[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]).
The walk hands each page's request to the caller by value, derived from the
copy it kept, and the executor applies the declared credential plan there — so
each page is signed over the request it is actually sending. A static header
behaves identically either way; a signature only works this way.

**The aggregate is one document, decoded once.** A completed walk returns its
final page with every collected item written where the plan declared the item
list. The declared output pointers then read that exactly as they read a single
page, so an operation's output contract does not depend on how many pages its
provider answered in — and the continuation field is absent from the aggregate
precisely because the walk reached the end.

## Alternatives

| Option | Why Not |
|--------|---------|
| Leave the walk to each Process, as ADR 055's four body-cursor operations do | Those four have no choice: no plan in the closed set can spend a body cursor. The other twenty-two publish a protocol the SDK already implements and already tests. Making every deployment re-implement a bounded walk in Rules would be the workflow-in-a-connector spec 010 §2 refuses, and it would leave `Pagination` as an unreachable library |
| Concatenate the pages' item lists and return a bare array | It would change an operation's output contract depending on whether it paginated, and it would throw away every other declared output the last page carries |
| Aggregate into the *first* page instead of the last | The first page carries the continuation the walk consumed; an aggregate carrying a stale cursor invites a caller to resume a walk that is already complete |
| Keep the walk's built-in `permanent` classification as a fallback under the operation's map | Two answers to one question, with the wrong one reachable. The operation's map already has a declared fallback for a status it did not name; a second fallback below it can only disagree |
| Re-sign only when the plan changed the URL | Every plan changes the request — that is what a plan is. A conditional re-sign would be a rule whose only purpose is to be got wrong once |
| Give the executor a per-page deadline instead of the attempt's | A sixteen-page walk would then be entitled to sixteen times the activity's deadline, which is how a bounded attempt becomes an unbounded one |
| Publish a `PaginationPlan` in the catalog snapshot as well as the bounds | The catalog's plan enum predates the SDK's and has no shape for `TokenInBody` or `NextUriInBody`; mapping six plans onto four would publish a description that disagrees with the module. The bounds are exact, so they are what is published |

## Consequences

An operation with a plan now costs what its provider's protocol costs: up to
sixteen calls where it used to cost one, spending the same activity deadline.
That is the honest price of the declaration, and it is bounded in five
directions at once. A deployment that wants one page from a paginated operation
no longer has one — the plan is a property of the connector, not of the call —
and if a real deployment needs that, the shape it needs is a declared page
ceiling per enabled operation, not a per-call flag a caller could widen.

The aggregate is re-serialized once at the end of a walk so that the module's
existing `decode` reads it, which costs one JSON round trip per paginated
attempt. The alternative was a second decode path taking a parsed value, and
duplicating the response boundary of every module to save a serialization on a
path that just made up to sixteen HTTP requests is the wrong trade.

Two ordering rules are now load-bearing and are asserted rather than commented:
the credential is applied *after* the plan primes the first request (a page-size
parameter added after signing would break page one too), and the next request is
derived from the *unauthenticated* request (so no page inherits another page's
signature). Both are proven by tests that fail if the order is reversed.

One residue is worth naming rather than hiding. `twilio::page_number_pagination`
is a second, complete plan for the same two operations, declared because Twilio
publishes both protocols; the executor walks `twilio::pagination`, the
continuation-URI one, because only the continuation carries the `PageToken`
Twilio needs past the first page. So one declared plan in this workspace is not
wired to anything. It is not the defect this ADR closes — nothing selects it,
and no deployment can enable it — but it is the same shape, and if a third
Twilio protocol ever appears the right answer is to delete the unwired one
rather than to add another.
