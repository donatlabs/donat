---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# An origin is a label, a table, or a whole value a deployment names — and a walk ends on an absence or not at all

## Context

Spec 023 (Batch G) has four per-tenant hosts in six connectors, and only two of
them are the shape every previous batch had. `OriginSpec::TemplatedHost` fills
**one lowercase DNS label** into an otherwise constant host, which is exactly
Shopify's shop, Jira's site, Zendesk's subdomain, Freshdesk's domain, and
Salesforce's My Domain. Two providers in the batch are not that shape at all:

* **Zoho CRM** serves one org from one of eight data centres it publishes, and
  the API host differs per centre — `https://www.zohoapis.com`, `.eu`, `.in`,
  `.com.au`, `.jp`, `.ca`, `.com.cn`, `.sa`. Two of those suffixes contain a
  dot, which a single host *label* cannot produce, so no template describes the
  set. Zoho's own guidance is to read `api_domain` off the token response, which
  spec 010 §4 forbids outright: an origin a provider response can move is not a
  fixed origin.
* **WooCommerce** has no vendor host at all. The store *is* the provider, its
  domain is whatever the merchant owns, and the only constant is the
  `/wp-json/wc/v3` route prefix.

The same batch produced two connectors whose provider publishes a cursor the
SDK's closed plan set cannot walk, for two reasons neither of which had come up
before — and [[055-a-cursor-in-a-body-is-not-a-pagination-plan]]'s three cases
are all "the cursor is somewhere a plan cannot reach", which is not what these
are.

## Decision

**Three shapes of deploy-time origin, and each provider gets the narrowest one
that describes it.**

*One label into a constant host* stays the default, and Zendesk, Freshdesk, and
Salesforce use it. Salesforce's **sandbox** host is deliberately not served:
`https://MyDomainName--SandboxName.sandbox.my.salesforce.com` has a different
constant suffix, a templated host declares one, and a connector that guessed
between two suffixes would be choosing an authority. A sandbox connector is its
own module, exactly as HubSpot's forms host is.

*A closed compiled table*, selected by name, is new here and is **narrower than
a template rather than wider**: a template admits any label a deployment types,
and Zoho's table admits eight origins this workspace compiled. The declaration
is built per deployment from the named region — the Twilio and Jira shape from
[[048-a-declaration-a-deployment-completes]] — and what it builds is an
`OriginSpec::Fixed`. The region's accounts host is compiled beside its API host,
so a deployment whose `config.oauth2.token_endpoint` belongs to another data
centre is refused before a listener opens rather than authorizing into an org it
cannot then reach. Canada is the reason the table is a table and not a rule: its
accounts host is `zohocloud.ca` and its API host is `zohoapis.ca`, and neither
is derivable from the other.

*A whole origin the deployment names* — `OriginSpec::DeploymentOrigin` — is what
WooCommerce declares, and this is the first hand-written connector to use the
variant the deploy-time declarative `http` connector introduced. It is the
honest description: the deployment names the provider because this workspace
cannot. It is **not** an escape from fixed origins — the value is read once,
validated, and becomes the same immutable `Origin` every other connector renders
against, and no input, response, or continuation can move it afterwards — and
the module owns two refusals that make it safe to point a credential at:

* **`https` only.** WooCommerce publishes Basic authentication for HTTPS and
  publishes OAuth 1.0a signing as the alternative for plain HTTP, and the SDK
  has no OAuth 1.0a plan. Sending the declared Basic credential to an `http://`
  store would put the consumer secret on the wire in clear.
* **No path.** `Origin::parse` already refuses one, and the refusal is kept
  rather than worked around: WooCommerce publishes nothing about subdirectory
  installations — the word does not occur in its reference — so a deployment
  whose WordPress lives under a path is refused with its configuration key named
  instead of served a URL this module composed by guessing.

**A walk ends on an absence, or the connector declares no walk.** Every plan in
the SDK's closed set stops when the thing it reads is *not there*: a cursor
field, a `Link` relation, a short page. Two providers in this batch publish a
continuation that never becomes absent, and each is declined for its own
recorded reason.

