---
type: decision
status: accepted
date: 2026-08-01
features:
  - "[[api-surfaces]]"
---

# donat does not own identity; the example ships an optional IdP profile

## Context

donat can consume JWTs from any OIDC provider — `crates/server/src/jwt.rs`
supports `jwk_url` with a background JWKS refresher, HS/RS/ES/EdDSA keys,
optional `audience` and `issuer` validation, the token in `Authorization`, a
cookie or a custom header, and `claims_map` to project claims onto session
variables. What the repository never showed was how an operator obtains such a
token in the first place. The petshop example papers over the gap:
`examples/petshop/docker-compose.yml` sets `DONAT_GRAPHQL_ADMIN_SECRET` and
selects the role with an `X-Donat-Role` header, described in its own comment as
"a demo stand-in for real edge auth / JWTs".

The question raised was whether donat should ship its own authentication —
a built-in user store with signup/login, in the shape Supabase popularised —
so that a first-time user gets a working login without deploying anything else.

Three existing constraints bear on the answer. The threat model in
`knowledgebase/security-audit.md` states donat is an internal component behind
a mesh, with TLS termination and edge authn/z in front of it: rate limiting,
CORS and request timeouts are explicitly delegated to the fronting layer, and
Postgres connections use `NoTls`. (That document's claim that `reqwest` has no
TLS backend is stale — the workspace enables `rustls-tls`, so outbound HTTPS,
including an `https://` JWKS, works.) The no-admin-role invariant means no surface
may hand out a role that was not explicitly permitted. And the embedded-SDK
direction (`knowledgebase/embedded-sdk/wasm-compiler-core.md`) already assigns
authentication to the host application: the host SDK owns "HTTP/WS with the
user's own middleware/auth/metrics; JWT → session vars", while the wasm core
receives session variables as input.

## Decision

**donat does not own identity.** No authentication server, no user store, no
password handling, no session issuance in the engine. Authentication happens
before donat; the engine's contract begins at a verified token or a trusted
request, and every data access still resolves to an explicit role with explicit
permissions. This matches the niche — Hasura, whose surface we are compatible
with, delegates authentication to an external service for the same reason —
and it keeps the documented threat model intact.

