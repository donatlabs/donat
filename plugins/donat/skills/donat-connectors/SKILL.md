---
name: donat-connectors
description: Use when a donat process must call an external HTTP provider, or when a provider call is duplicating, timing out ambiguously, or leaking credentials into logs.
---

# Connectors

A connector declares a provider's operations. It is never called from a
command — only from a process `request` state — because a provider call is not
part of a database transaction and must not be able to hold one open.

One file per provider in `connectors/`, listed in `connectors.yaml`.

## The header

```yaml
name: mock_payment
module: http
config:
  endpoint_identity: petshop_mock_payment_v1
  credential_identity: petshop_mock_payment_fixture
  base_url: { value_from_env: PETSHOP_PAYMENT_BASE_URL }
  headers:
    - name: Authorization
      value_from_env: PETSHOP_PAYMENT_API_TOKEN
operations: [...]
```

Credentials are `value_from_env`, never literals in metadata. The metadata
directory is reviewed, committed and often shipped; a token in it is a leaked
token.

## An operation

```yaml
- name: authorize
  version: v1
  method: POST
  path: /v1/payment-authorizations
  input_contract:
    order_id: uuid!
    payment_id: uuid!
    amount_minor: bigint!
    request_id: string!
  body:
    order_id:     { input: order_id }
    payment_id:   { input: payment_id }
    amount_minor: { input: amount_minor }
  success_statuses: [200, 201, 202]
  response:
    provider_event_id: { json_pointer: /provider_event_id, type: string!, max_bytes: 256 }
    status:            { json_pointer: /status, type: PaymentOutcome! }
    normalized_payload: { json_pointer: /normalized_payload, type: BoundedProviderEvidence! }
```

- `input_contract` is what the process must supply, typed against `rules.yaml`.
- **Every input must be consumed** by `body`, `query`, `path` or a header. One
  that nothing mentions is refused at execution as `connector operation input
  contains an undeclared value`, `class: invariant` — *before the request
  leaves*, so the provider sees nothing and the log says only that an activity
  failed. Adding a field to a process's request and forgetting to map it is the
  usual way to meet this, and the failure names neither the input nor the
  operation.
- The **keys of `body` are the wire's, not yours**: `To: { input: recipient }`
  is fine. Rename freely to match what the provider publishes; only the input
  *names* are fixed, because the process binds by them.
- `path` may interpolate an input: `/v1/payment-authorizations/{input.payment_id}/captures`.
- `response` maps JSON pointers to **typed** fields. `type: PaymentOutcome!`
  means the provider's string is validated into your enum at the boundary, so
  no later layer has to know the provider's vocabulary.
- `max_bytes` and `maximum_items` on a response field are part of the contract.
  A provider that returns a megabyte where you expected an id is a failure, not
  a surprise.

`version: v1` on each operation is what lets a provider's v2 be added beside v1
instead of replacing it under a running process.

## `success_contract` — when 200 is not success

An HTTP 200 saying "declined" is not a successful capture. Say so:

```yaml
success_contract:
  status: captured
```

For an operation whose shape depends on an outcome, discriminate:

```yaml
success_contract:
  discriminator: resolution
  cases:
    mutation_found:
      exactly_one_non_empty: captured
      empty: terminal_absences
      captured_outcome: captured
    terminal_absence:
      exactly_one_non_empty: terminal_absences
      empty: captured
  unproven_absence:
    error: { class: invariant, code: payment_capture_outcome_ambiguous }
```

That last clause is the important one: a lookup that cannot prove either the
mutation or its absence raises a named error rather than being read as "it did
not happen". See the ambiguity pattern in `donat-processes`.

## `effect` — what a retry means

```yaml
effect: read_only          # safe to retry freely
```

```yaml
effect:
  provider_idempotent:
    side_effect_steps:
      - step: request
        fixed_binding: { header: Idempotency-Key }
        scope: petshop-mock-payment-authorization-v1
        minimum_retention_ms: 604800000
        clock_safety_margin_ms: 300000
        evidence:
          source_record_id: source.petshop.mock-providers.v1
          fact_ids: [fact.mock-providers.fixed-idempotency-header, ...]
```

HTTP delivery is at-least-once at the provider boundary, so a mutation must
declare *how* the provider deduplicates: which header carries the key, the
scope the key is unique within, the minimum window the provider retains it, and
a positive clock-safety margin.

The evidence ids point at an immutable record — `provider-evidence/*.yaml` in
the petshop — of where each claim came from. This is not bureaucracy: the
process compiler derives each activity's maximum send horizon from its timeout
and retry policy and **checks it against retention minus the margin**. A retry
window longer than the provider's memory is caught at deploy time instead of by
a duplicate charge.

Scope the key per operation (`…-authorization-v1`, `…-capture-v1`). A key
shared across operations makes a capture look like a replayed authorization.

## Bounds, errors, retry, capacity, redaction

```yaml
bounds:
  deadline_ms: 2000
  maximum_calls: 1
  maximum_pages: 1
  maximum_items: 1
  maximum_aggregate_request_bytes: 16384
  maximum_aggregate_response_bytes: 16384
  maximum_redirects: 0
  maximum_json_depth: 8
  maximum_json_nodes: 128

error_map:
  rules:
    - { statuses: [401, 403], class: authentication, code: payment_authentication }
    - { statuses: [408],      class: timeout,        code: payment_timeout }
    - { statuses: [429],      class: http_429,       code: payment_rate_limited }
    - { statuses: [500, 502, 503, 504], class: http_5xx, code: payment_unavailable }
    - { statuses: [400, 404, 409, 422], class: validation, code: payment_rejected }
  fallback: { class: permanent, code: payment_provider_error }

timeout: 2s
retry:
  maximum_attempts: 3
  backoff: 100ms
  retry_on: [transport, timeout, http_429, http_5xx]

capacity:
  max_in_flight: 8
  rate_limit: { permits: 20, per: 1s, burst: 8 }
  serialize_by: { input: payment_id }

redaction:
  request_headers: [Authorization]
  response_body: [provider_reference, normalized_payload]
```

- **`maximum_redirects: 0`** by default. A redirect is a provider sending your
  credentials somewhere you did not declare.
- **Error classes are the vocabulary the process routes on.** Never retry
  `validation` or `authentication` — the answer will not change, and retrying a
  400 is how a rate limit becomes an outage.
- **`serialize_by`** puts operations on the same domain key in a queue of one.
  This is what keeps a capture from overtaking its own authorization.
- **`redaction`** applies to logs and stored evidence. Every credential header
  belongs here, and so does any response field carrying a provider reference
  you would not want in a log aggregator.

## Conventions

- One connector per provider, one operation per provider endpoint.
- Give every mutating operation a matching read-only `lookup_*`. Without it,
  ambiguity has nowhere to go.
- Normalise into your own types at the boundary (`type: PaymentOutcome!`), and
  keep provider strings out of everything downstream.
- Bound the response, not just the request.

## Files to read

- [`examples/petshop/metadata/connectors/mock-payment.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/connectors/mock-payment.yaml) — seven operations
  including `lookup_capture` with a discriminated success contract
- [`examples/petshop/metadata/connectors/mock-carrier.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/connectors/mock-carrier.yaml) — labels and
  tracking, the same shape for a different domain
- [`examples/petshop/provider-evidence/mock-providers-v1.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/provider-evidence/mock-providers-v1.yaml) — what the
  evidence ids resolve to
- [`crates/conformance/tests/connectors.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/connectors.rs)
