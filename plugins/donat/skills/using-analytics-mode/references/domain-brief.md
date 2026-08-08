# The domain brief

The one artifact your business partner reads. Written in their vocabulary, with
no technical terms at all, and confirmed line by line before anything is built.

Keep it short enough to read in one sitting. If it runs past two pages, the
scope is too big for one pass — split it and build the first part.

---

## Template

### What we are building

Two or three sentences, in their words, from the interview. If they would not
say it out loud to a colleague, rewrite it.

### The records we keep

| Record | What it is | Belongs to |
|---|---|---|
| Customer | someone who buys from us | — |
| Basket | what a customer is about to buy | a customer |
| Order | a basket that has been paid for | a customer |

One line each, no columns, no types. The goal is agreement on nouns.

### Who can see and do what

One block per kind of person. This is the part that matters most — read it out
loud if you can.

**Shopper**
- Sees: their own profile, addresses, basket and orders. The public catalogue.
- Changes: their own basket, while it is still open. Their own contact details.
- Cannot see: anyone else's anything. Cost prices. Internal notes.

**Support agent**
- Sees: every customer's contact details and every order.
- Changes: nothing. Reads only.
- Cannot see: card details. Payout records.

**Warehouse**
- Sees: orders that are paid and not yet shipped.
- Changes: marks an order packed and shipped.
- Cannot see: prices, customer contact details.

Note what each role **cannot** do explicitly. An absent line reads as an
oversight; a written one reads as a decision.

### What must never happen

| Rule | True for | What the person is told |
|---|---|---|
| A basket line is at most 20 units | shoppers only | "A cart line is limited to 20 units" |
| Stock never goes below zero | everyone, always | — |
| An order cannot be paid twice | everyone, always | — |

The middle column is the layer decision, in language they can check. The right
column is theirs to write — it is what their customer reads.

### What waits on what

| Flow | Waits for | How long | If it never comes |
|---|---|---|---|
| Refund | finance approval | 2 working days | escalates to the finance lead |
| Grooming slot | customer confirming | until the hold expires | the slot is released |
| Payment | the provider answering | 3 seconds, retried | we check with them, and hold the stock until we know |

The last column is the one people forget. Do not leave a blank in it.

### Outside systems

| System | Used for | What we do if it goes quiet mid-way |
|---|---|---|
| Payment provider | authorising and capturing | ask it what happened; hold the money claim until we know |

### Who else talks to this

Mobile app, partner integration, spreadsheet export, AI assistant — and as
which kind of person each one acts.

### Open questions

Anything you could not settle in the interview, phrased as a question and
addressed to a named person. Do not resolve these by guessing; a wrong
assumption here becomes a permission that is wrong in production.

---

## Confirming it

Ask plainly:

> Is any line here wrong or missing? Especially the "cannot see" lines and the
> "if it never comes" column.

Change what they correct, then build. Keep the brief beside the metadata — when
someone later asks why a role is shaped that way, this is the answer.
