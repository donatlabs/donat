---
type: decision
status: accepted
date: 2026-08-09
features:
  - "[[declarative-saas]]"
---

# Connectors are written by hand against provider documentation

## Context

[[010-static-community-connector-factory-and-runtime-boundaries]] planned a
development-time import pipeline: a hostile-acquisition tool, reviewed source
records, a normalized manifest, and an offline generator emitting checked-in
Rust. Three phases of it landed and are good: the shared value contract, the
neutral connector ABI, and `donat-connector-catalog` with its canonical
projections and hashes. The remaining phases — acquisition, code generation,
and the sealed processor boundary — were the larger half of the cost.

Their whole purpose was to make someone else's integration packages importable
at scale without a lawyer in the loop for each one. Two things about that
turned out to be wrong.

The first is arithmetic. Importing a donor artifact requires a per-version
source record, per-file hashes, a license decision for every dependency and
embedded asset, a notice, a reviewer, and a named failing test — before any
Rust exists. That review is not cheaper than writing the connector. It is more
expensive, and it recurs on every upstream version bump, while the connector
itself would not have needed to change.

The second is that the pipeline solved the wrong problem. The cost of shipping
an integration is not producing a declaration; a declaration is a few dozen
lines. The cost is the shared runtime underneath it — credential application,
token refresh, pagination, error classification, redaction, signature
verification, bounds. The factory generated declarations and left every one of
those to be written by hand anyway. Building the runtime and hand-writing the
declarations gets integrations sooner than building a generator for the cheap
half.

## Decision

A connector is ordinary reviewed Rust in this workspace, written from the
provider's own published documentation or its published API schema.

The shared runtime lives in one crate, `crates/connectors`: fixed-origin
transport, the operation declaration and its builder, credential auth plans,
pagination plans, the error map, webhook verification helpers, and the local
provider stub every connector test runs against. A connector module is a
static declaration plus, where a provider's protocol demands it, a narrow Rust
processor. Adding one touches a module, a test file, a registry line, and a
conformance fixture.

`crates/connector-acquire` and `crates/connector-codegen` are not built. There
is no acquisition tool, no offline generator, no `generate --check`, and no
donor source, fixture, or generated catalog in this repository. No third-party
integration project's code, tests, fixtures, or field text is copied,
translated, or used as ground truth; every request shape and every fixture is
authored here from the provider's own material.

The sealed processor crate is not built either, and this is the one place where
the decision gives something up. Its purpose was to make it mechanically
impossible for generated or donor-derived processor code to reach transport,
credentials, or crypto — a boundary enforced by crate graph and a source
checker rather than by review. Hand-written processors live as modules inside
`crates/connectors` instead, and the same property is held by review and by the
tests that prove a processor cannot construct a URL, header name, credential,
or request. That is a weaker guarantee, and it is acceptable only because every
processor is now code we wrote and reviewed rather than code a generator
produced from someone else's package.

`donat-connector-catalog` stays. Its descriptors remain the shape of a
connector and its canonical projections remain how a behavioral contract is
pinned into a process revision — the instances are now authored by hand rather
than emitted by a generator. Its provenance material narrows accordingly to
what a hand-written connector can honestly assert.

Every runtime boundary that ADR 010 established remains in force and is not
reopened by this decision: fixed compiled origins, the closed
`ConnectorErrorClass` set, bounded requests and responses, no dynamic
destination from input, deploy-time metadata as the only configuration, no
runtime plugin loading of any kind, and no admin role.

## Alternatives

| Option | Why Not |
|--------|---------|
| Finish the factory as specified | Per-connector admission review costs more than the connector, recurs on every upstream version, and produces the cheap half of the work |
| Load connectors at runtime (WASM, shared library, package) | Reintroduces untrusted code into production, which the one-binary model exists to prevent |
| Delegate integrations to a hosted integration service | Puts a third party in the data path of every client's provider calls, and moves credentials outside the deployment |
| Generate from provider OpenAPI only, keeping the generator | Provider schemas describe surface, not behavior: pagination, idempotency, error semantics, and webhook verification are prose. The generator would still emit only the cheap half |

## Consequences

We own every request shape, every fixture, and every error mapping, so a
failure is ours to read and fix rather than an upstream's to explain. There is
no donor version to track, no notice obligation per integration, and no
generated artifact whose determinism must be proven in CI.

We pay for it in breadth. There is no mechanism that turns a hundred upstream
packages into a hundred connectors; each one is hand-work, and the list grows
at the speed of the team. We also lose the automated upgrade diff: when a
provider changes its API, we find out because our tests fail or a client
reports it, not because a re-import reported a semantic difference. That makes
the per-connector test suite the only early warning, which is why every
connector specification requires the full request-shape, error-map, pagination,
and effect-classification proofs rather than a happy path.
