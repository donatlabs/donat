# donat admin

A platform admin panel for donat deployments, built on the
[`@refinest/*`](https://www.npmjs.com/package/@refinest/core) resource
framework. One panel, several **stands** — and it ships with none of them: a
stand is configuration.

**This engine has no admin role, and this panel is not one.** It runs as an
ordinary role — whatever the deployment calls its operator, `admin` or
`support` or `operator` — declared in that deployment's own YAML metadata with
ordinary per-role permissions. Naming the role here grants nothing: everything
the panel can read or write, any other client holding that role could read or
write, and a role the deployment never declared sees nothing at all.

```
npm install
cp .env.example .env      # point VITE_DONAT_GRAPHQL_URL at your engine
npm run dev               # http://localhost:5174
```

Deployed, the engine serves it — one container, one process, no proxy:

```
npm run build
DONAT_ADMIN_DIR=/path/to/apps/admin/dist donat serve
```

The repository's root `docker-compose.yml` does exactly that: the engine's
image builds this panel into `/usr/share/donat/admin` and sets that variable,
so `docker compose up -d --build` gives you the whole stack on one port.

One origin is the point, and it is not tidiness. A session cookie only comes
back to the origin that set it; the engine delegates CORS to its fronting layer
and would refuse a cross-origin request anyway; and the identity provider
behind `/auth/v1` compares `Origin` against its own public URL and sets a
`__Host-`-prefixed cookie. Served by the engine, all of that is true by
construction. The `nginx.conf.template` here still works for a deployment that
wants the panel somewhere else — set `DONAT_ADMIN_DIR` empty and keep the
proxy — but then the origin is yours to keep consistent, and getting it wrong
is a login that refuses everything without saying why.

## Stands

A **stand** is one deployment seen through one role. The role is part of its
identity rather than a setting inside it, because the panel *is* a role: two
roles against the same endpoint are two stands, and saying so is honest —
they show different data and can do different things.

```json
VITE_DONAT_STANDS=[
  {"id":"eu","label":"EU","graphqlUrl":"https://eu.example/v1/graphql","role":"support",
   "users":{"table":"customer","nameField":"name","emailField":"email",
            "mapping":{"updatableFields":["name","email"]}}},
  {"id":"us","label":"US","graphqlUrl":"https://us.example/v1/graphql","role":"admin"}
]
```

Omit it and the panel serves one stand: `VITE_DONAT_GRAPHQL_URL` as
`VITE_DONAT_ROLE`. Switching stands rebuilds the registry, the endpoint and the
query cache together — it is switching backends, and pretending otherwise would
leave one deployment's rows rendering under another's permissions.

## Users

The screen every stand has. "Who can get in" is the platform's question rather
than any one application's, so it is rendered identically everywhere — same
route, same columns — and the differences live in the declaration:

| Key | What it names |
|---|---|
| `table` | the root serving them: a table, or an **action** proxying a REST API |
| `nameField`, `emailField`, `identityField` | which columns those are |
| `extraFields` | anything else worth a column, in order |
| `mapping.updatableFields` | what this role may write. **Absent means nothing** |
| `mapping.selectFields` | overrides the derived selection |

Each mirrors a permission the deployment already declares: `selectFields` must
be a subset of the role's `select_permissions.columns`, `updatableFields`
should equal its `update_permissions.columns`, and `aggregate: false` belongs
wherever the select permission does not set `allow_aggregations`. Getting one
wrong produces an engine error, never wider access.

### Managing logins rather than rows

A stand that declares no `users` block is a **platform** stand, and gets eight
screens instead of one, under one `Identity` section: Users, Roles, Groups,
Attributes, Scopes, Applications, Blocked addresses and Sessions.

**Applications** is where another product is registered against this
deployment's identity: an id it will present as its `client_id`, whether it can
keep a secret, and the addresses it may be sent back to. Everything else about
a client — scopes, flows, allowed origins, whether it forces a second factor —
is edited afterwards, because that is what the provider accepts on a
registration. A confidential client's secret is not shown here and never
travels through these fields: the provider mints it and serves it to whoever
holds its API key, which is the engine. Each renders
fields the engine serves when a deployment configures `DONAT_OIDC.admin_key`
and `admin_role` — `idp_users`, `idp_roles`, `idp_clients` and the rest, and
everything its API key can reach is there. Nothing is configured here for that: the panel
knows those field names because the engine ships the declaration behind them
([ADR platform/003](../../knowledgebase/platform/decisions/003-the-identity-adapter-ships-in-the-binary-and-grants-nothing.md)),
and `src/stands/identity.ts` is where the screens are declared.

That keeps the provider's credential in the engine, where it belongs — the
panel never holds it — and who may call it stays an ordinary per-role
permission. It is also what makes the panel provider-agnostic: a different
identity provider is a different handler behind the same GraphQL field.

Where the people *are* rows, a stand says so (`users: { table: 'customer', … }`)
and gets the Users screen alone — the other seven render fields such a
deployment's engine does not serve, and a screen that fails on opening is worse
than one that is absent. `table` can also name an **action**
of the deployment's own, with `mapping.fields` saying which fields serve
list / one / create / update / delete and what their arguments are called.

## Talking to the engine

`src/data/donat-data-provider.ts` is the only file that knows GraphQL. donat
serves the Donat v2 API shape, so the documents are the ordinary ones —
`<table>(limit, offset, order_by, where)`, `<table>_by_pk`,
`<table>_aggregate`, `insert_<table>_one`, `update_<table>_by_pk`,
`delete_<table>_by_pk` — or, for a resource served by actions, exactly the
root fields its mapping named.

## Signing in

There are two shapes, and a deployment picks by configuration.

**The provider's page** (default). One button; `/auth/login` redirects to the
identity provider and `/auth/callback` brings the browser back with a session.
Everything interactive lives there — MFA, passkeys, password recovery, the
provider's own brute-force protection — and so does its markup. Any OIDC
provider works this way, and it is the only shape that needs no cooperation
from the one in front of you.

**Our markup on the provider's protocol** (`/idp/authorize`), for Rauthy. The
screen is this panel's, built from the same components as the rest of it; the
protocol underneath is the provider's, unchanged. The page establishes a
session at `POST /oidc/session`, solves the proof of work from `POST /pow`,
sends the credentials to `POST /oidc/authorize` and follows the `Location` the
provider answers with — which carries the code back to the engine's
`/auth/callback`. Authorization code with PKCE throughout; the panel never
sees a token, and the provider still owns every credential and every check it
makes on one.

The two-step email-then-password is the provider's design and is kept: an
account with a passkey and no password never sees a password field. The screen
also carries the rest of what happens around a login — a reset link on request,
"too many attempts" with the time it will listen again, and (behind
`VITE_DONAT_IDP_REGISTRATION`, because the provider decides and announces it
nowhere) creating an account.

**Everything else on the way in is here too.** A passkey is signed on this page
(`webauthn_start`, `navigator.credentials.get`, `webauthn_finish` — the same
three calls, in the same order); new terms are read and accepted on this page,
with declining offered exactly while the provider's `opt_until` still allows
it; and the reset link an email carries lands on `/idp/reset/…`, because the
engine turns the provider's own URL into that one. Two answers are not sign-in
steps at all — an application that demands a second factor the account has not
got, and an account the provider wants updated first — and for those the
provider's own page points at the account screen. So do we. The difference is
that the account screen is ours now.

## Your account

`/account` replaces the provider's `/auth/v1/account`: profile, password and
passkeys, with the password form stating every unmet rule at once from the
provider's `/password_policy` rather than one refusal at a time.

It talks to the provider **directly**, on the session cookie of the browser
that signed in — not through the engine, the way the Identity screens do. Those
act as the deployment and can reach any account; this one acts as a person and
can only ever reach their own, because the only thing it holds is their
session. It also sits outside the guarded shell, since two of the three ways in
are mid-login, when there is a provider session and no engine one. See
[ADR platform/004](../../knowledgebase/platform/decisions/004-the-account-screens-act-as-the-person-the-identity-screens-act-as-the-deployment.md).

It needs three things:

| Where | Setting |
|---|---|
| The engine | `DONAT_OIDC.login_api` — the provider's origin; the engine then serves its login API at `/auth/v1/…` |
| The engine | `DONAT_OIDC.authorization_endpoint` points at `<panel>/idp/authorize` |
| The provider | its public URL is the origin a **browser** uses (Rauthy: `PUB_URL`, `RP_ORIGIN`) |

All three say the same thing: the provider is reached on one origin, the one
the browser is already on. Its session cookie is `__Host-`-prefixed and it
compares the `Origin` header against its own public URL, so a browser calling
it anywhere else is refused — which is exactly the reverse-proxy deployment it
documents. The forwarding lives in the engine (`crates/server/src/idp_proxy.rs`)
rather than in whatever serves the panel, so a deployment states it once; nginx
here just sends all of `/auth/` to the engine as it already did.

`VITE_DONAT_IDP_BASE=` (empty) switches the screen off and the first shape
back on.

The one piece re-implemented rather than called is the proof of work
(`src/idp/pow.ts`), because it has to run in the browser. It is a port of
`spow`'s algorithm in TypeScript rather than the provider's WebAssembly build
of it, which keeps the panel a plain npm project at the cost of being ~10×
slower — so it is solved in a worker while the operator types, not after the
button is pressed.

Either way the panel holds no credential of any kind. It is authenticated the same way
every other client of this engine is — by a verified token — and the token is
not its to hold: the engine's `/auth/login` redirects to the deployment's
identity provider (authorization code + PKCE) and `/auth/callback` puts the
result in an `HttpOnly` cookie no script can read. Requests carry
`credentials: 'include'` and the browser attaches it.

That is the whole of `src/auth/session.ts`. What is left in it is the one
header that still matters — `X-Donat-Role`, which *selects* among the roles the
token already grants and can never add one — and what to do when the engine
says the session is gone (hand the browser back to `/auth/login`).

There is deliberately no local "am I signed in" check. The cookie is invisible
to this code by design, so only the engine can answer, and it answers by
refusing a request. Guessing would either bounce a signed-in operator or wave
through a stale session.

Any OIDC provider works — the engine's `DONAT_OIDC` names the endpoints, and
`session_token` / `client_auth` exist because providers disagree about which
token carries a deployment's roles and how a confidential client authenticates.
See
[ADR 013](../../knowledgebase/api-surfaces/decisions/013-a-role-is-established-by-a-verified-token-or-a-hook-and-by-nothing-else.md).

## Tests

Two suites, and they answer different questions. The unit and mount tests say
whether this code does what it means to; the end-to-end suite says whether the
panel, the engine and an identity provider still agree — which is where the
interesting failures have all been.

```
npm test          # unit + mount tests
npm run typecheck
```

`src/data/donat-data-provider.test.ts` asserts the shape of every document
against a mocked fetch. `src/app.render.test.tsx` mounts the whole app over a
stubbed engine — that one catches wiring the unit tests cannot see (it is how
the group/resource name collision that sent `hrefFor` into infinite recursion
was found).

### End to end

```
cd ../..                          # the repository root
make up
cd apps/admin && npm run e2e
```

Twenty-one cases through a real browser against that running stack: signing in
(two steps, and the proof of work solved in a worker), a wrong password, a
reset link, signing out, creating an account when the build offers it, every
settings screen, creating and removing a role and an account, a password set in
the form and then used at the login screen, and an account that signs in
holding none of the roles this panel acts as.

They are here because none of it can be asserted anywhere else. A session
cookie is `HttpOnly`, a proof of work is arithmetic in a worker, and what a
token grants is the provider's answer rather than ours. Every bug this suite
found on its first run was of that kind: a create form with no create field
behind it, a template that failed on a key nobody typed, a record page for a
resource whose engine offers no single-record read.

The operator's password is read from the repository's `.env` — the file that
stack was started from — so there is nothing to keep in step. `PANEL_URL`,
`PANEL_EMAIL` and `PANEL_PASSWORD` point the suite at a different deployment.

`src/data/donat-data-provider.smoke.test.ts` runs the same documents against a
**live** engine and is skipped unless you give it one:

```
DONAT_SMOKE_URL=http://localhost:8080/v1/graphql \
DONAT_SMOKE_TOKEN=$TOKEN \
DONAT_SMOKE_STAND='{"role":"support","users":{"table":"customer","nameField":"name"}}' \
  npm test -- donat-data-provider.smoke
```

It runs against **your** deployment, described the same way `VITE_DONAT_STANDS`
describes one, with a token for that role.

It is the only suite that can catch a document the engine rejects — a root
field that does not exist, an argument type that does not match, a column the
role may not select.
