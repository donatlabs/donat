---
status: accepted
date: 2026-08-14
tags: [operations, configuration, authentication]
---

# Configuration names a fact once, and refuses to choose between two answers

## Context

`DONAT_OIDC` was one JSON object holding eleven keys, and the deployment that
uses it is the repository's own `docker-compose.yml`. Reading that file back
after the panel moved into the engine, three things were visible at once.

**One fact was written six times.** The browser-facing port appears in the
provider's `PUB_URL` and `RP_ORIGIN`, in the JWT `issuer`, in
`authorization_endpoint`, in `redirect_uri`, and in the published port. The
provider's internal address appears four times. Every repetition is a place
where two copies of one fact can disagree, and when they disagree the symptom
is a login that refuses everything and explains nothing — because each half is
individually valid.

**A credential lived inside a JSON string.** `"admin_key":"API-Key
donat$$${DONAT_IDP_ADMIN_SECRET}"` — a secret spliced into a string inside an
object inside a variable, with `$$` escaping for the file that templates it. It
cannot come from a file or a secret mount without first being made into JSON,
and the escaping is exactly as fragile as it looks: writing this decision up
produced that same line with `$$` instead of `$$$`, which renders the literal
text of a variable name as the key. `docker compose config` caught it. Nothing
else would have, until the provider refused the engine at runtime.

**It was the only configuration shaped that way.** Everything else the engine
reads is a flat `DONAT_*` variable — twenty of them in the deploy guide's
table. Two JSON blobs are the exception, and the exception is where the
credential and the repetition both are.

## Decision

**The same fields are readable one variable per field**, `DONAT_OIDC_*`, and
the JSON form keeps working unchanged.

Two of the new variables are not fields but the facts the fields follow from:

| Variable | What follows from it |
|---|---|
| `DONAT_OIDC_PUBLIC_URL` | `authorization_endpoint` = `…/idp/authorize`, `redirect_uri` = `…/auth/callback` |
| `DONAT_OIDC_LOGIN_API` | `admin_api` = `…/auth/v1` (already), and that a browser can reach the provider same-origin |

Deriving those two endpoints is safe in a way that deriving a provider's own
endpoints would not be: **they are this engine's routes**, at paths this engine
chose, and they cannot vary by provider. `token_endpoint` stays explicit for
the opposite reason — it is Rauthy's `/auth/v1/oidc/token` and Keycloak's
`/realms/x/protocol/openid-connect/token`, and a default would be a guess about
somebody else's software.

**Setting one field both ways is refused at boot, naming both.** Not because a
precedence rule is hard to write, but because whichever we picked, somebody
would eventually edit the losing one and watch nothing happen. A refusal costs
one restart; a silent precedence costs an afternoon.

`DONAT_GRAPHQL_JWT_SECRET` stays a JSON object, and not only for compatibility
— though it is the Donat v2 name and shape, and conformance pins it. Its one
irreducible field is `claims_map`, which says where in *this* provider's token
the roles live. That is the one thing no default can supply: a wrong guess does
not fail loudly, it hands somebody a session with no role and nothing to read.

## Consequences

The compose file names the browser address once and the internal address once.
The secret is its own variable, so it can be a file, a secret mount, or
anything else a deployment already does with credentials — and there is no `$`
escaping anywhere near it.

`.env.example` now separates the one credential a person types from the six
that two programs use to recognise each other. That is not cosmetic: it is the
difference between "seven secrets to manage" and "one password, and some noise
`make env` writes for you".

What we pay: two ways to configure one thing, which is a thing to keep tested
and a thing to explain twice. The refusal on conflict is what keeps that from
becoming two ways to configure one thing *differently*.

## Alternatives considered

**Discovery from the issuer.** `oidc.rs` already refused this, and the reason
holds: it puts a network call between the process starting and the port
opening, and a login route that works only if a third party answered during
boot is worse than two lines of configuration. In this deployment it would also
be impossible — the `issuer` is the browser's address and the engine reaches
the provider on a different one.

**Replacing the JSON form.** It is the Donat v2 shape for the JWT half, and for
the OIDC half it is what existing deployments have written down. A
configuration format that stops reading what it used to read is a migration
somebody has to notice.

**A precedence rule instead of a refusal.** "The flat variable wins" reads
fine in a document and badly in an incident.
