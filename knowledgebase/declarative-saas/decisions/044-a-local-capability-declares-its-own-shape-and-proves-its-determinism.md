---
type: decision
status: accepted
date: 2026-08-09
features:
  - "[[declarative-saas]]"
  - "[[018-local-capabilities]]"
---

# A local capability declares its own shape, and `Pure` is admitted on a double render

## Context

Spec 018 adds work that has no provider: render an invoice, build a responsive
email, produce a spreadsheet, make a QR code. It needs no origin, no
credential, no network, and no idempotency evidence, because it is a function
of its input. Spec 018 §6 asks for these to "register into the same static
table as provider connectors, with `origin: None` and `credential: None`", and
spec 018 §3 admits a new effect class, `Pure`, on the strength of determinism —
"same input bytes produce byte-identical output" — while also saying the
requirement "is enforced, not documented".

The SDK's `Connector` is built entirely around a request: an `OriginSpec` that
resolves to one immutable authority, a `CredentialSpec` with an auth plan, and
`Operation`s carrying a method, a path template, success statuses, and response
pointers. `ConnectorBuilder::build` refuses a connector with no origin and no
credential on purpose, and `Effect::admit_method` exists because an effect
class and an HTTP method have to agree.

## Decision

A local capability is its own declaration, `donat_connectors::local`, not a
`Connector` with holes in it. It carries a reserved `local.*` name, a contract
version, and operations that declare an effect class, five bounds, a unit
count, and the executor compiled into this binary. What a connector declares —
origin, credential, method, path, statuses — is not declared as `None`; it is
absent, because none of it exists. `Connector` keeps refusing a declaration
without an origin, and `Effect::pure` is refused by `admit_method` on every
method, so a pure class can never end up classifying a request.

`Pure` carries `DeterminismEvidence`: one operation input to render on, and the
statement of what makes the output a function of that input alone. Carrying the
probe is what turns the determinism requirement into a registration condition —
`LocalOperationBuilder::build` renders the probe twice and compares the products
byte for byte, so an operation that reads a clock, a random seed, a locale, or a
system font produces two different results at build time and does not become an
operation. The static table is built through the same path, so such a capability
fails the process at startup rather than producing two different invoices in
production.

Execution runs on the blocking pool with a cooperative `StopSignal` mirrored
from the deployment's shutdown token, and the dispatcher waits for the thread
rather than abandoning it: a blocking task cannot be cancelled, so "drainable"
has to mean the work observes the signal and ends, not that the runtime drops
it. A drained execution fails `timeout`/`local_capability_drained`, which is
retryable, so the activity outlives the replica. Bytes never come back inline:
a producing operation returns a `LocalArtifact` that names the file column and
the role whose write will claim it, and the dispatcher stores it through
`crates/storage` and puts the stored identity in the activity result (ADR 033).

## Alternatives

| Option | Why Not |
|--------|---------|
| Model a capability as a `Connector` with `origin: None`, `credential: None` | Every reader of a connector would have to handle an origin that never resolves, and `Operation` would need a method and path for work that renders no request. A declaration full of fields the runtime must ignore is the defect ADR 034 is about. |
| Give a local capability a placeholder origin (`https://local.invalid`) | The same defect, spelled as a lie: it would resolve, be logged, and enter a configuration fingerprint, describing a request that is never made. |
| Document the determinism requirement and check it in review | Determinism is the entire reason `Pure` is safe to retry. A reviewer cannot see a system font lookup three crates down; a double render can. |
| Prove determinism in each capability's own test instead of at registration | A test proves the capabilities somebody wrote a test for. Registration proves every operation in the table, including the one added next month. |
| Compare only the typed output, not the artifact bytes | The bytes are the product for a PDF, a spreadsheet, and an image. Two byte-different renders of one input are exactly what the class forbids. |
| Run on the async runtime and rely on the deadline | A capability is CPU work that does not yield; on a worker thread it stalls every request, subscription, and timer sharing that thread until it finishes. |
| Abort the blocking task at shutdown | A blocking task cannot be aborted; dropping the handle leaves the thread running against a runtime that is going away. Waiting for a bounded, cooperative stop is the only drain that is real. |
| A new failure class for a drained execution | A Process routes `retry_on` over eight closed classes. A ninth is one no deployed Process can route; `timeout` already means "nothing happened, running it again is safe". |
| Classify every bound breach as `validation` | The deadline is the one bound a retry can pass — a loaded replica, a shorter input next time. Spec 018 §4 splits them for that reason. |
| Let a deployment declare several instances of one capability | There is nothing to configure, so two instances would be two names for one thing, and a Process referencing one would be choosing a name rather than a capability. |
| Return produced bytes inline in the activity result | Journals, retries, and process state would carry megabytes, and the file would bypass the signed-URL and permission path every other attachment goes through (ADR 033). |

## Consequences

Specs 019–022 add a capability as one module plus one table line, and get the
five bounds, the determinism proof, the drain, and the artifact handoff without
writing any of them. The cost is a second declaration type to learn beside
`Connector` — deliberately shaped the same way (declare, build once, hold in a
`static`, `admit_operation` with the same two refusals) so the second one is
mostly recognition rather than learning.

Metadata does not change: a local capability is a `ConnectorInstance` and a
`ProcessRequestActivity`, and `donat_metadata::local` refuses an origin, a base
URL, a header, a credential, an unknown capability, an unadvertised operation,
and a `local.*` webhook wait. The metadata crate does not depend on the
connector crate; the compiled table reaches it through a catalog trait, because
what a binary was built with is a fact about the binary.

A produced artifact is a pending upload with a 24-hour claim window, owned by
the role its process will write as. That role comes from the operation input,
which means a capability that cannot name it cannot produce a file — and a
process that names the wrong one finds out when its write fails to claim,
rather than by storing a file nobody can read.
