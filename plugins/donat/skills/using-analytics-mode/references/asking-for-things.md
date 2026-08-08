# Asking for the things only they can give you

A blocked-on-you list made of nouns is not a list, it is homework with no
instructions. "Нужны price id из Stripe, тенант Auth0 и счёт" is fine for an
engineer and useless to everyone else: they do not know which of the forty
screens has it, what it looks like when they find it, or whether the thing they
copied is the right one.

Every item gets four things:

1. **What it is**, in one plain clause.
2. **Where to click**, roughly — UI wording drifts, so name the section, not the
   pixel.
3. **What it looks like**, so they can tell they got the right string.
4. **How to send it** — and for anything secret, how *not* to.

## Never accept a secret in a message

This is the rule that matters most, because the natural way to ask produces the
unsafe answer. Ask a non-technical person for "the Stripe keys" and a live
secret key arrives in the chat, where it stays — in history, in backups, in
whatever the transcript is synced to. Rotating it afterwards is their problem
and your fault.

So split every request in two:

| Safe to paste | Must never be pasted |
|---|---|
| Price ids (`price_…`) | Secret key (`sk_live_…`, `sk_test_…`) |
| Publishable key (`pk_…`) | Webhook signing secret (`whsec_…`) |
| Auth0 tenant domain, Client ID | Auth0 Client Secret |
| Bank account for payouts (set in the provider, not sent to you) | Any password |

Say it out loud, once, in a sentence they will act on:

> Всё, что начинается на `sk_` или `whsec_`, мне присылать не надо — вообще
> никуда, ни в чат, ни в почту. Их вы вставите сами в одно поле, я покажу
> куда. Остальное можно просто скинуть сообщением.

Then make "вставите сами" real: a `.env` file they edit, a one-line command, or
a secret field in their hosting panel. Never "и пришлите мне, я вставлю".

## Worked: Stripe prices

> **Что нужно:** три идентификатора ваших тарифов в Stripe. Это то, что
> связывает кнопку «Оплатить» с конкретной ценой.
>
> **Где взять:** зайдите в Stripe → раздел с товарами (Products / Каталог) →
> откройте тариф → у каждой цены есть свой идентификатор, рядом с суммой.
>
> **Как выглядит:** длинная строка, начинается на `price_`, например
> `price_1QxYz2AbCdEfGh`. Если начинается на `prod_` — это товар, а мне нужна
> цена, она внутри него.
>
> **Что прислать:** три такие строки и рядом, какая из них какой тариф. Просто
> сообщением, это не секрет.
>
> Если тарифов в Stripe ещё нет — скажите, я напишу, какие создать, это
> пятнадцать минут.

Note the third paragraph: the `prod_` versus `price_` confusion is the single
most common wrong answer, and naming it up front saves a round trip.

## Worked: Auth0

> **Что нужно:** адрес вашего Auth0 и идентификатор приложения. По ним система
> проверяет, что вошедший — действительно вошедший.
>
> **Где взять:** Auth0 → Applications → ваше приложение. Там три поля: Domain,
> Client ID, Client Secret.
>
> **Как выглядит:** Domain — что-то вроде `dev-a1b2c3.eu.auth0.com`.
> Client ID — строка из букв и цифр.
>
> **Что прислать:** Domain и Client ID — сообщением, это не секреты.
> **Client Secret не присылайте.** Он понадобится один раз, вы вставите его
> сами — я пришлю файл с одной пустой строкой и скажу, куда положить.
>
> Аккаунта ещё нет — скажите, заведу тестовый, на нём всё будет работать, и
> потом перецепим на ваш одним движением.

## Worked: where the money lands

> **Что нужно:** счёт, на который Stripe переводит собранное.
>
> **Где сделать:** это настраивается внутри Stripe, в разделе выплат, и мне
> ничего присылать не надо — я к вашим банковским данным доступа не имею и не
> должен.
>
> **Что от вас:** просто скажите, когда заполните, чтобы я знал, что переход на
> живые платежи не упрётся в это.

Some items are not requests at all — they are things they do, and you only need
to know when. Saying so is what keeps the list honest.

## Ordering the list

Put it **after** the progress report, never before: the first thing they read
should be what now works, not what you want from them. And say plainly what
does not depend on it:

> До этого всего стенд работает на тестовых ключах, и всё остальное я делаю
> дальше — ждать не нужно.

An item that blocks nothing right now is worth a line saying so. An item that
blocks the next step is worth saying that too, because it changes whether they
do it today or on Friday.

## When they send the wrong thing

They will. Do not explain the taxonomy; just say what to look for instead.

**Bad.**

> Это product ID, а не price ID. Price принадлежит product'у, у одного product
> может быть несколько price'ов — например месячный и годовой.

**Good.**

> Это идентификатор товара, а мне нужна цена внутри него — откройте этот же
> тариф, там ниже блок с суммой, и рядом с ней такая же строка, только
> начинается на `price_`.

And if a secret arrives in the chat anyway, do not scold — say what to do, in
this order:

> Этот ключ лучше сразу отозвать: он побывал в переписке, а её история
> хранится. В Stripe → Developers → API keys есть «roll»/«revoke» рядом с ним.
> Новый мне присылать не надо, я покажу, куда вы его вставите сами.
