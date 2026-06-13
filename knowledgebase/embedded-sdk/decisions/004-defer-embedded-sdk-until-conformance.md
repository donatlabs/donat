---
type: decision
status: accepted
date: 2026-06-12
features:
  - "[[_index|embedded-sdk]]"
---

# Defer the embedded SDK until core conformance is done

## Context

As of June 2026 the engine has M0–M6 done; websockets, introspection,
inherited roles, relay, and subscriptions are pending (PLAN.md). An embedded
SDK is a permanent support surface: prebuilt binaries per platform, user
crash reports inside the FFI/wasm layer, debugging "segfault in prod" by
mail. That cost is worth paying when the engine has users.

## Decision

Sequence: core conformance → webhook event triggers (conformance work
anyway) → SDK API design over webhooks → embedded transport last, when
there is demand. The idea is archived in this knowledge-base domain so the
reasoning survives the pause.

## Alternatives

| Option | Why Not |
|--------|---------|
| Embedded SDK now | Highest-maintenance artifact built before any user exists; starves conformance work that everything else depends on |
| Drop the idea | The niche ("Donat compatibility + PocketBase DX") is genuinely unoccupied; archiving costs nothing |

## Consequences

No embedded work blocks the conformance critical path. When resumed, design
starts from [[001-webhooks-first-transport-agnostic-sdk]] and
[[005-wasm-compiler-core-over-cgo-or-go-rewrite]] instead of from scratch.
