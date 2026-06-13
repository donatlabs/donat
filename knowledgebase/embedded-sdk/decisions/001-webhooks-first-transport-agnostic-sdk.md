---
type: decision
status: accepted
date: 2026-06-12
features:
  - "[[hooks-and-events]]"
  - "[[ffi-boundary]]"
---

# Build webhook event triggers first; SDK handler API identical across transports

## Context

Event triggers (PG triggers → event journal → delivery with retries) are
required for Donat v2 conformance against tests-py regardless of the
embedded-SDK idea. The embedded idea wants the same events delivered to
native host-language functions instead of webhooks.

## Decision

Build the journal + delivery machinery with HTTP webhook delivery first (it
is conformance work anyway). Extract a `DeliveryTransport` abstraction
supporting both **pull** (Go consumer) and **push** (Node event loop) modes.
The SDK's user-facing handler signatures must be identical over the webhook
transport (sidecar mode) and the embedded transport — transport
interchangeability is the design acceptance criterion. The webhook-mode SDK
can ship before any FFI exists.

## Alternatives

| Option | Why Not |
|--------|---------|
| Embedded-first, webhooks later | Duplicates delivery machinery; blocks conformance; builds the risky part before the required part |
| Separate APIs per transport | Users can't switch embedded ↔ sidecar without rewrites; kills the cgo-averse escape hatch |
| Pull-only transport | Node has no thread to block; push is mandatory for the event-loop model |

## Consequences

The delivery machinery is built once where conformance tests cover it; the
embedded transport becomes an optimization layer, not a parallel system.
Users who refuse cgo always have a working sidecar path with unchanged code.
Cost: the transport abstraction must be designed before the second transport
exists (risk of speculative generality — kept small: pull + push + ack).
