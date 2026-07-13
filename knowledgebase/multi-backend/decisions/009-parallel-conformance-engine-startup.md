---
type: decision
status: accepted
date: 2026-07-13
features:
  - "[[multi-backend]]"
---

# Retry parallel conformance engine startup

## Context

The conformance harness starts one engine process per suite. Parallel test
threads are required to keep the full matrix fast, but the harness probes a
free TCP port and releases it before the child process binds. A transient port
claim or database startup error can therefore make an unrelated suite fail
before its health endpoint is ready.

## Decision

Keep suite parallelism and retry engine startup up to three times, selecting a
new port for each attempt. A failed `EngineProc` owns cleanup through `Drop`,
which kills and waits for a still-running child. Every attempt gets its own
log path, and the final error reports all attempt failures and logs.

## Alternatives

| Option | Why Not |
|--------|---------|
| Reduce conformance to one test thread | Avoids contention but makes the required full reference suite materially slower. |
| Pass an inherited listener to the child | Eliminates the port handoff race but requires a cross-process listener protocol in the engine CLI. |
| Ignore the occasional startup failure or rerun manually | Leaves CI nondeterministic and hides the underlying diagnostics. |

## Consequences

Parallel conformance remains fast and transient startup failures are absorbed
without leaking child processes. A persistent startup failure may take up to
three health deadlines before failing, but the error now points to every
attempt's log so the cause can be diagnosed.
