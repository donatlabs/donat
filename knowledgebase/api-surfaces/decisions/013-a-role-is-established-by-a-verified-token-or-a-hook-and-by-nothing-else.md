---
type: decision
status: accepted
date: 2026-08-11
features:
  - "[[api-surfaces]]"
---

# A role is established by a verified token or a hook, and by nothing else

## Context

Until now the engine accepted a role three ways: from a verified JWT, from an
authentication hook, and from `X-Donat-Role` on a request that presented
`DONAT_GRAPHQL_ADMIN_SECRET`. The third had a further property that made it
outrank the others — `resolve_session` checked the secret *first*, so a request
holding it asserted any role by header whatever a configured token said.

That was defensible as API-level authentication and was documented as such
everywhere. It was also the shape of the thing this repository exists to not
have. Two facts decided it:

- **A deployment with no authentication configured trusted every caller.** With
  no secret, no JWT and no hook, `secret_ok` was `true` and headers were
  honoured — so the *least* configured deployment was the *most* permissive.
  The boot logged a warning. Every conformance suite and both system stands ran
  in exactly that mode.
- **The secret authenticates a caller, not a person.** Anything behind it is
  outside an audit trail by construction, which makes it unfit for the one
  surface that most wants a login: an operator-facing panel.

The panel (`platform/001`) forced the question, and the answer had to hold for
a browser, which cannot put a bearer header on a navigation.

## Decision

**Remove the admin secret entirely, and reduce the ways a role is named to
two: a verified JWT, or an authentication hook.** No header names a role.
`X-Donat-Role` survives only as a *selector* among the roles a token already
granted — it can never add one — and `DONAT_GRAPHQL_ADMIN_SECRET` is gone from
the engine, the examples, the SDK, the stands and the skills.

A request nothing authenticated runs as `DONAT_GRAPHQL_UNAUTHORIZED_ROLE`, and
is refused when none is configured. **A deployment that configures none of the
three refuses to boot**, because it can resolve a session for nobody and would
otherwise discover that one denied request at a time.

**The engine may serve the login itself** (`DONAT_OIDC`): `/auth/login`
redirects to the configured provider with authorization code + PKCE,
`/auth/callback` verifies `state` against a short-lived cookie, exchanges the
code server-to-server and writes the resulting token into the `HttpOnly` cookie
`jwt.rs` already reads. It stores no users, holds no passwords and issues no
tokens — the boundary `api-surfaces/010` drew, which that decision already said
this side of stays legal. `?redirect=` accepts only a path on this origin: an
open redirect on the route that mints sessions is worth more than most bugs.

Two configuration fields exist because **providers differ, and a deployment may
use any of them**. `session_token` names which token of the exchange carries
the deployment's claims (rauthy carries them in the access token; Keycloak and
Auth0 are commonly configured to put roles in the id token). `client_auth`
names how a confidential client proves itself (`client_secret_post` or
`client_secret_basic` — OAuth makes Basic mandatory for servers and POST
optional, and real providers implement one or the other). Both are refused at
boot when misspelled, because sending the wrong token or the secret in the
wrong place fails as a login nobody can explain. When the login is configured,
the JWT reader accepts the session cookie *in addition to* its configured
location, so one deployment serves a browser panel and API clients at once.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep the secret as a documented local-stand mechanism | It stayed configured in every example and both stands, and it outranked the token wherever it was set. A mechanism that is "for local only" and present in every deployment file is not for local only. |
| Keep header trust, drop only the secret | Then the *default* configuration — nothing set — trusts every caller with a role header. Strictly worse than what was there. |
| Refuse to boot without a JWT or a hook | Rejects a legitimate shape: a public read-only deployment that serves one anonymous role and needs no identity at all. The boot check accepts `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` as a third answer for exactly that reason. |
| Have the panel run the OIDC flow itself as a public SPA client | Then the token lives where scripts can read it. The engine already terminates the request; a cookie it sets is `HttpOnly`, and the panel never holds a credential at all. It also needs no CORS, because the panel is served from the engine's origin. |
| Discover endpoints from an issuer's `.well-known` | Puts a network call between the process starting and the port opening, and a login route that works only if a third party answered during boot is worse than two lines of configuration. |
| Take the id token always (or the access token always) | Wrong for half the providers either way. It is a per-provider fact, so it is declared. |

## Consequences

The least-configured deployment is now the *most* restrictive rather than the
least, and the engine says so at boot instead of in a log line nobody reads.
A browser can obtain a session without the panel — or any application — holding
a credential.

What we paid: the conformance harness could no longer rely on header trust, so
every suite now runs behind an authentication hook the harness itself serves
(`crates/conformance/src/auth_hook.rs`). That is a real cost and a real gain —
the hook path had **zero** conformance coverage while the header path existed,
and now nearly the whole crate exercises it.

Three error shapes changed, and no Donat-derived fixture asserted any of them:
a request that names no role is now `401` with the hook's refusal rather than
`200` with `x-donat-role header is required`. Two of ours (petshop's
`permissions.yaml`, the REST unknown-endpoint case) were updated to say so.

`examples/petshop` gained a required service: the identity provider it used to
run behind an opt-in profile is now the only way in, which is honest about what
this engine needs and makes the example demonstrate the deployment shape it
recommends. The single-surface examples verify a development key and sign their
own tokens with `examples/mint-token.sh`.