* **Zendesk's cursor stops on a flag**: "Repeat the above steps until the
  `meta[has_more]` property is false." No plan reads a flag, so declaring the
  cursor would be declaring a walk that cannot end. Its *offset* regime
  publishes exactly the absence a plan reads — "Stop paging when the `next_page`
  attribute is null" — so that is what the walked collections declare, at 100
  records a page, inside Zendesk's own 100-page ceiling for the regime.
* **Zoho's cursor has a page a walk cannot parse.** Zoho publishes "No Content
  **HTTP 204** — There is no content available for the request" as the answer to
  an empty collection, and a walk reads the declared item list out of every page
  it receives. A plan whose first page is a documented empty body fails an
  attempt that should have returned nothing, so Zoho declares no plan at all and
  binds the `page`/`per_page` regime it publishes for a caller instead.

Two searches are declined on the provider's own words rather than on a
mechanism: Zendesk publishes "Offset pagination may result in duplicate results
when paging" for its search, and an aggregate assembled from pages the provider
says may repeat itself is not an aggregate; Freshdesk caps its search at ten
pages and publishes no `link` header for it.

## Alternatives

| Option | Why Not |
|--------|---------|
| Widen `TemplatedHost` to admit a dotted value, so Zoho's `com.au` fits | The check that a value is one label is what stops a configured value from being a different authority. Relaxing it for eight compiled origins would relax it for every deployment of every templated connector |
| Give Zoho a `DeploymentOrigin` and let the deployment type its API host | Strictly wider than the table, for a set the provider publishes and this workspace can compile. A deployment could then name any host at all, and the connector's origin would be a string rather than a decision |
| Follow Zoho's own guidance and take `api_domain` from the token response | Spec 010 §4: an origin nothing in a credential or a provider response may change. This is the exact case that rule exists for |
| Compose WooCommerce's origin from a `{store}` label under a fixed suffix | There is no suffix. A store is `shop.example.com` or `example.co.uk` or anything else the merchant owns |
| Accept an `http://` WooCommerce store, since WooCommerce permits it | It permits it *with OAuth 1.0a request signing*, which this SDK cannot produce. Accepting the scheme without the signing sends a consumer secret in clear |
| Accept a WooCommerce path prefix by appending it to every declared path | An origin is a scheme, a host, and a port; a path prefix would make the compiled path a function of configuration, and a deployment could then reach any route on the host by configuring a `..` |
| Declare Zendesk's cursor plan and rely on a short page to end the walk | The cursor plan does not read page length, and Zendesk does not publish a full page as a promise. The walk would end on a budget failure — a failed attempt where the provider had answered completely |
| Declare Zoho's cursor plan and accept that an empty module fails an attempt | A listing that returns nothing is the most ordinary thing a Process asks for, and answering it with `connector_validation` would be a defect the provider documented in advance |
| Add a `stop_on_flag` pagination plan for Zendesk's cursor | A seventh plan for one provider's boolean, when its own offset regime already publishes the absence the closed set reads. The plan set widens on a shape a provider forced ([[047-the-sdk-widens-where-a-provider-forced-it-and-nowhere-else]]), and nothing was forced here |

## Consequences

An `OriginSpec` variant that existed for one deploy-time connector now has a
hand-written user, and the rule that separates it from the templated host is
stated rather than implied: use a label when the provider owns the domain, a
table when the provider publishes a closed set, and the whole value when the
provider is the deployment's own installation. The third case carries two module
refusals that a fixed or templated origin gets for free, and a connector that
adds a `DeploymentOrigin` without them would be shipping a credential to
wherever configuration pointed.

Two of the six connectors in this batch send one request where their provider
would have allowed a walk, and both say so in their module headers. That is a
real cost: a Zoho listing reaches 200 records an attempt and a Zendesk search
reaches one page, and a Process that wants more advances a page number itself.
The alternative was a walk that fails on a documented empty collection or never
terminates, and a bounded declaration that says less is better than an unbounded
one that says more.
