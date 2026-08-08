# Asking for the things only they can give you

A blocked-on-you list made of nouns is not a list, it is homework with no
instructions. "I need the plan identifiers from the payment provider, your
identity provider account, and a payout account" is fine for an engineer and
useless to everyone else: they do not know which of forty screens has it, what
it looks like when they find it, or whether the thing they copied is the right
one.

Nothing here is specific to one provider. The shape of the request is the rule;
the screens and the string formats belong to whichever service they actually
use, and both change.

> Write these in your partner's language, whatever it is. The examples below
> are in English because everything in this repository is.

## The shape

Every item gets four things:

1. **What it is**, in one plain clause — what it is *for*, not what it is called.
2. **Where to click**, roughly. Name the section, not the pixel.
3. **What it looks like**, so they can tell they got the right string.
4. **How to send it** — and for anything secret, how *not* to.

## Look it up; do not recite it

You do not know this provider's current console, and neither does your memory
of it. Menus get renamed, settings move between sections, key formats change,
and a confident wrong click-path is worse than none — it sends someone hunting
through a menu that was renamed last quarter, and they conclude the problem is
them.

So before writing step 2 or 3 for any provider: **check the provider's own
current documentation**. Search for their page on the specific thing — "where
to find the price ID", "application settings", "API credentials" — and write
the path from what you find, not from what you remember.

Two things worth pulling from the docs while you are there:

- **The prefix or format** of the identifier, so step 3 is checkable.
- **The neighbouring value people grab by mistake.** Most consoles put an
  identifier next to a similar-looking one — a product beside a price, an
  application id beside a tenant id — and naming the wrong one up front saves a
  round trip every time.

If the docs are ambiguous, say so rather than guessing: "it is either in the
application settings or under API — check both; the value looks like this".

## Never accept a secret in a message

This is the rule that does not depend on the provider, and it matters most,
because the natural way to ask produces the unsafe answer. Ask a non-technical
person for "the keys" and a live secret arrives in the chat, where it stays —
in history, in backups, in whatever the transcript syncs to. Rotating it
afterwards is their problem and your fault.

So split every request in two. The test is not the prefix, it is what the value
can do:

| Safe to paste | Never pasted |
|---|---|
| Identifiers that name a thing — a price, a plan, an application, a tenant | Anything that can spend money, read customer data, or sign as you |
| Values the provider itself puts in front-end code (usually called *publishable* or *public*) | Anything the provider calls *secret*, *private* or *signing* |
| Domains and account names | Passwords, and anything shown once and then hidden |

The provider's own console is the reference: consoles hide secrets behind a
"reveal" button and warn beside them. If a value is treated that way there,
treat it that way here.

Say it once, in a sentence they can act on:

> Anything marked *secret* or *private* on their side — don't send it to me at
> all, not in chat and not by email. You'll paste those in yourself and I'll
> show you where. Everything else you can just message over.

Then make "you'll paste it in yourself" real: a `.env` file they edit, a
one-line command, or a secret field in their hosting panel. Never "send it to
me and I'll put it in".

## The shape, worked

An illustration of the four parts — not a reference for any particular service.
Look up the real path before you send it.

> **What I need:** the identifiers for your plans at the payment provider.
> They're what connects the "Pay" button to a specific price.
>
> **Where to find them:** in their dashboard, under products — open a plan, and
> the price inside it has its own identifier.
>
> **What it looks like:** a long string with a distinctive prefix (check their
> docs — the same page says how it differs from the identifier of the product
> itself; that one sits right beside it and is what people usually grab).
>
> **What to send:** those three strings, and which plan each one is. Just
> message them over, they aren't secret. The keys marked *secret* on their
> side — those, don't send.
>
> If the plans aren't set up there yet, say so and I'll write out which ones to
> create; it's about fifteen minutes.

Some items are not requests at all — they are things they do, and you only need
to know when:

> **The payout account** is set up inside their dashboard; there's nothing to
> send me — I have no access to your banking details and shouldn't. Just tell
> me when it's filled in.

Saying that plainly is what keeps the list honest: a list where two of four
items are "and tell me when" reads as a shorter list, which it is.

## Ordering

Put it **after** the progress report, never before: the first thing they read
should be what now works, not what you want from them. And say what does not
depend on it:

> None of this blocks anything today — the stand runs on test keys and I'm
> carrying on with the rest.

An item that blocks nothing right now is worth a line saying so. An item that
blocks the next step is worth saying that too, because it decides whether they
do it today or on Friday.

## When they send the wrong thing

They will, and it is usually the neighbouring field. Do not explain the
taxonomy; say what to look for instead.

**Bad.**

> That's the product identifier, not the price identifier. A price belongs to a
> product, and one product can have several prices — monthly and annual, say.

**Good.**

> That's the identifier for the product itself — I need the price inside it.
> Open the same plan, and under the amount there's a string just like that one,
> starting differently.

## When a secret arrives anyway

Do not scold. Say what to do, in this order, and check the provider's current
docs for where revocation lives before naming a path:

> Worth revoking that key straight away — it's been through a chat, and chat
> history gets kept. It'll be in their keys section, with a "revoke" or "roll"
> next to it. Don't send me the new one; I'll show you where you paste it in.
