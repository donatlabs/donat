---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A retention with an escape clause is not a minimum retention

## Context

[[042-the-effect-gate-admits-evidence-not-methods]] admits
`ProviderIdempotent::ExplicitKey` on three published things: a binding, a
uniqueness scope, and a retention. `ExplicitKeyEvidence::documented` takes them
together or not at all, and it takes the retention as a **minimum** with a clock
safety margin strictly under it, because the whole point of the class is that a
durable activity may hold a key across a retry window and still be replayed
rather than duplicated. Two providers have reached it: AWS SQS FIFO (five
minutes, in the body) and Zendesk (two hours, in a header). Two have missed it on
a *missing* window — Microsoft's `transactionId` and Mercado Pago's
`X-Idempotency-Key` — and both are recorded in `providers/INVENTORY.md` as
near-misses rather than dropped.

monday.com is a third shape, and it is the one that needed a decision. Its
idempotency page publishes everything ADR 042 asks for:

* the binding — "Send a unique `Idempotency-Key` header with any mutation
  request";
* the retention — "Cache duration — Cached responses expire after 30 minutes.
  After expiration, the same key will execute fresh";
* the scope — "Per-user budget — Each user+app combination has a memory budget
  for cached responses";
* and more behaviour than either admitted provider does: an
  `Idempotency-Replayed: true` response header, and a documented `409` with
  `IDEMPOTENCY_CONFLICT` and a `Retry-After` for a duplicate arriving while the
  first is still in flight — the concurrent case Zendesk is silent about.

In the same rules table, one row down from the retention, it also publishes:
"**If the budget is exceeded, new responses will execute but won't be cached for
replay.**" And one row below that: "Max response size — Responses larger than
1 MB are not cached."

## Decision

**A retention the provider publishes an unobservable escape clause for is not a
minimum retention, and the operations it covers are `AtMostOnce`.**

monday's four mutations — `item.create`, `update.create`, `item.update`,
`item.delete` — carry no idempotency binding, and the two whose repeat leaves a
second thing behind are admitted under [[063-an-at-most-once-send-is-admitted-only-where-a-process-says-what-an-unknown-outcome-means]]
with the whole mechanism quoted in the evidence string, so a reviewer reading the
declaration sees the key monday publishes and the sentence that disqualifies it
in one place.

The reasoning is about what each class *promises the runtime*, not about how much
the provider wrote down.

`ExplicitKey` tells the activity worker: send again, the provider will absorb it.
The engine acts on that — it retries, and it holds the same key across attempts
inside the published window. If monday's per-user budget was full when the first
attempt landed, the first response was never cached, the second attempt executes,
and the Process has two items and no way to know. Neither the connector nor the
runtime can observe the budget: it is not published as a number, not exposed on a
response header, and not derivable from anything an attempt sees. So the class
would be making a promise on the provider's behalf that the provider explicitly
declines to make.

`AtMostOnce` promises less and can keep it. It never sends twice, it costs the
retry, and it forces the Process to declare what an unknown outcome means. For a
board item that is the right trade: a duplicated item is silent and permanent,
and an ambiguous send is visible and routed.

**The 1 MB clause is bounded here and is recorded rather than relied on.** Every
mutation response this connector declares is a handful of scalar fields, far
under the SDK's own `MAX_HTTP_BODY_BYTES` ceiling of 1 MB, so that row could not
fire for these operations. It is written into `INVENTORY.md` anyway, because the
next connector to declare a monday mutation with a large selection set inherits
the question rather than the answer.

**What would change the class.** Two things, either of them: monday publishing a
floor for the budget (a number of cached responses, or a statement that a key is
retained for 30 minutes unconditionally), or this repository landing the class
ADR 063's "what this supersedes" section still names as open — a
`ProviderIdempotent` variant admitted on a *best-effort* provider mechanism,
which keeps the retry and accepts that the provider may not deduplicate. The
second is the honest home for monday's four, and it is out of scope here.

## Alternatives

| Option | Why Not |
|--------|---------|
| Classify them `ExplicitKey` on the 30 minutes and record the budget as a caveat, the way Zendesk's unpublished scope was recorded | Zendesk's gap is a fact it *did not state*; monday's is a behaviour it *did* state, and the two are not the same kind of silence. Recording "the provider says this may not work" beside a class whose whole content is "this works" is a note nothing enforces |
| Bind the header anyway and leave the class `InventoryOnly`, so the key travels but nothing relies on it | A binding is what `ExplicitKey` is admitted on; an operation of another class that wrote one would be a declaration the gate ignores, which is [[034-a-declaration-the-runtime-ignores-is-a-defect]] pointed the other way. And an inventory-only operation is unreachable, so the header would never be sent at all |
| Read `Idempotency-Replayed: true` and treat its absence as a duplicate | It answers the *replay*, not the first send. A worker that never got a response — the case this class exists for — has no header to read, and one that did already knows its own outcome |
| Set the declared minimum retention to something small, on the argument that the budget is unlikely to be exhausted in a few seconds | The margin would be a guess about a value monday does not publish, and `ExplicitKeyEvidence` exists precisely so a class cannot rest on a number nobody can cite. A guess that is usually right is the worst kind of idempotency guarantee |
| Make the class depend on deploy-time configuration, as [[046-an-effect-class-can-depend-on-deploy-time-configuration]] does for a FIFO queue | A queue's type is a property of the target that a deployment configures and startup can check. monday's budget is a property of a running account that no deployment declares and no check can read |

## Consequences

monday's writes are reachable from a Process, and reachable only through an
activity that says what an unknown outcome means. That is a worse contract than
`ExplicitKey` and a better one than the alternative, which was to promise a
deduplication that the provider's own documentation says may not happen.

The programme now has three recorded shapes of idempotency near-miss, and they
are usefully different: a mechanism on **another endpoint** (Linear's
`OAuthApplicationCreateInput.idempotencyKey`, Salesforce's UI API key, Todoist's
Sync `uuid`), a mechanism with **no published window** (Microsoft's
`transactionId`, Mercado Pago's `X-Idempotency-Key`), and — new here — a
mechanism with a window and a **published escape clause**. A reviewer meeting a
fourth provider now has three precedents to sort it against rather than one rule
to reinterpret.

The cost is that a genuinely good provider mechanism buys nothing here, and
monday's is the best-documented one in the workspace: it publishes the replay
header and the concurrent-conflict status that AWS and Zendesk do not. That is
the argument for landing the best-effort class rather than an argument against
this decision, and it is recorded in `providers/INVENTORY.md` as such.