The developer-experience gap is closed in the example rather than in the
architecture: an **optional `auth` compose profile** in the petshop example runs
[Rauthy](https://github.com/sebadob/rauthy) as a sidecar IdP, provisioned
entirely from JSON files checked into the repository (`bootstrap_dir` with
`clients.json`, `roles.json`, `users.json`). One command yields a working
login, roles, a real token and a query executed under that role, with zero
engine code. Rauthy runs against the same Postgres container in its **own
database** (`hiqlite = false`, `pg_db_name = 'rauthy'`) with its own role.

Roles are taken from Rauthy's `roles` claim, mapped by `claims_map` to
`x-donat-allowed-roles`. This is forced by the code, not by taste: `claims_map`
resolves values by JSON path with no transformation (`jwt.rs:456-466`) and role
comparison is exact (`jwt.rs:554`), so a prefixed-group convention would
require metadata role names to literally carry the IdP's client prefix.

An external IdP is **not** an admin-role bypass. It supplies session variables;
it grants nothing by itself.

The boundary is *owning* identity, not touching authentication at all. Teaching
donat to act as an OpenID Connect relying party — a login route that redirects
to the configured provider and a callback that sets the session cookie the
engine already reads (`TokenLocation::Cookie`) — stores no users, holds no
passwords and issues no tokens, so it stays on the allowed side of this
decision. It is not part of this change, but nothing here forbids it. *It was
built later: see
[[decisions/013-a-role-is-established-by-a-verified-token-or-a-hook-and-by-nothing-else]].*

## Alternatives

| Option | Why Not |
|--------|---------|
| Build a minimal built-in auth (users table, signup/login, argon2, refresh tokens) | The cost is not password hashing, it is the threat model: a password-accepting endpoint in a service with no TLS backend, no rate limiting and no CORS invalidates the entire posture in `security-audit.md` at once. It also spends time on the thing that is not the differentiator — `knowledgebase/declarative-saas/` holds 32 ADRs on commands, rules, processes and connectors, and that is the product. Feature parity is unreachable anyway: a bounded MVP means email and password, against Rauthy's MFA, passkeys, password recovery, brute-force protection and key rotation. |
| Build it because only a built-in issuer gives token revocation | Revocation is obtainable on the external path too: Rauthy exposes an introspection endpoint, and adding an introspection mode to the validation path is a small bounded feature next to writing an IdP. Not a reason to own identity. |
| Embed Rauthy as a library in the donat binary | Only `rauthy-client` (an OIDC client) is published; the server crates are not. It would mean a git dependency on another workspace's internals without semver, actix-web alongside our axum, and their frontend assets — a permanent fork of an IAM, not an integration. |
| Authelia instead of Rauthy | Its config is fully declarative and re-read on start, which is closer to our metadata model, but access tokens are not JWTs by default (opaque unless RFC9068 encoding is enabled), the OIDC provider role is still beta, and it is fundamentally a forward-auth portal. Wrong token format for a JWKS-validating engine. |
| A hosted IdP (Auth0, Okta, Cognito) in the example | Fully supported for operators and covered by "bring your own IdP" at zero cost to us, but it cannot be the example's default: petshop is built to run without an account anywhere and to come up offline and in CI. It would also point a self-hosted engine's first-run experience at a per-user SaaS. |
| Dex | Owns no user state; it delegates to an upstream IdP, so it cannot stand alone in an example. |
| Keycloak / Zitadel / Authentik | External database, multi-tenancy, admin-UI-centric provisioning. Contradicts the deploy-time-configuration model. |
| Rauthy sharing the petshop database via an extra schema | Rauthy has no schema/`search_path` option at all, and it would place password hashes in the database the data plane serves. A separate database in the same instance costs nothing operationally and gives real isolation. |
| Make the IdP a mandatory part of the example | Would force every reader through an identity setup to see the data plane. The profile keeps the minimal path unchanged. |

## Consequences

The engine gains no authentication code, no new dependency and no change to the
threat model, while the repository finally demonstrates a real login. The
`claims_map` and JWKS paths are identical for Rauthy and for any operator's own
IdP, so the example doubles as the "bring your own IdP" recipe.

What we pay for it:

- **Rauthy's bootstrap is a seed, not desired state.** The JSON files are
  applied only while initializing an empty database; later edits are ignored on
  restart. Resetting the demo means recreating the volume, and day-2 changes in
  a real deployment go through Rauthy's admin API. This is weaker than donat's
  own metadata contract, where the directory is re-read at every boot.
- **No token revocation** on the engine side; short access-token TTLs and
  client-side refresh until an introspection mode exists.
- **A hosted IdP is a documented option, not the example's.** Auth0, Okta and
  friends work — the workspace enables `rustls-tls`, so an `https://` JWKS
  fetches fine, and `claims_map` does not care who issued the token. They are
  not used *in* the example because petshop is deliberately built to run
  without an account anywhere (the same reason the five connector providers are
  answered by a local mock), and it must come up offline and in CI. A SaaS IdP
  would need a tenant, credentials in the repository and network access.
- **The admin secret outranks JWT.** In `resolve_session` a valid
  `X-Donat-Admin-Secret` short-circuited the JWT path entirely and built the
  session from headers — an authentication bypass, though not a permission
  one. *Superseded by
  [[decisions/013-a-role-is-established-by-a-verified-token-or-a-hook-and-by-nothing-else]]:
  the secret and the header path were removed outright, so this hazard no
  longer exists and the example's login is not decorative.*

If donat ever ships a deployment shape where no second process can exist, this
decision is worth revisiting. The embedded-SDK direction is not that case: it
hands HTTP and authentication to the host application, which needs identity
from donat even less than the server does.
