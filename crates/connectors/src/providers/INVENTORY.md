# Provider writes with no idempotency mechanism — the evidence behind each class

Spec 010 §7 admitted exactly two executable mutating classes:
`ProviderIdempotent::ExplicitKey`, where the provider documents a key and its
retention, and `ProviderIdempotent::NaturalMethod`, where the operation is a
`PUT` or `DELETE` against a fixed resource identity whose repeat-safe semantics
the provider documents.

[[063-an-at-most-once-send-is-admitted-only-where-a-process-says-what-an-unknown-outcome-means]]
adds a third, `AtMostOnce`, and this file is the population it was decided on.
The evidence is the same evidence that was already here — the search that found
no key, and what a second send would produce — and it is now carried on the
operation in Rust rather than only in this document. A `AtMostOnce` operation
is executable **only** from a Process activity that declares `at_most_once: true`
and a route for an outcome nobody can know; it is never reachable by silence.

`InventoryOnly` is still a real class, and 29 of the operations below are still
in it. What separates them is stated in "What ADR 063 admitted, and what it did
not" at the end of the Batch A section.

This file is the second deliverable of spec 012 §2 for **Batch A**
(`airtable`, `sendgrid`, `postmark`, `twilio`, `openai`, `typeform`): every
operation this batch classified `InventoryOnly`, the provider documentation
that establishes the absence of an idempotency mechanism, and what a
Process-level at-most-once opt-in would have to promise before any of them
could execute.

## How a negative is established here

No provider publishes a sentence saying "this endpoint has no idempotency key".
What a provider does publish is a *complete request contract*: the parameters,
headers, and behaviours the endpoint accepts. The evidence in each entry below
is therefore of one of two kinds, and each entry says which:

* **Complete published contract, no key in it** — the endpoint's own reference
  page enumerates its request contract, and no idempotency key, client-supplied
  request identifier, or deduplication behaviour appears in it, nor anywhere
  else in that provider's API documentation read on 2026-08-10.
* **Machine-checkable absence** — the provider publishes a machine-readable API
  description (OpenAPI), and the term does not occur in it for these endpoints.
  This is the stronger form and applies to OpenAI only.

Two near-misses are recorded rather than silently dropped, because a reviewer
searching the same documentation will find them:

* **Twilio** documents `Idempotency-Token` on the Monitor *Alarms* API, and
  `I-Twilio-Idempotency-Token` on *inbound* webhook deliveries. Neither is on
  the Message or Call resource, and neither is a key this connector could bind.
* **OpenAI** documents an `Idempotency-Key` header in its *Agentic Commerce*
  specification — a contract for merchants implementing an API that OpenAI
  calls, not for the OpenAI API this connector calls. The published OpenAI
  OpenAPI document contains no such header.

Every statement below was read from the provider's own documentation on
2026-08-10; the source URLs are in each connector module's header comment.

## The operations

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `airtable` | `record.create` | `POST /v0/{baseId}/{tableIdOrName}` | `AtMostOnce` |
| `airtable` | `record.update_patch` | `PATCH /v0/{baseId}/{tableIdOrName}/{recordId}` | `InventoryOnly` |
| `sendgrid` | `list.create` | `POST /v3/marketing/lists` | `AtMostOnce` |
| `sendgrid` | `list.update` | `PATCH /v3/marketing/lists/{id}` | `InventoryOnly` |
| `sendgrid` | `mail.send` | `POST /v3/mail/send` | `AtMostOnce` |
| `postmark` | `email.send` | `POST /email` | `AtMostOnce` |
| `postmark` | `email.send_template` | `POST /email/withTemplate` | `AtMostOnce` |
| `twilio` | `message.send` | `POST /2010-04-01/Accounts/{AccountSid}/Messages.json` | `AtMostOnce` |
| `twilio` | `call.create` | `POST /2010-04-01/Accounts/{AccountSid}/Calls.json` | `AtMostOnce` |
| `openai` | `chat.complete` | `POST /v1/chat/completions` | `InventoryOnly` |
| `openai` | `embedding.create` | `POST /v1/embeddings` | `InventoryOnly` |

`typeform` contributes none: this batch declares no Typeform create, and its one
mutation (`response.delete`) is admitted as `NaturalMethod` on Typeform's own
statement that "Not found response IDs will be ignored."

### `airtable` — `record.create`

* **Documented contract.** *Create records*
  (`POST https://api.airtable.com/v0/{baseId}/{tableIdOrName}`) documents the
  request body as `fields` (a single record) or `records` (an array), with the
  optional `typecast` and `returnFieldsByFieldId`. *Authentication* documents
  the one request header a call must carry: `Authorization: Bearer YOUR_TOKEN`.
* **Evidence kind.** Complete published contract, no key in it. No request
  header, body field, or query parameter in the Airtable Web API reference
  carries a client-supplied request identifier, and the reference's status-code
  page documents no status for a replayed or duplicate request.
* **What a repeat produces.** A second record with a new record ID. Airtable
  does not deduplicate on field values; a retry after an ambiguous worker loss
  therefore leaves two records that a Process would have to reconcile itself.
* **Nearest supported alternative.** Airtable's *Update multiple records*
  endpoint has a `performUpsert` option keyed on declared `fieldsToMergeOn`.
  That is a genuine upsert, not an idempotency key: it deduplicates on business
  data the caller chooses. It is out of this batch's operation set and would
  need its own declaration and its own evidence.

### `airtable` — `record.update_patch`

* **Documented contract.** *Update record* documents both halves of the same
  route and the difference between them: "A PATCH request will only update the
  fields you specify, leaving the rest as they were. A PUT request will perform
  a destructive update and clear all unspecified cell values."
* **Evidence kind.** Complete published contract, no key in it — plus a method
  the effect gate does not admit. Spec 010 §7 admits `NaturalMethod` only for
  `PUT` and `DELETE`, and `PATCH` is neither.
* **What a repeat produces.** For a `PATCH` whose body is a set of literal
  values, a repeat is in fact harmless; for one whose intent is relative (an
  incremented counter, an appended list built by the caller), it is not. The
  provider publishes nothing that distinguishes the two, so the connector cannot
  either. The `PUT` sibling, `record.replace`, is executable precisely because
  Airtable documents it as a whole-record replacement.

### `sendgrid` — `list.create`

* **Documented contract.** *Create list* (`POST /v3/marketing/lists`) documents
  a body of `{"name": …}`, a `201` success, and the constraint "You can create a
  maximum of 1000 lists."
* **Evidence kind.** Complete published contract, no key in it. Nothing on the
  page documents duplicate-name protection, an idempotency key, or a
  client-supplied request identifier, and the v3 API's shared *Responses* page
  documents no status for a replayed request.
* **What a repeat produces.** A second list with the same name and a new ID, and
  one more of the 1000 lists the account is allowed.

### `sendgrid` — `list.update`

* **Documented contract.** *Update list* documents `PATCH
  /v3/marketing/lists/{id}` with a `name` body and a `200` success.
* **Evidence kind.** Complete published contract, no key in it — plus the method
  the gate does not admit (`PATCH`).
* **What a repeat produces.** The same rename applied twice, which for this
  particular body is harmless; the gate does not admit it because nothing in the
  provider's contract makes that true for every body it accepts.

### `sendgrid` — `mail.send`

* **Documented contract.** *Mail Send* (`POST /v3/mail/send`) documents the
  required `personalizations`, `from`, `subject`, and `content`, a `202
  Accepted` success, and an `X-Message-Id` response header that identifies the
  message **after** SendGrid has accepted it.
* **Evidence kind.** Complete published contract, no key in it. The identifier
  SendGrid publishes is server-issued and arrives in the response, which is the
  opposite of an idempotency key: it cannot be supplied on the retry that needs
  it.
* **What a repeat produces.** A second delivered email, with a second
  `X-Message-Id`, to the same recipients. This is the operation in Batch A with
  the most visible external consequence of a duplicate.

### `postmark` — `email.send`

* **Documented contract.** The *Email API* documents `POST /email` with required
  `From`, `To`, `Subject`, and one of `HtmlBody`/`TextBody`, and a success body
  of `To`, `SubmittedAt`, `MessageID`, `ErrorCode`, `Message`. The *Overview*
  documents the required headers as `X-Postmark-Server-Token`, `Accept`, and
  `Content-Type`.
* **Evidence kind.** Complete published contract, no key in it. Postmark
  publishes a numeric `ErrorCode` list of more than fifty codes covering
  signatures, servers, templates, message streams, and bounces; none of them
  describes a replayed or duplicate request, which is what a provider that
  deduplicated would need in order to tell a caller it had.
* **What a repeat produces.** A second delivered email with a new `MessageID`.

### `postmark` — `email.send_template`

* **Documented contract.** The *Templates API* documents `POST
  /email/withTemplate` with required `TemplateId` or `TemplateAlias`,
  `TemplateModel`, `From`, and `To`, and the same response shape as `POST
  /email`.
* **Evidence kind.** Complete published contract, no key in it — identical in
  form to `email.send`, which is the endpoint it shares its delivery path with.
* **What a repeat produces.** A second delivered email.

### `twilio` — `message.send`

* **Documented contract.** The *Message resource* documents `POST
  /2010-04-01/Accounts/{AccountSid}/Messages.json` with required `To`, one of
  `From`/`MessagingServiceSid`, and one of `Body`/`MediaUrl`/`ContentSid`, plus
  the optional `StatusCallback`, `ValidityPeriod`, `ScheduleType`/`SendAt`, and
  `ShortenUrls`. The body is `application/x-www-form-urlencoded`.
* **Evidence kind.** Complete published contract, no key in it, **and** a
  documented idempotency mechanism that is demonstrably elsewhere: Twilio's
  Monitor *Alarms* API documents an `Idempotency-Token` request header, and
  Twilio's webhook documentation documents `I-Twilio-Idempotency-Token` on
  deliveries *it* makes. Twilio therefore has the concept, publishes it where it
  applies, and does not publish it here.
* **What a repeat produces.** A second SMS or MMS, a second `sid`, and a second
  charge.

### `twilio` — `call.create`

* **Documented contract.** The *Call resource* documents `POST
  /2010-04-01/Accounts/{AccountSid}/Calls.json` with required `To`, `From`, and
  exactly one of `Url`, `Twiml`, or `ApplicationSid`.
* **Evidence kind.** Complete published contract, no key in it; the same Twilio
  observation as above applies.
* **What a repeat produces.** A second outbound call to the same number, and a
  second charge.

### `openai` — `chat.complete`

