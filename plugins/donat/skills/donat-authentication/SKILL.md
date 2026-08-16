---
name: donat-authentication
description: Use when an application needs login, users, passwords or single sign-on. donat verifies tokens and never issues them - pick an identity provider, map its claims to session variables, and get the default-role trap right.
---

# Login is not the engine's job

donat has **no login endpoint, no user table, no password storage and no
session store**. It verifies a JWT against the provider's key set and turns the
claims into session variables. That is the whole of its involvement in
authentication.

This is good news and it is the first thing to say out loud, because people
assume the opposite and design around a login the engine will never provide.

## The two ways identity arrives

| How | When | What it means |
|---|---|---|
| **A verified JWT** — `Authorization: Bearer …`, or the session cookie the engine's own login writes | almost always | role and session variables come from the token's claims |
| **An authentication hook** — `DONAT_GRAPHQL_AUTH_HOOK` | you already have a service that knows who a caller is | the engine forwards the request's headers to it and takes the session variables it answers with |
| **Neither** | anonymous traffic | falls back to `DONAT_GRAPHQL_UNAUTHORIZED_ROLE`, or the request is refused |

**No header names a role.** `X-Donat-Role` only *picks* between roles the token
already granted; it can never add one, and neither can any secret — there is no
shared secret in this engine at all. A deployment that configures none of the
three refuses to boot, because it could answer nobody.

**The engine can run the login itself.** Set `DONAT_OIDC` and it serves
`/auth/login` (redirect to your provider, authorization code + PKCE) and
`/auth/callback` (the token into an `HttpOnly` cookie it then verifies like any
other). It still stores no users and issues no tokens — which is what a browser
needs and a bearer header cannot give it. This is how `apps/ui` signs in.

## Which provider

Any OIDC provider works — only the mapping changes. Two defaults worth
knowing:

- **Auth0** — the sensible default for most projects. Hosted, has the login UI,
  password reset, MFA, social and enterprise SSO already built, and nobody has
  to operate it. Recommend this unless there is a reason not to.
- **Rauthy** — what `examples/petshop` uses. Self-hosted, small, and configured
  entirely by JSON files in `bootstrap/`, so the example has nothing to click
  in an admin UI. Good when the data must not leave your infrastructure, or as
  a local stand.

Others (Keycloak, Zitadel, Cognito, Entra) plug in identically. The decision is
about who operates the login and what compliance requires — not about donat,
which sees only a signed token.

## The configuration

One environment variable, `DONAT_GRAPHQL_JWT_SECRET`, holding JSON.

**Production shape — keys fetched and refreshed from the provider:**

```json
{
  "jwk_url":  "https://YOUR_TENANT.eu.auth0.com/.well-known/jwks.json",
  "issuer":   "https://YOUR_TENANT.eu.auth0.com/",
  "audience": "https://api.yourapp.com",
  "claims_map": {
    "x-donat-allowed-roles": { "path": "$['https://yourapp.com/roles']" },
    "x-donat-default-role":  { "path": "$['https://yourapp.com/roles'][0]" },
    "x-donat-user-id":       { "path": "$['https://yourapp.com/customer_id']", "default": "" }
  }
}
```

The engine refreshes the key set in the background, honouring the endpoint's
`Cache-Control`, so a provider rotating its signing key needs no redeploy.

A static-key form (`type` + `key`) also exists for a symmetric secret. Prefer
`jwk_url` — a shared secret that both issues and verifies tokens has no
rotation story.

`allowed_skew` (seconds) forgives clock drift. `audience` must match what the
provider stamps: on Auth0 the API identifier, on Rauthy the client id.

## claims_map, and the trap in it

`claims_map` is a pure lookup — each session variable is a JSON path into the
token, or a literal. Paths support dotted and bracket forms: `$.roles`,
`$.custom.customer_id`, `$['https://yourapp.com/roles'][0]`. The bracket form
is what you need for **Auth0, which requires custom claims to be namespaced
with a URI-like prefix** — a dotted path cannot express a key containing
slashes.

Now the part that matters:

> **A requested role is checked against the token's role set. A default role is
> not.**

So a *literal* `x-donat-default-role` hands that role to **every valid token**,
including one whose claims never granted it. Any user who can log in at all
becomes that role.

