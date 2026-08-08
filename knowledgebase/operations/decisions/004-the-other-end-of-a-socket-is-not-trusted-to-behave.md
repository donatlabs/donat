---
type: decision
status: accepted
date: 2026-08-04
features:
  - "[[operations]]"
---

# The other end of a socket is declared, not trusted to behave

## Context

Two defects with one shape: the engine assumed that because a peer was
*declared*, it would act reasonably.

The first was memory. Four outbound paths — an action handler, the auth
webhook, a remote schema, a JWKS document — read their response with
`reqwest`'s `.json()`, which buffers whatever arrives. A compromised peer, a
misconfigured URL that lands on a file server, or an ordinary bug that streams
a table instead of a row would hand back as many bytes as the engine was
willing to hold, and it was willing to hold all of them. An allocation failure
in Rust aborts the process, so one peer could end every request in flight —
the same failure class as the parser stack overflow the depth guard closes.
The connector transport had bounded exactly this since it was written
(`MAX_HTTP_BODY_BYTES`), so the rule already existed; it had simply never been
applied to the paths that predate it.

The second was the database socket. No `RecyclingMethod` was configured, so
`deadpool` used its default `Fast`, which asks only `is_closed()`. Its own
documentation notes that a hard-closed connection can still answer "open" —
which is precisely what a managed Postgres, a connection proxy or a NAT
gateway produces when it reaps an idle socket. The engine retries nothing
anywhere, so that connection became an error on the next caller's query. This
mattered little while the engine could not reach a managed database at all;
after [[002-the-engine-speaks-tls-to-postgres]] it is the ordinary deployment.

## Decision

Every outbound response is read as a stream and abandoned the moment it passes
a ceiling. Two ceilings, because the paths differ in kind: data-bearing peers
(an action's result, a remote schema's response) get
`DONAT_UPSTREAM_MAX_BODY_BYTES`, default 16 MiB; configuration-bearing peers
(session variables, a key set) get a fixed 1 MiB, because neither is ever
large and a megabyte of either means something is wrong. The ceiling cannot be
set to "none" — that is the defect being closed, not a mode to offer.

Pooled connections are proved alive before they are handed out
(`RecyclingMethod::Verified`), which costs one round trip per checkout. A
source on a local socket, where the failure cannot occur and the round trip is
pure overhead, declines it with `pool_settings.verify_connections: false`.

## Alternatives

| Option | Why Not |
|--------|---------|
| One ceiling for every outbound path | A key set and an action's result are not the same size of thing; one number is either too small for data or far too large for configuration. |
| Trust `Content-Length` instead of counting | A chunked response has none, and a dishonest one is exactly the case being defended against. |
| Let a deployment remove the ceiling entirely | Restores the abort; a deployment that needs more should raise the number, which the error message names. |
| Keep `Fast` recycling and retry a failed query | A mutation may already have executed when the connection failed; a retry that cannot tell the difference is worse than an error. |
| Keep `Fast` and let the caller see the error | It is not the caller's mistake, and it happens on a schedule set by someone else's idle timeout. |
| Verify connections only for remote databases | The engine cannot tell where a URL points; a deployment can, and says so with one setting. |

## Consequences

A peer that returns more than its ceiling now fails that one request with a
message naming the limit, instead of ending the process. A deployment whose
action handlers legitimately return more than 16 MiB has to say so — the first
such response tells them exactly what to set.

Every pooled checkout costs one extra round trip. On a local socket that is
tens of microseconds; across an availability zone it is closer to a
millisecond, against a request that already spends at least one round trip on
the statement itself. The test for this kills a backend behind the pool's back
with `pg_terminate_backend` and asserts the dead one is never handed out; it
fails under `Fast`, which is the only way to know it is testing anything.
