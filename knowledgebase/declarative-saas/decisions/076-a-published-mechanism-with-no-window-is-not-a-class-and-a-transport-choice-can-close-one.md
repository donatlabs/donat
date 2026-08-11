---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A published mechanism with no window is not a class, and a provider's transport choice can close one

## Context

Spec 025 (Batch I) classified 39 operations across seven connectors, and two of
them are populations the effect gate had not met in this shape before.

**Discord publishes a deduplication mechanism for `message.send` and publishes no
retention for it.** Its own reference gives the binding — `nonce`: "Can be used
to verify a message was sent (up to 25 characters)" — and the behaviour and its
uniqueness scope — `enforce_nonce`: "If true and nonce is present, it will be
checked for uniqueness in the past few minutes. If another message was created by
the same author with the same nonce, that message will be returned and no new
message will be created." Spec 023 §3 requires a binding, a uniqueness scope,
**and** a retention, all cited. Two of the three are quotations. The third is
"the past few minutes".

**Dropbox serves every endpoint over `POST`.** Its RPC style is a published
transport choice — "RPC endpoints … accept arguments as JSON in the request
body, and return responses as JSON in the response body" — and it applies to
reads and writes alike. Spec 010 §7 admits `ProviderIdempotent::NaturalMethod`
for `PUT` and `DELETE` only, because HTTP defines repeat-safety for those two.
Every Dropbox write is therefore outside that class before any evidence is read.
And each of the three writes this batch declares is *idempotent in effect* by
Dropbox's own published error unions: a second `delete_v2` answers
`path_lookup/not_found`, a second `create_folder_v2` with `autorename` false
answers `path/conflict`, a second `create_shared_link_with_settings` answers
`shared_link_already_exists`.

## Decision

**A published mechanism whose window the provider declines to state is
`InventoryOnly`, and it is not an at-most-once candidate either.**

`ProviderIdempotent::ExplicitKey` is refused by
[[073-a-retention-is-read-from-the-reference-that-owns-the-operation]]'s rule:
"silence there is a refusal rather than a licence to use the number found
elsewhere". "A few minutes" is not a window a send horizon can be derived from,
and the class promises that a resend inside the horizon is deduplicated — a
promise nobody could hold Discord to.

`EffectClass::AtMostOnce` is refused for the sharper reason, and this is the part
worth writing down. [[063-an-at-most-once-send-is-admitted-only-where-a-process-says-what-an-unknown-outcome-means]]
admits that class on **evidence of an absence**: a recorded search that found no
mechanism, and a recorded consequence of a second send. There is no absence here.
ADR 073 already refused this exact move once — for `paypal.refund.create`,
"reaching for the weaker class to route around a missing number is exactly the
promotion-by-proximity that ADR 042 exists to prevent" — and this ADR states it
as the general rule rather than as a PayPal fact. The two refusals are not the
same refusal wearing different clothes: one says *the window is missing*, the
other says *the absence is missing*, and an operation can fail both at once.

Discord adds a second, independent bar that is worth recording because it
survives even if Discord publishes a window tomorrow: `nonce` is "up to 25
characters", and a durable activity's stable key is longer. A connector that
bound the key Discord publishes a slot for would have to truncate it, and a
truncated key is not unique.

**A provider's transport choice can close a class for every one of its writes,
and an operation that is idempotent in effect wants a class that keeps the
retry.** Dropbox's three writes are `InventoryOnly`, and their recorded reason is
one string because it is one finding: `NaturalMethod` is out of reach by method,
and `AtMostOnce` is the wrong trade. ADR 063 named the population that is still
waiting — "writes a provider documents as repeat-safe (they want a class that
*keeps* the retry, not one that trades it away)" — and Batch I adds five to it:
Dropbox's three, `box.folder.create`, and `box.file.share_link_create`.

