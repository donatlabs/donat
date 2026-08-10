---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[021-value-semantics]]"
---

# Money is declared in the shape the provider publishes it

## Context

Spec 026 §4 asks every payments connector for a proof named `_amounts_survive`:
"money is a decimal, and a connector that turns a minor-unit integer or a
decimal string into a float has corrupted a payment. Pin the exact
representation the provider documents, in both directions."

Writing it exposed that the four providers examined in this batch publish money
in four different shapes, and that the SDK's scalar set means something specific
by `Decimal`:

* **Paddle** — a **string in the lowest denomination**: "Monetary values are
  returned as strings in the lowest denomination for a currency", with its own
  table where `USD` $24.99 is `"2499"` and `JPY` ¥1000 is `"1000"`.
* **Xero** — a **JSON number**: every money field in its published OpenAPI is
  `"type": "number", "format": "double"` with Xero's own `x-is-money: true`
  marker, and a `unitdp` parameter that chooses two or four decimal places.
* **Mercado Pago** — **both**: *Get payment* types `transaction_amount` as
  `(number, optional)` and its own response example on the same page prints
  `"transaction_amount": "24.50"`.
* **PayPal** — a **string beside its currency**: `{ "currency_code": "USD",
  "value": "10.00" }` (recorded in `INVENTORY.md`; the connector is not in this
  slice).

`ValueScalar::Decimal` in this workspace admits a JSON **string** and nothing
else — that is how a decimal is kept out of a float on its way to Postgres
`numeric`. So `Decimal` is exactly right for Paddle and exactly wrong for Xero:
declaring Xero's `Total` as `Decimal` would refuse every real Xero response as a
contract violation, and declaring it `Int64` would refuse its fractional part.

## Decision

**A money field is declared in the shape the provider publishes it, and the
connector performs no conversion.**

* Where the provider publishes a **string**, the field is `ValueScalar::String`
  and the string is carried verbatim in both directions. Paddle's `"2499"`
  arrives and leaves as `"2499"`; an amount a caller supplies as a JSON string is
  rendered as a JSON string.
* Where the provider publishes a **number**, or publishes both forms, the field
  is `ValueScalar::Json`. It is the one scalar in the contract that carries the
  provider's own value through without coercing it, and the module header says
  why it is not `Decimal`.
* No connector converts between the two. A string is never parsed into a number,
  and a number is never rendered as a string.

**Every one of these is pinned by a test, in both directions**, and the tests
assert the *failures* as well as the shapes: that a `Decimal`-typed pointer
really would refuse Xero's own documented response, that Paddle's body carries
`"amount":"2499"` and not `2499.0`, that Mercado Pago's numeric and string forms
each survive as themselves, and that a declaration cannot be quietly retyped
later without a test failing.

Where a provider offers a *precision* choice, the declaration takes the wider
one: Xero's collections and reads send `unitdp=4` — "e.g. unitdp=4 – (Unit
Decimal Places) You can opt in to use four decimal places for unit amounts" —
because the alternative is Xero rounding a unit amount to two decimal places
before this connector ever sees it.

## Alternatives

| Option | Why Not |
|--------|---------|
| Widen `ValueScalar::Decimal` to admit a JSON number as well as a string | It would make `Decimal` mean "a decimal, or a float" across the whole workspace, and the float is the thing the type exists to exclude. One provider's wire format is not a reason to weaken every command argument's contract |
| Normalise every provider's money into one Donat form (a decimal string, say) | The connector would be *rewriting* the provider's answer, and a rewrite is where a payment gets corrupted: `24.50` → `"24.5"` is a value nobody sent. It would also make the output contract disagree with the provider's own reference, which is the second description [[049-a-connector-publishes-the-declaration-it-was-admitted-on]] refuses |
| Declare money as `Json` everywhere, since it always works | It would throw away the one place the type is exact: Paddle really does publish a string and nothing else, and `String` refuses a number that arrives where a string was documented — a provider change this connector should fail on rather than absorb |
| Declare Xero's amounts `Int64` in cents | Xero publishes neither cents nor an integer. That is a currency model invented here, and it would refuse the four-decimal unit amounts Xero explicitly offers |
| Skip the proof for a connector whose amounts are strings, since a string cannot lose precision | The outbound direction can still lose it: a caller's `"2499"` rendered as a JSON number would reach Paddle as `2499` and, for a body Paddle validates as a string, as a rejected request. The proof covers both directions for exactly that reason |

## Consequences

Money crosses this boundary in the provider's own shape, and every connector
that carries it says in one place — its module header — which shape that is and
why. A reviewer comparing a module against a provider reference is comparing
like with like.

Two costs are real. A `Json`-typed money output publishes no type information to
a Process: a deployment reading Xero's `total` gets whatever Xero sent, and if
Xero ever sent a string the contract would not notice. And this batch's
`_amounts_survive` proofs are the only thing standing between a future edit and a
quiet retyping, because nothing in the SDK knows which of an operation's outputs
are money — a `money: true` marker on `output_pointer`, mirroring Xero's own
`x-is-money`, is the shape that would make it structural, and it is not in this
slice.
