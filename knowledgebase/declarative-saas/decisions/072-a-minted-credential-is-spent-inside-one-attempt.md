---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A minted credential is spent inside one attempt

## Context

`AuthPlan::oauth2_client_credentials` and its `token_request` have been in the
SDK since spec 010. They validate a token endpoint, render an RFC 6749 §4.4
exchange, and hand the issued token to `AuthPlan::apply`, which puts it on the
provider request as `Authorization: Bearer …`. Their unit test has passed since
the day it was written.

Nothing in the serving executor ever called them. `ProviderInstance::attempt`
resolved the module's auth plan, passed `authorization: Option<&str>` — the
*stored* credential the spec 011 seam hands one attempt — and applied the plan.
For every plan whose credential is deploy-time configuration that is correct.
For this one it is not: the plan refuses to apply without a token, so an
instance declaring it would have failed every attempt with
`connector_credential_missing_field`. The failure mode was less bad than the
one [[043-the-credential-seam-refuses-before-it-sends]] closed — nothing would
have been sent unauthenticated — but the shape is the same, and it is the
fourth instance of it this programme has found:
[[034-a-declaration-the-runtime-ignores-is-a-defect]] is the rule, the OAuth2
authorization-code header (ADR 043), the `local.*` activity route, the
pagination walk ([[058-a-declared-walk-is-the-executors-walk]]), and the
idempotency-key binding ([[070-a-declared-idempotency-key-is-written-by-the-executor-and-a-window-is-a-startup-check]])
are the ones already closed.

It also blocked a connector whose evidence was already complete. PayPal's REST
API is authorized only by a client-credentials token, and Batch J recorded the
full idempotency evidence for it before stopping at the engine.

The problem is not "make the same call as `refresh::with_access_token`". The
two credentials have opposite lifetimes, and almost everything about the stored
path follows from the fact that it is *stored*: a sealed row, a sealing key, a
transactional row lock that makes refresh single-flight, a rotation that must
commit before it is used, and a permanent mark when the provider refuses. A
client-credentials grant has none of those, and spec 011 §8 says so in one line
that had never been implemented: `oauth_client_credentials_is_not_persisted` —
"the Spec 010 client-credentials plan writes no row and drops its token after
the attempt".

## Decision

**The executor mints the token, spends it, and drops it, and there is no store
anywhere on the path.** `crate::connectors::client_credentials` is a module
beside `credential` rather than inside it, and the separation is the point: it
holds no pool, no sealing key, and no `CredentialRuntime`, so there is no value
in scope that could write `donat.connector_credential`. An instance whose plan
is client credentials declares no `config.oauth2` block, so it is not in the
registry's `oauth2_instances` set and `ConnectorRegistry::execute` never routes
it through the stored path at all. "Not persisted" is therefore a property of
what is reachable rather than a rule someone remembers, and the token's whole
life is one `AccessToken` local — a type with no `Serialize`, no `Display`, and
a `Debug` that prints nothing — in one stack frame.

`AuthPlan::issues_its_own_token` is the one new question, and it is asked
twice. The executor asks it per attempt, to decide whether to mint. The
**registry asks it at startup**, and this is ADR 043's rule applied where it
was still missing: a module that declares the plan and hands back a credential
without the fields the exchange needs is refused before a listener opens,
naming the instance, rather than starting and failing every activity. The
stored plan's version of that refusal lives in each module
(`applies_stored_oauth2`); this one has to live in the registry, because the
credential the plan needs is not a header the module writes — it is an exchange
the executor makes on the module's behalf, from fields the module had to
remember to wire in.

**A `401` buys exactly one re-acquisition and one replay, and the replay's
failure is the operation's.** This is deliberately the same contract as the
stored path's, for the same reason ADR 043 gives: only the *first* pass converts
"the provider answered 401" into a new credential, and the second failure is
whatever the operation's own `error_map` produced. A seam that answered every
exhausted replay with a credential-shaped failure would silently override a
declared `error_map` rule for `401`. A failure that is not a `401` — a `429`, a
`5xx` — mints nothing: it is the operation's, and the retry policy above the
activity owns it.

