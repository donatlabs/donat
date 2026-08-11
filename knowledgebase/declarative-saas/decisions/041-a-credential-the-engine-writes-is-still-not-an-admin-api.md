---
type: decision
status: accepted
date: 2026-08-09
features:
  - "[[declarative-saas]]"
---

# A credential the engine writes is still not an admin API

## Context

[[010-static-community-connector-factory-and-runtime-boundaries]] fixed the
credential contract: compiled `CredentialSpec` values plus source-bound
deploy-time instances holding read-only `SecretRef` fields, resolved by an
environment resolver that "probes availability at startup, resolves again per
use, and returns an opaque capability limited to the selected compiled auth
action. It has no enumerate, write, refresh, compare-and-swap, delete, or
administration method." That last sentence is the shape of the whole engine in
miniature: configuration is deploy-time, the serving binary reads it, and there
is no surface through which a running deployment can be reconfigured. The same
decision then names its own gap — "Interactive OAuth, refresh-token
persistence, credential CRUD, and tenant onboarding require a separate
decision." This is that decision, for the first two only.

The gap is not academic. Of the widely used third-party systems, roughly a
quarter authenticate with authorization-code OAuth2: the Google and Microsoft
families, Slack, Notion, HubSpot, Salesforce, Shopify, Dropbox, Box, Intercom,
Linear, Zoom. Their access tokens expire in minutes to hours, and several of
them rotate the refresh token on every use, which means the client that fails
to store the new value atomically loses the account. A `SecretRef` cannot
express any of that. It is immutable by construction, and immutability is
precisely what a refresh token is not.

So the engine has to store a value it obtained at runtime. That is one step
away from two things this codebase refuses: a management API, and an admin
role. The refusal is a
[[../../CLAUDE|blocking rule]], not a preference — every data access goes
through an explicit per-role permission, the runtime admin/`run_sql` surface
was deleted, and the admin DATA role was removed. A credential the engine
writes has to be shown not to reopen any of it.

## Decision

The engine may write exactly one kind of credential — an OAuth2 access/refresh
token pair for one connector instance — into one source-local table,
`donat.connector_credential`, sealed with AES-256-GCM under a key that lives
only in `DONAT_CREDENTIAL_KEY`. Nothing else about the credential model
changes.

The reason this does not reopen the no-admin-role rule is that the rule is
about **authority over the deployment reached through the request path**, not
about which rows the engine writes. The engine writes rows constantly; that is
its job. What it must never have is a caller-reachable way to change what the
deployment *is* — its schema, its metadata, its permissions, or the identities
it acts as. Four properties keep the credential on the right side of that line,
and each is enforced structurally rather than by convention:

**The first token is obtained by an operator, not by a request.** `donat
connector authorize` is a deploy-time command in the same family as `migrate`
and `validate`. It needs the metadata directory, the source's database, the
client secret's environment variable, and the sealing key — the exact set of
things a person deploying the system already holds and a request never does.
There is no route that starts an authorization, accepts a `code`, completes a
flow, or displays a token; the acceptance test
`oauth_engine_accepts_no_credential_over_http` runs a real server against a
real seeded credential and asserts that every plausible route is a 404 and that
no GraphQL, REST, or MCP response contains the token or even a
credential-shaped name.

**What the engine writes at runtime is a replacement, never a grant.** The
serving binary can do exactly one thing to a credential row: exchange a refresh
token it already holds for a newer one at the origin the row already names, and
overwrite the two token columns. It cannot create a row, choose a subject,
widen a scope, or point an instance at a different provider. Every one of those
is decided at deploy time and checked before the first write. A compromised
request path therefore cannot manufacture authority; the most it could reach is
a credential that already existed and already had exactly those scopes.

**A credential names a provider account, never a Donat identity.** `subject` is
the provider's own account or tenant identifier, recorded so an operator can
tell two authorizations apart. It never enters a permission decision, never
maps to a role, and never appears in a session. This matters because the
tempting next step — per-end-user tokens — *would* create an identity the
engine acts as, which is why spec 011 puts it out of scope and why it stays
there until it has its own permission story.

