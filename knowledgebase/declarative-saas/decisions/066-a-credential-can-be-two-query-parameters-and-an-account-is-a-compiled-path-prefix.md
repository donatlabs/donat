---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A credential can be two query parameters, and an account is a compiled path prefix

## Context

Spec 024 (Batch H, project tracking and collaboration) added six connectors, and
two of them could not be described with anything the SDK or the serving seam had.

**Trello's credential is two secrets on the query string.** Its authorization
guide publishes exactly one form a deployment can send without OAuth 1.0a request
signing: `https://api.trello.com/1/members/me?key={{apiKey}}&token={{apiToken}}`.
The key identifies the *application* and the token identifies the
*authorization*, and neither authenticates alone —
`GET /1/members/me?key=…` with no token is a `401`, and the reverse is too.
`AuthKind::ApiKeyQuery` spends one value under one key, so describing Trello with
it would have meant putting the other half in the declaration: into its `Debug`,
into the credential contract the registry publishes, and into every diagnostic
that prints a connector. That is exactly what
[[064-a-credentials-scheme-and-its-username-are-the-providers]] refused for
Freshdesk's API key, one position over.

**Basecamp's per-tenant part is a path segment, not a host label.** "All URLs
start with `https://3.basecampapi.com/999999999/`. URLs are HTTPS only. The path
is prefixed with the account ID, but no `/api/v1` API prefix." Every previous
per-tenant connector in the programme fills a host: Shopify's shop, Jira's site,
Zendesk's subdomain, Salesforce's My Domain, and — as
[[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]] added —
Zoho's compiled table and WooCommerce's whole origin. Basecamp's host is a
constant that every account shares, and the tenant lives one level down. Its
`User-Agent` is the same shape of value for a different reason: Basecamp answers
a request without one with a `400`, and demands that it name the application and
a way to reach its author.

## Decision

**A credential's wire form is declaration material, and so is a path prefix a
deployment owns. Two narrow changes, each with the provider evidence behind it.**

`AuthPlan::api_key_query_pair(key_param, token_param)` appends
`key_param=<api_key>&token_param=<secret>` to the rendered query, reading two
fields off the resolved credential — `field::API_KEY` and the `field::SECRET`
every other plan reads. Neither half is declaration material; the two *parameter
names* are, and they are validated against the same static-key grammar
`api_key_query` uses and refused when they are equal. `required_fields` answers
with both, so startup refuses a half credential by name before a listener opens,
and `apply` refuses one before a byte leaves. The rendered URL is marked with
`mark_url_credential`, which is what makes `RequestPlan`'s `Debug`,
`redacted_url`, and every diagnostic and fingerprint built from either print the
origin followed by `/<redacted>` instead of the query — the same redaction the
single-value query plan and the path-segment plan already had, now covering a
URL that carries two secrets rather than one.

**A deploy-time path prefix is compiled into the operations, not filled at render
time.** `basecamp::connector(account_id, user_agent)` builds its operation set
with `/{account_id}` already in front of every path template and the `User-Agent`
already a static header, which makes it the "declaration a deployment completes"
of [[048-a-declaration-a-deployment-completes]] — Twilio's Account SID and Jira's
account address, applied to a path and a header rather than to a credential. The
account id is validated where the declaration is built, against Basecamp's own
grammar ("Basecamp account ID (numeric string)"): ASCII digits, non-empty,
bounded length. The `User-Agent` is validated against the rule Basecamp
publishes for it — a name and a bracketed contact, printable ASCII, bounded.

The alternative — a `{account_id}` binding in the path template, filled from
input — is the one thing this must not be. A path binding is a *caller* slot: it
is rendered from operation input, it is percent-encoded rather than refused, and
a connector whose account came from input would be a connector a Process could
aim at another tenant. Compiling it makes the account unreachable from input by
construction, and the test asserts all three directions — input, a provider body,
and a continuation — exactly as a templated host's proof does.

## Alternatives

| Option | Why Not |
|--------|---------|
| Describe Trello with `AuthPlan::api_key_query` and put the key in the declaration | It puts a secret into a `&'static`-shaped declaration, its `Debug`, and the published credential contract — the one thing `Secret` exists to make impossible, done by the type that carries the plan |
| Make Trello's key a non-secret `config.settings` value, like WooCommerce's consumer key | WooCommerce's consumer key is half an HTTP Basic pair whose *username* half a deployment may publish; Trello's key is a credential in its own right, and it is what a rate limit and an audit are keyed on. Spec 024 §1 says both fields are secret, and the provider's own advice is to treat them as a pair |
| Use Trello's other published form, OAuth 1.0a with `Authorization: OAuth oauth_consumer_key=…` | This SDK has no OAuth 1.0a request signing, and the header form Trello publishes for it is a signed envelope rather than a static credential. Sending the unsigned shape of it authenticates as nobody |
| Let one plan take an arbitrary list of query credentials | The plan set widens on a shape a provider forced ([[047-the-sdk-widens-where-a-provider-forced-it-and-nowhere-else]]), and one provider forced *two*. A list would admit a shape nothing in this population has and would make the credential contract a runtime value rather than a compiled one |
| Give Basecamp an `OriginSpec::DeploymentOrigin` and let the deployment name `https://3.basecampapi.com/999999999` | An origin is a scheme, a host, and a port; `Origin::parse` refuses a path for the reason ADR 065 recorded, and relaxing it here would relax it for WooCommerce too |
| Render Basecamp's account from a `{account_id}` path binding | A path binding is filled from operation input. The account would become a value a Process could choose, which is a Process choosing a tenant |
| Take Basecamp's `User-Agent` from operation input, since it is only a header | It identifies this deployment to the provider, and Basecamp uses it to contact whoever is misbehaving. A request that could choose it could impersonate another integration |

## Consequences

The SDK has one more credential plan and it is the narrowest form of the shape
Trello forced: two names, two secret fields, one redaction. Every other plan is
unchanged, and exactly one connector in the workspace declares this one, which a
test asserts.

Basecamp establishes that a per-tenant *path* is a compiled declaration rather
than a template, and the distinction now has a rule beside ADR 065's three
origin shapes: use a host label, a compiled table, or a whole origin when the
tenant is in the authority; compile the prefix into the paths when it is not. The
cost is that Basecamp's declaration cannot be a `&'static` constant — it is a
`ModuleDeclaration::PerDeployment` like Twilio's, Jira's, Zendesk's,
WooCommerce's, and Zoho's — and that a reviewer reading the module sees paths
with a placeholder account in them rather than the literal ones a deployment
renders. The `declaration_shape` helper every per-deployment module already has
is what a reviewer and the registry read instead.