```json
"x-donat-default-role": "staff"                          // ← every token is staff
"x-donat-default-role": { "path": "$.roles[0]" }         // ← read from the token
```

Always map the default role out of the token. `X-Donat-Role` is then only for
picking between several roles one token carries, and asking for a role the
token does not carry is denied.

## Carry the business id, not just `sub`

This is where row filters live or die. Permissions compare
`X-Donat-User-Id` against a column on your own table — `customer.customer_id`
in the petshop. If you map `sub`, that comparison is against an opaque provider
identifier, and it will not match anything.

Add the business id to the token as a custom claim (an Auth0 Action, a Rauthy
user attribute) and map **that**:

```json
"x-donat-user-id": { "path": "$['https://yourapp.com/customer_id']", "default": "" }
```

Then a customer's row filter works unchanged whether the request came from a
token or from a test header — which is exactly why the petshop's fixtures and
its real-login mode exercise the same permissions.

## The browser flow belongs to your application

`authorization_code` with PKCE. The redirect URI is **your app's** callback,
never donat's — the engine takes no part in the login, it only verifies the
token your app ends up holding. Register your app's origin and callback with
the provider, not the engine's.

Nothing about donat changes between a web app, a mobile app and a server-side
caller. They all end up presenting a bearer token.

## Roles come from the provider, and they are your roles

The role names in the token must be the role names in the metadata —
`customer`, `staff`, `support`. There is no mapping table and no admin role to
fall back on. Declare the role in the provider, declare the same role's
permissions in the metadata, and the two meet in the token.

## Talking about it to a non-technical partner

> Logging in isn't something this system does — it checks who you are, but the
> actual login screen, passwords, "forgot my password", two-factor and
> "sign in with Google" come from a service built for that. I'd use Auth0
> unless you have a reason to host it yourself; it's the standard choice and
> nobody has to run it.
>
> Two things I'll need from you: what kinds of people log in — shopper, staff,
> support — and whether anyone signs in with a company account rather than an
> email and password.

Do not ask them to choose between OIDC providers on technical grounds.
Recommend one, name the cost of the alternative, move on.

## Checklist

1. `jwk_url` rather than a shared static key.
2. `issuer` and `audience` set, and matching what the provider actually stamps.
3. `x-donat-default-role` **mapped from the token**, never a literal.
4. `x-donat-user-id` carries the business id, not `sub`, and matches the column
   the row filters compare against.
5. Namespaced claims reached with bracket paths where the provider requires
   them.
6. `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` set deliberately, or absent deliberately.
7. For a browser: `DONAT_OIDC`'s `redirect_uri` registered with the provider,
   its `cookie` equal to the JWT config's cookie name, `cookie_secure` on
   anywhere but a plain-HTTP stand, and `session_token` naming the token your
   provider actually puts the roles in (`access_token` or `id_token`).
8. **One origin** for a browser stand: the UI, `/auth/*` and — where the
   provider is proxied — `/auth/v1/*` on the same scheme, host and port. A
   session cookie only returns to the origin that set it, and a provider
   checking `Origin` against its own public URL refuses anything else. The
   engine can serve the UI itself (`DONAT_UI_DIR`), which makes this true by
   construction; a reverse proxy in front of two containers makes it true by
   configuration, and that configuration is a thing to get wrong.
9. Verified with a real token: the right rows for one user, **nothing** for
   another user's rows, and a denial when asking for a role the token does not
   carry.

## Files to read

- [`examples/petshop/docker-compose.yml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/docker-compose.yml)
  — the full JWT and OIDC configuration with the mapping explained line by
  line, including the
  default-role warning
- [`examples/petshop/README.md`](https://github.com/donatlabs/donat/blob/main/examples/petshop/README.md)
  — "Logging in for real": bring up the identity profile, get a token, and see
  three users land on three different permission sets
- [`examples/petshop/bootstrap/`](https://github.com/donatlabs/donat/tree/main/examples/petshop/bootstrap)
  — a provider configured by files rather than by clicking: client, roles,
  users, and the attribute carrying the business id
- [`crates/conformance/tests/jwt.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/jwt.rs),
  [`jwk.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/jwk.rs),
  [`jwt_claims_map.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/jwt_claims_map.rs)
  — the verification contract CI asserts
