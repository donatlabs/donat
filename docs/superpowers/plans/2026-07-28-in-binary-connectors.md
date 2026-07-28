# In-Binary Connectors Implementation Plan

> **For Codex:** Execute every checkbox in order with RED/GREEN evidence and a
> judge ACCEPT after each commit.

**Goal:** Provide a safe compiled connector boundary for declarative SaaS
processes: a generic declarative HTTP module and a narrow Stripe Checkout
module, both linked into the single Donat Rust binary with no dynamic plugins,
arbitrary caller URLs, or connector microservices.

**Architecture:** `connectors.yaml` describes instances and non-secret
configuration. `crates/server/src/connectors/` owns a registry of compiled
modules. The registry validates configuration at boot, resolves secrets by
environment variable name, pins module/operation versions and runtime ABI,
validates closed operations, performs safe HTTP requests, classifies typed
outcomes, and verifies signed raw webhook bytes. A later process worker owns
journal persistence, leases, globally coordinated capacity, retries, and
outcomes; connector modules are pure request/response contract adapters plus
bounded network I/O.

**Tech stack:** Rust, Tokio, Axum, `reqwest`, `url`, `hmac`/`sha2` only if not
already present, serde JSON/YAML, native conformance harness.

**Prerequisites:** Complete
[`Declarative Rules`](2026-07-28-declarative-rules.md) and
[`Declarative Commands`](2026-07-28-declarative-commands.md) first.

**Specification:**
[`specs/006-in-binary-connectors.md`](../../../specs/006-in-binary-connectors.md)

## Required interfaces

```rust
// crates/server/src/connectors/mod.rs
pub struct ConnectorRegistry { /* instance -> module + validated config */ }

pub trait ConnectorModule: Send + Sync {
    fn definition(&self) -> ConnectorDefinition;
    fn validate_config(&self, config: &ConnectorConfig) -> Result<(), ConnectorError>;
    fn validate_operation(&self, operation: &ConnectorOperation) -> Result<(), ConnectorError>;
    async fn execute(
        &self,
        operation: ValidatedOperation,
        request: ConnectorRequest,
    ) -> Result<ConnectorSuccess, ConnectorFailure>;
    fn verify_webhook(
        &self,
        config: &ResolvedConnectorConfig,
        request: &InboundRequest,
    ) -> Result<VerifiedWebhook, ConnectorFailure>;
}

pub struct ConnectorFailure {
    pub class: FailureClass, // Transport | Timeout | Http429 | Http5xx |
                             // Authentication | Validation | Permanent | Invariant
    pub code: &'static str,
    pub safe_message: String,
}

// server state publishes a fully validated immutable registry.
pub struct AppState {
    pub connectors: Arc<ConnectorRegistry>,
    // existing state
}
```

`execute` receives values derived from a validated process operation, never a
caller-controlled URL, HTTP method, header name, authentication scheme, or
TLS policy. `verify_webhook` consumes raw bytes before JSON parsing.

### Task 1: Add connector metadata, startup validation, and conformance writer

**Files:**
- Modify: `crates/metadata/src/types.rs`, `crates/metadata/src/loader.rs`,
  `crates/server/src/main.rs`, `crates/server/src/state.rs`,
  `crates/server/src/migrate.rs`, `crates/conformance/src/lib.rs`
- Test: `crates/metadata/tests/types_serde.rs`,
  `crates/metadata/tests/load_fixture.rs`, `crates/server/tests/state.rs`
- Add fixtures: `crates/metadata/tests/fixtures/connectors/`

- [ ] Add tests for absent `connectors.yaml`, a quoted include, duplicate
  instance names, unknown module, missing required config, static URL with a
  user-info component, non-secret endpoint/credential identities, operation
  capacity, and a secret reference that is an environment variable name rather
  than a secret literal.
- [ ] RED: run `cargo test -p donat-metadata connectors` and
  `cargo test -p donat-server connector_startup`. Expected: no connector
  metadata or startup validation exists.
- [ ] Define `ConnectorInstance`, `ConnectorConfig`, `SecretRef`, and a
  typed operation configuration in metadata. Load `connectors.yaml` through
  `load_section`; include it in the conformance metadata directory only when
  non-empty.
- [ ] Add a pre-listen registry-construction hook. It resolves and verifies
  every required environment variable but does not log, serialize, or expose a
  value through `Metadata`.
- [ ] Make `migrate::check_consistency` report static connector errors and
  variable-name shape errors without resolving a secret. Preserve
  no-runtime-admin semantics: connector instances can change only through a
  deployed metadata directory and restart.
