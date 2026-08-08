# Spec 009 — Authenticated POST endpoints

Status: proposed. This specification lets a REST endpoint authenticate a caller
by verifying a signature over the raw request, and run as a declared role when
it does. It applies to any method the endpoint declares, not only POST. It adds
one metadata block, one spelling of it named for what people look for, and one
handler change. It introduces no new execution path, and in particular no
inbound-trigger registry.

An earlier draft of this spec proposed exactly that registry — inbound triggers
compiled per connector instance, a parallel contract vocabulary, a new
dispatch. It was reviewed with the question "why not just allow custom
authorization on POST?", and the question was right. Most of what that draft
proposed to build already exists; what was actually missing is small and named
below.

## 1. What is true today, verified at `6bad681`

- **Only Stripe can receive a callback.** `RegistryInstance::Http` has no
  `webhook` field at all (`crates/server/src/connectors/mod.rs:168`). Inbound
  is a Stripe feature, not a connector capability.
- **A REST endpoint already accepts a POST** and runs a saved operation
  (`crates/server/src/rest.rs`). This is a working inbound route.
- **It parses the body before anything else**: the handler takes
  `body: Option<axum::Json<Json>>` (`rest.rs:83`).
- **Its role comes from headers.** `gql::resolve_session(&state, &headers)`
  (`rest.rs:86`), and the role is mandatory exactly as on `/v1/graphql`.
- **Commands already carry idempotency**, `CommandIdempotency`
  (`crates/metadata/src/types.rs:1122`), backed by `donat.command_invocations`.
- **Commands can already signal a parked process**, `SignalProcess`
  (`types.rs:1179`).
- **Nothing outside the Stripe module verifies a signature.** `grep hmac` over
  `crates/server/src/*.rs` returns one unrelated test.

## 2. What was actually missing

Of everything the earlier draft proposed to build, three of four halves already
exist:

| Need | Already there |
|---|---|
| A route that accepts the POST | `/api/rest/<url>`, any method |
| A typed contract for the payload | the saved operation's variables, and the command's typed arguments |
| Once-only under provider retries | `CommandIdempotency` + `donat.command_invocations` |
| Advancing a process that is waiting | a command's `signal_process` effect |
| **Proving the bytes came from the sender** | **missing** |
| **Running as somebody, without a JWT** | **missing** |

The two missing halves are one thing: **a verified signature should be able to
establish a role.** That is the whole change.

## 3. Why a header cannot do it

The obvious cheap answer is a shared secret in a header. It is not enough, for
a reason that is about the route rather than the credential:

**A signature covers the exact raw bytes.** The REST handler parses the JSON
before anything sees it, and re-serialising a parsed document produces
different bytes — so a valid signature fails. Verification has to happen before
the body is read as JSON, which no route currently does.

For a method that carries no body — a `GET` callback, a `DELETE` — there are no
bytes to sign, so the signed payload is the path and raw query string instead.
The mechanism is the same; only what goes into the digest differs, which is why
`signed_payload` is a template rather than a flag.

A bare shared secret sidesteps that and is worth supporting for senders that
offer nothing better, but it is replayable and it does not survive a log leak.
The mechanism has to accommodate both and prefer the signature.

The remaining alternatives are worse. `X-Donat-Admin-Secret` is transport
authentication rather than a permission and must not be in production. The
unauthorized role turns the endpoint into a mutation any stranger may call.

## 4. The change

### Metadata

One new block on a REST endpoint. Absent, everything behaves exactly as today.

