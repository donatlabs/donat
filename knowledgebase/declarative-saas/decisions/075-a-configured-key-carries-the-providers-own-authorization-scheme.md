---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A configured key carries the provider's own `Authorization` scheme, and a grant the SDK cannot render is not sent

## Context

[[064-a-credentials-scheme-and-its-username-are-the-providers]] established that
a provider's credential wire form is declaration material, and widened the SDK
twice on that basis: a Basic pair whose *username* is the secret (Freshdesk), and
an `Authorization` scheme other than `Bearer` for a **stored** OAuth2 token
(Zoho's `Zoho-oauthtoken`). Spec 025 produced the third case of the same shape
and the first for a *configured* key.

**Discord publishes a token type in front of a key a deployment configures.**
"For all authentication types, authentication is performed with the
`Authorization` HTTP header in the format `Authorization: TOKEN_TYPE TOKEN`",
with the worked example `Authorization: Bot MTk4NjIy…` for a bot credential and
`Authorization: Bearer …` for an OAuth2 user token. The two authenticate
*different principals*: a bot token sent under `Bearer` is not the bot. None of
the three existing plans can describe it — `Bearer` fixes the scheme,
`AuthorizationCredential` has none, and `ApiKeyHeader` refuses the
`Authorization` name on purpose — so a Discord connector either could not be
written or would have to put the scheme inside the configured secret, where a
deployment's `SecretRef` would carry a literal `Bot ` prefix nobody reviewed.

The same batch produced a credential the SDK deliberately does **not** widen for.
Spec 025 asks for Zoom's server-to-server app, whose token exchange Zoom
publishes as `grant_type=account_credentials` with an `account_id` parameter.
`AuthPlan::oauth2_client_credentials` renders RFC 6749 §4.4's
`grant_type=client_credentials` and nothing else
([[072-a-minted-credential-is-spent-inside-one-attempt]]).

## Decision

**`AuthPlan::api_key_authorization_scheme(scheme)` applies `Authorization:
<scheme> <secret>` for a configured key.** The scheme is a compile-time constant
of the connector, validated against RFC 9110's `token` grammar — the same
grammar `oauth2_authorization_code_scheme` already validated, now factored into
one `is_authorization_scheme_token` both call — so a value carrying a space, a
comma, or a control character cannot forge a second header field value. The
applied header is marked sensitive, exactly as `Bearer` is.

`AuthPlan::bearer` is unchanged and is still the plan for every provider whose
token type is RFC 6750's; exactly one connector in the workspace declares
anything else for a configured key, which its own test asserts. This is
[[047-the-sdk-widens-where-a-provider-forced-it-and-nowhere-else]] applied for
the third time, and the widening is one enum variant, one constructor, one arm
in `required_fields`, and one arm in `apply`.

**Zoom declares the authorization-code plan it publishes for a general OAuth
app, and its server-to-server grant is not served.** Describing
`account_credentials` with the client-credentials plan would send an exchange
Zoom does not publish, to a token endpoint that would refuse it — and would do so
under a plan whose contract says the request it renders is RFC 6749 §4.4's.
Describing it properly is a *fourth* auth plan with its own token request, its own
`account_id` field on the credential, and its own tests, and nothing in this
batch forced it: the authorization-code path spec 011 already serves reaches the
same API with the same scopes. The refusal is recorded in the module header
rather than worked around, and a deployment that needs the server-to-server app
is told what is missing instead of being handed a plan that fails at the first
attempt.

The asymmetry between the two halves of this ADR is the rule it establishes:
**the SDK widens for a wire form a provider publishes and this workspace can
render exactly, and declines a wire form it would have to approximate.** A
scheme in front of a token is the first; a grant type with its own parameter set
is the second.

## Alternatives

| Option | Why Not |
|--------|---------|
| Configure Discord's secret as the complete header value, using `AuthPlan::authorization_credential` | It puts a scheme nobody reviewed inside a `SecretRef`, so the wire form becomes a property of an environment variable rather than of the declaration. A deployment that typed `bot ` or omitted the space would authenticate as nobody, and no test in this workspace could catch it |
| Let `ApiKeyHeader` accept the `Authorization` name for this one connector | The refusal exists so that exactly one place in the workspace writes that header. Relaxing it per connector is how two places come to write it differently |
| Rewrite `Bearer <token>` into `<scheme> <token>` in the transport when the plan declares a scheme | The SDK would be editing a credential a plan already applied, and "the applied value is the complete header" — the property that makes a missing credential a structural failure rather than a silent one — would become "complete, unless something later disagrees" |
| Add a fourth auth plan for Zoom's `account_credentials` grant in this batch | A new token exchange, a new credential field, and a new set of failure classifications, for a credential whose published alternative this engine already serves end to end. An SDK change lands in spec 010 with its own tests, not inside a provider batch |
| Describe Zoom's server-to-server grant with the existing client-credentials plan | It renders `grant_type=client_credentials` with no `account_id`, which is not the exchange Zoom publishes. A plan that sends a request the provider does not document is exactly what ADR 042 refuses for effect classes, one layer down |

## Consequences

A provider whose credential carries its own token type is now describable, and
the declaration says which one — so a reviewer comparing the module against
Discord's reference is comparing like with like, and the header a deployment's
requests carry is a decision taken in Rust rather than in an environment
variable.

The cost is one more variant in a closed set that is meant to stay small. The
guard against that is the same one ADR 047 named: a new plan needs a provider
that published a wire form no existing plan produces, and both halves of this
ADR were decided by asking that question rather than by convenience.

Zoom's connector reaches the whole meeting surface under a credential this
engine already refreshes on use, and its module names the grant it does not
serve. If a later batch needs the server-to-server app, the work is one SDK
change with one new plan, and this ADR is the record of why it was not taken
early.
