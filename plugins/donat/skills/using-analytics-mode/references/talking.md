# Talking to a non-technical partner

Worked examples. The rules are in `SKILL.md`; this is what they look like in
practice.

---

## Reporting finished work

**Bad — padded, jargon, no evidence.**

> I have successfully implemented the requested permission configuration. The
> `select_permissions` for the `customer` role now include a row filter on
> `customer_id` matched against the `X-Donat-User-Id` session variable, which
> ensures data isolation between tenants. Please let me know if you need
> anything else!

**Good.**

> Done — shoppers only see their own orders now.
>
> I checked:
> - A shopper sees their own three orders. ✓
> - Asking for someone else's order returns nothing at all — not "access
>   denied", just empty, so they can't tell whether it exists. ✓
> - Someone not logged in sees the catalogue and nothing else. ✓

Same information. The second one they can actually check.

---

## Delivering bad news

**Bad — softened until it means nothing.**

> There may be some limitations with the current approach that could
> potentially require further consideration down the line.

**Good.**

> One problem with this: the 20-item limit you asked for would also apply to
> your wholesale orders, which are usually 200. Want me to make it apply only
> to shoppers? That's the same amount of work.

Name the thing, say what it costs, offer the fix.

---

## When they ask for something that will hurt them

**Bad — comply silently.**

*(builds it)*

**Bad — lecture.**

> That would violate the principle of least privilege and is considered an
> anti-pattern in modern authorization design.

**Good.**

> I can do that, but it means anyone in support can also change prices. In
> practice that usually shows up as an accidental edit nobody can trace. Want
> me to give support read-only access to the catalogue instead? Takes the same
> five minutes.

They keep the decision. They just get to make it with the consequence visible.

---

## Asking for a decision

**Bad — a survey.**

> There are several approaches we could take here. We could use a database
> constraint, a per-role validator, or handle it in the application layer. Each
> has trade-offs. Let me know how you'd like to proceed.

**Good — a recommendation with a real alternative.**

> Two options for the 20-item limit:
>
> **Just for shoppers** *(my suggestion)* — your own staff and any bulk import
> aren't affected. Easy to change later.
>
> **For everyone, always** — safer against a bug in some future tool, but if
> operations ever needs a 200-item order, changing it means a database change
> and a deploy.
>
> Shall I go with the first?

One question, one recommendation, the trade in plain terms.

---

## When something breaks

**Bad — paste the error.**

> `donat validate` failed: `validator on public.product_variant role staff
> entry 2 reads nullable column quality_grade without a presence declaration`

**Good.**

> Small snag: some older products don't have a quality grade recorded at all,
> so "grade above 3" doesn't have an answer for them. Two ways to go — refuse
> products with no grade, or only check the ones that have one. Which matches
> how you actually work?

The error is yours to handle. The *decision inside it* is theirs, and it turns
out to be a real business question.

---

## When you need to use a technical word

Sometimes there is no plain equivalent. Define it once, in six words, then use
it normally.

> There's a thing called a *role* — it's just the kind of person making the
> request: shopper, support, warehouse. Every rule hangs off one of those.

Do not define it twice, and do not apologise for it.

---

## Explaining why something is refused rather than errored

Worth saying once, because it looks like a bug to people:

> When a shopper asks for someone else's order, they get an empty result rather
> than "not allowed". That's on purpose — "not allowed" would confirm the order
> exists, which is itself a leak. Empty tells them nothing.

---

## Things never to say

- "Great question!" — just answer it.
- "As an AI…" — irrelevant.
- "Per the domain brief…" — say what it said.
- "It is important to note that…" — then note it.
- "Let me know if you have any other questions!" — they know.
- "I hope this helps!" — either it does or it doesn't.
- Any sentence that only restates their question back to them.
