# Task 5 report — signed inbound connector routing

## Scope delivered

- Added only `POST /v1/connectors/{instance}/webhooks`, mounted independently
  from the optional GraphQL, REST, and MCP data APIs.
- The route resolves a declared compiled Stripe instance before it reads any
  request bytes. Unknown names, and declared HTTP instances that have no
  inbound verifier, receive the same minimal `404` response.
- A declared webhook reads raw bytes once with the shared 1 MiB limit, passes
  those original bytes and headers to the Stripe verifier, and returns a
  minimal `400` for every verifier rejection. Responses contain no body,
  provider diagnostic, metadata identity, configuration, or secret.
- A correctly verified Stripe event receives a minimal `503 Service
  Unavailable`, never a `2xx` acknowledgement. There is no generic webhook,
  runtime-admin, Action conversion, queue, retry loop, process signal, or
  activity `retry_on` / `on_error` input.

## Durable audit-state boundary

The approved brief's wording about webhook audit state was checked against the
current implementation. The repository has durable cron and table-event
journals, but no durable process ingress/audit model, process signal table, or
process worker. Adding an in-memory queue or a new persistent state model in
this task would falsely acknowledge provider delivery or pre-empt the process
plan.

Therefore Task 5 deliberately records no ingress state and does **not**
satisfy the audit or duplicate-ingress persistence requirement. The
verified-event test proves only the safe observable boundary: the route
completes verification and returns `503` with an empty body, so the provider
retains the event. Duplicate handling, one durable audit row per ingress
outcome, correlation, and durable acknowledgement belong to the durable
process ingress/journal implementation in
`docs/superpowers/plans/2026-07-28-declarative-processes.md`, **Task 6:
Process timers and verified inbound events**. Connector-plan Task 6 is
conformance-only and proves the temporary `503`/no-acknowledgement boundary;
it does not implement persistence.

## TDD evidence

Initial RED, before the inbound module existed:

```text
cargo test -p donat-server --test connector_webhook
error[E0432]: unresolved import `donat_server::connector_webhook`
```

The Donat-owned mounted-route integration tests then became green and cover:

- unknown oversized instance request returns `404` before the body-size path;
- a declared Stripe instance rejects a raw body over 1 MiB with `413`;
- an invalid signature and a credential-sentinel raw body return only `400`
  with an empty safe body;
- a valid raw Stripe signature for `checkout.session.completed` returns only
  `503` with an empty body, proving no acknowledgement or synchronous
  dispatch before durable process ingress exists.

The test invokes `connector_webhook::router()` through an actual Axum `POST`
request, rather than calling the handler directly, so it also protects the
literal route registration contract.

## GREEN verification

```text
cargo fmt --check                                                        # pass
cargo test -p donat-server --test connector_webhook                     # pass (4)
cargo test -p donat-server action::tests                                 # pass (17 lib + 17 bin)
cargo test -p donat-server events::tests                                 # pass (3 lib + 3 bin)
cargo test -p donat-server                                               # pass (all server targets)
cargo test -p donat-metadata                                             # pass (57)
cargo clippy -p donat-server --all-targets -- -D warnings                # pass
cargo build -p donat-server --bin donat                                  # pass
DONAT_BIN=/home/dev/.cache/donat-runtime-target/debug/donat \
  PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres \
  cargo test -p donat-conformance --quiet -- --test-threads=4            # pass, PTY exit_code: 0
```

## Fix Round 1 — independent review

### Regression coverage

The review identified that the original tests did not prove that a *declared*
non-webhook module was indistinguishable from an unknown instance. The fixture
now compiles both a Stripe `payments` instance and a declared HTTP `logistics`
instance. The new mounted-route test proves `POST` to `logistics` and to an
unknown instance both return the same empty `404` response.

The existing implementation already selected only Stripe instances, so the
new regression was verified with a controlled temporary mutation that returned
`400` for `logistics`. Its RED result was:

```text
declared_http_instance_is_indistinguishable_from_an_unknown_webhook_instance
assertion `left == right` failed
  left: 400
 right: 404
```

The mutation was removed before the final implementation and is not present in
the commit.

### Durable ingress ownership correction

Review also found an inaccurate statement that connector-plan Task 6 would
own durable audit, duplicate-ingress, and acknowledgement persistence. The
actual owner already exists in `specs/005-durable-processes.md` and
`docs/superpowers/plans/2026-07-28-declarative-processes.md`, **Task 6:
Process timers and verified inbound events**. It creates `process_inbound_events`,
deduplicates provider IDs, writes redacted audit outcomes, correlates process
signals, and returns success only after commit.

The accepted durable-process ADR was amended to make that ownership explicit.
The connector plan now states that Task 5 does not satisfy the audit/duplicate
persistence checklist, and that connector-plan Task 6 is conformance-only: it
proves the temporary `503` / no-acknowledgement boundary and implements no
process persistence.

### Fix-round GREEN verification

```text
cargo fmt --check                                                        # pass
cargo test -p donat-server --test connector_webhook                     # pass (5)
cargo test -p donat-server                                               # pass (all server targets)
cargo clippy -p donat-server --all-targets -- -D warnings                # pass
cargo build -p donat-server --bin donat                                  # pass
DONAT_BIN=/home/dev/.cache/donat-runtime-target/debug/donat \
  PG_URL=postgresql://postgres:postgres@127.0.0.1:15433/postgres \
  cargo test -p donat-conformance --quiet -- --test-threads=4            # pass, PTY exit_code: 0
```
