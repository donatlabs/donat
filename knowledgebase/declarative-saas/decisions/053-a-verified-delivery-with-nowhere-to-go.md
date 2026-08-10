---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A verified delivery with nowhere to go

## Context

Spec 013 adds six connectors that each publish an inbound webhook route, and it
holds one boundary open on purpose: a successfully verified event still answers
`503`, because the Process-owned inbound transaction
([[025-verified-inbound-delivery-and-wait-correlation]]) has landed for exactly
one module — Stripe — and for none of these.

The ingress route did not have a shape for that. `WebhookInstance` held a
`&TriggerSpec` and a `&StripeConnector` by name, and `connector_webhook.rs`
read the raw-body ceiling out of the catalog snapshot, verified through the
Stripe module, and then committed. Every one of those three steps assumed the
thing it was answering for was correlatable. A second kind of route — one that
verifies, rejects, and stops — could not be expressed without either giving the
new connectors a half-built correlation or letting them borrow the correlated
path and fail somewhere inside it.

There was also a smaller shape problem underneath. A Batch B connector declares
one trigger *per provider event* — GitHub declares `issues`, `pull_request`,
`push`, and `release` — and every one of them arrives on the same URL with the
same signature over the same secret, with the event named by a header. The route
is per instance, so it has to answer for the whole set.

## Decision

**The route's answer is a property of the delivery, not of the module.**
`WebhookInstance` carries a closed two-variant `WebhookDelivery`: `Correlated`,
which holds the catalog trigger snapshot and the module that produces a
`VerifiedInboundEvent`, and `Verified`, which holds only a compiled
`ProviderWebhook`. Verification answers with a `VerifiedDelivery` — an event, or
`Unacknowledged`. The route matrix is then literally the match: `404` for an
instance the registry does not carry, `413` from the ceiling the trigger
declares, `400` for any rejection, `204` for a correlated event that committed,
and `503` for one that verified with nowhere to go.

**An unacknowledged delivery writes nothing at all — not even an audit row.**
The correlated path records an invalid-signature audit through
`persist_invalid_from_engine`, and it is tempting to give the new connectors the
same thing, since a rejected delivery is exactly what an operator wants to see.
It is refused here because that audit is *part of* the inbound transaction: it
shares the delivery table, the dedupe identity, and the outcome vocabulary with
the accepted path. Writing half of it for a connector whose other half does not
exist would mean a `process_inbound_deliveries` row that no correlation will
ever complete, and a first reading of that table that is wrong in a way nothing
would notice. This batch delivers verification and rejection; the audit arrives
with the transaction it belongs to, and until then
`<name>_verified_event_is_not_persisted` asserts three empty tables rather than
one.

**One instance is one route, so one route has one scheme.**
`ProviderWebhook::compile` takes the connector's whole declared trigger set and
refuses at startup if any two members disagree about the verification or the
raw-body ceiling. A connector with no trigger compiles no route at all, so its
instance name is indistinguishable from an absent one at the ingress boundary,
and `config.webhook_secret` on such a module stays the refusal it already was
([[034-a-declaration-the-runtime-ignores-is-a-defect]]).

## Alternatives

| Option | Why Not |
|--------|---------|
| Answer `204` for a verified Batch B delivery | It is a lie in the direction that costs the most. The providers here all retry on a non-2xx and give up on a 2xx: GitHub, Shopify, Calendly, and Typeform each document a retry ladder. Acknowledging an event nothing recorded converts every delivery during this gap into a permanently lost one |
| Answer `404` for a Batch B instance until correlation lands | Typeform documents that a `404` or `410` from the endpoint **disables the subscription immediately, with no retry**. A status chosen to mean "not ready" would silently uninstall the integration |
| Give the new connectors a `TriggerSpec` and publish them to Process compilation | The snapshot's `event_id`, `event_type`, and `output` contracts are the correlation contract. Publishing them before anything consumes them is a version-hashed material that nothing validates, and [[049-a-connector-publishes-the-declaration-it-was-admitted-on]] is exactly the shape that goes wrong when two descriptions of one thing exist and only one is exercised |
| Write the invalid-signature audit row anyway | It is one half of a two-table transaction whose other half is unwritten. The table's first reading would be "every delivery of this connector was invalid", which is true and useless, and would have to be migrated when the accepted path lands |
| Keep `WebhookInstance` Stripe-shaped and add a parallel route for Batch B | Two ingress routes with two `413` policies and two `404` behaviours. The route matrix is the thing spec 013 §4 asks to be exact, and it is exact because there is one of it |
| Let each declared trigger carry its own verifier and pick one per request | Picking would mean reading the delivery — a header, or worse the body — to decide how to authenticate it. The scheme must be fixed before anything about the request is trusted |

## Consequences

The ingress boundary now says out loud which of its two answers a connector
gets, and the answer is decided at startup rather than at request time. Adding
the seventh webhook-bearing connector is a module file, a table line, and a
deploy-time rule; moving one from `Verified` to `Correlated` is the arrival of
its inbound transaction and nothing else.

Two costs are worth naming. A Batch B deployment is observable only through its
HTTP status: an operator debugging a signature mismatch sees a `400` and has no
row to read, which is a real regression against what Stripe offers and is the
direct price of not writing half a transaction. And `503` is a status a load
balancer may retry or a monitor may page on — it is the honest answer, but a
deployment that turns these connectors on before their correlation lands is
choosing to receive deliveries it will have to receive again.

One SDK note belongs here rather than in a spec. `Pagination`'s items pointer
now admits RFC 6901's empty pointer, because GitHub answers every list endpoint
with a bare JSON array and the collection *is* the document. Every other pointer
in the SDK is still required to be static and absolute; this is the one place
where "the whole document" is a real answer rather than a missing one.
