---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A credential can be an authentication parameter, and a credential in a body is a version that was superseded

## Context

Spec 027 §1 named two credential shapes this batch could not describe, and asked
for a decision on each rather than a default.

**PagerDuty's REST API key is an `auth-param`, not a token.** Its own published
OpenAPI (`PagerDuty/api-schema`, `reference/REST/openapiv3.json`) declares one
security scheme: an `apiKey` in the `Authorization` header, described as "The API
Key with format `Token token=<API_KEY>`". RFC 9110's `credentials` production
admits two forms after the scheme — a `token68` blob, or a comma-separated list
of `auth-param`s — and PagerDuty chose the second, with one parameter named
`token`. Every existing plan produces the first form or none:
`AuthPlan::bearer` fixes the scheme, `AuthPlan::api_key_authorization_scheme`
renders `Token <API_KEY>`, `AuthPlan::authorization_credential` renders the key
alone, and `AuthPlan::api_key_header` refuses the `Authorization` name on purpose
([[064-a-credentials-scheme-and-its-username-are-the-providers]]). None of them
reaches the wire form PagerDuty answers.

**UptimeRobot's key is in the request body — in the version spec 027 read.** Its
published v2 call is `curl -X POST https://api.uptimerobot.com/v2/getMonitors -d
'api_key=YOUR_API_KEY_HERE&format=json'`, and spec 027 §1 was explicit about
what that must not become: "Do not smuggle a credential through a body template:
the credential must stay unreadable and must never enter the request
fingerprint." A body template leaf is operation declaration material — it is
projected into the operation's input contract, it is printed by `RequestPlan`'s
`Debug`, and it is hashed into the request fingerprint. Putting a secret there
would undo everything `Secret` exists for, at the one seam that has no redaction.

## Decision

**One new auth plan, for the shape a provider forced; and no new plan for the
shape a provider has already superseded.**

`AuthPlan::api_key_authorization_parameter(scheme, parameter)` renders
`Authorization: <scheme> <parameter>=<secret>`. Both names are compile-time
declaration material, each validated against the same RFC 9110 `token` grammar
`api_key_authorization_scheme` uses, so neither can carry a space, a comma, or a
control character and forge a second header field value; `header_value` still
refuses anything that is not one visible header value, so a credential cannot do
it either. The plan reads `field::SECRET` like every other single-value plan, and
the applied header is marked sensitive like every other applied credential.
Exactly one connector in the workspace declares it, which
`provider/devops.rs`'s own test asserts. This is
[[047-the-sdk-widens-where-a-provider-forced-it-and-nowhere-else]] applied
literally: the plan set widened by one, on a wire form a provider publishes and
answers `401` without.

**UptimeRobot is declared on its v3 surface, and the SDK gained nothing.**
UptimeRobot publishes an OpenAPI for v3 at
`https://cdn.uptimerobot.com/api/openapi.yaml` — "UptimeRobot API" version
`3.0`, server `https://api.uptimerobot.com/v3` — whose single security scheme is
`type: http, scheme: bearer`, described as "Enter your API token (without the
"Bearer" prefix — it is added automatically)". UptimeRobot's own site now links
the body-parameter API as `/api/legacy/`. So the connector declares v3 with
`AuthPlan::bearer`, and the question spec 027 raised — a new plan, or a deferred
connector — is answered by a third option the specification did not know was
available: **read the version the provider currently publishes.**

That is a rule worth stating, because it will come up again. **When a provider
publishes two API versions and only one of them can be described safely, the
connector declares that one and says so in its header.** It is not a workaround:
a version is part of a connector's identity in this workspace already — the
compiled origin, the path prefix and the operation set are all version-specific —
and declaring v2 would have meant either a credential in a body template or a
plan that exists for a surface its own vendor calls legacy.

## Alternatives

| Option | Why Not |
|--------|---------|
| Describe PagerDuty with `api_key_authorization_scheme("Token")` and accept `Token <key>` | It is a different wire form and PagerDuty answers it `401`. [[064-a-credentials-scheme-and-its-username-are-the-providers]] refused exactly this reasoning for Discord's `Bot` and Linear's bare key: a scheme is the provider's, not a spelling preference |
| Make the whole value `token=<key>` the configured secret and use `api_key_authorization_scheme` | The wire format would live inside the secret, so rotating the key would mean re-typing a protocol fragment, and a deployment that got it wrong would produce a header nothing here could validate. The declaration must own the shape and the credential must own only the value |
| Let one plan take an arbitrary list of authentication parameters | The plan set widens on a shape a provider forced, and this one forced *one* parameter. A list would admit a shape nothing in this population has and would make the credential contract a runtime value rather than a compiled one — the same refusal [[066-a-credential-can-be-two-query-parameters-and-an-account-is-a-compiled-path-prefix]] made for a list of query credentials |
| Add an `AuthPlan::ApiKeyBodyField` for UptimeRobot v2 | The credential would have to reach the rendered body, which is where the request fingerprint and the operation's projected input contract are computed. Every other plan applies a credential *after* the request is rendered, and that ordering is what makes redaction total. One provider's legacy version is not enough to move it |
| Defer the UptimeRobot connector and record the gap | The gap is not real: the provider publishes a header form, in the version it directs integrators to, with its own machine-readable description of it |
| Declare v2 anyway, on the argument that more deployments still use it | It cannot be declared safely, and a connector that could only be written unsafely is one this repository does not write ([[037-connectors-are-written-by-hand-against-provider-documentation]]) |

## Consequences

The SDK has one more credential plan and it is the narrowest form of the shape
PagerDuty forced: one scheme, one parameter name, one secret field, and the same
sensitive-header marking every other `Authorization` plan has. `AuthKind` now has
ten variants, and the test that proves each one's exact wire form has a tenth
case.

UptimeRobot costs nothing and gains a rule. A future batch meeting a provider
whose current version is describable and whose legacy version is not now has a
precedent for reading the version rather than widening the SDK — and, equally, a
precedent for *saying which version it read* in the module header, because a
reviewer checking a v2 page against this connector would otherwise find a
mismatch and no explanation.

The cost of the UptimeRobot decision is real: a deployment whose account still
uses a v2-only key cannot use this connector, and the module says so rather than
half-serving it.
