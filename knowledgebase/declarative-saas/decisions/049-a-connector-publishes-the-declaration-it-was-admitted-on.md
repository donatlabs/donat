---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A connector publishes the declaration it was admitted on

## Context

[[048-a-declaration-a-deployment-completes]] left the nine hand-written
connectors deployable and executable through the registry, and unreachable from
a Process. The reason it recorded is exact: publishing a catalog `OperationSpec`
needs the request shape, and an SDK `Operation` exposed only its id, version,
method, effect class, idempotency binding, and success statuses. Rather than
re-declare a path, a query binding, and a response mapping in
`crates/server/src/connectors/`, that slice published nothing and said so, and
its alternatives table named the fix: *the projection belongs in the SDK, beside
the declaration it is derived from*.

Building it surfaced a second fact. The declaration was not merely
*unexposed* — for several operations it was **incomplete**. It could not say
which slots the connector fills itself (an Airtable base, a Twilio Account SID,
a FIFO `MessageDeduplicationId`), which inputs a module consumes without
rendering them into the request (the bytes of an S3 `PUT`, the source key a copy
composes `x-amz-copy-source` from), which slots a module defaults (a page size,
a prefix), or which outputs a module composes rather than reads from a JSON
pointer (an `ETag` that only ever arrives as a response header, a key list
lifted out of XML). Every one of those already existed in the module — inside
`plan` and `decode` — where nothing could read it.

## Decision

**`Operation::project` is the one derivation, and it lives in the SDK.** It
returns an `OperationProjection`: inert, owned data carrying the method, path
template, query and header bindings, request body shape, success statuses, the
input and output contracts, the effect class, and the explicit-key evidence. It
carries no credential, no resolved origin, no provider text, and no value, and
there is no constructor from it back into an `Operation` or a `RequestPlan` — so
reading one tells a caller what the operation *is* without letting it aim,
compose, or send anything. `crates/server/src/connectors/catalog.rs` translates
a projection plus the deployment's own half — which instance, which resolved
origin, which capacity — into an `OperationSpec`. It restates nothing.

**The declaration gained the four things it could not say**, each as a builder
method with the check that keeps it honest:

- `supplied_input(name)` — a slot the connector fills: from configuration, from
  the durable activity's stable key, or composed from other declared inputs. It
  is removed from the contract a Process binds, which is correct in the strong
  sense: every one of those names is *refused* as input by the module that reads
  it, so publishing it would publish a field whose only possible value is a
  failure.
- `declared_input(name, scalar, required)` — an input the template cannot
  describe: one a module consumes without rendering, one a module defaults, or
  one whose type a query key does not carry.
- `declared_output(name, scalar, required)` — an output-contract field the
  module composes rather than extracts through a pointer.
- `deadline(duration)` — the operation's own declared deadline, defaulting to
  five seconds, which is the shortest `start_to_close` a Process may give it.

**The catalog effect maps by the question it answers.** The catalog's
`OperationEffect` has two variants and asks one thing: does this operation need
a stable key bound into its request and retained by the provider?
`ProviderIdempotent::ExplicitKey` answers yes and publishes the evidence — the
binding, the scope, the retention, the margin. `ReadOnly` and
`ProviderIdempotent::NaturalMethod` both answer no: a naturally idempotent `PUT`
or `DELETE` against a fixed resource identity has no key to bind and no
retention window a send horizon must fit inside. The SDK keeps the two classes
apart because the evidence behind them differs and a reviewer must see which one
an operation stands on; the catalog does not, because the runtime behaviour is
identical.

**The published error map is the closed class table and not the module's
rules.** Every class carries its Donat-owned code; which class a given provider
response reaches stays in the module's own ordered map, because that is the part
a second copy could get wrong, and nothing at runtime reads the published one.

**Every published bound is one the SDK really holds.** The request, response,
and header ceilings are its own constants; the JSON depth is the decoder's
recursion limit; the node ceiling follows from the bounded body, since a node
costs at least one byte; one attempt is one call, because the provider executor
sends one request and follows no continuation.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep deriving the input contract from the template slots alone, with no annotations | It publishes contracts the modules refuse. `aws_s3.object.copy` would demand `copy_source`, which is a reserved input name; `airtable.record.list` would demand the configured base. The declaration was incomplete, and hiding that behind a derivation makes the projection wrong rather than the module honest |
| Widen the catalog's `OperationEffect` with a `NaturalMethod` variant | The variant set is part of `OperationEffectMaterialV1`, a versioned canonical material whose hashes are a contract. A third variant is a format change, and it would buy nothing at runtime: both classes need no key and no retention window |
| Publish `NaturalMethod` operations as unpublished, like inventory-only ones | It would delete most of the batch's value — an S3 `PUT`, an Airtable record replace, a SendGrid upsert — and would make [[046-an-effect-class-can-depend-on-deploy-time-configuration]] unobservable at publication, since the versioned-bucket difference *is* a `NaturalMethod` delete |
| Publish the module's error rules into the catalog map | A second copy of a mapping the module already owns, in a field nothing reads. The class table is the part that is complete and cannot disagree |
| Put the projection in `crates/server` next to the catalog types | Exactly the second description [[048-a-declaration-a-deployment-completes]] refused, one crate further away from the declaration it describes |
| Give the projection accessors for everything on `Operation` | The projection would become a way to rebuild a request from outside the SDK. It exposes what a catalog snapshot is made of and stops there |

## Consequences

A Process may now reference any operation of any of the nine connectors that its
deployment could enable, and the publication follows the gate rather than
widening it: `admit_operation` answers with the compiled operation, and that
same operation is what gets projected, so a deployment cannot publish one
declaration and run another. An inventory-only operation is refused twice — at
deploy time with its `connectors.yaml` path, and at Process compilation with
`processes[i].states[j].request.operation` — and a class this deployment's own
target denies never reaches either.

Two costs are worth naming. A query key and a body leaf carry no type of their
own, so an input the module has not typed with `declared_input` admits any
scalar in the published contract and fails at render rather than at
compilation; tightening one is a one-line change in the module that owns it. And
a module author who forgets `supplied_input` on a new operation publishes a
field the module would refuse — which is why `crates/connectors/tests/projection.rs`
asserts, for every executable operation of every module, that no projected input
names a value the connector supplies.
