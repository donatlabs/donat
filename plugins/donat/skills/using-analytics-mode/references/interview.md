# The interview script

Ask these in order, one at a time. Let the answer be a story; you do the
sorting. Stop when you have enough to write the brief — not every section
applies to every domain.

Never read the "what this decides" notes aloud. They are for you.

---

## 1. The shape of the thing

> Describe what happens here, start to finish, as if I were a new hire on my
> first day.

Let them talk. Do not interrupt to classify. Afterwards, play back the nouns
you heard and ask which ones the business actually keeps records of.

*What this decides: the tables, and which ones are real entities versus passing
detail.*

> Which of these do you look up by name or number? Which ones only ever exist
> as part of something else?

*What this decides: tables versus columns, and relationships.*

---

## 2. The people

> Who are the different kinds of people who touch this? Include the ones who
> never log in — customers, suppliers, someone in finance who only approves
> things.

> Is there anyone who does two of these jobs at once?

*What this decides: roles. A person with two jobs is two roles, not a third
merged one.*

---

## 3. What each of them sees

For each kind of person, one at a time:

> When [a shopper] opens this, what should they see?

> Is there anything on that record they should *not* see? A cost price, an
> internal note, someone's phone number?

> Should they be able to find records that aren't theirs at all — by searching,
> or by guessing a number?

*What this decides: row filters and column masks. The last question separates
"cannot see" from "sees an error", which matters: a well-built system returns
nothing rather than "access denied", so an outsider cannot even confirm the
record exists.*

---

## 4. What each of them changes

> What can [a shopper] create, edit or delete?

> Is there a point after which they can't change it any more? What is that
> point?

> When they create one of these, is there anything the system should fill in
> for them rather than trusting them to type it?

*What this decides: insert/update/delete permissions, state-based filters, and
presets. The last one is important — anything the system fills in cannot be
forged.*

---

## 5. What must never happen

> What would be a disaster here? The thing that makes someone call you at the
> weekend.

> Are there numbers that must never go negative, or never exceed something?

For each rule, the layer question — this is the one people never volunteer:

> Is that true of absolutely everyone, forever — including your own staff, an
> import, a data fix? Or is it a limit on this particular kind of user?

*What this decides: a database constraint (everyone) versus a per-role
validator (one kind of user). Getting it backwards either blocks their own
operations later or leaves the rule bypassable.*

> When someone hits that limit, what should they be told? Give me the exact
> sentence.

*What this decides: the validator's message. This is product copy — it is
theirs to write, not yours.*

---

## 6. Things that happen together

> Is there anything where two or three things have to happen together, and it
> would be a mess if only some of them did?

> Has anyone ever double-clicked and got two of something?

*What this decides: a command, and its idempotency key.*

---

## 7. Waiting

> Is there anything where you do one part, then wait, then do the rest?

> Who or what are you waiting for — a person approving, a payment provider, a
> delivery, a date?

> **And if that never comes?** How long do you wait, and what happens then?

> While you're waiting, is anything held or reserved? Stock, money, a slot?

*What this decides: a durable process with waits, deadlines and timeout
branches. The "if it never comes" answer is the one that stops orders sitting
forever, and people almost never volunteer it.*

---

## 8. The outside world

> What other systems are involved? Payments, shipping, tax, email, anything you
> log into separately.

> Has one of them ever charged twice, or gone quiet halfway through so you
> didn't know whether it worked?

*What this decides: connectors, their idempotency policy, and whether the flow
needs an ambiguity lookup. The second question is worth asking even when they
say no — "we're not sure whether that charge went through" is the failure that
justifies the whole design.*

---

## 9. Files

> Does anything here have a document or a picture attached? Who can see it?

*What this decides: file columns, and whether the attachment is public or
signed. "Anyone with the link" is a real grant and should be a deliberate
answer.*

---

## 10. Reaching it

> Who or what needs to talk to this besides your own screens? A mobile app, a
> partner, a spreadsheet, an AI assistant?

*What this decides: which surfaces to mount — GraphQL, REST endpoints, MCP
tools — and which roles they run as.*

---

## Closing

> Anything you were expecting me to ask that I didn't?

Then write the brief and get it confirmed before building anything.