* **Documented contract.** OpenAI's published OpenAPI document
  (<https://github.com/openai/openai-openapi>, `openapi.yaml`) declares `POST
  /chat/completions` with `required: [model, messages]` and no request-header
  parameter of any kind beyond the `ApiKeyAuth` bearer scheme.
* **Evidence kind.** Machine-checkable absence. The string `Idempotency-Key`
  does not occur in the document; the only occurrences of "idempotent" are on
  the certificate activation and deactivation endpoints ("You can atomically and
  idempotently activate up to 10 certificates at a time"), which this connector
  does not publish.
* **What a repeat produces.** A second billed completion, and — because the
  endpoint is generative and non-deterministic — very likely a different answer.
  This operation would remain inventory-only even if OpenAI published a key
  tomorrow, on the reasoning spec 012 §2 gives: a duplicate is a second charge
  and a different result, not a repeat.

### `openai` — `embedding.create`

* **Documented contract.** `POST /embeddings` with `required: [model, input]`,
  same document, same absence.
* **Evidence kind.** Machine-checkable absence, as above.
* **What a repeat produces.** A second billed embedding request. Embeddings are
  far more nearly deterministic than completions, but the charge is not.

## What the at-most-once class promises — and what ADR 063 admitted

`ExplicitKey` and `NaturalMethod` are *provider* guarantees: the duplicate
reaches the provider and the provider absorbs it. `AtMostOnce` is not that. The
provider will happily accept the second send, so the only thing Donat can do is
decide not to make it. ADR 063 built the class on exactly the five things this
file said it would have to promise, and every one of them is now enforced:

1. **A durable pre-commit before the request leaves.** The activity claims one
   send authorization in `process_activity_provider_steps`, in a transaction that
   commits before any byte leaves, and the row survives the loss of the worker
   that wrote it.
2. **A refusal, not a retry, on an ambiguous outcome.** The authorization is
   claimed exactly once, so a later worker cannot send. Compilation refuses a
   non-empty `retry_on` and any `max_attempts` above one, so `transport` and
   `timeout` are not retryable here even though they are for every other class.
3. **An explicit, per-activity opt-in.** `at_most_once: true` is written on the
   Process activity, not on the connector: the same operation is admitted in one
   Process and refused in another, and the person accepting "this email may
   silently not be sent" is the person who wrote it down.
4. **A visible unresolved destination.** `on_ambiguous` is mandatory and is not
   an `on_error` route: the instance goes to a state the deployment chose for
   "sent, outcome unknown" rather than collapsing into success or failure.
5. **No promotion by proximity.** The class is a property of the operation, and
   the opt-in is refused on an operation that does not carry it (ADR 034).

What it does not promise, stated plainly: **the operation may not happen at
all.** For `mail.send`, `email.send`, and `message.send` that is a message the
customer never receives; for `list.create` and `record.create` it is a resource
a later step will not find. "Never twice, sometimes never" is the trade, and it
is made per activity.

## What ADR 063 admitted, and what it did not

An operation is `AtMostOnce` when this file records **both** halves: a search of
the provider's own documentation that found no idempotency mechanism, *and* a
consequence of a repeat that is **not the same outcome as the first send** — a
second resource, a second delivery, a second charge, or a documented conflict
failure a retry cannot tell from success.

It stays `InventoryOnly` when any of these holds:

* **A repeat changes nothing**, and the provider says so. `values.clear`,
  `message.modify_labels`, `message.trash`, `company.create_or_update`,
  `worksheet.update_range`, and `aws_sqs.message.delete` are repeat-*safe*.
  Admitting them here would trade away a retry they do not need; what they want
  is the *other* decision — a `ProviderIdempotent` class whose evidence is a
  documented repeat-safe write on a method HTTP does not define it for.
* **No consequence is recorded at all.** Every partial update in this file —
  `record.update_patch`, `list.update`, `issue.update` (GitHub, Jira, Linear,
  Sentry), `page.update`, `contact.update`, `deal.update`, `file.update_metadata`,
  `message.update`, `file.move`, `file.rename`, `product.update`,
  `contact.update` (Intercom), `message.update` (Slack), `message.delete` (Slack)
  — plus `telegram.message.edit_text` and `telegram.message.delete`, where
  Telegram publishes nothing about repeating either. ADR 063's opt-in is an
  operator accepting a *named* consequence, and there is none to name.
* **The provider publishes a client-supplied deduplicating identifier this
  connector has not bound.** `microsoft_outlook.event.create`
  (`transactionId`) and `google_calendar.event.insert` (a caller-supplied
  `Event.id`). A key a connector could bind is not something a deployment steps
  past with an opt-in; the right move is to bind it, or to leave the operation
  where it is until the provider publishes the retention that would complete the
  evidence.
* **A recorded product judgement.** `openai.chat.complete` and
  `openai.embedding.create`. The evidence here is the strongest in the file
  (machine-checkable absence), and the reason they stay is the one this file
  recorded before the class existed: a generative call is billed and
  non-deterministic, so an ambiguous outcome leaves a charge nobody can look up
  and an answer nobody can reproduce. Admitting them is a decision about this
  provider, not a gap in the class.

---

# Batch B (spec 013) — the webhook-bearing connectors

The same two evidence kinds, for `github`, `shopify`, `telegram`, `calendly`,
and `sentry`. `typeform` contributes nothing new: spec 013 adds only its
trigger, and its one mutation was already admitted as `NaturalMethod`.

Every statement below was read from the provider's own documentation on
2026-08-10; the source URLs are in each connector module's header comment.

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `github` | `issue.create` | `POST /repos/{owner}/{repo}/issues` | `AtMostOnce` |
| `github` | `issue.update` | `PATCH /repos/{owner}/{repo}/issues/{issue_number}` | `InventoryOnly` |
| `github` | `issue.comment_create` | `POST /repos/{owner}/{repo}/issues/{issue_number}/comments` | `AtMostOnce` |
| `github` | `workflow.dispatch` | `POST /repos/{owner}/{repo}/actions/workflows/{workflow_id}/dispatches` | `AtMostOnce` |
| `shopify` | `product.update` | `PUT /admin/api/{version}/products/{product_id}.json` | `InventoryOnly` |
| `shopify` | `order.create` | `POST /admin/api/{version}/orders.json` | `AtMostOnce` |
| `telegram` | `message.send` | `POST /bot{token}/sendMessage` | `AtMostOnce` |
| `telegram` | `message.edit_text` | `POST /bot{token}/editMessageText` | `InventoryOnly` |
| `telegram` | `message.delete` | `POST /bot{token}/deleteMessage` | `InventoryOnly` |
| `sentry` | `issue.update` | `PUT /api/0/organizations/{org}/issues/{issue_id}/` | `InventoryOnly` |

`calendly` contributes none: every operation this batch declares for it is a
`GET`.

Two operations in this batch *are* executable mutations, and they are recorded
here as the contrast: `github.file.put` and `shopify.product.delete`, both
`ProviderIdempotent::NaturalMethod`. Their evidence is in their modules.

### `github` — `issue.create` and `issue.comment_create`

* **Documented contract.** *Create an issue* documents the `accept`,
  `Authorization`, `X-GitHub-Api-Version` and `User-Agent` headers, the
  `owner`/`repo` path parameters, and a body whose **only** required field is
  `title`. *Create an issue comment* documents one required field, `body`.
* **Evidence kind.** Complete published contract, no key in it. GitHub's REST
  guide pages — getting started, authentication, best practices, troubleshooting,
  rate limits, pagination — document no `Idempotency-Key` header, no
  client-supplied request identifier, and no deduplication. The nearest
  occurrence anywhere in GitHub's documentation is the dependency-submission
  endpoints' *parsed-dependency* deduplication, which is not request idempotency.
* **What a repeat produces.** A second issue, or a second comment, with a new
  identifier.
* **What GitHub does publish.** Two replay-safety primitives, both real and
  neither applicable here: `X-GitHub-Delivery` for *inbound* dedupe, and the
  contents endpoint's required blob `sha` with its documented `409 Conflict`.

### `github` — `issue.update`

* **Documented contract.** *Update an issue* is a `PATCH` with **no** required
  body field at all.
* **Evidence kind.** Complete published contract, no key in it — **plus a method
  the gate does not admit.** Spec 010 §7 admits `NaturalMethod` for `PUT` and
  `DELETE` only, and the reason applies literally here: a `PATCH` body whose
  fields are absolute is repeat-safe and one whose intent is relative is not, and
  GitHub publishes nothing that distinguishes the two.

### `github` — `workflow.dispatch`

* **Documented contract.** `ref` required, `inputs` optional, and — at the
  pinned `2026-03-10` version — a `200` response carrying `workflow_run_id`,
  `run_url`, and `html_url`.
* **Evidence kind.** Complete published contract, no key in it, **and the
  provider's own response is the proof of the negative**: each call answers with
  a fresh `workflow_run_id`, so a repeat starts a second run.
* **What a repeat produces.** A second workflow run, with whatever that workflow
  does.

### `shopify` — `order.create`

* **Documented contract.** `POST /admin/api/{version}/orders.json`, body wrapped
  in `{"order": …}`, `201 Created`.
* **Evidence kind.** **A documented exclusion, which is stronger than an
  absence.** Shopify publishes an idempotency mechanism for the REST Admin API —
  a body field named `unique_token`, not a header — and publishes exactly what it
  covers: "POST requests that process credit card payments, create billing
  attempts for subscriptions, or capture revenue details accept idempotency
  keys." Order creation is none of the three, and the Order reference mentions
  idempotency nowhere.
* **What a repeat produces.** A second order, with a new `id` and a new
  `order_number`. On a trial or Partner development store, Shopify additionally
  documents a cap of "no more than 5 new orders per minute", so a retry storm is
  also a rate-limit failure.

### `shopify` — `product.update`

* **Documented contract.** `PUT /admin/api/{version}/products/{product_id}.json`,
  `200 OK`, returning the whole product.
* **Evidence kind.** **A `PUT` whose replace semantics the provider does not
  publish.** Shopify never states whether the endpoint merges or replaces. Its
  own examples send only the fields being changed ("Update a product's SEO title
  and description"), while an array is replaced by presence ("Update a product by
  clearing product images" sends `images: []`). A partial update is not a write
  to a fixed resource identity, so the `NaturalMethod` evidence is not there to
  cite even though the method is right.
* **What a repeat produces.** The same fields set to the same values — but
  Shopify does not publish whether a no-op `PUT` bumps `updated_at` or emits a
  second `products/update` webhook, and it explicitly warns that "`updated_at`
  changes with every update" in the webhook-debouncing context.

### `telegram` — `message.send`

* **Documented contract.** `sendMessage` with required `chat_id` and `text`, "On
  success, the sent Message is returned."
* **Evidence kind.** Complete published contract, no key in it. The strings
  `idempot` and `dedup` do not occur anywhere on `core.telegram.org/bots/api`,
  `/bots/webhooks`, `/bots/features`, or `/bots/faq`. The only "duplicate" in the
  Bot API page is `getUpdates` advice about the inbound `offset`.
* **What a repeat produces.** A second message with a new `message_id`.

### `telegram` — `message.edit_text` and `message.delete`

* **Documented contract.** `editMessageText` identifies its target with
  `chat_id` + `message_id` (or `inline_message_id`); `deleteMessage` takes
  `chat_id` and `message_id` and "Returns True on success", under a documented
  48-hour window and a list of permission limitations.
* **Evidence kind.** **A method the gate does not admit, and no repeat statement
  at all.** Both are `POST`s — the Bot API is not REST, and every method is a
  `GET` or a `POST` — so spec 010 §7's `NaturalMethod` does not reach them.
  Telegram publishes nothing about repeating either. The one adjacent statement
  is about a *different* method: `deleteMessages`, the plural, documents "If some
  of the specified messages can't be found, they are skipped."
* **A near-miss worth recording**, because a reviewer will find it: the
  `400 Bad Request: message is not modified` behaviour every Telegram client
  library handles is **not in Telegram's documentation**. It appears only in
  community error lists. The `messages.messagesNotModified` constructor that
  search engines surface is MTProto, a different API, and is a success there.
* **What a repeat produces.** For an edit whose text is identical, in practice a
  refusal rather than a second edit — which is repeat-*safe* but is not a
  provider statement, and is not what the gate admits.

### `sentry` — `issue.update`

* **Documented contract.** `PUT /api/0/organizations/{org}/issues/{issue_id}/`,
  described as "Update an individual issue's attributes. **Only the attributes
  submitted are modified.**", with a `200` whose published example carries no
  body.
* **Evidence kind.** **A `PUT` the provider documents as a partial update**, plus
  a complete absence of idempotency material: across Sentry's whole API
  reference the word "idempotent" occurs exactly once, on *Link a Repository to a
  Project* ("Idempotent: returns 200 if the link already exists, 201 if
  created"), which is a per-endpoint behavioural note on an endpoint this
  connector does not publish.
* **What a repeat produces.** For `{"status": "resolved"}`, the same resolved
  issue — but Sentry publishes no statement to that effect, and the same endpoint
  accepts `merge` and `discard`, whose repeat semantics are not the same. The
  class is refused on the contract, not on the body a particular caller sends.
* **A documentation defect worth recording**, because it contradicts the sentence
  above: the rendered *Update an Issue* page marks **every** body parameter
  `REQUIRED`, while the same page's own example sends `-d '{}'`. That is a
  docs-generation artifact, not a contract, and this connector does not encode it.

---

# Batch C (spec 014) — the Google Workspace connectors

`google_sheets`, `google_drive`, `google_gmail`, `google_calendar`. Ground truth
is each API's own machine-readable discovery document, read on 2026-08-10 —
`sheets:v4` revision `20260803`, `drive:v3` revision `20260805`, `gmail:v1`
revision `20260803`, `calendar:v3` revision `20260803` — plus each API's own
error-handling guide. The URLs are in each connector module's header comment.

**The evidence kind is machine-checkable absence for all four.** Google
publishes a complete machine-readable description of every method, parameter,
request body, and response schema of these APIs, and neither `idempot` nor
`dedup` occurs anywhere in any of the four documents. Google has the concept and
publishes it elsewhere — Cloud APIs document a `requestId` on several
long-running operations — and publishes it on none of the twenty-nine methods
these four connectors declare.

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `google_sheets` | `values.append` | `POST /v4/spreadsheets/{id}/values/{range}:append` | `AtMostOnce` |
| `google_sheets` | `values.clear` | `POST /v4/spreadsheets/{id}/values/{range}:clear` | `InventoryOnly` |
| `google_sheets` | `spreadsheet.create` | `POST /v4/spreadsheets` | `AtMostOnce` |
| `google_drive` | `file.update_metadata` | `PATCH /drive/v3/files/{fileId}` | `InventoryOnly` |
| `google_drive` | `file.copy` | `POST /drive/v3/files/{fileId}/copy` | `AtMostOnce` |
| `google_drive` | `folder.create` | `POST /drive/v3/files` | `AtMostOnce` |
| `google_drive` | `permission.create` | `POST /drive/v3/files/{fileId}/permissions` | `AtMostOnce` |
| `google_gmail` | `message.send` | `POST /gmail/v1/users/me/messages/send` | `AtMostOnce` |
| `google_gmail` | `message.modify_labels` | `POST /gmail/v1/users/me/messages/{id}/modify` | `InventoryOnly` |
| `google_gmail` | `message.trash` | `POST /gmail/v1/users/me/messages/{id}/trash` | `InventoryOnly` |
| `google_gmail` | `label.create` | `POST /gmail/v1/users/me/labels` | `AtMostOnce` |
| `google_calendar` | `event.insert` | `POST /calendar/v3/calendars/{calendarId}/events` | `InventoryOnly` |

Four operations in this batch *are* executable mutations, all
`ProviderIdempotent::NaturalMethod`, and they are recorded here as the contrast:
`google_sheets.values.update` (`PUT` on a fixed range), `google_drive.file.delete`,
`google_gmail.label.delete`, `google_calendar.event.update` (`PUT` on a fixed
event id) and `google_calendar.event.delete`. Their evidence is in their modules.

## The three that are idempotent in effect and still inventory-only

Spec 014 §2 names this exactly, and it is the batch's most interesting result:
three operations are repeat-safe *in what they do to the provider* and are still
not executable, because spec 010 §7 admits `NaturalMethod` for `PUT` and
`DELETE` only and Google publishes all three as `POST`s. ADR 063 deliberately
leaves all three here: at-most-once trades a retry away, and these three do not
need that trade — a repeat of them changes nothing. What they want is a decision
about whether the effect gate should admit a documented repeat-safe `POST` at
all, which is a different ADR and a stronger contract than this one.

### `google_sheets` — `values.clear`

* **Documented contract.** "Clears values from a spreadsheet. The caller must
  specify the spreadsheet ID and range. Only values are cleared -- all other
  properties of the cell (such as formatting, data validation, etc..) are kept."
  The request body, `ClearValuesRequest`, has no fields at all.
* **What a repeat produces.** Nothing. The range is already empty, the response
  reports the same `clearedRange`, and no cell changes.
* **Why it is inventory-only anyway.** The method. `POST
  .../values/{range}:clear` is a Google "custom method" — a verb after a colon —
  and the gate reads methods, not verbs. ADR 063 does not reach it either: there
  is no second effect for an operator to accept.

### `google_gmail` — `message.modify_labels`

* **Documented contract.** "Modifies the labels and the Classification Label
  values on the specified message", with `addLabelIds` and `removeLabelIds`
  arrays. Label membership is a set, not a counter.
* **What a repeat produces.** The same labels. Adding `STARRED` twice leaves one
  `STARRED`.
* **Why it is inventory-only anyway.** The method, and one thing worth naming:
  the operation is repeat-safe *for a body of absolute label sets*, which is the
  only body it accepts. That is a stronger position than the `PATCH` cases in
  Batch A, where a relative body was expressible.

### `google_gmail` — `message.trash`

* **Documented contract.** "Moves the specified message to the trash." No body.
* **What a repeat produces.** A message that is still in the trash, and a `200`
  carrying the same message resource.
* **Why it is inventory-only anyway.** The method. The sibling `messages.delete`
  *is* a `DELETE` and would be admissible; it permanently deletes rather than
  trashing, and is not in this batch's operation set.

## The rest

### `google_sheets` — `values.append`

* **Documented contract.** "Appends values to a spreadsheet. The input range is
  used to search for existing data and find a \"table\" within that range. Values
  will be appended to the next row of the table".
* **What a repeat produces.** A second copy of the same rows, below the first.
  This is an append by construction; nothing in the request identifies the rows
  it wrote.

### `google_sheets` — `spreadsheet.create`

* **Documented contract.** "Creates a spreadsheet, returning the newly created
  spreadsheet."
* **What a repeat produces.** A second spreadsheet with a new `spreadsheetId`
  and a new `spreadsheetUrl`.

### `google_drive` — `file.update_metadata`

* **Documented contract.** "Updates a file's metadata, content, or both. When
  calling this method, only populate fields in the request that you want to
  modify. … This method supports patch semantics."
* **Evidence kind.** Machine-checkable absence, **plus a method the gate does
  not admit.** The `PATCH` reasoning of Batch A applies literally: an absolute
  body is repeat-safe and a relative one is not, and Drive publishes nothing
  that tells them apart.

### `google_drive` — `file.copy` and `folder.create`

* **Documented contract.** "Creates a copy of a file and applies any requested
  updates with patch semantics." / "Creates a file." A folder is a file whose
  MIME type is `application/vnd.google-apps.folder`.
* **What a repeat produces.** A second file, or a second folder with the same
  name in the same parent — Drive does not merge folders by name, which is why
  `folder.create` is a create rather than an upsert.

### `google_drive` — `permission.create`

* **Documented contract.** "Creates a permission for a file or shared drive.
  **Warning:** Concurrent permissions operations on the same file aren't
  supported; only the last update is applied."
* **Evidence kind.** Machine-checkable absence, **plus a published warning that
  points the other way.** This is the one operation in the batch whose provider
  documentation states that concurrent writes are unsupported, which is the
  opposite of the property a retry after an ambiguous worker loss needs.
* **What a repeat produces.** A second permission resource with a new id, or —
  under concurrency — an outcome Google declines to define.

### `google_gmail` — `message.send`

* **Documented contract.** "Sends the specified message to the recipients in the
  `To`, `Cc`, and `Bcc` headers", with the message as base64url-encoded RFC 2822
  in `raw`.
* **What a repeat produces.** A second delivered email with a new message id.
  This is the operation in Batch C with the most visible external consequence of
  a duplicate, and the one closest in kind to Batch A's `mail.send`.

### `google_gmail` — `label.create`

* **Documented contract.** "Creates a label."
* **What a repeat produces.** Not a second identical label: Gmail answers a
  duplicate name with `409`. That is still not idempotency — the first call
  returns a label and the second returns a failure, which are different
  outcomes — so a retry after an ambiguous worker loss cannot tell "I created
  it" from "someone else did".

### `google_calendar` — `event.insert`

* **Documented contract.** "Creates an event."
* **A near-miss worth recording**, because a reviewer will find it: Calendar
  lets a caller supply its own `Event.id` on insert, and answers a duplicate
  with `409`. That is the closest thing to a client-supplied request identifier
  in this batch and it is not an idempotency key, for the same reason as
  `label.create`: a provider that deduplicates replays the *first* response, and
  Calendar returns a different one. Admitting it would also require this
  connector to derive the id from the durable activity's stable key and publish
  the retention Google does not document — the exact incompleteness
  `ExplicitKeyEvidence` refuses.
* **What a repeat produces.** A second event with a new id, and a second
  notification to every attendee.

---

# Batch E (spec 016) — the product SaaS connectors

The same two evidence kinds, for `slack`, `linear`, `notion`, `intercom`,
`hubspot`, and `jira`. Four of the six publish a machine-readable API
description, so four of these entries are of the stronger kind.

Every statement below was read from the provider's own documentation on
2026-08-10; the source URLs are in each connector module's header comment.

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `slack` | `message.post` | `POST /api/chat.postMessage` | `AtMostOnce` |
| `slack` | `message.update` | `POST /api/chat.update` | `InventoryOnly` |
| `slack` | `message.delete` | `POST /api/chat.delete` | `InventoryOnly` |
| `slack` | `reaction.add` | `POST /api/reactions.add` | `AtMostOnce` |
| `linear` | `issue.create` | `POST /graphql` (`issueCreate`) | `AtMostOnce` |
| `linear` | `issue.update` | `POST /graphql` (`issueUpdate`) | `InventoryOnly` |
| `linear` | `comment.create` | `POST /graphql` (`commentCreate`) | `AtMostOnce` |
| `notion` | `page.create` | `POST /v1/pages` | `AtMostOnce` |
| `notion` | `page.update` | `PATCH /v1/pages/{page_id}` | `InventoryOnly` |
| `notion` | `block.children_append` | `PATCH /v1/blocks/{block_id}/children` | `AtMostOnce` |
| `intercom` | `contact.create` | `POST /contacts` | `AtMostOnce` |
| `intercom` | `contact.update` | `PUT /contacts/{contact_id}` | `InventoryOnly` |
| `intercom` | `company.create_or_update` | `POST /companies` | `InventoryOnly` |
| `intercom` | `conversation.reply` | `POST /conversations/{id}/reply` | `AtMostOnce` |
| `hubspot` | `contact.create` | `POST /crm/v3/objects/contacts` | `AtMostOnce` |
| `hubspot` | `contact.update` | `PATCH /crm/v3/objects/contacts/{id}` | `InventoryOnly` |
| `hubspot` | `deal.create` | `POST /crm/v3/objects/deals` | `AtMostOnce` |
| `hubspot` | `deal.update` | `PATCH /crm/v3/objects/deals/{id}` | `InventoryOnly` |
| `jira` | `issue.create` | `POST /rest/api/3/issue` | `AtMostOnce` |
| `jira` | `issue.update` | `PUT /rest/api/3/issue/{issueIdOrKey}` | `InventoryOnly` |
| `jira` | `issue.transition` | `POST /rest/api/3/issue/{issueIdOrKey}/transitions` | `AtMostOnce` |
| `jira` | `comment.add` | `POST /rest/api/3/issue/{issueIdOrKey}/comment` | `AtMostOnce` |

**No operation in this batch reaches a `ProviderIdempotent` class.** That is the
batch's own finding rather than a gap in the work: none of the six providers
publishes an idempotency key for any operation in this set, and none of the
writes is a `PUT` or `DELETE` against a fixed resource identity whose repeat-safe
semantics the provider documents. Under ADR 063 the creates and the replies are
now `AtMostOnce` — each leaves a second thing behind — while every update in the
batch stays `InventoryOnly`, because a repeat of one sets the same values and
this file records no second effect to accept.

### `slack` — `message.post`, `message.update`, `message.delete`, `reaction.add`

* **Documented contract.** `chat.postMessage` documents `channel` and the message
  content as its required arguments, and everything else it accepts is
  presentation: `thread_ts`, `parse`, `unfurl_links`, `unfurl_media`,
  `link_names`, `mrkdwn`, `reply_broadcast`, `metadata`, `username`, `icon_*`.
  `chat.update` and `chat.delete` identify their target with `channel` + `ts`;
  `reactions.add` with `channel` + `timestamp` + `name`.
* **Evidence kind.** Complete published contract, no key in it — **plus a
  structural bar**. The Web API is not REST: every write is a `POST` to a method
  name, so spec 010 §7's `NaturalMethod` is unreachable for the whole surface,
  including the delete. The shared errors table Slack publishes on every method
  page documents no status or error string for a replayed or duplicate request.
* **What a repeat produces.** A second message with a new `ts` — and the
  provider's own response is the proof, since every successful `chat.postMessage`
  answers with a fresh one.
* **A near-miss worth recording**, because a reviewer will find it: Slack
  documents an `already_reacted` error on `reactions.add`, so a repeated reaction
  is in practice refused rather than doubled. That is an error string, not a
  published repeat-safety statement, and the method is still a `POST`. The
  connector records it as the operation's own inventory reason.

### `linear` — `issue.create`, `issue.update`, `comment.create`

* **Documented contract.** Linear's published GraphQL schema
  (`packages/sdk/src/schema.graphql`) declares `issueCreate(input:
  IssueCreateInput!)`, `issueUpdate(id: String!, input: IssueUpdateInput!)` — "A
  partial issue object to update the issue with" — and `commentCreate(input:
  CommentCreateInput!)`.
* **Evidence kind.** **A documented exclusion, which is stronger than an
  absence.** Linear publishes a client-supplied idempotency key and publishes
  exactly where it applies: `OAuthApplicationCreateInput.idempotencyKey`,
  "Optional client-supplied idempotency key. Reusing the same key with the same
  managing OAuth application returns the existing OAuth application instead of
  creating a duplicate." No such field exists on any of the three inputs above.
  The only other occurrences of "idempotent" in the whole schema are
  `favoriteDelete` and `viewPreferencesDelete`, neither of which this connector
  publishes.
* **Spec 016 §2 asked one question explicitly** — whether the API accepts a
  client-supplied mutation identifier the provider documents as deduplicating —
  and the answer is **no**. `IssueCreateInput.id` and `CommentCreateInput.id`
  exist and are documented as "The identifier in UUID v4 format. If none is
  provided, the backend will generate one." Linear publishes nothing about what a
  second create with the same `id` does and no retention window it would be held
  for, and ADR 042 admits `ExplicitKey` only on a binding **plus** a documented
  minimum retention with a clock safety margin strictly under it. The identifier
  is real; the evidence for the class is not.
* **What a repeat produces.** A second issue or comment with a new identifier —
  or, if the same `id` is supplied twice, an outcome Linear does not publish,
  which is the reason the class is refused rather than guessed.

### `notion` — `page.create`, `page.update`, `block.children_append`

* **Documented contract.** *Create a page* requires `parent` and takes
  `properties` and `children`; *Update page properties* is a `PATCH` taking
  `properties`, `icon`, `cover`, `archived`/`in_trash`; *Append block children* is
  a `PATCH` requiring `children`, with "Arrays of block children longer than 100
  will result in an error."
* **Evidence kind.** Complete published contract, no key in it — **and the
  provider says so itself**. Notion's *Request limits* page tells a client that a
  request that is "idempotent, such as GET or DELETE" may be retried on a `500`,
  `502`, `503` or `504`, while a non-idempotent request "should not be retried"
  on those "without its own idempotency protection". A provider that offered one
  would not tell a client to bring their own.
* **What a repeat produces.** A second page, or a second copy of the same
  children appended below the first — Notion documents the append as additive and
  states that "Existing blocks cannot be moved using this endpoint", so there is
  no version of it that is a no-op. The two `PATCH`es would be refused by the
  method in any case.

### `intercom` — `contact.create`, `contact.update`, `conversation.reply`

* **Documented contract.** The published 2.16 description declares `POST
  /contacts` (create), `PUT /contacts/{contact_id}` — "You can update an existing
  contact", with an all-optional request body — and `POST
  /conversations/{conversation_id}/reply`.
* **Evidence kind.** **Machine-checkable absence.** The string `idempot` occurs
  exactly once in the whole published description, on an endpoint this connector
  does not publish: the banner dismiss, "The request is idempotent: dismissing an
  already-dismissed banner succeeds". No request header, body property, or query
  parameter anywhere else in the document carries a client-supplied request
  identifier.
* **What a repeat produces.** A second contact, or a second reply visible to the
  customer in the same conversation. `contact.update` is a `PUT`, and it is
  refused for the reason Shopify's `product.update` is: the provider publishes no
  statement that the endpoint replaces the resource, and a `PUT` whose body is
  partial is not a write to a fixed resource identity.

### `intercom` — `company.create_or_update`

This is the batch's most interesting entry, because the provider **does** publish
repeat-safety and the gate still refuses it.

* **Documented contract.** "Companies are looked up via `company_id` in a `POST`
  request, if not found via `company_id`, the new company will be created, if
  found, that company will be updated." That is a genuine upsert on a
  caller-chosen key, and Intercom pins the key down further: "You can set a unique
  `company_id` value when creating a company. However, it is not possible to
  update `company_id`."
* **Evidence kind.** **A documented upsert on a method the gate does not admit.**
  Spec 010 §7 admits `NaturalMethod` for `PUT` and `DELETE` only, and this is a
  `POST`. Spec 016 §2 proposed the operation as `NM` and asked for the semantics
  to be verified; they verify, and the class still does not follow.
* **What a repeat produces.** *Nothing new* — the same company, updated twice.
  This is the same shape as `aws_sqs.message.delete`
  ([[046-an-effect-class-can-depend-on-deploy-time-configuration]]): a provider
  documenting repeat-safety that the two admitted classes cannot express. It is
  recorded here as evidence for the at-most-once opt-in decision, and as the
  clearest example in the programme so far of an operation a third class — or a
  widened `NaturalMethod` — would unlock on real published evidence.

### `hubspot` — `contact.create`, `contact.update`, `deal.create`, `deal.update`

* **Documented contract.** The published v3 descriptions declare `POST
  /crm/v3/objects/{type}` with a body of `properties` and `associations`, and
  `PATCH /crm/v3/objects/{type}/{objectId}` with a body of `properties`.
* **Evidence kind.** **Machine-checkable absence.** The string `idempot` does not
  occur anywhere in the published Contacts, Companies, Deals, or Tickets v3
  descriptions: no request header parameter, no body property, no response field.
* **What a repeat produces.** A second contact or deal with a new object id.
  HubSpot deduplicates contacts by email in some flows, but that is a portal
  setting rather than a published request contract, and the API answers a create
  of an existing email with a `409` rather than with the existing record.
* **Nearest supported alternative.** `POST /crm/v3/objects/{type}/batch/upsert`,
  keyed on a unique property. That is a genuine upsert on business data the caller
  chooses, exactly as Airtable's `performUpsert` is — not an idempotency key — and
  it is out of this batch's operation set.

### `jira` — `issue.create`, `issue.update`, `issue.transition`, `comment.add`

* **Documented contract.** Atlassian's published platform OpenAPI declares `POST
  /rest/api/3/issue` (`201`, answering with `id`, `key`, `self`), `PUT
  /rest/api/3/issue/{issueIdOrKey}` — "Edits an issue", whose "edits to the
  issue's fields are defined using `update` and `fields`" — `POST
  /rest/api/3/issue/{issueIdOrKey}/transitions` (`204`), and `POST
  /rest/api/3/issue/{issueIdOrKey}/comment` (`201`).
* **Evidence kind.** **Machine-checkable absence.** The string `idempot` does not
  occur once in the whole published description — no request header, no body
  property, no response field.
* **What a repeat produces.** A second issue with a new key, a second comment
  with a new id, or a transition applied twice (which fails on the second attempt
  when the workflow no longer offers it — a refusal Atlassian does not publish as
  a contract).
* **`issue.update` is the `PUT` that does not qualify.** The method is right and
  the evidence is the opposite of what `NaturalMethod` needs: Atlassian documents
  a partial *edit*, not a replacement, so two identical sends are only safe for
  bodies whose fields are absolute — and the same endpoint accepts `update`
  operations (`add`, `remove`, `set`) whose intent is explicitly relative.

## What Batch E added to the at-most-once question, and how it was answered

Batch A's entry in this file set out what a Process-level at-most-once opt-in
would have to promise. Batch E added one observation to it, from
`intercom.company.create_or_update`: the class the programme keeps needing is not
always "never twice, sometimes never". Sometimes the provider *is* repeat-safe
and says so, and the only thing standing between the operation and an executable
class is that spec 010 §7 spells its evidence in terms of `PUT` and `DELETE`.

Two decisions were therefore available rather than one, and they are not the
same. **ADR 063 made the first and left the second open:**

1. **A third executable class with an at-most-once opt-in** — decided. It
   weakens the guarantee, so it is admitted only per activity and only with a
   destination for an unknown outcome. `slack.message.post` and
   `hubspot.contact.create` reach it.
2. **A `ProviderIdempotent` class whose evidence is a documented upsert on a
   caller-chosen key, whatever the method** — still open. It weakens nothing —
   the provider still absorbs the duplicate — and
   `intercom.company.create_or_update`, `aws_sqs.message.delete`,
   `google_sheets.values.clear`, `google_gmail.message.modify_labels` and
   `message.trash`, and Airtable's and HubSpot's batch upserts would all reach
   it. Its risk is not the guarantee but the *evidence*: "documented upsert" is
   a prose reading, where `PUT`-and-`DELETE` is a mechanical one, so the class
   would rest on a reviewer rather than on a type. Every operation in that list
   is deliberately **not** at-most-once: giving a repeat-safe write a class that
   forbids the retry would be the wrong trade.

---

# Batch D (spec 015) — the Microsoft 365 connectors

`microsoft_outlook`, `microsoft_teams`, `microsoft_excel`,
`microsoft_onedrive`. Ground truth is Microsoft's own v1.0 reference on
`learn.microsoft.com`, read on 2026-08-10 — one reference page per operation,
plus *Microsoft Graph error responses and resource types*, *Microsoft Graph
throttling guidance*, *Paging Microsoft Graph data in your app*, and the
`driveItem`, `workbookRange`, `workbookTable`, `chatMessage`, `channel`,
`message` and `event` resource pages. The URLs are in each connector module's
header comment.

**The evidence kind is a complete published contract with no key in it.** Every
Graph reference page enumerates its operation's request contract — the HTTP
method, the path, the permission table, the *complete* request-headers table,
and every request-body property — and in none of the forty operations these four
connectors declare does an idempotency key, a client-supplied request
identifier, or a deduplication behaviour appear, with the two documented
near-misses recorded below. This is the weaker of the two evidence kinds in this
file: Microsoft publishes a machine-readable description of Graph, and these
statements were not checked against it.

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `microsoft_outlook` | `message.send` | `POST /v1.0/me/sendMail` | `AtMostOnce` |
| `microsoft_outlook` | `message.move` | `POST /v1.0/me/messages/{id}/move` | `AtMostOnce` |
| `microsoft_outlook` | `message.update` | `PATCH /v1.0/me/messages/{id}` | `InventoryOnly` |
| `microsoft_outlook` | `draft.create` | `POST /v1.0/me/messages` | `AtMostOnce` |
| `microsoft_outlook` | `draft.send` | `POST /v1.0/me/messages/{id}/send` | `AtMostOnce` |
| `microsoft_outlook` | `event.create` | `POST /v1.0/me/events` | `InventoryOnly` |
| `microsoft_outlook` | `event.update` | `PATCH /v1.0/me/events/{id}` | `AtMostOnce` |
| `microsoft_teams` | `channel.create` | `POST /v1.0/teams/{id}/channels` | `AtMostOnce` |
| `microsoft_teams` | `channel_message.create` | `POST /v1.0/teams/{id}/channels/{id}/messages` | `AtMostOnce` |
| `microsoft_teams` | `chat_message.create` | `POST /v1.0/chats/{id}/messages` | `AtMostOnce` |
| `microsoft_excel` | `worksheet.update_range` | `PATCH …/workbook/worksheets/{id}/range(address='…')` | `InventoryOnly` |
| `microsoft_excel` | `table.add_row` | `POST …/workbook/tables/{id}/rows/add` | `AtMostOnce` |
| `microsoft_onedrive` | `file.copy` | `POST /v1.0/me/drive/items/{id}/copy` | `AtMostOnce` |
| `microsoft_onedrive` | `file.move` | `PATCH /v1.0/me/drive/items/{id}` | `InventoryOnly` |
| `microsoft_onedrive` | `file.rename` | `PATCH /v1.0/me/drive/items/{id}` | `InventoryOnly` |
| `microsoft_onedrive` | `folder.create` | `POST /v1.0/me/drive/items/{id}/children` | `AtMostOnce` |

Three operations in this batch *are* executable mutations, all
`ProviderIdempotent::NaturalMethod`, and they are recorded here as the contrast:
`microsoft_outlook.message.delete`, `microsoft_outlook.event.delete`, and
`microsoft_onedrive.file.delete`. Each is a `DELETE` against a fixed resource
identity — `/me/messages/{id}`, `/me/events/{id}`, `/me/drive/items/{item-id}` —
which Microsoft documents as removing *that* item and answering "`204 No
Content` … It doesn't return anything in the response body". **Microsoft
publishes no sentence about the repeat itself**, for any of the three; the
evidence admitted is the fixed identity of the documented request, and a repeat
is answered either with the same `204` or with `itemNotFound`, which these
connectors classify `permanent`. `microsoft_teams` publishes no executable
mutation at all.

## The operation spec 015 §2 asked to verify

### `microsoft_excel` — `worksheet.update_range`

Spec 015 §2 classified it `NM` with an explicit instruction: "`PATCH` on a fixed
range is documented as a full-range replacement — verify; if the documentation
does not state repeat-safety, drop to `IO`." The verification fails twice.

* **By method.** Microsoft publishes it as a `PATCH`, and spec 010 §7 admits
  `NaturalMethod` for `PUT` and `DELETE` only.
* **By evidence.** The reference page's entire description is one sentence:
  "Update the properties of range object." Its request-body rule is the
  partial-merge one — "supply the values for relevant fields that should be
  updated. Existing properties that aren't included in the request body
  maintains their previous values or be recalculated based on changes to other
  property values" — which is the opposite of a documented full replacement. The
  strings `idempot`, `dedup`, and `retry-safe` do not appear on the page, on
  `workbookRange`, on the Excel overview, on *Best practices for working with
  the Microsoft Graph Excel API*, or on the Excel error-handling page, and the
  endpoint documents no `If-Match`.
* **And the provider says the state is unknowable after a failure.** *Best
  practices*: "when you receive a failure response, there is no way to confirm
  the status of other pending requests, which makes it difficult to determine or
  to recover the state of the workbook." That is precisely the ambiguous-loss
  situation a durable retry has to survive.

A repeated identical `PATCH` of a fixed `values` grid at a fixed `address` *is*
derivably the same write twice — every documented input mode (a literal value,
`null` meaning "ignored", `""` meaning "cleared", and the single-value CTRL+Enter
broadcast) is a pure function of the request body and the address. ADR 042
admits evidence rather than derivations, so the class is `InventoryOnly`, and
ADR 063 leaves it there: a write whose repeat is the same write does not want a
class that forbids the repeat. It is a candidate for the repeat-safe evidence
decision, not for this one.

## The two near-misses

### `microsoft_outlook` — `event.create` and `transactionId`

This is the closest any operation in the programme has come to
`ProviderIdempotent::ExplicitKey` without reaching it. Microsoft publishes a
client-supplied key **with a documented deduplicating purpose**: the `event`
resource defines `transactionId` as "A custom identifier specified by a client
app for the server to avoid redundant POST operations in case of client retries
to create the same event", and the *Create event* page's own example "sets the
**transactionId** property to reduce unnecessary retries on the server".

It is still not the class, because `ExplicitKeyEvidence::documented` requires
four things and Microsoft publishes three of them: the binding (a body property),
the uniqueness scope (the calendar), and the fact that the server deduplicates —
but **no retention window**. Neither the event resource page nor the create page
states how long the server remembers a `transactionId`, and a durable activity's
send horizon has to fit inside that window with a clock safety margin strictly
smaller than it. A key whose retention is unknown cannot bound a retry, so the
operation is inventory-only and the key is declared as what Microsoft documents
it to be: part of the request. ADR 063 keeps it there deliberately: a
client-supplied deduplicating identifier a connector could bind is not something
a deployment steps past with an at-most-once opt-in.

### `microsoft_teams` — `chat_message.create` and `createdDateTime`

*Send chatMessage in a chat* documents a uniqueness constraint: "The
**createdDateTime** must be unique down to the millisecond within the target
chat. If a message with the same **createdDateTime** exists, the request fails
with `409 Conflict`. Adjust the **createdDateTime** and retry." Two reasons it is
not an idempotency key. It is documented **only** for the import/migration path,
which requires the `Teamwork.Migrate.All` application permission and a chat in
migration mode — neither of which this delegated-permission batch has. And a
`409` is a *different* outcome from the first call rather than the same one,
which is what `ProviderIdempotent` means.

## The rest, briefly

* **`message.send`, `draft.send`** — `202 Accepted` with an empty body, and
  Microsoft's own note that "`202 Accepted` … doesn't indicate that the request
  processing has completed". Mail a second accepted send emits cannot be
  recalled, and no key binds a retry to the first attempt.
* **`message.move`** — "This creates a new copy of the message in the
  destination folder and removes the original message", answering `201 Created`
  with a message whose id is a new one. A repeat names an id the mailbox no
  longer holds.
* **`message.update`, `event.update`, `file.move`, `file.rename`** — `PATCH`es
  with published partial-merge semantics. `event.update` is worse than the
  method alone suggests: Microsoft documents that an update "sends a meeting
  update" to attendees, so a repeat is observable outside the resource even
  where the resource would be unchanged.
* **`draft.create`, `channel.create`, `channel_message.create`,
  `chat_message.create`, `folder.create`** — each call creates a new resource
  with a new id. `folder.create` pins `@microsoft.graph.conflictBehavior: fail`,
  which Microsoft defines as "The entire operation fails when a conflict
  occurs", so a repeat that reaches the provider is answered
  `nameAlreadyExists`; that is still a different outcome from the first call.
  Microsoft publishes no default conflict behaviour for `POST /children` at all.
* **`table.add_row`** — "Adds rows to the end of the table", and the one place
  in the whole Excel surface where Microsoft publishes retry guidance: "This
  request might occasionally receive a 504 HTTP error. The appropriate response
  to this error is to repeat the request." That is guidance about a transport
  failure on an *append*, with nothing published about the duplicate row it can
  produce, and this connector does not read it as an idempotency contract.
* **`file.copy`** — the sharpest of the four OneDrive writes. Its `202 Accepted`
  is acceptance rather than result: "The response indicates whether the copy
  operation was accepted or rejected", and Microsoft's own Example 3 shows a
  copy that is accepted and then fails during processing with
  `nameAlreadyExists`. The progress handle is a monitor URL on the tenant's
  SharePoint host — another origin this engine does not follow — so a second
  call could neither be deduplicated by the provider nor told apart from the
  first by this engine.

---

# Batch F (spec 017) — AWS

The AWS modules are recorded in their own module headers rather than in a batch
section here, and ADR 063 reaches three of their operations.

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `aws_ses` | `email.send` | `POST /v2/email/outbound-emails` | `AtMostOnce` |
| `aws_ses` | `email.send_template` | `POST /v2/email/outbound-emails` | `AtMostOnce` |
| `aws_sqs` | `message.send` (standard queue only) | `POST /` (`SendMessage`) | `AtMostOnce` |
| `aws_sqs` | `message.delete` | `POST /` (`DeleteMessage`) | `InventoryOnly` |

* **`aws_ses.email.send` and `email.send_template`.** *Complete published
  contract, no key in it.* The API v2 `SendEmail` reference documents the whole
  request body and publishes no idempotency token, client token, or
  deduplication field; the `MessageId` it does publish is "generated when the
  message is accepted", which is server-issued and therefore not a key a retry
  could carry. **What a repeat produces:** a second delivered email with a new
  `MessageId`.
* **`aws_sqs.message.send` on a standard queue.** *A documented exclusion.*
  Amazon documents `MessageDeduplicationId` as applying "only to FIFO
  (first-in-first-out) queues" and documents standard queues as at-least-once
  delivery. On a FIFO queue the same operation is
  `ProviderIdempotent::ExplicitKey` on Amazon's documented five-minute window;
  the class is per instance
  ([[046-an-effect-class-can-depend-on-deploy-time-configuration]]).
  **What a repeat produces:** a second message on the queue.
* **`aws_sqs.message.delete` stays `InventoryOnly`.** Amazon documents it as
  safe to repeat — "If you use an old `ReceiptHandle`, the request will succeed,
  but the message might not be deleted" — and the AWS JSON protocol expresses it
  as a `POST`. It needs a class that *keeps* the retry, which ADR 063 is not.

---

# Batch J (spec 026) — payments and billing

This is the batch where the classification decides whether a duplicate moves
money twice, and it produced all four possible answers: a provider that
publishes complete idempotency evidence (`xero`), a provider that publishes none
at all (`paddle`), a provider that publishes a key without a window
(`mercado_pago`), and — recorded below rather than implemented — two providers
whose evidence is complete but whose connectors this slice could not land.

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `paddle` | `customer.create` | `POST /customers` | `AtMostOnce` |
| `paddle` | `transaction.create` | `POST /transactions` | `AtMostOnce` |
| `paddle` | `customer.update` | `PATCH /customers/{id}` | `InventoryOnly` |
| `paddle` | `adjustment.create` (the refund) | `POST /adjustments` | `InventoryOnly` |
| `mercado_pago` | `customer.create` | `POST /v1/customers` | `AtMostOnce` |
| `mercado_pago` | `refund.create` | `POST /v1/payments/{id}/refunds` | `InventoryOnly` |
| `mercado_pago` | `customer.update` | `PUT /v1/customers/{id}` | `InventoryOnly` |

`xero` contributes none: every one of its mutations is
`ProviderIdempotent::ExplicitKey` on the binding, scope, and retention Xero
publishes (see `xero.rs`).

## `paddle` — the whole connector, on a machine-checkable absence

* **Evidence kind.** *Machine-checkable absence.* Paddle publishes a
  documentation index at `developer.paddle.com/llms.txt` and a Markdown sibling
  of every reference page. The string `idempot` does not occur in the index, in
  any endpoint reference this connector declares, or in the shared "about" pages
  that enumerate the headers the API reads — `Authorization`, `Content-Type`,
  `Paddle-Version`, `Skip-Count`. The only request identifier Paddle publishes is
  `meta.request_id`, which Paddle generates and returns: "Every response includes
  a `meta.request_id`. Log it alongside the error and include it when you contact
  Paddle support."
* **`customer.create` — what a repeat produces.** A second customer with a new
  `ctm_` id. Paddle does not deduplicate on the email address: a create with an
  address that already exists answers `409 customer_already_exists` rather than
  returning the existing customer, so neither outcome equals the first send.
* **`transaction.create` — what a repeat produces.** A second transaction with a
  new `txn_` id for the same items, and — for an automatically-collected
  transaction Paddle completes — a second charge against the customer's payment
  method. This is the sharpest at-most-once consequence in the programme so far,
  and it is the reason the opt-in is per activity rather than per connector.
* **`customer.update` stays `InventoryOnly`.** A `PATCH` whose repeat sets the
  same fields to the same values is not the consequence ADR 063 exists to bound,
  which is the line already drawn for HubSpot's and SendGrid's updates.
* **`adjustment.create` stays `InventoryOnly`, and this one is a decision rather
  than a default.** It is the refund. Paddle publishes no key for it, so
  `ExplicitKey` is unavailable; ADR 063 *would* reach it on the absence above,
  and spec 026 §3 refuses that trade for a refund. Paddle's own contract is why:
  "Most refunds for live accounts are created with the status of
  `pending_approval` until reviewed by Paddle", so a second send that Donat
  cannot rule out leaves a second pending refund against the same transaction for
  a human to approve, while refusing the second send leaves a customer's refund in
  an outcome nobody can read. An operator may reasonably accept "this email might
  never be sent"; nobody should casually accept "this customer might be refunded
  twice, or not at all". The way to make it executable is a Paddle idempotency
  key, not a Process opt-in.

## `mercado_pago` — the near-miss, and the absence beside it

* **The near-miss (`refund.create`).** Mercado Pago publishes the **binding** and
  makes it mandatory — the *Create refund* reference lists `X-Idempotency-Key`
  under **Header** as `(string, required)`: "This feature allows you to safely
  retry requests without the risk of accidentally performing the same action more
  than once. This is useful for avoiding errors, such as creating two identical
  refunds, for example." Its idempotency guide publishes the **replay**: "If the
  payment has already been created, your information is returned without creating
  a new payment." It publishes **no retention** and **no uniqueness scope**
  anywhere. Spec 026 §2 requires all three, so the operation is `InventoryOnly`
  for exactly the reason `microsoft_outlook.event.create`'s `transactionId` is:
  a key the provider may already have forgotten is not a key. The at-most-once
  class is not available either — ADR 063 is admitted on evidence of an absence,
  and there is no absence here. **One sentence from Mercado Pago naming a window
  would make this executable.**
* **A duplicate rejection is not an idempotency contract.** The *Get payment* and
  *Search payments* references publish the error `400 | 2001 | Already posted the
  same request in the last minute.` That rejects a duplicate inside one minute
  rather than replaying the first response, and says nothing about a key: the
  same send a minute later succeeds and moves money again.
* **`customer.create` — the absence.** *Complete published contract, no key in
  it.* The customer references publish their whole request contract — `email`,
  `first_name`, `last_name`, `phone`, `identification`, `default_address`,
  `address` — with no header section at all, and Mercado Pago publishes where its
  one key applies: "it is mandatory to use the idempotency header
  (X-Idempotency-Key) in requests to the **Payments and Refunds API**", which the
  customer API is not. **What a repeat produces:** a second customer record with
  a new id.
* **`customer.update` stays `InventoryOnly`.** It is a `PUT`, and Mercado Pago
  documents it as "Renew the data of a customer. Indicate the customer ID and
  send the parameters with the information you want to update" — a partial update
  rather than a write of a whole resource — so spec 010 §7's `NaturalMethod`
  evidence is not there to cite.

## `paypal` — the recorded evidence, verified and corrected, and the connector built on it

`paypal` was recorded below as "complete `ExplicitKey` evidence, blocked on the
engine". The engine slice landed
([[072-a-minted-credential-is-spent-inside-one-attempt]]), the connector is
implemented, and the recorded evidence was re-read against PayPal's own
documentation rather than trusted. Two of the three facts held exactly; the
retention did not, and the correction is what
[[073-a-retention-is-read-from-the-reference-that-owns-the-operation]] is about.

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `paypal` | `order.create` | `POST /v2/checkout/orders` | `ProviderIdempotent::ExplicitKey` (6h) |
| `paypal` | `order.capture` | `POST /v2/checkout/orders/{id}/capture` | `ProviderIdempotent::ExplicitKey` (6h) |
| `paypal` | `subscription.create` | `POST /v1/billing/subscriptions` | `ProviderIdempotent::ExplicitKey` (72h) |
| `paypal` | `invoice.create` | `POST /v2/invoicing/invoices` | `AtMostOnce` |
| `paypal` | `refund.create` | `POST /v2/payments/captures/{id}/refund` | `InventoryOnly` |

* **The scope held, verbatim.** "The `PayPal-Request-Id` header value must be
  unique for both each request and an API call type. For example, authorize
  payment and capture authorized payment."
* **The replay status held, verbatim, and in two places.** *Create order*: "A
  successful response to an idempotent request returns the HTTP `200 OK` status
  code with a JSON response body that shows order details", beside its `201`.
  *Capture payment for order*: "A successful response to a non-idempotent
  request returns the HTTP `201 Created` status code… If a duplicate response is
  retried, returns the HTTP `200 OK` status code." Every keyed operation here
  declares both statuses as success, because a declaration that admitted only
  `201` would read a successful deduplication as a failure.
* **The retention did not hold as recorded.** It was recorded as two numbers for
  one key — six hours in Orders v2 and 45 days in the general guide — with the
  instruction to bind the shorter. There are in fact **four** answers, one per
  API: Orders v2 "The server stores keys for 6 hours. The API callers can
  request the times to up to 72 hours by speaking to their Account Manager";
  Billing Subscriptions v1 "The server stores keys for 72 hours"; Payments v2
  publishes the header ("A unique ID identifying the request header for
  idempotency purposes") and **no window**; Invoicing v2 does not publish the
  header at all. The general guide's 45 days is an example about *refund
  captured payment* — the one API whose own reference declines to give a number
  — on a page that itself says "See the API reference to verify the API supports
  this header". So the retention is read per API, and the instance's send
  horizon is bounded by the shortest of them (six hours less the clock safety
  margin).
* **`refund.create` is a near-miss, not a class.** PayPal publishes the binding
  and the replay for it — its own example "Demonstrates an idempotent refund
  request where the same `PayPal-Request-Id` is used, resulting in a `200 OK`
  response with the existing refund details" — and no retention in the Payments
  v2 reference. Spec 026 §2 needs all three, and spec 026 §3 refuses an
  at-most-once trade for a refund. **One sentence from PayPal naming a window in
  the Payments v2 reference would make this executable.** ADR 063's class is not
  available either: it is admitted on evidence of an *absence*, and there is no
  absence here.
* **`invoice.create` — the absence.** *Machine-checkable absence.* PayPal
  publishes its own OpenAPI description of Invoicing v2
  (`invoicing_v2.json`), which enumerates every parameter of every endpoint, and
  the string `PayPal-Request-Id` does not occur in it. **What a repeat
  produces:** a second draft invoice with a new `INV2-` id against the same
  recipient, which a `send` would then deliver as a duplicate bill.
* **No customer surface.** Spec 026 §3 asks for "the customer read, list, create
  and update"; PayPal publishes no customer API in this surface, so none is
  invented.
* **Webhooks are the gap that matters.** PayPal's own notification API
  (`notifications_webhooks_v1.json`) is where a payments deployment learns that a
  capture completed or a subscription lapsed, and it is out of scope for this
  batch (spec 026 §6). For a payments provider that is the most consequential
  omission in the batch.

## Recorded but not implemented in this slice

These two are the rest of spec 026 §1. Each is recorded here with what was
read, so a later slice starts from the evidence rather than from the search.

* **`paypal` — implemented; this entry is superseded.** It was recorded here as
  "complete `ExplicitKey` evidence, blocked on the engine", and both halves have
  since moved: the engine slice landed
  ([[072-a-minted-credential-is-spent-inside-one-attempt]]) and the connector is
  written. The evidence as recorded was *re-read* rather than trusted, and one
  of its three facts needed correcting — the retention is published per API, not
  per provider. See the `paypal` section above for the verified quotations and
  the resulting classes.
* **`chargebee` — a documented window, an undocumented namespace.** Chargebee
  publishes the **binding** ("To make an idempotent request in Chargebee, provide
  the `chargebee-idempotency-key` header with a unique value for each request")
  and the **retention** ("Chargebee has an idempotency window of **30 minutes**,
  during which time any requests with the same idempotency key and parameters
  will be considered as a replay of the original request"), together with a
  replay signal (`chargebee-idempotency-replayed: true`) and a mismatch rule
  ("we verify the incoming retry request signature by comparing the URI path,
  request body and headers with the original request"). What it never states is
  the **namespace** it deduplicates within — it says "the same idempotency key
  and parameters" and never names a site, an account, or an API key. Whether that
  matching rule *is* a uniqueness scope is the open judgement; it is recorded
  rather than decided here. The second cost is shape: Chargebee's v2 API takes
  form-encoded parameters with indexed repeated keys (`-d
  "billing_address[first_name]"="John"`), so its writes need a processor-assembled
  body like Stripe's rather than a JSON template.
* **`quickbooks` — not classified, because the documentation could not be read.**
  Intuit serves `developer.intuit.com/app/developer/qbo/docs/...` as a JavaScript
  shell to every non-browser client tried (plain fetch, a browser user agent, a
  crawler user agent, and the Wayback Machine's 2019, 2021, and 2024 snapshots):
  the rendered pages carry no `minorversion`, no `STARTPOSITION`, and no
  `requestid`. Spec 026 §0 and ADR 037 make the provider's own published
  documentation the ground truth, so nothing about this connector — not its base
  URL, not its `requestid` semantics, not its error shapes — was written from
  memory or from a third party. It is unstarted, and what would unblock it is a
  readable copy of Intuit's own reference.

# Batch G (spec 023) — the CRM and helpdesk connectors

`zendesk`, `salesforce`, `pipedrive`, `freshdesk`, `woocommerce`, and
`zoho_crm`. Every statement below was read from the provider's own
documentation on 2026-08-10; the source URLs are in each connector module's
header comment.

This batch is the first since Batch A to reach a `ProviderIdempotent` class, and
it is also the batch with the most operations that stay `InventoryOnly` for the
*opposite* reason to the usual one — three of its providers publish an upsert or
a create-or-update and document it as repeat-safe, which is a contract ADR 063's
at-most-once class is the wrong answer for.

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `zendesk` | `ticket.create` | `POST /api/v2/tickets` | `ProviderIdempotent::ExplicitKey` |
| `zendesk` | `comment.add` | `PUT /api/v2/tickets/{id}` (a `comment`) | `AtMostOnce` |
| `zendesk` | `user.create` | `POST /api/v2/users` | `AtMostOnce` |
| `zendesk` | `ticket.update` | `PUT /api/v2/tickets/{id}` | `InventoryOnly` |
| `zendesk` | `user.update` | `PUT /api/v2/users/{id}` | `InventoryOnly` |
| `zendesk` | `user.create_or_update` | `POST /api/v2/users/create_or_update` | `InventoryOnly` |
| `salesforce` | `record.create` | `POST /services/data/v67.0/sobjects/{sObject}` | `AtMostOnce` |
| `salesforce` | `record.update` | `PATCH /services/data/v67.0/sobjects/{sObject}/{id}` | `InventoryOnly` |
| `salesforce` | `record.upsert` | `PATCH .../sobjects/{sObject}/{field}/{value}` | `InventoryOnly` |
| `salesforce` | `record.delete` | `DELETE /services/data/v67.0/sobjects/{sObject}/{id}` | `InventoryOnly` |
| `pipedrive` | `deal.create` | `POST /api/v2/deals` | `AtMostOnce` |
| `pipedrive` | `person.create` | `POST /api/v2/persons` | `AtMostOnce` |
| `pipedrive` | `note.add` | `POST /v1/notes` | `AtMostOnce` |
| `pipedrive` | `deal.update` | `PATCH /api/v2/deals/{id}` | `InventoryOnly` |
| `pipedrive` | `person.update` | `PATCH /api/v2/persons/{id}` | `InventoryOnly` |
| `freshdesk` | `ticket.create` | `POST /api/v2/tickets` | `AtMostOnce` |
| `freshdesk` | `note.add` | `POST /api/v2/tickets/{id}/notes` | `AtMostOnce` |
| `freshdesk` | `reply.add` | `POST /api/v2/tickets/{id}/reply` | `AtMostOnce` |
| `freshdesk` | `contact.create` | `POST /api/v2/contacts` | `AtMostOnce` |
| `freshdesk` | `ticket.update` | `PUT /api/v2/tickets/{id}` | `InventoryOnly` |
| `freshdesk` | `contact.update` | `PUT /api/v2/contacts/{id}` | `InventoryOnly` |
| `woocommerce` | `order.create` | `POST /wp-json/wc/v3/orders` | `AtMostOnce` |
| `woocommerce` | `customer.create` | `POST /wp-json/wc/v3/customers` | `AtMostOnce` |
| `woocommerce` | `order_note.create` | `POST /wp-json/wc/v3/orders/{id}/notes` | `AtMostOnce` |
| `woocommerce` | `order.update` | `PUT /wp-json/wc/v3/orders/{id}` | `InventoryOnly` |
| `woocommerce` | `customer.update` | `PUT /wp-json/wc/v3/customers/{id}` | `InventoryOnly` |
| `zoho_crm` | `record.create` | `POST /crm/v8/{module}` | `AtMostOnce` |
| `zoho_crm` | `note.create` | `POST /crm/v8/Notes` | `AtMostOnce` |
| `zoho_crm` | `record.update` | `PUT /crm/v8/{module}/{id}` | `InventoryOnly` |
| `zoho_crm` | `record.upsert` | `POST /crm/v8/{module}/upsert` | `InventoryOnly` |

## The one published key, and the one thing it does not publish

### `zendesk` — `ticket.create`

Zendesk is the only provider in this batch that publishes an idempotency
mechanism at all, and it publishes it for exactly one operation.

* **Documented contract.** *Idempotency* (Ticketing API introduction): "The
  Ticketing API lets you specify an idempotency key that allows you to retry a
  ticket creation request without the risk of creating duplicate records", "To
  specify an idempotency key, provide a `Idempotency-Key: {unique_key}` header
  with your request", "If you repeat the same request using the same body and
  idempotency key, another ticket is not created. Instead, you'll get the same
  response as before that is cached under the idempotency key."
* **Retention.** "Keys expire after two hours. If a request with a duplicate key
  is sent two hours after the original request, the request will create a new
  ticket." The connector's clock safety margin is five minutes, strictly inside
  it, which is what `ExplicitKeyEvidence::documented` enforces.
* **What is *not* published: the uniqueness scope.** No sentence in Zendesk's
  documentation states whether a key is unique per account, per endpoint, or
  globally. The scope recorded on the evidence is the narrowest one Zendesk's
  published *behaviour* establishes — one account, because every request is
  authenticated to one subdomain, and one request body, because "If you create a
  request using the same idempotency key but with a different body, you'll
  receive the following error: `400 Bad Request: {"error":
  "IdempotentRequestError"}`". Spec 023 §3 asks for all three cited; two are
  quotations and the third is a reading, and it is recorded here rather than
  presented as a citation.
* **A second thing not published: the concurrent case.** Zendesk documents the
  replay of a *completed* request and says nothing about a duplicate key
  arriving while the first request is still in flight.
* **The send horizon.** Two hours is a short window for a durable retry policy.
  A deployment whose retry window outlives it still holds a key Zendesk has
  forgotten, which is a second ticket rather than a replay — the same shape as
  the SQS send horizon in
  [[046-an-effect-class-can-depend-on-deploy-time-configuration]]. It is
  recorded in the module header and is not enforced here, because a Process's
  retry window is Process metadata rather than connector configuration.

## The three operations a provider documents as repeat-safe

These are the batch's sharpest `InventoryOnly` entries, and they are all the
same finding: the provider publishes a genuine repeat-safe write, over a method
spec 010 §7's `NaturalMethod` does not admit. ADR 063's at-most-once class is
not the answer for them, because it *trades the retry away* — an operation that
is safe to send twice wants a class that keeps the retry, and that class still
does not exist. They are the population ADR 063's "what this supersedes" section
names as still waiting, and this batch adds three to it.

### `salesforce` — `record.upsert`

* **Documented contract.** "Based on whether the value of the external ID
  exists, the request either creates a record or updates an existing one", "If
  the external ID matches one existing record, then the existing record is
  updated", and Salesforce's own framing in the SOAP guide: "In most cases, we
  recommend that you use `upsert()` instead of `create()` to avoid creating
  unwanted duplicate records (idempotent)."
* **Why it is not executable.** The method is `PATCH`. Spec 010 §7 admits
  `NaturalMethod` for `PUT` and `DELETE` only, and the reason it does is that
  HTTP defines repeat-safety for those two — a class keyed on a provider
  sentence over an arbitrary method is the widening ADR 042 exists to refuse.
* **What a repeat produces.** The same one record, updated again, with `created`
  answering `false` and the status `200` where the first send answered `201`.
  That is the *opposite* of the consequence ADR 063 requires, which is why the
  at-most-once class is refused rather than granted.
* **One further caveat worth recording.** Salesforce publishes a permission
  condition on the guarantee: "If you're upserting a record for an object that
  has the External ID attribute selected but not the Unique attribute selected
  (a non-unique index), your client application must have the permission "View
  All Data" to execute this call", and "If the external ID matches multiple
  existing records, then a 300 error is returned". The repeat-safety holds on a
  unique external id and not otherwise.

### `zoho_crm` — `record.upsert`

* **Documented contract.** "The Upsert API allows you to insert a new record or
  update an existing one based on duplicate check field values", "The system
  checks for duplicate records using the values of the duplicate check fields.
  If a matching record exists, it gets updated. If no matching record is found,
  a new record is inserted", and the response distinguishes the two in an
  `action` field (`"insert"` / `"update"`) with `duplicate_field` naming the
  match.
* **Why it is not executable.** The method is `POST`.
* **What a repeat produces.** The same one record, updated, with
  `action: "update"`.

### `zendesk` — `user.create_or_update`

* **Documented contract.** "Creates a user if the user does not already exist,
  or updates an existing user identified by e-mail address or external ID", with
  the status distinguishing the two: "If the user already exists in Zendesk, a
  successful request returns a 200 OK status code. If the user does not exist in
  Zendesk and is created, the request returns a 201 Created status code."
* **Why it is not executable.** The method is `POST` — the same structural bar
  `intercom.company.create_or_update` hit in Batch E.
* **What a repeat produces.** The same one user, updated, answering `200`.

## `salesforce` — `record.delete`, and a silence that is evidence

`DELETE /services/data/vXX.X/sobjects/{sObject}/{id}` is a `DELETE` against a
fixed resource identity, which is exactly the shape `NaturalMethod` is for. It
is still `InventoryOnly`, because the evidence that class needs is the
*provider's own repeat statement* and Salesforce publishes none: its reference
publishes "Example request body — None needed" and "Example response body — None
returned", and nothing about a second send.

What makes the silence evidence rather than an omission is that Salesforce
publishes exactly that statement where it holds. Its Big Objects reference says
"Repeating a successful `deleteByExample()` operation results in success, even
if the data has already been deleted." A provider that documents repeat-safety
when it means it, and does not document it here, has not said this is repeat
safe. The nearest published facts — the generic `404` row and the
`ENTITY_IS_DELETED` error code, which Salesforce never binds to an HTTP status —
describe a *failure* on the second send rather than the same one absent record.

Compare `shopify.product.delete`, which is admitted: Shopify documents "Deletes
a product." with a `200 OK` and the body `{}`, which is a statement about what
the call does to one fixed identity.

## The rest, briefly

Each of these is *complete published contract, no key in it*, and the search
recorded on each operation names the documentation it covered.

* **`pipedrive`** is the batch's one *machine-checkable* absence: `idempot` does
  not occur in either published OpenAPI document — 1.78 MB of v1 and 1.02 MB of
  v2 — nor in any core-concepts page. `deal.create`, `person.create`, and
  `note.add` each leave a second record with a new id. The two updates are
  `PATCH` by Pipedrive's own migration ("V1 endpoints, which were using HTTP PUT
  method have been switched to use HTTP PATCH method in v2 for compliance with
  REST best practices"), and Pipedrive publishes nothing about repeating one.
* **`freshdesk`** publishes no machine-readable description at all — no OpenAPI,
  no Postman collection; every artifact that turns up in a search is
  third-party — so the evidence is its own 3.2 MB reference, in which `idempot`
  does not occur. `contact.create` is the interesting one: a repeat leaves a
  second contact *unless* the payload carries a value on a field Freshdesk
  enforces as unique, where it is refused with `409` and the published code
  `duplicate_value`. Both outcomes differ from the first send, which is what ADR
  063 asks for; the module records both.
* **`woocommerce`** publishes no idempotency key, and its two nearest
  mechanisms are recorded as near-misses because a reviewer will find them:
  `cart_hash` is "MD5 hash of cart items to ensure orders are not modified.
  <read-only>", which is server-computed, and the OAuth 1.0a `oauth_nonce`
  protects a signature against replay on the plain-HTTP path rather than an
  application-level create. The updates publish nothing beyond "This API lets
  you make changes to an order."
* **`zoho_crm`**'s `record.create` has a second published outcome worth
  recording: "Duplicates are checked for every insert record API call based on
  unique fields", and a collision answers `DUPLICATE_DATA` on `HTTP 400` whose
  "details" carries "the API name and ID of the existing record with the same
  value". So a repeat leaves either a second record or a refusal that names the
  first one — neither of which is the first send's answer.
* **`salesforce`**'s `record.create` near-miss is the sharpest in the batch and
  is the reason its evidence is a *documented exclusion* rather than an absence:
  Salesforce publishes a real `Idempotency-Key`, in UUID v4 format, with a
  30-day retention — for the **User Interface API**'s `/ui-api/records`
  resources, off by default ("Idempotent record writes aren't enabled in your
  org. Contact Salesforce to enable this feature"), and for none of the
  `/sobjects/` resources this connector calls. It also publishes a trap worth
  recording for whoever binds it later: "An idempotent request supports a
  response size up to 9 MB. If your request results in a larger response size,
  idempotency isn't honored, but the request doesn't fail."
* **`zendesk`**'s `comment.add` is a write with no endpoint of its own: "The
  Tickets Comments API has no endpoint to create comments. Ticket comments are
  created by including a comment object in the ticket object when creating or
  updating the ticket." A repeat appends a second comment, and Zendesk's own
  ceiling makes the accumulation visible — "up to 5000 comments in total ...
  Once this limit is reached, any additional attempts to add comments results in
  a 422 error."
* **`zendesk`**'s `user.update` is `InventoryOnly` with one field deliberately
  left out of the declaration. Zendesk publishes that a repeat carrying `email`
  is *not* harmless — "On update, a secondary email is added" — so a declaration
  that sent it would accumulate an identity per attempt. Leaving it out is what
  makes the operation's repeat consequence empty, and the module says so.

# Batch H (spec 024) — the project-tracking and collaboration connectors

Read on 2026-08-10 against each provider's own published documentation; every
source URL is in the connector module's header comment. Five of the six publish
a machine-readable description, so five of the searches below are of the
stronger *machine-checkable absence* kind.

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `asana` | `task.create` | `POST /api/1.0/tasks` | `AtMostOnce` |
| `asana` | `story.create` | `POST /api/1.0/tasks/{gid}/stories` | `AtMostOnce` |
| `asana` | `task.update` | `PUT /api/1.0/tasks/{gid}` | `InventoryOnly` |
| `asana` | `task.delete` | `DELETE /api/1.0/tasks/{gid}` | `InventoryOnly` |
| `trello` | `card.create` | `POST /1/cards` | `AtMostOnce` |
| `trello` | `comment.add` | `POST /1/cards/{id}/actions/comments` | `AtMostOnce` |
| `trello` | `card.update` | `PUT /1/cards/{id}` | `InventoryOnly` |
| `trello` | `card.delete` | `DELETE /1/cards/{id}` | `InventoryOnly` |
| `clickup` | `task.create` | `POST /api/v2/list/{id}/task` | `AtMostOnce` |
| `clickup` | `comment.create` | `POST /api/v2/task/{id}/comment` | `AtMostOnce` |
| `clickup` | `task.update` | `PUT /api/v2/task/{id}` | `InventoryOnly` |
| `clickup` | `task.delete` | `DELETE /api/v2/task/{id}` | `InventoryOnly` |
| `monday` | `item.create` | `create_item` (`POST /v2`) | `AtMostOnce` |
| `monday` | `update.create` | `create_update` (`POST /v2`) | `AtMostOnce` |
| `monday` | `item.update` | `change_multiple_column_values` (`POST /v2`) | `InventoryOnly` |
| `monday` | `item.delete` | `delete_item` (`POST /v2`) | `InventoryOnly` |
| `todoist` | `task.create` | `POST /api/v1/tasks` | `AtMostOnce` |
| `todoist` | `task.close` | `POST /api/v1/tasks/{id}/close` | `AtMostOnce` |
| `todoist` | `comment.create` | `POST /api/v1/comments` | `AtMostOnce` |
| `todoist` | `task.update` | `POST /api/v1/tasks/{id}` | `InventoryOnly` |
| `todoist` | `task.delete` | `DELETE /api/v1/tasks/{id}` | `InventoryOnly` |
| `basecamp` | `todo.create` | `POST /{account}/todolists/{id}/todos.json` | `AtMostOnce` |
| `basecamp` | `comment.create` | `POST /{account}/recordings/{id}/comments.json` | `AtMostOnce` |
| `basecamp` | `todo.complete` | `POST /{account}/todos/{id}/completion.json` | `InventoryOnly` |

`basecamp.todo.replace` (`PUT /{account}/todos/{id}.json`) and
`basecamp.todo.uncomplete` (`DELETE /{account}/todos/{id}/completion.json`) are
not in this table at all: they are `ProviderIdempotent::NaturalMethod`, on the
provider's own marks, and are the first hand-written operations in the programme
admitted on a machine-readable idempotency assertion.

## `monday` — a published key with an escape clause

monday is the only provider in this batch that publishes an application-level
idempotency mechanism for the endpoints this connector calls, and it is still
not `ExplicitKey`. The whole finding is recorded here because a reviewer reading
monday's documentation will reach the opposite conclusion in about thirty
seconds.

* **The binding.** "Send a unique `Idempotency-Key` header with any mutation
  request", with a worked `curl` example against `https://api.monday.com/v2`.
* **The retention.** "Cache duration — Cached responses expire after 30 minutes.
  After expiration, the same key will execute fresh."
* **The scope.** "Per-user budget — Each user+app combination has a memory budget
  for cached responses."
* **The behaviour.** "Retry with same key: Returns the cached response with an
  `Idempotency-Replayed: true` header — no duplicate side effect occurs", and a
  concurrent duplicate answers `409` with `IDEMPOTENCY_CONFLICT` and a
  `Retry-After`.
* **The escape clause, in the same table as the retention.** "If the budget is
  exceeded, new responses will execute but won't be cached for replay." A second
  row adds "Max response size — Responses larger than 1 MB are not cached."

`ExplicitKeyEvidence::documented` is built from a **minimum** retention with a
clock safety margin strictly under it. monday's 30 minutes is not a minimum: it
is what a deployment gets when an unquantified per-user budget has room, and
neither the connector nor the durable runtime can observe whether it did. A
class that told the activity worker "send it again, monday will absorb it" would
be a promise monday explicitly declines to make, and the failure it produces is a
silently duplicated item. The 1 MB clause is the only one this connector *can*
bound — every declared mutation response here is a handful of fields, well under
the SDK's own 1 MB ceiling — and it is recorded rather than relied on.

So the four monday mutations are classified on the evidence of an *inadequate*
mechanism rather than an absent one, and the ADR is
[[067-a-retention-with-an-escape-clause-is-not-a-minimum-retention]]. A future
`ProviderIdempotent` variant whose evidence is a best-effort cache — a class that
retries and accepts that the provider may not deduplicate — would be the honest
home for these four, and it does not exist.

## `todoist` — a published key on a different endpoint, with no window

Spec 024 §1 named Todoist as "the one provider in this batch likely to reach
`ExplicitKey`". It does not, for two independent reasons, and both are worth
recording because each alone would be enough.

* **It is on the Sync endpoint, not these.** "Clients should generate a unique
  string ID for each command and specify it in the `uuid` field. The Command
  UUID will be used for two purposes: 1. Command result mapping … 2. Command
  idempotency: Todoist will not execute a command that has same UUID as a
  previously executed command. This will allow clients to safely retry each
  command without accidentally performing the action twice." That is the
  `POST /api/v1/sync` command envelope. This connector calls the REST resources —
  `POST /api/v1/tasks`, `POST /api/v1/comments`, and the rest — and none of them
  takes a `uuid`, an idempotency header, or any other client-supplied request
  identifier.
* **No retention is published anywhere.** Not for the Sync `uuid`, not in the
  *Request limits* guide, not in the migration notes. The string `idempot`
  occurs **exactly once** in Todoist's whole 1.1 MB published OpenAPI
  description, in the sentence quoted above. ADR 042 admits `ExplicitKey` on a
  binding *plus* a documented uniqueness scope *plus* a documented retention with
  a clock safety margin under it; a mechanism with no window is exactly the
  near-miss the programme already recorded for Microsoft's `transactionId`, and
  spec 024 §2 said in advance that it would not qualify.

The interesting operation in this connector is not a create. `task.close` is
`AtMostOnce` on a consequence Todoist publishes outright: "Regular tasks are
marked complete and moved to history, along with their subtasks. Tasks with
recurring due dates will be scheduled to their next occurrence." A second close
of a *recurring* task advances the recurrence again and skips an occurrence
nobody completed — a different state from the first send's, which is the bar ADR
063 sets.

`task.delete` is the mirror image and stays `InventoryOnly`: Todoist publishes
what the second send does — "Returns `NOT_FOUND` when the task does not exist and
`FORBIDDEN` when the authenticated user cannot modify the task" — and a refusal
is neither the repeat statement `NaturalMethod` needs nor a consequence ADR 063
admits a send on. It is the `salesforce.record.delete` shape with the silence
filled in, and the answer is the same.

## `basecamp` — the provider that publishes which writes are repeat-safe

Basecamp's own published OpenAPI carries a vendor extension per operation:
`x-basecamp-idempotent: {"natural": true}` on 83 of its 250 operations, and
nothing on the other 167. That is a provider assertion of repeat-safety, made
machine-readably, per operation — the first in this programme — and this batch
takes it exactly as far as spec 010 §7 allows and no further.

* **`todo.replace` — `NaturalMethod`.** `PUT /{account}/todos/{id}.json`, marked
  `natural: true`, with the prose to match: "Replace a todo with a new complete
  representation. The request body is the todo's full writable state: any
  writable field omitted from the request is cleared server-side (empty/missing
  assignee_ids clears assignees, missing description clears it, and so on).
  content is required — a request without it is rejected." A `PUT` of a complete
  representation against a fixed identity is precisely the shape the class is
  for.
* **`todo.uncomplete` — `NaturalMethod`.** `DELETE
  /{account}/todos/{id}/completion.json`, marked `natural: true`, "Mark a todo as
  incomplete".
* **`todo.complete` — `InventoryOnly`, and it is this batch's sharpest entry.**
  `POST /{account}/todos/{id}/completion.json`, marked `natural: true`, "Mark a
  todo as complete". The provider says it is repeat-safe. The method is a `POST`,
  and spec 010 §7 admits `NaturalMethod` for `PUT` and `DELETE` only, because
  HTTP defines repeat-safety for those two and a class keyed on a provider
  sentence over an arbitrary method is the widening ADR 042 exists to refuse.
  ADR 063's at-most-once class is not the answer either: it *trades the retry
  away*, and an operation that is safe to send twice wants a class that keeps it.

  This connector adds one more to the population this file already records under
  that heading — `intercom.company.create_or_update`, `aws_sqs.message.delete`,
  the three Google operations that are idempotent in effect,
  `salesforce.record.upsert`, `zoho_crm.record.upsert`,
  `zendesk.user.create_or_update`, and `microsoft_excel`'s fixed-address
  `PATCH` — and every one of them is waiting for the same class ADR 063's "what
  this supersedes" section names as still open: a `ProviderIdempotent` variant
  whose evidence is a documented repeat-safe write on a method HTTP does not
  define repeat-safety for. Basecamp is the strongest case yet for landing it,
  because the evidence here is *machine-readable*, published per operation by the
  provider, rather than a sentence a reviewer had to read and weigh.

* **`todo.create` and `comment.create` — `AtMostOnce`.** Neither carries the
  mark, and nothing anywhere in Basecamp's published description is a
  client-supplied request identifier: every one of the 88 occurrences of
  `idempot` in it is the `x-basecamp-idempotent` extension itself. A repeat
  leaves a second to-do or a second comment, with a new id, and — where `notify`
  is true — a second notification to every assignee.

## The rest, briefly

Each of these is a *machine-checkable absence*, and the search recorded on each
operation names the description it covered.

* **`asana`** publishes a 3.0 MB OpenAPI description covering every endpoint,
  parameter, and schema, and `idempot` does not occur in it once — nor in the
  authentication, pagination, rate-limit, or errors guides. `task.create` leaves
  a second task with a new gid; `story.create` leaves a second comment and a
  second notification to every follower. `task.update` is a documented partial
  update — "Only the fields provided in the `data` block will be updated; any
  unspecified fields will remain unchanged" — so a repeat changes nothing, and
  `task.delete` publishes only what the *first* send does: "Deleted tasks go into
  the 'trash' of the user making the delete request. Tasks can be recovered from
  the trash within a period of 30 days."
* **`trello`** publishes a 262 KB OpenAPI description in which `idempot` does not
  occur. Its one near-miss is recorded because a reviewer will find it: Trello's
  *other* authorization form is OAuth 1.0a, whose `oauth_nonce` "is a random
  string, uniquely generated for each request". That is a signature replay guard
  on the transport rather than an application-level deduplication of a card
  create, and it is not a value this SDK can produce at all. Every parameter of
  `PUT /1/cards/{id}` is published as optional, which is a partial update, and
  `DELETE /1/cards/{id}` publishes "Delete a Card" and nothing about a second
  send.
* **`clickup`** publishes a 518 KB v2 OpenAPI description in which `idempot` does
  not occur, and no `ECODE` in its (open, incompletely published) error
  vocabulary names a duplicate. `comment.create` is the one worth naming: ClickUp
  publishes that "other assignees and watchers on the task are always notified
  regardless of this setting", so a second send is a second notification to every
  one of them whatever `notify_all` says. `task.update` is "Update a task by
  including one or more fields in the request body" — a partial update — and
  `task.delete` publishes "Delete a task from your Workspace" and nothing about a
  second one.
* **`monday`**'s two non-mutating-in-repeat writes are ordinary:
  `change_multiple_column_values` writes the columns it is given and leaves the
  rest alone, and `delete_item` publishes "Deletes an item (or subitem) and its
  nested subitems" with nothing about a second send. Both are `POST` in any case,
  because a GraphQL mutation is one.

# Batch I — storage and messaging (spec 025)

Seven connectors, 39 declared operations, and two classifications this file had
not carried before. The whole batch's reasoning is
[[076-a-published-mechanism-with-no-window-is-not-a-class-and-a-transport-choice-can-close-one]];
this section is its evidence.

Every statement below was read from the provider's own documentation on
2026-08-10; the source URLs are in each connector module's header comment.

## The operations

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `dropbox` | `folder.create` | `POST /2/files/create_folder_v2` | `InventoryOnly` |
| `dropbox` | `file.delete` | `POST /2/files/delete_v2` | `InventoryOnly` |
| `dropbox` | `share_link.create` | `POST /2/sharing/create_shared_link_with_settings` | `InventoryOnly` |
| `box` | `file.delete` | `DELETE /2.0/files/{file_id}` | `NaturalMethod` |
| `box` | `folder.delete` | `DELETE /2.0/folders/{folder_id}` | `InventoryOnly` |
| `box` | `folder.create` | `POST /2.0/folders` | `InventoryOnly` |
| `box` | `file.share_link_create` | `PUT /2.0/files/{file_id}` | `InventoryOnly` |
| `discord` | `message.send` | `POST /api/v10/channels/{channel.id}/messages` | `InventoryOnly` |
| `mattermost` | `post.create` | `POST /api/v4/posts` | `AtMostOnce` |
| `mailchimp` | `member.upsert` | `PUT /3.0/lists/{list_id}/members/{subscriber_hash}` | `NaturalMethod` |
| `zoom` | `meeting.create` | `POST /v2/users/{userId}/meetings` | `AtMostOnce` |
| `zoom` | `meeting.delete` | `DELETE /v2/meetings/{meetingId}` | `NaturalMethod` |

`dropbox_content` contributes none: it declares one operation, a download, and
it is `ReadOnly`.

## The mechanism a provider published without a window

### `discord` — `message.send`

This is the batch's sharpest entry and the one spec 025 §4 asked to be settled
from documentation rather than from a guess.

* **Documented contract.** *Create Message*
  (`POST /channels/{channel.id}/messages`) publishes a deduplication mechanism
  in its own JSON/Form params table: `nonce` — "Can be used to verify a message
  was sent (up to 25 characters). Value will appear in the Message Create
  event." — and `enforce_nonce` — "If true and nonce is present, it will be
  checked for uniqueness in the past few minutes. If another message was created
  by the same author with the same nonce, that message will be returned and no
  new message will be created."
* **The binding and the uniqueness scope are quotations.** The binding is a body
  field, and the scope is "the same author", which for a bot credential is the
  bot.
* **The retention is not.** "The past few minutes" is the whole of what Discord
  publishes about the window.
  [[073-a-retention-is-read-from-the-reference-that-owns-the-operation]] settles
  what that means: the reference that owns the operation wins, and silence there
  is a refusal. So `ProviderIdempotent::ExplicitKey` is refused.
* **Why it is not `AtMostOnce` either.** ADR 063 admits that class on evidence of
  an **absence**, and there is no absence: Discord published the mechanism.
  Reaching for the weaker class to route around a missing number is the
  promotion-by-proximity ADR 042 exists to prevent, and ADR 073 refused exactly
  that move for `paypal.refund.create`.
* **A second, independent bar.** `nonce` is published as "up to 25 characters",
  and a durable activity's stable key is longer than that. Even with a published
  window this connector could not bind the slot without truncating the key into a
  value that is no longer unique.
* **What Discord would have to publish to change this.** A duration for the
  uniqueness check, in its Create Message reference, and a `nonce` ceiling a
  36-character identifier fits. Both, not either.

## The writes a provider's own transport choice puts out of reach

### `dropbox` — `folder.create`, `file.delete`, `share_link.create`

* **Documented contract.** Dropbox's HTTP reference publishes its endpoint
  styles: "RPC endpoints … accept arguments as JSON in the request body, and
  return responses as JSON in the response body." Every endpoint in this
  connector — read and write alike — is a `POST`.
* **Evidence kind.** Complete published contract, no key in it. Dropbox's own
  Stone specification (`files.stone`, `sharing.stone`) declares each route's
  whole argument struct: `CreateFolderArg` is `path` and `autorename`,
  `DeleteArg` is `path` and `parent_rev`, `CreateSharedLinkWithSettingsArg` is
  `path` and `settings`. No request header, argument field, or result field
  carries a client-supplied request identifier, and neither `idempot` nor `dedup`
  occurs in the specification.
* **Why `NaturalMethod` cannot reach them.** Spec 010 §7 admits it for `PUT` and
  `DELETE` only. This is not a gap in the evidence; it is a property of the
  provider's transport choice, and it closes the class for **every** Dropbox
  write at once.
* **Why `AtMostOnce` is the wrong trade.** Each of the three is idempotent in
  effect by Dropbox's own published error unions: a second `delete_v2` answers
  `path_lookup/not_found`, a second `create_folder_v2` with `autorename` false
  answers `path/conflict`, and a second `create_shared_link_with_settings`
  answers `shared_link_already_exists`. An operation whose repeat changes nothing
  wants a class that *keeps* the retry; ADR 063's trades it away.
* **The declaration pins what makes that true.** `autorename` is a literal
  `false` rather than an input, because the two values have different repeat
  behaviour and a consequence that depends on caller input is not one consequence.

### `box` — `folder.create` and `file.share_link_create`

* **Documented contract.** Box's published OpenAPI declares `POST /folders` with
  required `name` and `parent`, and `PUT /files/{file_id}` with a `shared_link`
  object whose empty form (`{ "shared_link": {} }`) Box documents as "use the
  default settings for shared links".
* **Evidence kind.** Machine-checkable absence. The string `idempot` does not
  occur once in Box's published 1.77 MB OpenAPI description — 186 paths — and
  neither does `dedup`.
* **What a repeat produces.** For the folder create, `409 item_name_in_use`: "A
  resource with this value already exists." For the shared link, Box publishes
  nothing at all — its success text, "Returns the base representation of a file
  with a new shared link attached", describes the first send.
* **Why neither is executable.** The create is idempotent in effect over a
  `POST`, which is the same group as Dropbox's. The shared link is a `PUT`
  against a fixed identity — the shape `NaturalMethod` is for — with no provider
  repeat statement, which is the `salesforce.record.delete` finding on another
  provider.

## The delete a provider documented, and the one beside it that it did not

### `box` — `file.delete` is admitted; `folder.delete` is not

The two are the same method against the same kind of identity in the same API,
and Box's own response table separates them.

* `DELETE /files/{file_id}` publishes "204 — Returns an empty response when the
  file has been successfully deleted" and "404 — Returned if the file is not
  found **or has already been deleted**, or the user does not have access to the
  file." That last clause is a statement about the second send, which is what
  ADR 042 admits `NaturalMethod` on.
* `DELETE /folders/{folder_id}` publishes "404 — Returns an error if the folder
  could not be found, or the authenticated user does not have access to the
  folder", with no such clause — and publishes "503 — Returns an error when the
  operation takes longer than 600 seconds. **The operation will continue after
  this response has been returned**", so a repeat may name a folder Box is still
  deleting.

A provider that documents repeat-safety where it means it, and does not document
it here, has not said this is repeat-safe. The `recursive` query is pinned
`false` in the declaration for a second reason: Box publishes `400
folder_not_empty` for a non-empty folder, and an operation that could delete a
subtree because a caller passed a flag is a blast radius nobody declared.

### `zoom` — `meeting.delete`

* **Documented contract.** "Delete a meeting", `DELETE /meetings/{meetingId}`,
  with "**HTTP Status Code**: `204` Meeting deleted."
* **The repeat statement.** "**HTTP Status Code:** `404` Not Found — **Error
  Code:** `3001` — Meeting does not exist: {meetingId}."
* **One thing the declaration does deliberately.** `occurrence_id` is not
  declared. Zoom publishes that its presence changes *which* thing is deleted —
  "For recurring meetings, the `occurrence_id` is required to delete a specific
  occurrence. If not provided, the entire recurring series will be deleted" — and
  an operation whose identity a caller can narrow is not one fixed identity.

## The upsert this batch could admit, and the two it still cannot

### `mailchimp` — `member.upsert`

The strongest repeat evidence in the programme, and the contrast the three
recorded upserts were waiting for.

* **Documented contract.** `PUT /lists/{list_id}/members/{subscriber_hash}`,
  where the path segment is "The MD5 hash of the lowercase version of the list
  member's email address" — a fixed resource identity derived from the member's
  own address. Its title is "Add or update list member" and its description is
  "Add or update a list member."
* **The provider publishes what the *second* send does.** The request body's
  required `status_if_new` is documented as "Subscriber's status. This value is
  required only if the email address is not already present on the list", and
  `email_address` carries the same clause. A provider that publishes a different
  field for the first send has published that the second one updates the member
  the identity names.
* **Why this one is admitted where two others are not.** The method.
  `salesforce.record.upsert` is a `PATCH` and `zoho_crm.record.upsert` is a
  `POST`; spec 010 §7 admits `NaturalMethod` for `PUT` and `DELETE`, because HTTP
  defines repeat-safety for those two. Same semantics, different method,
  different answer — which is the rule working rather than a loophole.
* **The `POST` create is not declared at all.** `POST
  /lists/{list_id}/members` — "Add a new member to the list" — is the same effect
  with a worse contract, and a connector that published both would publish one
  operation a Process can reach and one it cannot, for the same intent.

## The two at-most-once sends, and what an operator accepts

### `mattermost` — `post.create`

* **Evidence kind.** Machine-checkable absence. Neither `idempot` nor `dedup`
  occurs anywhere in Mattermost's published 1.18 MB OpenAPI description. `POST
  /api/v4/posts` declares `channel_id`, `message`, `root_id`, `file_ids`,
  `props`, and `metadata`, and no request header, body property, or response
  field carries a client-supplied request identifier.
* **What a repeat produces.** A second post in the same channel, with a new id,
  delivered to every member who can read it and broadcast again over the
  WebSocket.

### `zoom` — `meeting.create`

* **Evidence kind.** Machine-checkable absence. Neither `idempot` nor `dedup`
  occurs in Zoom's published 1.19 MB OpenAPI description of the Meetings API.
  The create declares `agenda`, `default_password`, `duration`, `password`,
  `pre_schedule`, `recurrence`, `schedule_for`, `settings`, `start_time`,
  `template_id`, and `timezone`, and none of them is a request identifier.
* **What a repeat produces.** A second scheduled meeting with a new id and a new
  join URL, and — where the account's settings send them — a second set of
  invitations to the host and alternative hosts. It also spends a second unit of
  Zoom's published ceiling of "100 create/update requests per day (UTC) per
  user", which makes the accumulation visible to the account rather than only to
  the Process.

## Two near-misses, recorded because a reviewer will find them

* **Box's `If-Match` / `etag`.** Box publishes an optimistic-concurrency header
  on its writes — "Pass in the item's last observed `etag` value into this header
  and the endpoint will fail with a `412 Precondition Failed`" — which is not an
  idempotency key: it prevents a write against a *changed* resource, and a repeat
  of an unchanged one succeeds twice. It is not bound by this connector.
* **Dropbox's `parent_rev`.** `DeleteArg` publishes "Perform delete if given
  \"rev\" matches the existing file's latest \"rev\". This field does not support
  deleting a folder." Same shape as Box's `etag` and the same answer: a
  precondition, not a deduplication key.

## The running count this batch adds to

ADR 063 named a population that is still waiting for a class that keeps the
retry: writes a provider documents, or its own error contract makes, repeat-safe
over a method spec 010 §7's `NaturalMethod` does not admit. Batch I adds five —
`dropbox.folder.create`, `dropbox.file.delete`, `dropbox.share_link.create`,
`box.folder.create`, `box.file.share_link_create` — to the twelve already
recorded across the programme, for seventeen.

# Batch K — development and monitoring (spec 027)

Six connectors: `gitlab`, `bitbucket`, `pagerduty`, `grafana`, `uptimerobot`,
`cloudflare`. Every statement below was read from the provider's own
documentation on 2026-08-10; the source URLs are in each module's header.

## The operations

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `gitlab` | `issue.create` | `POST /api/v4/projects/{id}/issues` | `AtMostOnce` |
| `gitlab` | `issue_note.create` | `POST /api/v4/projects/{id}/issues/{iid}/notes` | `AtMostOnce` |
| `gitlab` | `merge_request.create` | `POST /api/v4/projects/{id}/merge_requests` | `AtMostOnce` |
| `gitlab` | `merge_request_note.create` | `POST …/merge_requests/{iid}/notes` | `AtMostOnce` |
| `gitlab` | `pipeline.trigger` | `POST /api/v4/projects/{id}/pipeline` | `AtMostOnce` |
| `bitbucket` | `issue.create` | `POST /2.0/repositories/{ws}/{repo}/issues` | `AtMostOnce` |
| `bitbucket` | `issue_comment.create` | `POST …/issues/{id}/comments` | `AtMostOnce` |
| `bitbucket` | `pull_request.create` | `POST /2.0/repositories/{ws}/{repo}/pullrequests` | `AtMostOnce` |
| `bitbucket` | `pull_request_comment.create` | `POST …/pullrequests/{id}/comments` | `AtMostOnce` |
| `pagerduty` | `incident.create` | `POST /incidents` | `AtMostOnce` |
| `pagerduty` | `incident_note.create` | `POST /incidents/{id}/notes` | `AtMostOnce` |
| `pagerduty` | `incident.update` | `PUT /incidents/{id}` | `InventoryOnly` |
| `grafana` | `alert_rule.update` | `PUT /api/v1/provisioning/alert-rules/{uid}` | `InventoryOnly` |
| `uptimerobot` | `incident_comment.create` | `POST /v3/incidents/{id}/comments` | `AtMostOnce` |
| `uptimerobot` | `monitor.pause` | `POST /v3/monitors/{id}/pause` | `InventoryOnly` |
| `cloudflare` | `zone.create` | `POST /client/v4/zones` | `AtMostOnce` |
| `cloudflare` | `zone.update` | `PATCH /client/v4/zones/{zone_id}` | `InventoryOnly` |
| `cloudflare` | `dns_record.create` | `POST /client/v4/zones/{zone_id}/dns_records` | `AtMostOnce` |

`cloudflare.dns_record.update` is the batch's one `ProviderIdempotent::NaturalMethod`
and is therefore not in this file: Cloudflare publishes it as "Overwrite DNS
Record — Overwrite an existing DNS record.", a `PUT` against the record id in
the path, and publishes the partial verb separately as "Update DNS Record —
Update an existing DNS record." (`PATCH`) on the same identity.

## How each negative was established

* **`gitlab` — complete published contract.** The string `idempot` does not
  occur in GitLab's published references for issues, merge requests, notes or
  pipelines, nor in its REST API guide or its troubleshooting page. Each of
  those references enumerates the complete supported-attribute table of every
  endpoint declared here. The two occurrences in the projects reference are
  behavioural notes on endpoints this module does not declare: "This endpoint is
  idempotent. Archiving an already-archived project does not change the
  project", and the same sentence for unarchiving.
* **`bitbucket` — machine-checkable absence.** `idempot` occurs twice in
  Bitbucket's own published Swagger description of the whole v2 REST API
  (`https://api.bitbucket.org/swagger.json`), both on default-reviewer endpoints
  this module does not declare: "Adds the specified user to the repository's
  list of default reviewers. This method is idempotent. Adding a user a second
  time has no effect."
* **`pagerduty` — machine-checkable absence, with a mechanism on the endpoint.**
  See the next section; this is the batch's one real near-miss.
* **`grafana` — machine-checkable absence.** `idempot` does not occur anywhere
  in `api-merged.json`, Grafana's own description of its whole HTTP API.
* **`uptimerobot` — machine-checkable absence.** `idempot` occurs four times in
  UptimeRobot's published v3 OpenAPI and every occurrence is a repeat-safety
  statement on a `POST`, never a client-supplied key.
* **`cloudflare` — machine-checkable absence.** `idempot` occurs 17 times in
  Cloudflare's published `openapi.json` and not once on a zone or DNS-record
  endpoint: a SAML certificate set, a WARP IP subnet delete, an origin
  cloud-region mapping, and a registrar note that a domain name is "a natural
  idempotency key for registration requests".

## The near-miss this batch adds: PagerDuty's `incident_key`

`POST /incidents` publishes a deduplication key **on the endpoint this connector
declares**, which is the first of
[[067-a-retention-with-an-escape-clause-is-not-a-minimum-retention]]'s three
shapes it does *not* fail:

> A string which identifies the incident. Sending subsequent requests
> referencing the same service and with the same `incident_key` will result in
> those requests being rejected if an open incident matches that `incident_key`.

It fails the other two, and one more this workspace had not met:

* **No published window.** PagerDuty publishes no retention of any kind for the
  key. Its lifetime is the incident's, and an incident is resolved by a human or
  an automation at a moment nothing here can observe, so there is no minimum a
  clock safety margin could sit strictly under — the Microsoft `transactionId`
  and Mercado Pago `X-Idempotency-Key` shape.
* **An escape clause.** The mechanism lapses the moment the incident is
  resolved, and the same request then opens a *second* incident — monday.com's
  shape, with a lifecycle in place of a memory budget.
* **A rejection is not an absorption.** `ExplicitKey` tells the activity worker
  "send again, the provider will absorb it". PagerDuty publishes the opposite:
  the repeat is *rejected* while the incident is open. That is a third outcome,
  not the first one repeated, and the class would be promising something the
  provider explicitly declines to do.

`knowledgebase/declarative-saas/decisions/080-*` records the decision. The key
stays a declared *input* — a deployment that wants PagerDuty's own behaviour may
send one — and nothing in the runtime writes it or relies on it.

The Events API v2's `dedup_key` is the plain "mechanism on another endpoint"
shape: it lives on `https://events.pagerduty.com/v2`, an origin this connector
does not declare, and PagerDuty describes it as "The key used to correlate
triggers, acknowledges, and resolves for the same alert" rather than as a
request-deduplication mechanism at all.

## Why the three `InventoryOnly` operations are not `AtMostOnce`

* **`pagerduty.incident.update`** and **`grafana.alert_rule.update`** are partial
  state changes for which no consequence is recorded at all — PagerDuty's
  "Acknowledge, resolve, escalate or reassign an incident" and Grafana's "Update
  an existing alert rule." Neither is spec 010 §7's `NaturalMethod` evidence,
  and ADR 063 is admitted on a *recorded consequence* a repeat produces, which a
  change whose effect the provider never described does not have.
* **`cloudflare.zone.update`** is the same shape, and its own provider makes the
  point one line down: "Edits a zone. Only one zone property can be changed at a
  time." beside a `PUT` published as "Overwrite".
* **`uptimerobot.monitor.pause`** is the other group: a write the provider
  documents as **repeat-safe** — "This operation is idempotent - pausing an
  already paused monitor will return successfully" — over a `POST`, which spec
  010 §7's `NaturalMethod` does not admit. `AtMostOnce` would trade away a retry
  this provider has said in writing it will absorb, so it stays declared, typed
  and unreachable until the class ADR 063 still names as open exists.

## The running count this batch adds to

Batch K adds one operation to the population waiting for a class that keeps the
retry — a write a provider documents as repeat-safe over a method spec 010 §7's
`NaturalMethod` does not admit: **`uptimerobot.monitor.pause`**.

# Batch L, the scheduling and people half (spec 028)

Every statement below was read from the provider's own documentation on
2026-08-10; the source URLs are in the module's header.

## The operations

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `harvest` | `time_entry.create` | `POST /v2/time_entries` | `AtMostOnce` |
| `harvest` | `time_entry.update` | `PATCH /v2/time_entries/{id}` | `InventoryOnly` |
| `bamboohr` | `employee.create` | `POST /api/v1/employees` | `AtMostOnce` |
| `bamboohr` | `employee.update` | `POST /api/v1/employees/{id}` | `InventoryOnly` |
| `clockify` | `time_entry.create` | `POST /api/v1/workspaces/{ws}/time-entries` | `AtMostOnce` |
| `clockify` | `time_entry.update` | `PUT /api/v1/workspaces/{ws}/time-entries/{id}` | `InventoryOnly` |
| `eventbrite` | `event.create` | `POST /v3/organizations/{org}/events/` | `AtMostOnce` |
| `eventbrite` | `event.update` | `POST /v3/events/{id}/` | `InventoryOnly` |

## How each negative was established

**`harvest` — complete published contract.** Harvest's API v2 reference
enumerates the whole request contract of `POST /v2/time_entries` — `project_id`,
`task_id`, `spent_date`, `user_id`, `hours`, `started_time`, `ended_time`,
`notes`, `external_reference` — and none of those is a client-supplied request
identifier. The three pages that carry Harvest's cross-cutting rules and would
be where such a mechanism lived — the authentication guide, the overview (which
is where Harvest publishes its required headers, its status table and its rate
limit) and the pagination guide — document no idempotency header, no
deduplicating parameter, and no replay behaviour.

`external_reference` is named here because a reviewer will find it and ask.
Harvest publishes it as a link to an object in *another* system — an object
carrying `id`, `group_id`, `permalink`, `service` and `service_icon_url` — and
publishes no uniqueness scope, no rejection of a repeat, and no replay for it.
It is the `pagerduty.incident_key` treatment one step weaker: a declared input a
deployment may send, which reaches no effect class, because nothing in Harvest's
documentation says a second create carrying the same reference is absorbed.

**`bamboohr` — complete published contract.** BambooHR publishes no idempotency
mechanism of any kind. Not in its Technical Overview, which is where it
publishes its status families ("200, 201 … 400, 401, 403, 404, 406, 409, 429 …
500, 502, 503"), its throttling rule ("API requests can be throttled if BambooHR
deems them to be too frequent. Implementations should always be ready for a `503
Service Unavailable` response") and its request and response formats; not in its
getting-started guide, which is where it publishes its credential; and not in
the request contract of `POST /api/v1/employees`, whose whole documented body is
a JSON object of employee field name/value pairs with "at least a first name and
a last name" required. No request header, query parameter, or body attribute
carries a client-supplied request identifier, and the only identity the create
returns is the one BambooHR mints: "The ID of the newly created employee is
included in the `Location` header of the response."

**`clockify` — complete published contract.** Clockify's published API
documentation names no idempotency mechanism anywhere: not in the cross-cutting
sections that carry its credential ("make sure to include either the
`X-Api-Key` or the `X-Addon-Token` in the request header"), its pagination
regime (`page`, `page-size`), its rate limit or its status codes, and not in the
request contract of the create, whose documented body is the time entry's own
fields — `start`, `end`, `billable`, `description`, `projectId`, `taskId`,
`tagIds`. No request header, query parameter, or body attribute carries a
client-supplied request identifier, and no response field or header reports a
replay.

**`eventbrite` — complete published contract.** Eventbrite's published v3 API
description carries no idempotency header, no client-supplied request identifier
in any endpoint's parameter set, and no deduplication or replay behaviour in the
three cross-cutting sections that would carry one: paginated responses (whose
`pagination` object is `object_count`, `page_number`, `page_size`, `page_count`,
`continuation`, `has_more_items`), the error envelope (`status_code`, `error`,
`error_description`), and the rate limit. The whole documented body of the event
create is an `event` object of the event's own attributes, and the only identity
it returns is the one Eventbrite mints.

## Why the four `InventoryOnly` operations are not `AtMostOnce`

It is the "partial update with no recorded consequence" group. Harvest publishes
it as a `PATCH` whose unset parameters are left unchanged, and publishes nothing
at all about a second identical send. Spec 010 §7 admits `NaturalMethod` for
`PUT` and `DELETE` only, because HTTP defines repeat-safety for those two; ADR
063 is admitted on a recorded absence **and** a recorded consequence, and a
partial update that writes the same values a second time has no consequence to
record. So the operation stays declared, typed, tested, and unreachable — the
same call as `asana.task.update`, `clickup.task.update`,
`pagerduty.incident.update` and `grafana.alert_rule.update`.

`bamboohr.employee.update` is the same finding one method over, and it is the
sharper of the two: BambooHR publishes the update as a **`POST`**, so
`NaturalMethod` is out of reach before any evidence is read — the shape
`knowledgebase/declarative-saas/decisions/076-*` records for a provider's
transport choice — and "Only the fields you include will be updated; omitted
fields are left unchanged" is a statement about *which* fields change, not about
what a second send does. BambooHR also publishes that "Unknown or misspelled
field names are silently ignored — the endpoint returns 200 but the field is not
updated", which is a statement about a *wrong* request rather than a repeated
one, and it is recorded here so a later reviewer does not read it as a repeat
guarantee.

`eventbrite.event.update` is the third of the same shape: Eventbrite publishes
the update over a `POST` taking an `event` object of the attributes to change,
and publishes nothing about a second identical send.

`clockify.time_entry.update` is the one worth reading twice, because it is the
*only* operation in this half of the batch that reaches spec 010 §7's admitted
methods. Clockify publishes it as a **`PUT`** against one fixed resource
identity — which is the method half of `ProviderIdempotent::NaturalMethod` — and
the class is still refused, because
[[042-the-effect-gate-admits-evidence-not-methods]] admits it on the *provider's
own repeat statement* and Clockify publishes none: no repeat-safety note, no
statement about the response of a replaced entry, and no marked idempotency
anywhere in its documentation. This is the `grafana.alert_rule.update` finding
one provider over, and it is the same call `salesforce.record.delete` got for a
`DELETE`: a provider that has not said a write is repeat-safe has not said it.
`AtMostOnce` is not the escape either — a replacement that writes the same
representation twice has no consequence to record.

## The running count this half adds to

Nothing. None of `harvest`, `bamboohr`, `clockify` or `eventbrite` publishes a
write its own documentation calls repeat-safe over a method the gate does not
admit, so the programme's count of operations waiting for a class that *keeps*
the retry is unchanged by this half of the batch.

What this half does add is a fifth shape to sort a `NaturalMethod` candidate
against: not "the mechanism is on another endpoint", not "there is no window",
not "there is an escape clause", not "a rejection is not an absorption", but
**the method is right and there is no statement at all**. `clockify.time_entry.update`
is the entry to compare a future `PUT` against.

# Batch L, the forms half — forms and surveys (spec 028)

Four connectors: `jotform`, `surveymonkey`, `cal_com`, `acuity`. Every statement
below was read from the provider's own published documentation on 2026-08-10;
the source URLs are in each module's header.

## The operations

| Connector | Operation | Method and path | Class |
|---|---|---|---|
| `jotform` | `submission.delete` | `DELETE /submission/{id}` | `InventoryOnly` |
| `surveymonkey` | `response.delete` | `DELETE /v3/surveys/{id}/responses/{id}` | `InventoryOnly` |
| `cal_com` | `booking.create` | `POST /v2/bookings` | `AtMostOnce` |
| `cal_com` | `booking.cancel` | `POST /v2/bookings/{uid}/cancel` | `InventoryOnly` |
| `acuity` | `appointment.create` | `POST /api/v1/appointments` | `AtMostOnce` |
| `acuity` | `appointment.cancel` | `PUT /api/v1/appointments/{id}/cancel` | `InventoryOnly` |

## How each negative was established

* **`jotform` — complete published contract.** The string `idempot` does not
  occur anywhere in Jotform's published API documentation: not in its overview,
  not in its authentication section, not in its FAQ, and not in the request
  contract of any endpoint in the reference `api.jotform.com/docs/` publishes.
  Each of those endpoint entries enumerates its complete parameter list — every
  query, path and body parameter with its type, whether it is required, and its
  description — and none names a client-supplied request identifier, an
  idempotency key, or a deduplication behaviour. No response field carries one
  either: the envelope is `{responseCode, message, content, limit-left}` and,
  on a collection, `resultSet`. Jotform's own summary of the surface is "Jotform
  API v1 is mostly read only."
* **`surveymonkey` — complete published contract.** The string `idempot` occurs
  **zero** times in SurveyMonkey's whole published v3 documentation: not in its
  authentication guide, its "Pagination", "Headers", "Data Types" or "Error
  Codes" sections, and not in any endpoint entry. Each endpoint entry publishes
  its complete "Optional Query Strings" table and its request body schema, and
  the "Headers" table enumerates every custom response header the API returns —
  eight of them, all `X-OAuth-Scopes-*` or `X-Ratelimit-App-Global-*`. Nothing
  in any of them is a client-supplied request identifier or a deduplication
  behaviour.
* **`cal_com` — machine-readable description.** Cal.com publishes an OpenAPI 3.0
  document at `cal.com/docs/api-reference/v2/openapi.json`, and the term
  `idempot` occurs in it exactly twice — see the near-miss below. Neither
  occurrence is on an endpoint this connector declares, and no request header,
  query parameter or body property of `POST /v2/bookings` or
  `POST /v2/bookings/{uid}/cancel` carries a client-supplied request identifier
  or a deduplication behaviour.
* **`acuity` — machine-readable description.** Acuity embeds an OpenAPI 3.1
  definition of each endpoint on that endpoint's own reference page, and the
  term `idempot` occurs in none of them, nor in its Quick Start, its API Errors
  page, or its webhook guide. Each definition enumerates the complete parameter
  list and request-body schema of its endpoint — the create's is
  `required: ["datetime", "appointmentTypeID", "firstName", "lastName",
  "email"]` with eleven further optional properties — and none of them is a
  client-supplied request identifier or a deduplication behaviour.

## The near-miss this half adds: Cal.com's `externalRef`

`POST /v2/credits/charge` publishes a deduplication mechanism, and Cal.com even
names it as such:

> Charge credits for an authenticated user. Uses externalRef for idempotency to
> prevent double-charging.

with the property itself described as "Unique external reference for
idempotency". It fails
[[067-a-retention-with-an-escape-clause-is-not-a-minimum-retention]]'s first
shape outright — **the mechanism is on another endpoint** than the ones this
connector declares — and it would fail the window test too if it were on one:
Cal.com publishes no retention for `externalRef` anywhere, which is the Discord
`nonce` shape
([[076-a-published-mechanism-with-no-window-is-not-a-class-and-a-transport-choice-can-close-one]]).
The endpoint is not declared here, so nothing in this connector binds it.

## The delete this half declares and cannot reach

`jotform.submission.delete` is a `DELETE` against a fixed resource identity —
`/submission/{id}`, where "Submission ID … You can get submission IDs when you
call /form/{id}/submissions" — which is the *method* half of spec 010 §7's
`ProviderIdempotent::NaturalMethod`. It is not the *evidence* half. That class
is admitted on the provider's own repeat statement, in the shape Typeform
publishes one ("Not found response IDs will be ignored.") or Dropbox does, and
Jotform publishes nothing of the kind: the only outcome its reference names for
this endpoint at all is "404 — User not found", and the published success is the
prose `content` "Submission #{submissionID} deleted successfully."

`AtMostOnce` is not the home either, and the reason is ADR 063's bar rather than
a preference. That class is admitted on **both** halves of the evidence: a
recorded search that found no mechanism — which this connector has — **and** a
recorded consequence a second send produces, which is what an operator accepts
when they write the opt-in. "The provider does not say what a second delete of
an already-deleted submission answers" is the absence of a consequence rather
than one, so there is nothing for an operator to accept. This is the same answer
`trello.card.delete`, `monday.item.delete` and `todoist.task.delete` already
carry in this file, for the same reason.

`surveymonkey.response.delete` is the same shape and the same answer.
SurveyMonkey publishes the method list for the resource — "GET: Returns a
response", "PATCH: Modifies a response", "PUT: Replaces a response", "DELETE:
Deletes a response" — and publishes no response schema and no repeat behaviour
for the last of them. The `PUT` beside it is *not* the `NaturalMethod` escape
either: SurveyMonkey documents it as taking "same arguments and requirements as
POST /surveys/{id}/responses", which is a whole response body a Process would
have to reconstruct, and it publishes no repeat statement for that one either.

## The cancel that is not `AtMostOnce` either

`cal_com.booking.cancel` is the third group `INVENTORY.md` already names — a
state change for which **no consequence of a repeat is recorded at all**, beside
`pagerduty.incident.update`, `grafana.alert_rule.update` and
`cloudflare.zone.update`. Cal.com publishes what the endpoint does — "Cancel a
booking", with the seated and recurring variants spelled out in detail — and
publishes nothing about sending it twice: not that a repeat is absorbed, not
what a repeat of an already-cancelled booking answers, and not whether a second
`BOOKING_CANCELLED` webhook is delivered. It is also a `POST`, so spec 010 §7's
`NaturalMethod` does not reach it whatever its effect turns out to be. Writing a
consequence this connector guessed at would be the one thing ADR 063's evidence
bar exists to stop, because that sentence is what an operator accepts when they
write `at_most_once: true`.

## The method-eligible cancel that is still not `NaturalMethod`

`acuity.appointment.cancel` is the sharpest operation in this half, because it
is the one that **passes the method test**. `PUT /api/v1/appointments/{id}/cancel`
is a `PUT` against a fixed resource identity, which is exactly what spec 010 §7
admits — *on the provider's own repeat statement*. Acuity publishes none.

What it publishes about repetition is about a different operation:

> Once canceled, appointments will have a `noShow` attribute. This attribute may
> be updated, but it isn't possible to un-cancel the appointment.

That says the state is terminal. It does not say a second cancel is absorbed,
and it does not say whether the notifications Acuity sends — "Skip sending the
cancellation e-mail and SMS by canceling the appointment with the `noEmail=true`
query parameter" — are sent again. [[042-the-effect-gate-admits-evidence-not-methods]]
is the whole answer: the gate admits evidence, and a `PUT` with no repeat
statement has the method and not the evidence. ADR 063 does not take it either,
for the reason every other silent write in this half is refused — the
consequence of the second send is unrecorded, and the consequence is what the
operator is accepting.

This is the **second** entry of the fifth near-miss shape this batch named — the
method is right and the statement is absent — and it is the one to compare a
future `PUT` against when the provider is not silent but *nearly* speaks.
`clockify.time_entry.update`, one half of this batch over, is the pure form:
Clockify publishes no repetition-adjacent sentence at all. Acuity publishes one,
about a neighbouring operation, and a reviewer who reads it quickly will take it
for a repeat statement. The two entries together are the shape and its trap.

## The two at-most-once writes

`acuity.appointment.create` is the other, with the recorded consequence "a
second appointment at the same time wherever the appointment type still has a
slot for one — a second entry on the calendar and a second confirmation to the
client, which Acuity sends itself unless the caller suppresses it". All three
halves are the provider's own: the create, the availability validation it
performs, and the notification suppression parameter that exists precisely
because the notification is otherwise sent.

`cal_com.booking.create` is the first executable mutation in this half, and its
recorded consequence is "a second booking of the same slot wherever the event
type still has capacity for one — a second calendar event, a second confirmation
to the attendee and to the host, and a second `BOOKING_CREATED` webhook delivery
to every subscriber". Cal.com publishes all three halves of that sentence: the
booking creation, the confirmation emails, and `BOOKING_CREATED` in its webhook
trigger list.

## The running count this half adds to

Nothing. No operation in this half is a write a provider documents as
repeat-safe over a method spec 010 §7's `NaturalMethod` does not admit.
