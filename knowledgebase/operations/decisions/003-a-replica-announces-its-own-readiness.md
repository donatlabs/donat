---
type: decision
status: accepted
date: 2026-08-04
features:
  - "[[operations]]"
---

# A replica announces that it is going away before it goes away

## Context

[[001-bounded-and-drainable-by-default]] made `SIGTERM` drain instead of cut:
the HTTP server stops accepting, finishes the requests it holds, and lets the
background workers finish their item. That fixed the requests already in
flight. It did nothing for the ones still arriving.

A load balancer routes by its own view of which replicas are alive, and it
learns that view by asking. The engine's only answer was `/healthz`, a static
`"OK"` that knew nothing about anything — not the database, and not whether
the process had already decided to stop. So the sequence a rolling deployment
actually produced was: signal arrives, listener closes, balancer keeps routing
here for another second or several, and every request it sends in that window
is refused at the socket. The connections already open were drained politely
and the arriving ones were dropped on the floor, which is a strange way to
finish a deploy — and a change that improved the visible symptom while leaving
the interesting half of it in place.

## Decision

Stopping happens in two phases. On the signal the process reports itself **not
ready** and keeps serving; only after `DONAT_SHUTDOWN_READINESS_DELAY_SECONDS`
(default 5) does it stop accepting and begin the drain that ADR 001 describes.
`/readyz` answers `200 READY` until that first phase, `503 DRAINING` after it.
A deployment fronted by something that does not poll readiness can set the
delay to `0` and get the previous behaviour exactly.

`/healthz` stays a static `"OK"`, and neither route probes the database. That
is deliberate in both directions. A *liveness* probe that fails because the
database is unreachable asks the orchestrator to restart a process whose
restart cannot help, converting one outage into a crash loop across every
replica at once. A *readiness* probe that follows a transient database blip
removes every replica from rotation simultaneously, which is worse than the
blip it is reacting to; a source that is genuinely gone surfaces as an ordinary
error on the request that needed it, which is both more accurate and more
localised.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep one endpoint and make it fail on shutdown | An orchestrator reads liveness and readiness differently; a failing liveness probe during a graceful stop invites a `SIGKILL` in the middle of the drain. |
| Have readiness check the database | Ties every replica's rotation to one shared dependency, so a blip empties the pool of replicas rather than failing a few requests. |
| Have liveness check the database | Restarting cannot fix an unreachable database, and a restart loop makes the recovery slower. |
| Close the listener immediately and let the balancer find out | That is the behaviour this replaces: it drains open connections and refuses arriving ones. |
| Drain the workers during the readiness delay too | The delay exists so this replica can still serve; standing its workers down first would mean accepting work with nobody to consume it. |

## Consequences

Every stop now takes the readiness delay longer — five seconds per replica by
default, which a rolling deployment pays once per replica and can set to zero.
In exchange a deploy stops producing refused connections, and an operator has
a route that answers the question they actually have ("should this replica be
receiving traffic?") rather than the one it is easy to answer.

Deployment manifests should point their readiness probe at `/readyz` and their
liveness probe at `/healthz`. A manifest that points both at `/healthz` gets
the old behaviour silently, which is the one way to hold this wrong.
