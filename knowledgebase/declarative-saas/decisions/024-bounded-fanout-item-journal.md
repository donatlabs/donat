---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# Bounded fan-out uses a source-local item journal

## Context

Petshop shipment and vendor-payout Processes need to invoke one declared
Command or connector request for every item in a finite list. A worker may
crash after some items complete, request activities may finish out of order,
and multiple workers may poll the same Process. The declaration therefore
needs real durability and a concurrency limit; replaying an in-memory loop or
starting an unrestricted set of activity jobs is insufficient.

The item key is a typed scalar. Its human-readable string is needed in failure
output, while its type-sensitive identity must distinguish values such as the
string `"1"` and the number `1`. Command idempotency bindings evaluated inside
the fan-out must also differ per item.

## Decision

Entering a non-empty `for_each` writes one bounded
`donat.process_fanout_items` set and advances the instance version without
leaving the state. Expansion rejects an input above `max_items`, a non-object
item, an absent or non-scalar key, and a duplicate canonical typed key by
failing the Process before any item work is scheduled.

The journal preserves the original ordinal, display key, canonical JSON scalar
identity, input object, terminal result or safe failure, and optional existing
activity-job link. Request fan-out persists every closed activity descriptor
in the expansion transaction, but only the first `max_concurrency` items are
due; the remaining jobs use an infinite due time until a terminal item
atomically activates the next ordinal. Command fan-out emits at most
`max_concurrency` durable `fanout_item` events and replenishes that window as
items finish. Commands still execute through the existing savepoint boundary,
and requests still use the existing lease, capacity, retry, provider-key, and
takeover machinery.

Every fan-out logical activity ID and every Process `activity_key` evaluated
inside its body includes the canonical item identity. Item completion leaves
the instance version unchanged until the last item commits. The last
completion collects successes and failures in input order, validates the
compiled aggregate state contract, advances once, and appends one continuation
event. `ordered_results` contains successful raw activity results;
`successful_items` additionally merges the original item only when
`preserve_input` is true; `failed_items` contains the original item plus the
closed safe diagnostic fields.

If request failures declare different `on_error` destinations, the first
failed input ordinal selects the post-collection route. All items still reach
a terminal journal state before routing, so every route receives the same
complete collection contract. A conflicting result field under
`preserve_input` becomes a safe invariant item failure rather than silently
overwriting its input value.

## Alternatives

| Option | Why Not |
| --- | --- |
| Start every request job immediately | Ignores the declaration's `max_concurrency` contract and can overload a provider before connector-wide capacity applies. |
| Reconstruct progress only from activity jobs | Cannot represent command items, unscheduled ordinals, stable input ordering, or closed per-item failures. |
| Execute every Command item in one transaction | Creates a long transaction, loses per-item crash progress, and makes the concurrency declaration meaningless. |
| Model each item as a child Process | Introduces dynamic subflows and a second lifecycle contract that the bounded grammar deliberately excludes. |
| Use the display key as the unique identity | Collides across distinct scalar types and can rotate item-specific idempotency keys after serialization changes. |

## Consequences

Fan-out remains one finite state rather than a general workflow language.
Restarts and competing workers can repeat preparation, but item work, domain
Commands, activity activation, collection, and state advancement commit once
through existing source-local fences. The journal adds at most 256 rows and
request jobs per state execution, all bounded by compiled metadata.

Request descriptors for inactive ordinals consume bounded storage before they
become due. This is intentional: a restart never reevaluates a connector input
against mutable state, and activation needs only a short source-local update.
