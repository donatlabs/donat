# Reference porting register

This register is the authoritative provenance record for the declarative SaaS
runtime. A specification may summarize entries, but an implementation may not
copy, vendor, generate from, or adapt an upstream file until this register has
the required immutable record.

## Admission rule

A source-level port is allowed only when all of the following are recorded in
the same pull request before the imported artifact:

1. upstream repository URL and full immutable commit;
2. exact source file or generated-artifact path and SHA-256;
3. license classification and the required copyright or NOTICE text;
4. destination path in this repository and the responsible module;
5. the Donat test that was added first and failed before the implementation;
6. review of every copied diff, including any generated output.

Apache-2.0, MIT, and BSD-compatible sources are eligible only after that
record. Non-permissive sources are behavior-only references: no source file,
fixture, generated artifact, or large verbatim text may be copied. A future
port creates or updates root THIRD_PARTY_NOTICES.md before it lands. This
register does not waive upstream obligations.

## Active references

| ID | Upstream and revision | Exact source surface | License | Status and permitted use | First Donat test destination |
| --- | --- | --- | --- | --- | --- |
| DONAT-NATIVE | current repository revision at implementation time | crates/metadata, catalog, schema, ir, sqlgen, server, conformance | Apache-2.0 | native source; extend established patterns rather than copy external code | crate-local unit/insta plus crates/conformance |
| CEL-0252 | google/cel-spec cb51b4176013ad19bd00df94be273c322916a620 | doc/, conformance/proto2/, conformance/proto3/, conformance/test/ | Apache-2.0 | behavior and selected expected-value cases only; no protobuf or generated Go is planned | crates/rules tests and command/process integration |
| TEMPORAL-RUST | temporalio/sdk-rust d2769368df9077a311537431ff4594c9c14db4e7 | ARCHITECTURE.md, crates/sdk-core/, crates/workflow/ | MIT | durable-history, replay, lease, and retry behavior only; no Temporal client, server, protocol, or source import | crates/server process integration |
| STRIPE-OAS | stripe/openapi 6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69 | openapi/spec3.json, openapi/fixtures3.json | MIT | Checkout contract reference; an individual schema/generated artifact is eligible but no file is copied yet | crates/server connectors/stripe contract tests |
| STRIPE-MOCK | stripe/stripe-mock 3f370d112ba55a8a12c09b162547ba32f26b9693 | executable release; main_test.go, server/, spec/ for behavior reading | MIT | black-box test server only; Go source is not compiled or ported | connector stripe-mock integration test |
| AIRBYTE-STRIPE | airbytehq/airbyte 32ec364b51e96f748e6aea28bbbac2dd9aac8bd9 | source-stripe manifest.yaml, acceptance-test-config.yml, integration_tests/, unit_tests/ | ELv2 | behavior and test-category reference only; no source or fixture copy | Donat-owned Stripe acceptance fixtures |
| TEMPORAL-DOCS | [Temporal safe deployments](https://docs.temporal.io/develop/safe-deployments), accessed 2026-07-28 | deployment compatibility and worker versioning concepts | documentation | behavior-only reference for revision ABI fencing; no code, fixture, or text copy | process rolling-deploy integration tests |
| AWS-SFN-DOCS | [AWS Step Functions Task](https://docs.aws.amazon.com/step-functions/latest/dg/state-task.html) and [error handling](https://docs.aws.amazon.com/step-functions/latest/dg/concepts-error-handling.html), accessed 2026-07-28 | task timeout and typed error-routing concepts | documentation | behavior-only reference for distinct activity deadlines and on_error routes; no code, fixture, or text copy | process timeout/error-route tests |
| CAMUNDA-DMN-DOCS | [Camunda decision-table hit policies](https://docs.camunda.io/docs/8.8/components/modeler/dmn/decision-table-hit-policy/), accessed 2026-07-28 | first/unique decision-table semantics | documentation | behavior-only reference; no code, fixture, or text copy | rules decision-table tests |
| INNGEST-DOCS | [Inngest concurrency](https://www.inngest.com/docs/guides/concurrency) and [throttling](https://www.inngest.com/docs/guides/throttling), accessed 2026-07-28 | deployment-wide concurrency and throttling concepts | documentation | behavior-only reference for shared operation capacity; no code, fixture, or text copy | two-worker capacity integration tests |
| IANA-IPV6-SPECIAL | [IANA IPv6 Special-Purpose Address Registry](https://www.iana.org/assignments/iana-ipv6-special-registry/iana-ipv6-special-registry.xhtml), accessed 2026-07-29 | `Globally Reachable` classifications for special-purpose IPv6 prefixes | documentation | behavior-only source for `public_only` egress screening; no code, fixture, or text copy | `crates/server/src/connectors/http.rs` IPv6 public-address regression |

## Verified behavior references

### STRIPE-CHECKOUT-PHASE-1 — behavior-only contract audit

- Upstream: [stripe/openapi](https://github.com/stripe/openapi) @
  `6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69`
- Source: `openapi/spec3.json`
  (SHA-256: `e24a26de4188fd64dec4c043d5d3726277fdcb07556a493ea481c305b0a223d8`)
- License: MIT. No OpenAPI schema, generated artifact, fixture, or source text
  is copied into Donat.
- Selected behavior: `POST /v1/checkout/sessions` accepts
  `application/x-www-form-urlencoded`; this Phase-1 module independently
  encodes only `mode`, `success_url`, `cancel_url`, `client_reference_id`, and
  `line_items[*].price`/`line_items[*].quantity`, then extracts the Checkout
  Session's `id`, `url`, `status`, and `expires_at` from a JSON response.
- Destination: independent Rust implementation in
  `crates/server/src/connectors/stripe.rs` and Donat-owned tests in
  `crates/server/tests/connectors_stripe.rs` plus crate-local unit tests.
- Initial RED evidence: the Donat-owned `connectors_stripe` integration target
  failed before the module existed with `no StripeConnector in
  connectors::stripe`. The later crate-local test
  `stripe_checkout_posts_form_and_returns_typed_session` covers the resulting
  form contract.

### STRIPE-MOCK-CHECKOUT-PHASE-1 — behavior-only mock-server audit

- Upstream: [stripe/stripe-mock](https://github.com/stripe/stripe-mock) @
  `3f370d112ba55a8a12c09b162547ba32f26b9693`
- Sources: `server/server.go`
  (SHA-256: `74e227ddf08787f7b070213dbc2e95c5dece69788ba664b0058b71693caa82fc`)
  and `server/server_test.go`
  (SHA-256: `96a68725ac45e277e90f7094fe236689ae078cca5c4dc5948b66c38abeafe154`)
- License: MIT. No Go source, fixture, executable, generated output, or test
  text is copied into Donat.
- Selected behavior: local tests independently verify form content type and a
  stable `Idempotency-Key` request header against a Donat-owned Axum stub; no
  Stripe account or live endpoint is used.
- Destination: independent Rust implementation in
  `crates/server/src/connectors/stripe.rs` and Donat-owned tests in
  `crates/server/tests/connectors_stripe.rs` plus crate-local unit tests.
- Initial RED evidence: the Donat-owned `connectors_stripe` integration target
  failed before the module existed with `no StripeConnector in
  connectors::stripe`. The later crate-local test
  `stripe_checkout_posts_form_and_returns_typed_session` covers the resulting
  form contract.

No source-level port is made by these records, so `THIRD_PARTY_NOTICES.md` is
not created or changed for this slice.

## Per-port record template

Add one subsection for every actual imported file:

~~~markdown
### PORT-YYYY-NNN — short description

- Upstream: owner/repo @ full-commit
- Source: exact/path.ext (SHA-256: ...)
- License: SPDX identifier; notice copied to THIRD_PARTY_NOTICES.md line ...
- Destination: crates/.../file.rs
- Adaptation: what was changed in the independent Rust rewrite
- Red test: crate/test path and exact test identifier
- Green evidence: command and reviewed output
- Reviewer: name/date
~~~

A code review rejects a port whose source path, license, notice, red test, or
destination is absent. A source reference with status behavior only cannot be
promoted to copied code by changing its description; it needs a new eligible
record.