- [ ] GREEN: run `cargo test -p donat-metadata`,
  `cargo test -p donat-server connector_startup`, and
  `cargo test --workspace --no-run`.
- [ ] Commit the metadata/startup slice and obtain judge ACCEPT.

### Task 2: Create the compiled registry and safe outbound HTTP core

**Files:**
- Add: `crates/server/src/connectors/mod.rs`,
  `crates/server/src/connectors/http.rs`,
  `crates/server/tests/connectors_http.rs`
- Modify: `crates/server/Cargo.toml`, `crates/server/src/main.rs`,
  `crates/server/src/lib.rs` if module exports require it

- [ ] Add tests using a local Axum test server for allowed static HTTPS-like
  base URLs, escaped path substitution, JSON-pointer result extraction,
  redirect rejection, 1 MiB request/response limit, timeout classification,
  typed 429/5xx classification, and a resolved hostname that changes from
  public to loopback before connection.
- [ ] RED: run `cargo test -p donat-server --test connectors_http`.
  Expected: connector registry and HTTP module do not exist.
- [ ] Implement `ConnectorRegistry` with an explicit built-in module table
  containing only `http` and `stripe`. Unknown module names are validation
  errors; there is no `dlopen`, shell command, dynamic URL handler, or network
  plugin discovery.
- [ ] Implement the HTTP module with a static parsed base URL, allowlisted
  method, percent-encoded declared path/query substitutions, static headers
  plus approved credential header injection, JSON body construction, bounded
  body read, disabled redirects, finite timeout, and JSON Pointer extraction.
- [ ] Enforce `network_policy: public_only` by resolving every destination at
  request time and rejecting loopback, link-local, private, multicast,
  unspecified, and non-global addresses. Re-check the connected peer where
  the HTTP client exposes it; otherwise record this as a documented resolver
  limitation and keep redirects disabled.
- [ ] Classify DNS/connect errors as transport, operation deadline as timeout,
  429 as http_429, declared 5xx as http_5xx, malformed responses as validation,
  and 401/403 as authentication. Do not encode retry policy in the module: the
  pinned process activity decides which typed outcomes are retryable. Return
  only provider-safe messages.
- [ ] GREEN: run `cargo test -p donat-server --test connectors_http` and
  `cargo test -p donat-server connectors`. Expected: local tests demonstrate
  no open redirect or private-address bypass.
- [ ] Commit the HTTP-core slice and obtain judge ACCEPT.

### Task 3: Make operations declarative and idempotency-aware

**Files:**
- Modify: `crates/metadata/src/types.rs`,
  `crates/server/src/connectors/http.rs`,
  `crates/server/src/connectors/mod.rs`
- Test: `crates/server/tests/connectors_http.rs`,
  `crates/metadata/tests/types_serde.rs`

- [ ] Add tests for a metadata-declared operation whose only dynamic values are
  named input bindings; reject arbitrary request URL/method/headers supplied
  in a job/input JSON. Add test coverage for `Idempotency-Key` injection from
  a logical activity's stable key and validation of max-in-flight/rate-limit
  capacity declarations plus an optional scalar `serialize_by` binding.
- [ ] RED: run `cargo test -p donat-server --test connectors_http declarative`.
  Expected: operation validation cannot distinguish declared values from raw
  user request fields.
- [ ] Define a typed `ConnectorOperation` union. For HTTP it contains only a
  named static operation, declared input bindings, static base/path/query/header
  templates, JSON body template, expected response extraction, and retry
  policy. Reject values not declared by its module schema.
- [ ] Validate operations in registry construction and again at job dispatch;
  canonicalize input JSON before calculating its request fingerprint. Attach
  a connector idempotency header only when metadata declares its exact name.
  Include module ABI, operation version, endpoint_identity,
  credential_identity, static capacity/serialization, and an HTTP base-URL digest in the
  non-secret configuration fingerprint consumed by process revisioning.
- [ ] Expose a narrow `registry.execute(instance, operation, input,
  idempotency_key, deadline)` API. It accepts no `reqwest::Request`, raw URL,
  secret, or HTTP client supplied by a GraphQL caller. Capacity is deliberately
  not enforced here: Process Task 5 acquires a shared Postgres permit before
  this API can be called, including a serialization-key permit when the
  operation declares one.
- [ ] GREEN: run `cargo test -p donat-server --test connectors_http` and
  `cargo test -p donat-metadata connectors`.
- [ ] Commit the declarative-operation slice and obtain judge ACCEPT.

