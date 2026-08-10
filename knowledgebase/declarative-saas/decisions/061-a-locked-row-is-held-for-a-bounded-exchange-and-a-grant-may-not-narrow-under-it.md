---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A locked row is held for a bounded exchange, and a grant may not narrow under it

## Context

[[041-a-credential-the-engine-writes-is-still-not-an-admin-api]] made the
transactional row lock the whole single-flight mechanism: `SELECT … FOR UPDATE`
on the one credential row, the token exchange *inside* that transaction, and the
second claimer blocking until it can read the first one's committed result.
That is what makes refresh correct across replicas during a rolling deploy,
where two binaries are running at once and no in-process cache spans them.

It also means everything the exchange waits for is something every other
attempt on that credential waits for. The exchange was bounded only around
`send`, which in `reqwest` resolves when the response *headers* arrive; the body
was then read with `Response::bytes`, unbounded in time and in size. A token
endpoint that answers `200` and then trickles — a proxy holding a connection
open, a load balancer that has lost its backend, a provider having a bad day —
pinned the row and the pooled connection under it indefinitely. Every other
activity on that credential blocked to the 30-second `statement_timeout`
([[../../operations/decisions/001-bounded-and-drainable-by-default]]) and failed
`credential_store_unavailable`, in a loop, for as long as the endpoint kept the
socket open.

Two smaller things had the same shape — a success reported at the one moment an
operator is watching, and a deployment that stops working afterwards. A refresh
wrote `grant.granted_scopes` straight into the row with no check, though
`authorize::complete` refuses exactly that on the first grant. And
`store::upsert` keys on `subject`, so re-running `donat connector authorize` and
approving as a *different* provider account inserted a second row and printed
`authorized`, after which `CredentialRuntime::subject` answered
`credential_ambiguous` for every activity on that instance, permanently.

## Decision

**The exchange keeps its place inside the transaction, and the budget covers all
of it.** One `tokio::time::timeout` now spans DNS, connect, headers *and* body,
and the body is read chunk by chunk against a 256 KiB ceiling rather than
buffered whole. The lock is therefore held for at most the caller's own budget —
the operation's remaining deadline on the request path, 30 seconds for the CLI —
which is a bound the deployment already chose, rather than one the provider
chooses. Moving the call out of the transaction was considered and refused: the
lock *is* the single flight ADR 041 relies on and `oauth_refresh_is_single_flight`
proves, and every way to keep that property with the network call outside —
an advisory lock, a claim column, a lease — is the same wait under a different
name, minus the property that a crash rolls it back. A slow provider should cost
one bounded wait, not a second mechanism.

**A refresh may not narrow the grant.** What the provider returns has to cover
what the instance declares, the same check `authorize::complete` applies to the
first grant and `CredentialRuntime::validate_stored_credentials` applies at
startup ([[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]).
A shortfall refuses the refresh and marks the row, like an `invalid_grant`: the
row is kept so `credentials list` says why, and the mark is what stops a
provider that narrows once from being asked again on every attempt — at a
rotating provider each of those asks burns the stored refresh token. Writing the
narrower set instead would leave the deployment dispatching operations that
grant no longer authorizes: opaque provider `403`s now, and a startup
`ScopeShortfall` at the next restart, neither of which names the refresh that
caused it.

**One instance holds one provider account, and the authorization says so.**
`authorize::complete` reads the instance's stored subjects inside its
transaction, before anything is written, and refuses a grant that came back as a
different account — naming the account it holds, the account that arrived, and
the `revoke` command that switches deliberately. Re-authorizing the *same*
account is untouched: it replaces the row, resets the rotation count, and clears
any unusable mark, which is how an operator recovers from both of the marks
above.

## Alternatives

| Option | Why Not |
|--------|---------|
| Move the exchange out of the transaction and re-take the lock to write | The window between the two is exactly where two replicas both exchange, which is the thing the row lock exists to prevent and what a rotating provider punishes hardest |
| Keep the exchange inside but bound only its time | A body with no size limit is an allocation the provider chooses, made while holding a credential row and a pooled connection. Time and bytes are the same defect measured two ways |
| Rely on `reqwest`'s per-request timeout instead of an explicit one | It is the same bound, spelled where a later edit can drop it silently. The budget is a field of `TokenRequest`, documented as covering the whole exchange, and now enforced where it is read |
| Refuse a narrowed grant without marking the row | Every later attempt exchanges again, and each exchange at a rotating provider invalidates the token the row still holds. The credential dies anyway, silently, instead of being reported once |
| Write the narrowed grant and warn | A log line nobody reads against a deployment that keeps dispatching unauthorized operations. The startup check already treats a short grant as a refusal; a refresh is not the place to make it advisory |
| Check the refreshed grant against the stored row's scopes rather than the declaration | The declaration is what the enabled operations need (ADR 056). A row may legitimately hold more than the declaration; it may never hold less |
| Let the second account be written and let the request path report `credential_ambiguous` | It already does, permanently, for every activity — which is the defect. The CLI is the one moment an operator is watching |
| Add a unique index on `(source, connector, instance)` instead | The right long-term shape, and a migration that would refuse to apply to any deployment that already has two rows — a boot failure for the exact deployments that need to be told to revoke one. The check belongs where it can explain itself |

## Consequences

A token endpoint that stops talking now costs one attempt and one bounded wait
instead of a credential nobody can use until the process is restarted. The bound
is the caller's budget, so an operation with a short deadline holds the row for a
short time, which is the behaviour a deployment can reason about.

Two new ways a credential becomes unusable, both requiring `donat connector
authorize` to clear: a provider that narrowed the grant, and — unchanged from
before — one that refused the refresh token. Both keep the row and both name
themselves in `credentials list`. An operator switching a connector instance to a
different provider account now has one more step, `credentials revoke`, and gets
told about it at the authorization rather than discovering it from an activity.

The 256 KiB ceiling is far past any real token response, including the largest ID
tokens, and a provider that exceeds it is reported as a contract failure —
permanent, per [[043-the-credential-seam-refuses-before-it-sends]]'s mapping,
because the same question would get the same answer.
