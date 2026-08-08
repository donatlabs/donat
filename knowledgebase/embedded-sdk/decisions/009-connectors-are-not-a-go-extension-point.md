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

The embedded SDK lets an application supply Go code at several points: a
function behind a declared action, event handlers after a commit, its own
writes inside the engine's transaction through `ExecuteTx`, and its own HTTP
routes beside `eng.Handler()`. One more was proposed by analogy — let an application implement a *connector* in Go, so an
outbound integration would be an ordinary function in the host binary rather
than checked-in Rust.

The analogy is appealing and wrong for two independent reasons, and it is worth
recording both, because each alone would be enough.

## Decision

Connectors are not a place user Go code plugs in.

The first reason is reachability. A connector is invoked from a durable Process
activity (`crates/server/src/processes/activity.rs`), and that activity loop —
along with transitions, timers, leases and webhook ingress — lives host-side in
`donat-server` and has no counterpart in the embedded host. A Go connector
would therefore have nothing to call it. "Connectors in Go" is not a feature
next to the other three; it is "port the durable-process runtime to Go", which
is a different project with a different decision to make.

This is narrower than it first appeared. The embedded core does compile Process
definitions, because a command that starts or signals one needs its effect
contract, so a command *may* originate durable work: the journal write compiles
into the same statement as the command's own writes. What the host lacks is the
loop that carries that work forward, and a `donat-server` against the same
database supplies it. Originating durable work and executing a connector
activity are separate capabilities, and only the second is blocked.

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
access, running after the commit. What it does not get *in-process* is the
Process machinery: retries, leases, idempotency windows and the journal. It can
have those by starting a Process and letting an engine drive it. That is the
honest trade, and it is the same one [[002-keep-durable-journal-alongside-in-memory-hooks]]
already describes for in-memory hooks.

## Alternatives

| Option | Why Not |
|--------|---------|
| Let an application register a Go connector at startup | Moves credential policy and origin pinning out of the reviewed boundary [[010-static-community-connector-factory-and-runtime-boundaries]] built, and there is nothing to invoke it until Processes run in the host anyway. |
| Port the durable-process runtime to Go first, then add connectors | A real option, but a project — transition queue, leases, timers, webhook ingress — not a fourth extension point. It needs its own decision, not this one's. |
| Refuse commands with effects, so the host never originates work it cannot finish | Costs a deployment every command that touches a Process, including the ones whose Process an engine beside it would drive perfectly well. The journal is source-local and in the same database precisely so its writer and its driver need not be the same process. |
| Call connectors from commands rather than Processes | Puts external I/O inside the single statement a command compiles to, so a provider outage becomes a failed write and a rollback cannot undo a call already made. |
| Say nothing and let it be discovered | The gap is invisible from the Go side: a host with no connector support looks identical to one whose connectors were not declared. |

## Consequences

The SDK's extension surface is a closed list, and its README says so, including
what is refused and where. An application whose integrations are
fire-and-forget after a commit embeds and writes them as handlers. One that
needs durable retry semantics starts a Process from a command and runs an
engine beside it to drive that Process — the journal is in the same database,
so the writer and the driver need not be the same process.

What is paid: an embedded host alone cannot finish durable work, only originate
it, and the difference is not visible at deploy time. A snapshot compiles, the
command succeeds, the journal row is written — and if nothing is driving the
Process, it simply waits. That is a deployment mistake the engine cannot catch
for you, and it is the price of not refusing the whole command outright.