```yaml
rest_endpoints:
  - name: stripe_events
    url: hooks/stripe
    methods: [POST]
    authenticate:
      # Verified over the exact raw body, before it is parsed.
      signature:
        header: Stripe-Signature
        scheme: hmac_sha256
        encoding: hex
        # Literal text with {body} and {timestamp} substituted; providers
        # differ, and this is the difference.
        signed_payload: "{timestamp}.{body}"
        timestamp_from: header_field   # Stripe puts it in the same header
        tolerance_seconds: 300
        secret: { value_from_env: STRIPE_WEBHOOK_SECRET }
      # What the request runs as once the signature verifies. An ordinary
      # declared role, resolving through its own table permissions.
      run_as: billing
      max_body_bytes: 65536
      # Anything not listed is acknowledged with 204 and does nothing —
      # see §5. Absent means "accept everything the operation accepts".
      accept:
        - { json_pointer: /type, equals: invoice.paid }
        - { json_pointer: /type, equals: invoice.payment_failed }
    definition:
      query:
        collection_name: hooks
        query_name: RecordStripeInvoiceEvent
```

`RecordStripeInvoiceEvent` is an ordinary saved mutation calling an ordinary
command. Its arguments are typed, its idempotency key is bound from the body,
its writes go through `billing`'s table permissions, and if it needs to advance
a waiting process it declares `signal_process` — all of which exists.

### Handler

`crates/server/src/rest.rs`:

- take `body: Bytes` instead of `Option<axum::Json<Json>>`;
- when the endpoint declares `authenticate`, verify over those bytes **before**
  parsing, and on success construct the session from `run_as` rather than from
  headers;
- when it does not, parse and resolve the session from headers exactly as now;
- bound the body by `max_body_bytes` before verifying.

That is the entire engine change. No registry, no new dispatch, no schema.

## 4b. `webhook:` — the spelling people look for

Everything in §4 is the mechanism, and almost nobody will find it. Someone
integrating a payment provider does not search for "authenticated endpoint
bound to a saved operation"; they search for **webhook**. An agent asked to
"receive Stripe events" does the same. A capability nobody reaches for is a
capability that does not exist.

So the same machinery gets a second spelling, named for the job:

```yaml
webhooks:
  - name: stripe_invoices
    url: hooks/stripe
    methods: [POST]
    authenticate:
      signature:
        header: Stripe-Signature
        scheme: hmac_sha256
        signed_payload: "{timestamp}.{body}"
        tolerance_seconds: 300
        secret: { value_from_env: STRIPE_WEBHOOK_SECRET }
      run_as: billing
      max_body_bytes: 65536
    accept:
      - { json_pointer: /type, equals: invoice.paid }
      - { json_pointer: /type, equals: invoice.payment_failed }
    # What happens when a verified event arrives: one command, which records
    # the row and — if a process is waiting on it — signals that process.
    command: record_stripe_invoice_event
    arguments:
      event_id:   { json_pointer: /id }
      event_type: { json_pointer: /type }
      invoice_id: { json_pointer: /data/object/id }
      amount_minor: { json_pointer: /data/object/amount_paid }
    idempotency_key: { json_pointer: /id }
```

### It is sugar, and that is the whole point

A `webhooks` entry **compiles to** exactly what §4 describes: an endpoint with
an `authenticate` block, bound to a generated saved operation that invokes the
named command. There is no second dispatch, no second verification, no second
idempotency mechanism, and nothing in the engine that can behave differently
between the two spellings — because after compilation there is only one.

A deployment that outgrows the sugar writes the §4 form and loses nothing. A
deployment that never needs to never learns it exists.

### Write, then continue the flow

The command is where "record it and carry on" happens, and both halves are
existing machinery:

- **Record.** The command's steps write the row, inside one transaction, under
  `run_as`'s ordinary table permissions.
- **Continue.** If a durable process is parked waiting on this, the command
  declares `signal_process` and the same transaction that recorded the event
  releases the wait. The row and the transition commit together or not at all,
  which is the property that makes "we recorded it but the flow never resumed"
  unrepresentable.

That is why the sugar names a **command** rather than an operation. A command
is the only thing in the format that can write and signal atomically, and a
webhook that can do one without the other is the bug this shape prevents.

### What it does not hide

The generated saved operation is inspectable, the command is an ordinary
command anyone can call through GraphQL if their role permits, and `run_as` is
an ordinary role. The sugar removes typing, not visibility.

## 5. What must stay true

