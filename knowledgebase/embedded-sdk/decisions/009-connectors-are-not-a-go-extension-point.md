---
type: decision
status: accepted
date: 2026-08-04
features:
  - "[[embedded-sdk]]"
  - "[[007-community-connector-factory]]"
---

# Connectors are not a Go extension point, on either host

## Context

The embedded SDK lets an application supply Go code at three points: event
handlers after a commit, its own writes inside the engine's transaction through
`ExecuteTx`, and its own HTTP routes beside `eng.Handler()`. A fourth was
proposed by analogy — let an application implement a *connector* in Go, so an
outbound integration would be an ordinary function in the host binary rather
than checked-in Rust.

The analogy is appealing and wrong for two independent reasons, and it is worth
recording both, because each alone would be enough.

## Decision

Connectors are not a place user Go code plugs in.

The first reason is reachability. A connector is invoked from a durable Process
activity (`crates/server/src/processes/activity.rs`). Processes need a journal,
a transition queue, leases and webhook ingress, all of which live host-side in
`donat-server` and have no counterpart in the embedded host — which is why
`finalize_command_effects` refuses, at snapshot compile time, any command whose
effects target a Process. A Go connector would therefore have nothing to call
it. "Connectors in Go" is not a feature next to the other three; it is
"port the durable-process runtime to Go", which is a different project with a
different decision to make.

The second reason survives even if that project is ever done.
[[010-static-community-connector-factory-and-runtime-boundaries]] settles what a
connector *is*: repository-reviewed source, checked-in generated data, and
native processor code whose dependencies are constrained at the crate boundary
so that fixed-origin and non-exfiltration claims are enforceable. It explicitly
rejects loading any plugin at runtime, and it rejects letting processors own
their own transport or credentials. A connector registered by an application at
startup would reintroduce exactly the trust boundary that decision removes: the
credential policy and the origin pinning would move from a reviewed crate into
whatever the operator wrote.

An application that needs to call an external service from an embedded host
still can — from an event handler, which is ordinary Go with ordinary network
access, running after the commit. What it does not get is the Process
machinery: retries, leases, idempotency windows and the journal. That is the
honest trade, and it is the same one [[002-keep-durable-journal-alongside-in-memory-hooks]]
already describes for in-memory hooks.

## Alternatives

| Option | Why Not |
|--------|---------|
| Let an application register a Go connector at startup | Moves credential policy and origin pinning out of the reviewed boundary [[010-static-community-connector-factory-and-runtime-boundaries]] built, and there is nothing to invoke it until Processes run in the host anyway. |
| Port the durable-process runtime to Go first, then add connectors | A real option, but a project — journal, transition queue, leases, webhook ingress — not a fourth extension point. It needs its own decision, not this one's. |
| Call connectors from commands rather than Processes | Puts external I/O inside the single statement a command compiles to, so a provider outage becomes a failed write and a rollback cannot undo a call already made. |
| Say nothing and let it be discovered | The gap is invisible from the Go side: a host with no connector support looks identical to one whose connectors were not declared. |

## Consequences

The embedded SDK's extension surface is exactly three points, and its README
says so, including what is refused and where. An application whose integrations
need durable retry semantics runs the standalone engine; one whose integrations
are fire-and-forget after a commit can embed.

What is paid: an application cannot move an existing connector-backed
integration into an embedded host without also giving up its durability
guarantees, and there is no partial path — the refusal is at snapshot compile
time, not per request.
