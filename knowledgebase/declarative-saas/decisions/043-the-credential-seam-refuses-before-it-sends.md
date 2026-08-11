---
type: decision
status: accepted
date: 2026-08-09
features:
  - "[[declarative-saas]]"
---

# The credential seam refuses before it sends

## Context

[[041-a-credential-the-engine-writes-is-still-not-an-admin-api]] built the whole
OAuth2 credential lifecycle — sealed storage, the deploy-time `authorize`
command, single-flighted refresh at use — and then named its own open seam in
the last paragraph: "the `config.oauth2` declaration is consumed today by the
authorize, list, revoke, and refresh paths. Applying the resulting header to a
provider request lives in the connector executor and is the one seam left open."

Under [[034-a-declaration-the-runtime-ignores-is-a-defect]] that is not an
incomplete feature, it is a defect with a familiar shape. A deployment could
declare `oauth2`, run `donat connector authorize`, watch it print a subject and
a scope list, and then serve provider requests with no `Authorization` header at
all — every piece reporting success while the one thing that mattered never
happened. That is the same failure as the idempotency header Petshop never sent:
metadata that parses, validates, deploys, and does nothing.

Closing it needed three decisions that are not obvious, because the credential
half and the connector half have different closed sets, different failure types,
and different ideas of what a `401` means.

## Decision

**A declared credential that cannot be applied fails the attempt; it never
downgrades to an unauthenticated request.** The registry records which instances
declared `config.oauth2` from metadata, not from whatever runtime happened to
resolve. An instance in that set with no resolved credential runtime fails
`invariant` with `connector_credential_runtime_absent`, before a socket opens;
one whose stored credential is missing fails `authentication` with
`credential_missing`, also before a socket opens. There is deliberately no path
in which the header is merely absent. For the same reason a module that cannot
apply an OAuth2 credential — `stripe`, which authenticates with a secret key —
refuses `config.oauth2` at deploy time rather than accepting and ignoring it,
and the `RegisteredConnector::execute_authorized` default is a refusal rather
than a fall-through to the unauthenticated method.

**The credential seam classifies, it does not reclassify.** Mapping
`CredentialErrorClass` onto the SDK's `ConnectorErrorClass` is total and invents
nothing: four classes have exact counterparts and keep their names, so a Process
declaring `retry_on: [http_429]` retries a throttled *token endpoint* exactly as
it retries a throttled provider. The fifth, `Contract` — the token endpoint
answered something that is not a token response — has no counterpart, and lands
on `Permanent`, the class that means the same question gets the same answer. It
does not get a ninth connector class: a `retry_on` name no deployed Process can
route is not a class.

**A `401` triggers exactly one refresh and one replay, and the operation still
owns what a `401` means.** The request path reports "the provider answered 401"
alongside the failure its own `error_map` produced, and only the *first* pass
converts that into a refresh. The replay's failure is returned unchanged. A seam
that answered every exhausted replay with its own credential-shaped failure
would silently override a declared `error_map` rule for `401` — the very defect
this wiring exists to remove, reintroduced one layer up.

Two smaller choices follow. Refresh-and-replay is routed through
`refresh::with_access_token` rather than reimplemented, so the fast path, the
row lock, the single exchange, and the header's wipe have exactly one
description. And the provider account (`subject`) is read per attempt rather
than pinned at boot, so re-authorizing an instance takes effect without a
restart; more than one stored subject for one instance is an operator error the
attempt names (`credential_ambiguous`) rather than a coin it flips.

## Alternatives

| Option | Why Not |
|--------|---------|
| Send the request unauthenticated when no credential runtime resolved | The defect itself. A deployment cannot tell a working credential from an ignored one until the provider decides, which may be months later and in production |
| Let the credential seam answer every `401` with its own failure | Overrides the operation's declared `error_map`, so an operation that classified `401` as `permanent` would be retried, and one that classified it `validation` would be journaled as an auth failure |
| Retry the `401` more than once | A provider that refuses a freshly minted token is answering, not rate-limiting. The retry policy above the activity is where "try again later" belongs |
| Add a `contract` class to `ConnectorErrorClass` | The set is closed because Processes route on it. A ninth name no deployed Process can name is not a class, it is a hole |
| Pin the subject at boot alongside the declaration | A re-authorization would then need a restart to take effect, and the row the CLI wrote would be ignored by the binary reading it |
| Accept `config.oauth2` on `stripe` and ignore it | ADR 034 again, in miniature |
| Cache the applied header per instance for reuse across attempts | A second copy of the token outliving the attempt, for no gain: the fast path is already a single indexed read and the row lock is what makes refresh correct |

## Consequences

Connectors whose providers require authorization-code OAuth2 now work
end-to-end, which was the point of spec 011 and is most of the interesting
providers. `oauth_credential_reaches_the_provider_request` asserts the exact
`Authorization` value on the wire through the SDK's provider stub — both the
stored token and, after a mid-attempt `401`, the refreshed one — and
`a_declared_oauth2_connector_never_sends_an_unauthenticated_request` asserts
that the two ways a credential can be unavailable each fail the attempt with the
provider having received nothing at all.

The cost is one new startup obligation and one new startup failure. A deployment
that declares `config.oauth2` must hold a stored credential for every declared
instance before it will serve; an operator who deploys the metadata before
running `donat connector authorize` gets a boot failure naming the instance
rather than a running deployment whose activities fail later. That is the
trade spec 011 §7 already chose, now enforced where it was only checked by
`donat connector credentials list`.

The registry keeps its instances behind `Arc` rather than `Box`, because the
callback that borrows the applied header may borrow nothing else — which is the
same constraint that keeps the header's life confined to the attempt.