The contrast inside one provider is what makes the rule legible.
`box.file.delete` **is** executable, because Box publishes the sentence the class
needs about that exact operation: "404 — Returned if the file is not found **or
has already been deleted**". `box.folder.delete` is the same method and shape
and is refused, because Box's `404` there says only "could not be found" — and
its `503` says "The operation will continue after this response has been
returned", so a repeat may name a folder Box is still deleting. A provider that
documents repeat-safety where it means it, and does not document it here, has
not said this is repeat-safe. That is the `salesforce.record.delete` finding,
one batch on and now with a same-provider control.

**What the batch admitted, for the record.** Two `NaturalMethod`:
`box.file.delete` and `zoom.meeting.delete`, each on the provider's own
statement about a second send, plus `mailchimp.member.upsert` — a `PUT` against
"The MD5 hash of the lowercase version of the list member's email address",
titled "Add or update list member", whose required `status_if_new` is documented
as "required only if the email address is not already present on the list". A
provider that publishes a *different field for the first send* has published what
the second one does; it is the strongest repeat evidence in the programme, and it
is the contrast with `salesforce.record.upsert` and `zoho_crm.record.upsert`,
which are the same semantics over `PATCH` and `POST` and stay unreachable. Two
`AtMostOnce`: `mattermost.post.create` and `zoom.meeting.create`, each with a
machine-checkable absence and a named consequence.

## Alternatives

| Option | Why Not |
|--------|---------|
| Bind Discord's `nonce` to the activity key and declare `ExplicitKey` with a window read as "five minutes" | It invents the number the class is built on. If it is wrong, the failure is a duplicate message a Process believed was deduplicated — and the slot is 25 characters, so the key would have to be truncated into something no longer unique |
| Declare `message.send` `AtMostOnce` anyway, since the outcome is the same unreachability today | It would record an absence that is not there, and `NoIdempotencyEvidence::searched` is read by a reviewer as a claim about the documentation. A wrong evidence string is worse than an unreachable operation, because the next reader trusts it |
| Declare Dropbox's writes `AtMostOnce`, since their repeats are technically different outcomes | ADR 063's class trades the retry away. These writes cannot duplicate anything, so the trade buys nothing and costs an ordinary `connection refused` its retry. `INVENTORY.md` already has a group for exactly this shape |
| Widen `NaturalMethod` to admit a `POST` a provider documents as repeat-safe | The widening ADR 042 exists to refuse. HTTP defines repeat-safety for `PUT` and `DELETE`; a class keyed on a provider sentence over an arbitrary method is a judgement per provider rather than a property of the method — and this batch would have applied it to five operations across two providers, which is how a carve-out becomes the rule |
| Admit `box.folder.delete` alongside `box.file.delete`, since they are the same method against the same kind of identity | The evidence is per operation, and Box's own table distinguishes them. Reading one operation's sentence onto another's is the proximity argument this ADR is about |
| Skip the `nonce` near-miss in `INVENTORY.md`, since the operation is unreachable either way | A reviewer searching Discord's reference finds it in a minute. An inventory entry that does not mention it reads as a search that missed something |

## Consequences

The effect gate now has a stated answer for the case where a provider is
*generous* — it published a mechanism — and still cannot be relied on, and the
answer is the conservative one in both directions: no `ExplicitKey` without a
window, no `AtMostOnce` without an absence. Discord's send is declared, typed,
tested, and unreachable, and the module says in one paragraph exactly which
sentence Discord would have to publish to change that.

`INVENTORY.md`'s inventory-only population grows by six across this batch — five
in the wants-a-class-that-keeps-the-retry group, one in a group of its own — and
the programme's running count of "operations a provider documents as repeat-safe
over a method the gate does not admit" grows from twelve to seventeen. That
number is the case for the class that does not exist yet, and it is now large
enough to be a design input rather than an anecdote.

The cost is that Dropbox's connector is read-only from a Process's point of
view: a folder create, a delete, and a share link are all declared and none is
reachable. For a storage connector that is a real limitation, and the honest
alternative — giving a write that cannot duplicate anything a class that forbids
its retry — would have made the connector worse rather than better.
