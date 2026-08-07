---
type: decision
status: accepted
date: 2026-08-04
features:
  - "[[operations]]"
---

# The engine speaks TLS to Postgres, and refuses an unusable auth configuration

## Context

Two unrelated-looking defects shared one root: the engine's configuration
described a deployment that no longer existed.

`tokio_postgres::NoTls` was hard-coded at every connection site — the request
pools, `migrate`, `validate`, the event loop, the Process reconciler.
[[security-audit]] recorded this as acceptable under a trusted-network threat
model, and noted the consequence only for outbound HTTP. But `NoTls` does not
mean "TLS is terminated elsewhere"; it means the client has no TLS
implementation, so a URL with `sslmode=require` is refused before a socket
opens. Every managed Postgres — RDS, Cloud SQL, Neon, Supabase, Aiven — either
requires TLS or defaults to it. The engine could not connect to any of them,
while the README told people to `docker pull` and deploy.

Separately, `JwtConfig::from_env_value` returned `Option`, and `main` read
`None` as "no JWT configured". Any defect in the value — malformed JSON, an
unknown `type`, an RSA PEM that did not parse, a missing `key` — silently
disabled token verification. With no admin secret set (the documented default
of the fixture mode and every example) the engine then treated each request as
trusted, and a caller chose its own role with a header. A typo in a deployment
variable was a full authorization bypass, announced by nothing.

## Decision

One shared `pgtls::connector()` replaces every `NoTls`. The mode still comes
from the URL, because that is where libpq puts it and where deployments already
set it: `disable` keeps a plaintext socket, the default `prefer` uses TLS when
the server offers it, and `require` / `verify-ca` / `verify-full` now work
instead of failing. Roots come from the host trust store, falling back to the
bundled Mozilla roots when an image has none. A deployment behind a private CA
names its bundle in `DONAT_PG_SSL_ROOT_CERT`, which *replaces* the host store
rather than adding to it — a named bundle is a statement about what should be
trusted, and quietly widening it would defeat the point. A bundle that cannot
be read leaves the trust store empty and the connection failing, rather than
falling back to a different set of certificates than the operator named.

`from_env_value` returns `Result`, and `main` propagates it: an unusable JWT
configuration stops the boot. The messages name the field and repeat the
parser's complaint; they never carry the key. And because "no authentication at
all" remains a legitimate configuration — the conformance harness and the
fixture mode depend on it — a deployment with no admin secret, no JWT and no
auth hook now says so at boot, once, at `warn`.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep `NoTls`, tell operators to run a TLS proxy sidecar | Adds a component to every managed-database deployment to reach a feature the driver already has. |
| Always require TLS | Breaks every existing deployment against a plaintext Postgres, including the test and example stacks. |
| Read `sslmode` ourselves and pick a connector | Duplicates libpq semantics the driver already implements correctly. |
| Add a private CA to the host roots instead of replacing them | A named bundle says what to trust; widening it silently is not what the operator asked for. |
| Fall back to the host store when `DONAT_PG_SSL_ROOT_CERT` is unreadable | Trusts a different set of certificates than the deployment named, at exactly the moment something is already wrong. |
| Keep `Option` and log a warning on an unusable JWT config | A warning in a log nobody is reading is what the silent failure already was; the difference between "insecure" and "will not start" is the whole point. |
| Refuse to boot with no authentication configured at all | Breaks the fixture mode and the conformance harness, and takes a deploy-time decision away from the deployment. |

## Consequences

The engine reaches a managed Postgres, which is the ordinary deployment. The
binary gains `rustls` (with the `ring` provider reqwest's rustls already pulls
in, so there is one provider in the process), `tokio-postgres-rustls`, and two
root-certificate sources. Connections to a plaintext server are unchanged,
which the tests assert directly.

A deployment whose JWT configuration was quietly broken will now fail to start
instead of serving unauthenticated. That is the intended outcome, and it is the
one upgrade note this change carries.
