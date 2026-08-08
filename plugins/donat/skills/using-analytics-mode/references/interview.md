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

## 3. The matrix, filled in before you ask

Do **not** walk through "what can this person see? and change? and delete?" role
by role in prose. It takes twenty minutes, and the operations nobody mentions —
almost always delete, and almost always "who creates this in the first place" —
simply never come up.

Put up the table instead, **already filled with your best guess**, and ask what
is wrong. Correcting a table is faster and more accurate than answering eight
open questions, and it makes the gaps visible.

> Пробегусь по табличке — для каждой записи, кто что может. Я заполнил как
> мне кажется правильным, скажите, где не так.
>
> | Запись | Клиент | Оператор |
> |---|---|---|
> | Тариф | видит | видит |
> | Клиент | видит и меняет только себя | видит всех, не меняет |
> | Подписка | видит свою | видит все, не меняет |
> | Платёж | видит свои | видит все, не меняет |
>
> Три вопроса к ней:
> — кто заводит клиента: он сам при регистрации, или вы вручную?
> — что-нибудь здесь вообще удаляется, или только помечается закрытым?
> — есть запись, которую видно, но трогать нельзя даже владельцу?

*What this decides: every select/insert/update/delete permission at once, plus
the presets. Four cells per record, and the ones people forget are the create
("who brings this row into existence") and the delete.*

**Delete deserves its own question every time.** In most business systems
nothing is deleted — a subscription is cancelled, a client is closed, a payment
is refunded. If they say "delete" out loud, ask whether they mean the row is
gone or the record is marked finished. Getting that wrong is unrecoverable in
the direction that matters.

Then, per record, the one that decides masks:

> Есть что-то в этой записи, что человек видеть не должен, хотя саму запись
> видит? Себестоимость, внутренняя заметка, чей-то телефон?

And the one that separates "cannot see" from "sees an error":

> Может ли клиент найти чужую запись — поиском или подобрав номер?

*What this decides: a well-built system returns nothing rather than "access
denied", so an outsider cannot even confirm the record exists. Worth saying out
loud once, because it looks like a bug to people.*

---

## 4. When the right to change runs out

For every row in the matrix that says "меняет", one follow-up:

> До какого момента это можно менять? Что должно произойти, чтобы стало
> нельзя?

*What this decides: the state filter on the update permission — a basket
editable while open, a subscription editable until it is cancelled. Without
this the permission is "may edit forever", which is almost never what they
meant.*

> Когда он это создаёт, есть что-то, что система должна подставить сама, а не
> доверять ему вписать?

*What this decides: presets. Anything the system fills in cannot be forged.*

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

## 10. Logging in

Do **not** ask which identity provider to use. That is your call — Auth0 unless
something overrides it. Two questions are genuinely theirs:

> Is there already something that handles logins for you, or are we starting
> from nothing?

> Does anyone sign in with a company account — Google Workspace, Microsoft —
> rather than an email and a password?

*What this decides: whether to point at an existing provider or stand one up,
and whether the provider needs enterprise SSO. Never a users table with a
password column.*

---

## 11. The platform

Do **not** ask whether they want screens. They do, they come from the same
declarations, and asking hands them a decision that was never theirs. State it,
then ask what it should show:

> Из этого же описания вы получите работающую платформу — отдельно рисовать
> ничего не придётся. Кто в неё заходит, и что каждому нужно видеть сразу при
> открытии?

Say *platform*, not *admin panel*: a client managing their own subscription and
an operator reviewing every client are two roles in one platform, not a product
plus a side tool.

> Is there anything in there they should be able to look at but never change?

*What this decides: which resources appear, the list columns, and which fields
are read-only for that role.*

---

## 12. Reaching it

> Who or what else needs to talk to this — a mobile app, a partner, a
> spreadsheet, an AI assistant?

*What this decides: which surfaces to mount — GraphQL, REST endpoints, MCP
tools — and which roles they run as.*

---

## Closing

> Anything you were expecting me to ask that I didn't?

Then write the brief and get it confirmed before building anything.
