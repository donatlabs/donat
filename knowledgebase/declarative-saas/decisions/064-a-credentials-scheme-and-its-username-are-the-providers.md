---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A credential's scheme and its username are the provider's, not the SDK's

## Context

Spec 023 (Batch G, CRM and helpdesk) is the first batch where two connectors
could not send a credential the SDK knows how to build, and neither gap is a
stylistic difference the SDK may paper over: in both cases the wire form the
provider publishes is the *only* one it publishes, and the form the SDK could
produce authenticates as nobody.

**Freshdesk puts the API key where a username goes.** "You can use your personal
API key to authenticate the request. If you use the API key, there is no need
for a password. You can use any set of characters as a dummy password", with the
worked example `curl -v -u apikey:X`. `AuthPlan::basic` builds
`base64(username:secret)` and takes its username where the *plan* is built, so
describing Freshdesk with it would put the API key into the declaration — into
its `Debug`, into the credential contract the registry publishes, and into every
diagnostic that prints a connector.

**Zoho CRM publishes an `Authorization` scheme of its own.** "Once your app
receives the access token, send the token in your HTTP authorization header to
Zoho CRM API with the value `Zoho-oauthtoken {access_token}` for each endpoint
(for each request)", and every example on every v8 endpoint page uses it.
`Bearer` appears in Zoho's CRM documentation twice, and neither occurrence is an
instruction: once as the `token_type` *value* in a token response, and once in a
line of generic OAuth preamble its own instruction and examples contradict.
`crates/server/src/credentials/refresh.rs` formatted `Bearer {token}` as a
constant, and `AuthPlan::oauth2_authorization_code` asserted the same prefix on
the applied header — so the whole stored-credential path spoke one scheme.

A third gap surfaced in the same batch and belongs here because it is the same
shape. Zendesk is the first hand-written connector to reach
`ProviderIdempotent::ExplicitKey` with a **header** binding, and nothing applied
it. `Effect::idempotency_binding` was declared, projected, and tested; the AWS
FIFO send fills its own *body* pointer inside the module's `plan`, and no code
path wrote a header binding onto a rendered request.

## Decision

**A provider's credential wire form is declaration material, and the SDK widens
to say it.** Three narrow changes, each with the provider evidence behind it and
each keeping the boundary it was tempting to relax.

`AuthPlan::basic_secret_username(password)` is `base64(secret:password)`: the
credential is the *username* half and the dummy password is the constant.
`AuthPlan::basic` is unchanged and is still the plan for a provider whose
username is a public identifier — Twilio's Account SID, Jira's address,
WooCommerce's consumer key, all of which a deployment configures and a
declaration may carry. The new plan reads the same `secret` field every other
plan reads, so nothing about credential resolution changed; what changed is
which half of the Basic pair it goes in.

`AuthPlan::oauth2_authorization_code_scheme(scheme)` carries the scheme name the
provider publishes, and `AuthPlan::oauth2_authorization_scheme` reads it back.
The scheme is a compile-time constant of the connector, validated against RFC
9110's `token` grammar so it cannot forge a second header field value, and
`AuthPlan::oauth2_authorization_code()` still means `Bearer` — the default is
RFC 6750 and always was.

**The credential lifecycle formats the header the connector declared.**
`AccessToken::authorization_header` takes the scheme, `with_access_token` and
`CredentialRuntime::with_authorization` pass it through, and
`RegisteredConnector::oauth2_authorization_scheme` is where the executor reads
it — off the instance's own compiled auth plan, defaulting to `Bearer`. So the
header the lifecycle builds and the header the connector admits are one
decision, taken in the module, and a seam that drifted would be caught by the
plan's own refusal rather than by a `401` in production.

**`Operation::plan_keyed_request` applies the binding the effect declared.** A
class admitted on `ExplicitKey` evidence names a header; the same declaration is
now what writes it, so a runtime cannot render a request of that class without
its key. An operation whose class binds nothing, or binds a body pointer its
module fills, renders exactly as `plan_request` does — so the new entry point is
safe to use everywhere and Batch G's runtime uses it for all six of its
connectors.

The key is deliberately **not** marked sensitive on the header. It is a
Donat-owned activity identifier rather than a credential, and a redacted
diagnostic that could not name the key would make a duplicated send impossible
to trace.

## Alternatives

| Option | Why Not |
|--------|---------|
| Describe Freshdesk with `AuthPlan::basic` by putting the API key in the declaration | It puts a secret into a `&'static`-shaped declaration, its `Debug`, and the published credential contract. The one thing `Secret` exists to make impossible would be done by the type that carries the plan |
| Use Freshdesk's other published form — HTTP Basic with an agent's email and password | Freshdesk publishes it, and it is a worse credential in every direction: a human's password, no scoping, and revocation that logs a person out. A connector should not prefer the credential a provider offers for interactive use |
| Have the SDK rewrite `Bearer <token>` into `Zoho-oauthtoken <token>` when the plan declares a scheme | The SDK would be editing a credential the lifecycle owns, and the contract that the applied value is the complete header — which is what makes "a declared credential that cannot be applied fails the attempt" structural — would become "the complete header, unless the plan disagrees" |
| Give `OauthDeclaration` the scheme, so it travels with the stored row | The scheme is a property of the *connector*, not of one deployment's authorization: it would be sealed into the credential row's AAD, and re-authorizing would be required to fix a value no operator chose. Reading it off the compiled plan cannot drift, because there is only one copy |
| Let each module apply its own idempotency binding inside `plan`, as the AWS FIFO send does | The AWS send composes a *body* field under its own validation rules; a header binding has no such per-module work, and four modules writing the same three lines is how two of them come to differ. `plan_keyed_request` is the declaration applying what the declaration published |
| Mark the applied idempotency key sensitive, like a credential | It is not one. Redacting it would remove the only handle an operator has on "did this activity send twice", which is the exact question the class exists to answer |

## Consequences

Two providers this workspace could not have authenticated to are reachable, and
each is reachable in the one form its own documentation publishes. The
stored-credential path now has a per-connector scheme, which is one more thing a
module declares — and exactly one module in the workspace declares anything but
the default, which a test asserts.

The cost is that the credential lifecycle takes a `&str` it did not take before,
threaded through three functions and one trait method. That is a wider signature
for a value that is constant per connector; the alternative was a second copy of
the scheme somewhere it could disagree with the declaration, and a wider
signature is the cheaper of the two.

`plan_keyed_request` closes a real gap rather than a speculative one: before it,
an `ExplicitKey` header binding was a declaration nothing applied, which is the
defect [[034-a-declaration-the-runtime-ignores-is-a-defect]] names. It is not
retrospective — the only header binding in the workspace is the one this batch
added — but the next one inherits the wiring instead of rediscovering the gap.