### Task 4: Implement the narrow Stripe Checkout module from pinned references

**Files:**
- Add: `crates/server/src/connectors/stripe.rs`,
  `crates/server/tests/connectors_stripe.rs`
- Modify: `crates/server/src/connectors/mod.rs`, `crates/server/Cargo.toml`,
  `knowledgebase/declarative-saas/reference-porting-register.md`
- Add when source is ported: `THIRD_PARTY_NOTICES.md`

- [ ] First record the selected narrow behaviour in the porting register:
  Stripe OpenAPI pinned commit `6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69`,
  `openapi/spec3.json`; stripe-mock pinned commit
  `3f370d112ba55a8a12c09b162547ba32f26b9693`; exact file SHA-256, licence,
  destination file, and a failing local test. Do not copy code before this
  entry exists.
- [ ] Add local tests for `POST /v1/checkout/sessions` form encoding,
  `client_reference_id` UUID-to-string conversion, stable idempotency header,
  success response extraction, 4xx/429/5xx classification, raw-body Stripe
  signature verification, timestamp tolerance, duplicate webhook identity,
  and rejection of a modified raw byte sequence.
- [ ] RED: run `cargo test -p donat-server --test connectors_stripe`.
  Expected: module `stripe` is unavailable.
- [ ] Implement only `checkout.create_session` and
  `checkout.completed_webhook`. Resolve the API key and webhook secret from
  named environment variables; form-encode only documented fields; use the
  registry's fixed Stripe base URL and no user-selected endpoint.
- [ ] Verify `Stripe-Signature` using constant-time HMAC comparison over the
  exact raw body, parse JSON only after verification, and normalize only
  supported event objects into a provider event ID plus command-safe payload.
- [ ] If any source code is copied or substantially translated, add the
  required licence notice and the source file SHA to `THIRD_PARTY_NOTICES.md`.
  The OpenAPI document and stripe-mock are behavioural/test references, not
  runtime dependencies.
- [ ] GREEN: run `cargo test -p donat-server --test connectors_stripe` and
  `cargo test -p donat-server connectors`. Expected: Stripe's narrow contract
  works against the local test server without importing Stripe SDK code.
- [ ] Commit the Stripe slice and obtain judge ACCEPT.

### Task 5: Add signed inbound routing without a public generic webhook API

**Files:**
- Add: `crates/server/src/connector_webhook.rs`,
  `crates/server/tests/connector_webhook.rs`
- Modify: `crates/server/src/main.rs`, `crates/server/src/connectors/mod.rs`

- [ ] Add tests that `POST /v1/connectors/{instance}/webhooks` routes raw bytes
  only to a declared instance, rejects unknown instance before body parsing,
  rejects signature failures, applies the 1 MiB body limit, and returns no
  internal configuration/secret data in failure bodies.
- [ ] RED: run `cargo test -p donat-server --test connector_webhook`.
  Expected: route is absent.
- [ ] Add the one route outside GraphQL and preserve existing enabled API
  surface rules. Read the bounded raw body, construct `InboundRequest`, invoke
  the resolved module's verifier, and return a minimal acknowledgement only
  after the process layer has durably accepted it in the later plan.
- [ ] Until the processes plan adds journal persistence, have a verified webhook
  return a documented `503` without acknowledging delivery; this prevents an
  accepted event from being lost. Do not add an in-memory queue.
- [ ] GREEN: run `cargo test -p donat-server --test connector_webhook` and all
  existing action/event webhook tests to prove their routes did not change.
- [ ] Commit the inbound-route boundary and obtain judge ACCEPT.

### Task 6: Prove the connector boundary before processes integrate it

**Files:**
- Add: `crates/conformance/tests/connectors.rs`,
  `crates/conformance/fixtures/connectors/`
- Modify: `crates/conformance/src/lib.rs`

- [ ] Add conformance cases for metadata validation, unavailable secret env,
  HTTP path encoding, redirect/private-address denial, retry classification
  observable through a controlled local endpoint, Stripe request shape, and
  Stripe signature rejection. Do not claim delivery success until the process
  journal test exists.
- [ ] RED: rebuild and run
  `cargo test -p donat-conformance --test connectors`. Expected: new fixtures
  fail before implementation; record exact public responses.
- [ ] GREEN: run the focused connector suite, `cargo test -p donat-server`,
  `cargo test -p donat-metadata`, and the command conformance suite to confirm
  command transport was not changed.
- [ ] Run `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and inspect all
  porting-register/notice changes.
- [ ] Commit any test-proven correction and obtain final judge ACCEPT before
  starting durable processes.