**The exchange is bounded in time and in bytes, headers and body.**
[[061-a-locked-row-is-held-for-a-bounded-exchange-and-a-grant-may-not-narrow-under-it]]
found the stored exchange bounded around `send`, which resolves when the
response *headers* arrive, with the body then read whole by `Response::bytes` —
unbounded in time and in size, under a credential row lock. There is no lock
here, but there is an activity's deadline and a worker's slot, so the same
defect would still cost the deployment a stalled attempt for as long as the
endpoint kept the socket open. One `timeout_at` spans DNS, connect, headers and
body, and the body is read chunk by chunk against a 256 KiB ceiling — the same
number, for the same reason. The ceiling is carried on `PreparedHttpRequest`
rather than hard-coded in the exchange, so it is enforced where the bytes are
counted and can only ever *narrow* the shared 1 MiB connector ceiling. A body
past it is a **contract** failure rather than a size complaint, because a
response too large to be a token response is not one, and the same question
would get the same answer.

**It classifies, it does not reclassify.** Every token-endpoint failure here is
one of the `CredentialFailure` values the stored path already publishes, crossed
into the connector set through the one total mapping in
`crate::connectors::credential`. A throttled token endpoint is `http_429` with
its own `Retry-After`, a failed one is `http_5xx`, a refusal is a permanent
`authentication`, and an answer that is not a token response is `permanent`.
An operator reading `credentials list` and an operator reading an activity
journal see one vocabulary, and a Process declaring `retry_on: [http_429]`
routes both exchanges the same way.

One smaller choice: a grant whose `token_type` is not `Bearer` is refused. The
plan applies RFC 6750's scheme, so a token of another type is one this connector
cannot send, and sending it as a bearer token would be a request the declaration
does not describe.

## Alternatives

| Option | Why Not |
|--------|---------|
| Cache the minted token per instance until it expires | It is a second copy of a credential outliving the attempt that bought it, for a saving the provider already prices in — and the moment there is a cache there is cache invalidation, a clock to trust, and a value that survives a `revoke` nobody ran. Spec 011 §8 chose the other trade explicitly, and the test asserts it: a second attempt mints a second token |
| Store the minted token in `donat.connector_credential` alongside the stored ones | It would make the table hold two kinds of credential with different lifetimes and different failure modes, and it would put a value the engine can re-mint for free behind a sealing key, a migration, and a `revoke` command. `oauth_client_credentials_is_not_persisted` exists to keep that table meaning one thing |
| Route client-credentials instances through `credential::execute_with_credential` | That function's whole body is the stored path: a subject lookup, a row lock, a refresh, a rotation. None of it applies, and threading a second mode through it would make the single-flight reasoning conditional |
| Reuse `HttpTokenExchange` from `crate::credentials::oauth` | It builds its own `reqwest::Client` and resolves its own DNS, so a connector's token request would bypass the resolve-then-pin rule every provider request obeys. The exchange goes through the instance's own resolver and transport instead, and the connected peer is checked — this is the one request in an attempt that carries the client secret |
| Bound the exchange with `reqwest`'s per-request timeout only | The same bound spelled where a later edit can drop it silently, which is exactly what ADR 061 found. The bound is the caller's deadline, applied where the exchange is made |
| Give the token exchange its own budget, shorter than the operation's | A number nobody chose. Spec 011 §6 says the exchange spends "the same call, byte, and deadline budget as the operation itself", which is a bound the deployment already picked |
| Let a `401` re-mint more than once | A provider that refuses a token issued seconds ago is answering, not rate-limiting. The retry policy above the activity is where "try again later" belongs — ADR 043's reasoning, unchanged |
| Refuse a plan the module cannot satisfy at the first attempt instead of at startup | It is the failure ADR 043 already rejected once, one credential kind later: a deployment cannot tell a working credential from a broken one until an activity runs, which may be days later and in production |

## Consequences

Connectors whose providers authorize with client credentials now work
end-to-end, which unblocks PayPal and every provider with the same shape. The
serving executor has one more thing it can do before a provider request, and
that thing is proven by nine unit tests against a loopback stub — including
`oauth_client_credentials_is_not_persisted`, which asserts that two attempts on
one instance mint two different tokens and that the whole path runs to success
with no database in the process at all.

The costs are real and worth naming. Every logical attempt on such a connector
makes **two** requests where a bearer connector makes one, and both spend the
same deadline: an operation with a tight deadline and a slow token endpoint now
has two ways to time out. That is the price of not caching, and it is the price
spec 011 §8 chose. And `PreparedHttpRequest` grew a field, so the transport now
has a per-request ceiling as well as a shared one; it can only narrow, and a
test asserts that a caller cannot use it to read past `MAX_HTTP_BODY_BYTES`.

The pattern this closes is the fourth of its kind, and the count is the point.
A declaration is not a feature until something spends it, and the cheapest place
to notice that is a test that asserts what reached the wire.