1. **Verification before parsing, always.** The ordering is the security
   property; everything else here is metadata.
2. **`run_as` is an ordinary role.** It resolves through its own table
   permissions. A command it may not call is refused, not escalated. There is
   still no admin role, and an authenticated POST must not become one.
3. **An accepted-but-unmatched event is acknowledged, not refused.** Return
   204. A 400 makes a provider retry and eventually disable the endpoint,
   taking the events we do want with it.
4. **A failed signature is a 401 with nothing written**, and does not
   distinguish "wrong signature" from "unknown endpoint" any more than the
   existing 404-before-body-read does.
5. **Once-only is the command's idempotency key**, not a second mechanism. Bind
   it from whatever the sender's stable event id is.
6. **Fail-closed.** Anything after successful verification that cannot commit
   returns 503 with nothing written, so the sender retries.

## 6. What this does not cover, and what to do about it

**Stripe's existing `wait.webhook` integration stays as it is.** It is a
different path — `/v1/connectors/{instance}/webhooks`, correlating to a parked
process — and nothing here changes or replaces it. A deployment can use both.

**A process waiting on an event** is reached from this route the same way any
command reaches one: `signal_process`. That is a longer sentence in metadata
than a direct correlation, and it is one mechanism instead of two.

**Delivery audit rows.** The connector route writes one per delivery, including
rejected ones. This route does not. If a deployment needs that record, the
command writes it — which also makes it queryable through ordinary permissions
rather than through a table only the engine knows about.

**Per-event typed output.** The connector route maps a provider's JSON onto
typed fields with `json_pointer`. Here, the saved operation binds body keys to
variables and the command declares their types. That is typing at the command
boundary rather than at a trigger boundary, and it is the boundary that already
refuses a wrong type.

## 7. Tests

**Unit — `crates/server/src/rest.rs`:**

- a correctly signed body authenticates and runs as `run_as`;
- a tampered body does not, and nothing is written;
- a timestamp outside the tolerance does not;
- a body over `max_body_bytes` is refused before verification;
- an endpoint with no `authenticate` block behaves exactly as before, including
  its header-derived role.

**Conformance — extending `crates/conformance/tests/rest_endpoints.rs`:**

- a signed POST invokes the command exactly once as the declared role;
- the same delivery again returns the original result and writes nothing new,
  through the command's own idempotency;
- a signed body that matches no `accept` entry returns 204 and invokes nothing;
- a forged signature returns 401, writes nothing, and never needs a trusted id
  from the body;
- a command the `run_as` role may not call is refused rather than escalated;
- an endpoint without `authenticate` still requires a role from headers — the
  new path must not become a way around that;
- a `webhooks:` entry and the hand-written `authenticate` form it compiles to
  produce byte-identical behaviour on the same delivery, which is the test that
  keeps the sugar from becoming a second implementation;
- a signed `GET` callback authenticates over path and query, since a method
  with no body still has something to sign.

## 8. Sequence and size

| Step | Schema | Rough size |
|---|---|---|
| `authenticate` metadata types and validation | none | ~0.5 day |
| `webhooks:` sugar, compiling to the above | none | ~0.5 day |
| Handler: raw bytes, verify, session from `run_as` | none | ~1 day |
| Signature schemes (hmac_sha256 and a bare shared secret) | none | ~0.5 day |
| Tests | none | ~1 day |

Around three and a half days against the four to five the registry version
needed. Two spellings, one execution path — the count that matters is the
second one.

Sizes are read off the code, not measured by having written it.

## 9. Deliberately out of scope

Generalising the connector route itself. If a second provider needs the
parked-process correlation that Stripe has, that is the moment to revisit the
registry — with a real case rather than an anticipated one.

Asymmetric signature schemes, replay ledgers separate from command idempotency,
and mTLS. Each is a reasonable next step and none is needed to receive a
webhook.

Form-encoded request bodies remain unexpressible on the **outbound** side. That
is a real gap, unrelated to this one, and still unclaimed.
