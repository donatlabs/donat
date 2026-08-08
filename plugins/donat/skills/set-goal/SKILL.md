---
name: set-goal
description: Use when the user runs /donat:set-goal, or at the start of a piece of donat work, to fix what "done" means in one sentence. Once fixed, drive toward it and stop asking at every step.
argument-hint: [what you are trying to end up with]
allowed-tools: [Read, Write, Edit, Glob, Grep, Bash]
---

# Fix the goal, then drive to it

Work without a stated goal turns into a sequence of small permissions. Every
seam becomes a question, each one reasonable on its own, and together they hand
the partner a project-management job they did not ask for and cannot do — they
do not know which order is cheaper.

So: agree the goal once, write it down, and then **proceed**.

## Fixing it

Three lines. Their words, not yours.

```markdown
# Goal

**What we are building:** a subscription billing system where customers
subscribe to a plan, are charged monthly, and lose access when payment fails.

**Done means:** a real customer can pick a plan, pay with a card, and see their
subscription and payments in the platform; an operator can see all of it there
too. Failed payment closes access; a repeated charge from the provider does not
charge twice.

**Not in this:** refunds, plan trials, invoicing by email, anyone changing
prices from the app.
```

Rules for each line:

- **What we are building** is one sentence a non-technical partner would say
  out loud. If it contains a technical noun they would not use, rewrite it.
- **Done means** is observable, in their terms, and specific enough to argue
  with. "Billing works" is not a goal; "a customer can pay and an operator can
  see it" is.
- **Not in this** is the line that protects the goal. Anything they mention in
  passing and you are not building goes here, so it is a decision rather than
  an omission.

Write it to `docs/goal.md`, beside the domain brief. Read it back, get one
confirmation, and then stop asking about direction.

## After it is fixed

**Do not ask which of the remaining steps to do next.** You know the order
better than they do — that is why they are talking to you. Say what you are
doing and do it:

> Next I'm wiring the payments, then the operator's side of the platform — it
> is thin until there are real subscriptions in it.

Not:

> Shall I do payments first, or the operator screens? I'd start with payments.

The second sentence of that pair proves the question was unnecessary. If you
have a recommendation, you have an answer; state it and go.

### Stop only for these

| Stop | Why |
|---|---|
| Something irreversible or expensive to undo | deleting data, going live with real charges, mailing real customers |
| Something that changes the goal | they asked for refunds, which the goal excluded |
| An outside constraint that changes what is possible | "these amounts are in roubles" — Stripe cannot process them |
| An artifact only they can give you | Stripe price ids, the Auth0 tenant, a real bank account |

The last one is not a question, it is a **list** — and a list of nouns is
homework with no instructions. Each item needs what it is, where to click,
what it looks like when they find it, and how to send it. Anything secret is
never sent at all: they paste it themselves, into a place you point at.

> Three things before we can take real money. None of them blocks anything
> today — the stand runs on test keys and I'm carrying on with the rest.
>
> **The plan identifiers at your payment provider.** In their dashboard, under
> products: open a plan, and the price inside it has its own identifier. I need
> three, and which plan each one is. Message them over — they aren't secret.
> Anything marked *secret* on their side, don't send at all.
>
> **Your identity provider account.** The application settings there show a
> domain and a client id — both fine to send. The client secret is not: you'll
> paste that in yourself and I'll show you where.
>
> **The payout account** is set up inside their dashboard; nothing to send me.
> Just tell me when it's filled in.

Worked versions of each, plus what to do when a secret arrives in the chat
anyway, are in `using-analytics-mode`'s `references/asking-for-things.md`.

### Never stop for these

- Which order to do the remaining work in.
- Anything already covered by a standing default — the platform, the
  identity provider, read-only first, deadlines on waits, protection against
  double charges. See `using-analytics-mode`.
- "Shall I continue?"
- Confirming something they already confirmed.

## Reporting against it

Every report is measured against the goal, not against the work:

1. **Where we are** relative to `Done means` — what now works, as scenarios.
2. **What is next**, stated, not asked.
3. **Blocked on you**, as a list, if anything is.
4. **One thing that differs from the goal**, if it does — see below.

That last slot matters. When the built thing diverges from what was agreed,
say so plainly, once, with the reason and the option to change it back:

> One difference from the brief. Instead of "the plan change takes effect once
> you pay the difference", it came out stricter: the customer cannot change
> their own plan at all. Changes go through payment, so there is nothing to
> warn about — the message belongs later, on the change-plan screen. Safer this
> way, but say if you wanted the warning.

## When the goal is met

Say so and stop. Do not roll into the next thing because it seems useful. A
finished goal is a decision point that belongs to them:

> That is everything in the goal — a customer can subscribe, pay, and lose
> access on failure, and an operator can see all of it. `docs/goal.md` has the
> "not in this" list if you want to pick the next piece from there.

## If there is no goal yet

`/donat:set-goal` with no argument: ask the two questions that produce one, then
write it.

> What do you want to be able to do at the end of this that you can't do now?

> How will you know it's working — what would you click, and what should
> happen?

If they answer with a feature list, convert it into one sentence and read it
back. A goal made of five bullet points is five goals, and none of them will be
finished.
