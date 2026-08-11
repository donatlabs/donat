---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# An instance a deployment operates is a whole origin it names, not a tenant label

## Context

[[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]] gave the
programme three deploy-time origin shapes and a rule for choosing between them:
"use a label when the provider owns the domain, a table when the provider
publishes a closed set, and the whole value when the provider is the deployment's
own installation." WooCommerce was the first hand-written user of the third
shape, on the grounds that a merchant's store *is* the provider.

Spec 027 describes GitLab and Grafana as "templated instance host" and adds the
observation that decides the question: "`grafana` and `gitlab` are usually the
deployment's own instances, so the templated host is not a tenant identifier but
infrastructure."

The two claims do not fit together. `OriginSpec::TemplatedHost` fills exactly one
lowercase DNS label into an otherwise **constant** host, and there is no constant
here. GitLab's own worked examples are `https://gitlab.example.com/api/v4/…` —
a self-managed instance lives at whatever host its operator owns, which may be
`git.acme.internal`, `gitlab.acme.co.uk`, or `gitlab.com` itself. Grafana is the
same: self-hosted anywhere, or a Grafana Cloud stack. A template needs a suffix
to be a template of, and neither provider has one.

## Decision

**A provider the deployment *operates* names a whole origin, exactly as a
provider the deployment *owns* does — and the module owns the same two refusals
either way.**

`gitlab` and `grafana` each declare `OriginSpec::DeploymentOrigin` over one
`config.settings.instance_origin`, resolved once at startup into the same
immutable `Origin` every other connector renders against. ADR 065's rule needs no
new clause: a self-managed GitLab is the deployment's own installation in exactly
the sense a WooCommerce store is. What is new is that the *reason* is different —
WooCommerce's host is unknowable because the merchant owns the domain; GitLab's
is unknowable because the operator does — and the outcome is the same, which is
the useful part. The rule is about who can name the host, not about why.

Each module refuses two configurations before a listener opens, and both
refusals are the ones ADR 065 said a `DeploymentOrigin` does not get for free:

* **Not `https`.** Both credentials are bearer tokens. An `http://` instance
  would put a personal access token or a service account token on the wire in
  clear, and neither provider publishes a signing alternative the way WooCommerce
  publishes OAuth 1.0a.
* **No path.** An origin is a scheme, a host and a port; `Origin::parse` refuses
  a path and the refusal is kept rather than worked around. GitLab supports a
  *relative-URL installation* — `https://example.com/gitlab` — and this is the
  one place that costs something real: such a deployment is refused, by name,
  with its configuration key. Composing the prefix into every declared path
  instead would make the compiled path a function of configuration, which is the
  refusal ADR 065 already made for a WooCommerce subdirectory install and which a
  `..` in a configured prefix is exactly the reason for.

**Neither is a `ModuleDeclaration::PerDeployment`.** This is the difference from
Batch G's templated hosts and from Basecamp. The origin is resolved *by the SDK*
from configuration; nothing about the operation set changes per deployment, so
both declarations stay `&'static` constants and the registry publishes them
directly. A declaration a deployment completes is for values compiled into the
operations themselves — a Basic username, a path prefix, a `From` identity — and
an origin is not one of those.

**A deployment's SaaS instance is the same declaration.** A GitLab.com user
configures `https://gitlab.com` and a Grafana Cloud user configures
`https://acme.grafana.net`. There is deliberately no second connector for the
hosted case: unlike Salesforce's sandbox suffix or HubSpot's forms host, the
hosted instance is the same API on the same paths with the same credential, so a
second declaration would differ only in a string a deployment types anyway.

## Alternatives

| Option | Why Not |
|--------|---------|
| Widen `TemplatedHost` to admit a dotted value, so `gitlab.acme.co.uk` fits | The check that a value is one label is what stops a configured value from being a different authority. ADR 065 refused this for Zoho's eight compiled origins; relaxing it for an arbitrary self-hosted host is strictly worse |
| Declare `{instance}.gitlab.com` and `{stack}.grafana.net`, serving only the hosted products | It would serve the case spec 027 explicitly says is *not* the common one, and a self-managed instance — the whole reason these two providers are in a batch about a deployment's own infrastructure — would be unreachable |
| Accept a path prefix for GitLab's relative-URL install, since GitLab supports it | The compiled path would become a function of configuration, and a deployment could then reach any route on the host by configuring a `..`. The refusal is stated with its key so the deployment learns why rather than getting a `404` on the first activity |
| Accept an `http://` instance for an internal network | The credential is a bearer token with no signing alternative, and "internal" is not a property this repository can check. WooCommerce set the precedent for the one provider that *does* publish an alternative, and even there the answer was no |
| Make both `ModuleDeclaration::PerDeployment` for symmetry with Batch G | Nothing is compiled per deployment: the origin resolves through the SDK's own seam, and a per-deployment declaration would be a `Connector` rebuilt for every instance to hold a value the `OriginSpec` already holds |
| Give the hosted products their own connectors, as Salesforce's sandbox has | Salesforce's sandbox has a *different constant suffix*, which a templated host cannot express two of. GitLab.com and Grafana Cloud are the same API at a different host, which is precisely what a `DeploymentOrigin` is |

## Consequences

`OriginSpec::DeploymentOrigin` now has three hand-written users — WooCommerce,
Mattermost, and this batch's two — and the shape has a name in the module
headers: the deployment names the provider because this workspace cannot. Every
one of them carries the same pair of startup refusals, and a fourth that added a
`DeploymentOrigin` without them would be shipping a credential wherever
configuration pointed.

The cost is the relative-URL GitLab, which is a real installation shape this
connector refuses. It is refused loudly, at startup, with the configuration key
named — which is better than the alternative, where the same deployment would
have configured a prefix this workspace concatenated into every path.
