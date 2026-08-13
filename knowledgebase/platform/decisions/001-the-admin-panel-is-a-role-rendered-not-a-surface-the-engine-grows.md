---
type: decision
status: accepted
date: 2026-08-11
features:
  - "[[platform]]"
---

# The admin panel is a role rendered, not a surface the engine grows

## Context

`research-what-a-platform-needs` recorded, among the gaps found by building a
real product on the engine, that *"Admin panel" collides with "no admin role"*
— and concluded it is the platform, not a feature. That leaves the question
this decision answers: where does such a panel live, what does it talk to, and
how does an operator sign in to it, given three settled constraints.

- **No admin role**, and no runtime configuration surface. The runtime
  admin/`run_sql` API was deleted; there is no permission bypass to build a
  panel on.
- **donat does not own identity** — `api-surfaces/010`. Tokens are verified,
  never issued.
- The engine's GraphQL is the Donat v2 shape, and `api-surfaces/006` accepts
  `x-hasura-*` as a session-variable alias, so an existing Hasura-facing
  client is close to a working client.

The concrete prior art is a panel already in production against Hasura
(`solar-app-mono`), built on the `@refinest/*` resource framework: resources
declared as field maps, a registry that generates routes and navigation, and a
single pluggable data provider translating the framework's descriptors into
GraphQL. Its provider is ~900 lines, and every document it emits is one donat
already serves.

## Decision

**The panel is an ordinary role with a UI in front of it, and it lives outside
the engine.** A new `apps/admin` — the repository's first JavaScript — holds a
`@refinest/*` application whose data provider speaks the engine's existing
GraphQL endpoint. The engine grows nothing: no admin API, no panel-specific
route, no privileged role. What the panel can do is exactly what the
deployment's YAML granted the role it asserts, and a mapping that asks for
more produces an engine error rather than wider access.

**It is a platform panel, not an application's.** It ships with no deployment
of its own: a **stand** — an endpoint, a role, and how that deployment spells
its people — is configuration (`VITE_DONAT_STANDS`). One panel serves several,
and the role is part of a stand's identity rather than a setting inside it,
because two roles against one endpoint show different data and can do different
things. The role name is per deployment (`admin`, `support`, `operator`); this
engine has no admin role, so naming one grants nothing.

The screen every stand has is Users, because "who can get in" is the platform's
question rather than any one application's. Where those people are rows, the
mapping names a table. Where they are the identity provider's accounts, it
names an **action** instead — donat proxies a REST API into GraphQL with a root
field backed by an HTTP handler — which keeps the provider's credential in the
engine, leaves who may call it an ordinary per-role permission, and makes a
different identity provider a different handler behind the same field.

A resource's declarations each mirror a permission that already exists:
`selectFields` ⊆ `select_permissions.columns`, `updatableFields` =
`update_permissions.columns`, and `aggregate: false` wherever the role holds no
`allow_aggregations`. This is deliberate duplication of a fact rather than
derivation from it: the panel cannot introspect permissions without a surface
that reports them, and adding one would be exactly the admin API this
repository deleted.

**The panel holds no credential.** It is authenticated the way every other
client of this engine is — by a verified token — and the token is not its to
hold: the engine's own `/auth/login` sends the browser to the deployment's
identity provider and `/auth/callback` puts the result in an `HttpOnly` cookie
no script can read. That login is the relying party `api-surfaces/010`
explicitly permits, and it was built alongside this panel; see
[[api-surfaces/decisions/013-a-role-is-established-by-a-verified-token-or-a-hook-and-by-nothing-else]].

## Alternatives

| Option | Why Not |
|--------|---------|
| Serve the panel from the engine (an `/admin` route on `crates/server`) | The engine would then have a surface whose only purpose is administration, which is the shape the no-admin-role rule exists to prevent — and the first request after that is "let it edit metadata". Static assets are also not a thing this binary should be in the business of serving. |
| Generate the panel from metadata at runtime | Requires the engine to publish its per-role permissions over HTTP. That is a new privileged read surface, and it re-creates the admin API by another name. The declarations are cheap; the surface is not. |
| Build the front end from scratch instead of on `@refinest/*` | The framework's data-provider seam is precisely the boundary this needs, and the reference provider was already ~900 lines of Hasura-shaped GraphQL that donat answers unchanged. Rewriting list/filter/sort/pagination/forms to reach the same place is weeks against days. |
| Use the `refine` ecosystem (or another off-the-shelf admin) | Nothing off the shelf is closer: the shipped Hasura providers assume Hasura's admin secret and its introspection habits, and both are things this engine deliberately does not have. |
| Ship email-and-password login in the panel | The panel would then own identity by proxy — a user store somewhere, passwords in a browser-facing service. `api-surfaces/010` settles this; the relying-party route is the answer. |

## Consequences

An operator gets a working panel today against any deployment, by describing
its stands in one environment variable. The engine is unchanged by the panel
itself, so nothing about it can regress the conformance contract, and the panel
is deletable without trace.

What we pay: a second place where a permission is written down, and a
JavaScript toolchain in a Rust repository (its own `npm` project under
`apps/admin`, outside the Cargo workspace and outside `make test`). The
duplication is bounded — it is per-resource, it is data, and a mistake fails
loudly at the engine — but it is real, and a permission change in metadata that
is not reflected in a mapping shows up as a failed query rather than as a
diff.

The panel is useless without an identity provider, and that is correct rather
than a gap: a deployment with no way to say who someone is has no business
having an operator console. It also means the panel cannot be demonstrated
without one, which is part of why the example now runs its IdP as an ordinary
service rather than behind an opt-in profile.
