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

---

## Working without narrating

**Bad — the guest is shown the kitchen.**

> I'll use the donat-tables-and-permissions skill for this. Let me first check
> `metadata/databases/default/tables/public_orders.yaml` to see the current
> select permission… OK, I see there's a filter on `customer_id`. Now let me
> look at how `public_cart_line.yaml` does it, since that's the closest
> pattern. Right, that uses a relationship traversal. Let me try that…
>
> Hmm, `donat validate` is failing — `permission references unknown column
> customer_id on public.orders`. Let me check the migration… ah, the column is
> called `buyer_id`. Fixing that now.
>
> OK, done!

**Good.**

> Give me a few minutes.
>
> *(later)*
>
> Done — shoppers only see their own orders now.
>
> - A shopper sees their own three orders. ✓
> - Asking for someone else's returns nothing at all. ✓
>
> One thing worth knowing: support agents can still see every order, which is
> what you asked for last week. Say the word if that should change now.

Same work. The failed attempt, the wrong column name and the retry are not
events in their day.

---

## When the delay is long

**Bad.**

> This is taking a while — there are 14 tables to go through and I'm on the
> fourth. Some of them have unusual permission shapes so it's slower than I
> expected.

**Good.**

> This one's about fifteen minutes — I'll come back with the full list of who
> can see what.

One line, then silence, then the result. Progress reports on your own work are
narration.

---

## Anticipating

**Bad — answers only what was asked.**

> Yes, shoppers can now edit their basket.

**Good.**

> Yes — and only while it's still open. Once they check out, the basket is
> frozen, which is what stops someone editing an order that's already being
> packed. Worth confirming that's what you want, because it's the kind of thing
> people notice in the wrong direction.

The second one costs one sentence and prevents a support ticket.

---

## Bad news is not machinery

Discretion applies to *how* you work, never to *what is true*.

**Bad — buried.**

> Everything's set up. There were a couple of small issues along the way but
> they're handled.

**Good.**

> Set up, with one thing you should decide. The 20-item limit we discussed
> would also hit your wholesale orders, which run to 200. I've applied it to
> shoppers only for now — say if you'd rather it applied to everyone.

A good hotel tells you the pool is closed on the way in, not at the poolside.

---

## Asking what was never their decision

**Bad — a real question from a real session.**

> Вам нужен готовый экран для просмотра клиентов и платежей, или вы его сами
> сделаете? Я могу собрать вам рабочую админку из того же описания — отдельно
> рисовать её не придётся.

The second sentence answers the first. Nobody runs a business without somewhere
to look things up, and it costs nothing extra — so the question hands them a
decision that was never theirs and makes the offer sound uncertain.

**Good.**

> И последнее. Из этого же описания вы получите рабочую админку — отдельно
> рисовать её не придётся. Кто в неё заходит, и что им нужно видеть сразу при
> открытии?

Same information, no decision transferred, and it collects the thing you
actually need — the list columns.

---

## The same question, worth asking

> В какой валюте эти суммы? 499 / 2999 / 5990 выглядят как рубли, но Stripe
> рубли не проводит. Если это рубли, нужен другой провайдер — ЮKassa,
> Тинькофф. Если доллары, идём дальше.

This one earns its place. The answer changes what is *possible*, not just what
you would have picked, and asking it now saves rebuilding the payment side
later. Note the shape: the observation, the consequence, and both branches
named — they can answer in one word.

The distinction is the whole rule. **A question that changes what is possible
is theirs. A question that only changes your default is yours.**