**The stored value is useless where it is stored.** Sealing is AES-256-GCM with
a fresh nonce per write and additional authenticated data binding the row to
`source | connector | instance | subject | token_origin`, length-framed so two
adjacent fields cannot be slid into one another. Postgres never holds the
plaintext, a database backup does not carry usable credentials without the
separate key, and sealed bytes lifted from one row do not open in another — so
an attacker who can write the table cannot promote a low-privilege credential
into a high-privilege one. The key is an ordinary `SecretRef`-shaped
environment read, so the credential path fails closed, naming only the
variable, when it is absent.

Two runtime behaviors follow from the same reasoning and are recorded here
because they are the parts most likely to be got wrong later.

Refresh is single-flighted by a transactional row lock and nothing else.
`SELECT … FOR UPDATE` on the one credential row, re-read and re-check under the
lock: a second claimer blocks, then observes the first writer's committed
result and performs no second exchange. There is deliberately no in-process
cache, because a cache is per-binary and this property has to hold across
replicas during a rolling deploy, which is exactly when two binaries are
running at once.

Rotation commits before it uses. A provider that returns a new refresh token
invalidates the old one at that instant, so the new value is written and
committed first, and the access token handed to the caller is read back out of
the row the database wrote (`RETURNING`). A crash between the exchange and the
commit rolls back and loses one attempt; it never leaves a stored token that
was never issued, and it never marks the credential unusable. An
`invalid_grant` does mark it — permanently, for that row — and keeps the row,
because a deleted row tells an operator nothing and a retried one loops
forever.

Refresh happens on use, inside the attempt that needs the header, and is not a
background loop. If proactive refresh is ever added it must wait on
`donat_server::shutdown::idle`, per
[[../../operations/decisions/001-bounded-and-drainable-by-default]] — a loop
that cannot observe the shutdown token cannot be drained on `SIGTERM`.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep tokens in environment variables, refreshed by an external sidecar | Moves the rotation race outside the engine without removing it: two replicas and a sidecar all hold a copy, and the provider invalidates the old one. Nothing but a transaction on the row the engine reads can single-flight it |
| An HTTP callback route so the provider redirects straight to the deployment | That route accepts a credential over the request path, which is the management API this engine deleted. A `127.0.0.1` one-shot listener in the CLI gets the same convenience with no deployment surface |
| Store tokens in plaintext and rely on database access control | A backup, a replica, a read-only analytics role, or a `pg_dump` in a bug report then carries live provider credentials. The AAD-bound seal also blocks moving a row between identities, which access control does not |
| One shared secret for the whole table, no AAD | Sealing without binding leaves the rows interchangeable: an operator who can write the table could open a high-privilege connector under a low-privilege one |
| Delete the row on `invalid_grant` | Deleting the evidence turns "an operator must re-authorize" into "the credential vanished". The row is kept and marked, so `credentials list` says what happened |
| A background refresher that keeps every token warm | One more loop to drain, and it does not remove the on-use path, because a token can still expire between the loop's last pass and the request |
| Per-end-user tokens now | Creates an identity the engine acts as, which needs its own permission story. Out of scope until it has one |

## Consequences

Connectors whose providers require authorization-code OAuth2 become possible,
which is most of the interesting ones. A deployment now has one more
deploy-time step per provider account and one more secret to manage, and losing
`DONAT_CREDENTIAL_KEY` means re-authorizing every instance — which is the
intended failure mode, not an accident of the design.

The engine gains a table it writes outside a caller's request, and with it the
obligation to keep proving that no request can reach it. That proof is a test,
not a comment: the acceptance suite asserts the absence of every credential
route, the redaction of every diagnostic surface, and the single-flight and
rotation properties against a local token-endpoint stub.

The `config.oauth2` declaration was consumed at first only by the authorize,
list, revoke, and refresh paths, which left applying the resulting header to a
provider request as the one open seam — and under
[[034-a-declaration-the-runtime-ignores-is-a-defect]] that was a debt with a
deadline, not a permanent state. It is now closed:
[[043-the-credential-seam-refuses-before-it-sends]] wires the connector executor
through `refresh::with_access_token`, resolves the declaration and the sealing
key at boot rather than per CLI command, and makes a declared credential that
cannot be applied fail the attempt instead of downgrading it to an
unauthenticated request.
