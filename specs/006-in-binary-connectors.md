# Spec 006 — In-binary connector modules

Status: proposed. A connector is a Rust module compiled into donat, not a
microservice, a dynamically downloaded plugin, or a generic arbitrary-URL
webhook.

Supersession note (2026-07-29): Spec 007 Sections 5.3 and 7 replace two
contracts in this earlier proposal. The blanket stable-key/idempotency-header
rule is now the closed, evidence-backed `ReadOnly` or per-compiled-step
`ProviderIdempotent` model; `ReadOnly` is headerless, and every side-effecting
step fixes its own binding, scope, minimum retention, and clock margin. The
broad `ConnectorModule::execute`/`verify_webhook` sketches in Sections 3 and 6
are historical migration context, not the implementation authority:
provider-specific execution uses the sealed compiled-step processor ABI, while
the server retains all transport, codec, credential, crypto, clock, and
control capabilities. Spec 006's static registry, fixed-origin egress, closed
errors, redaction, no-plugin, and temporary verified-webhook `503` boundaries
remain in force.

## 1. Goal and topology

Connectors make external systems available to durable processes. The first
modules are http and stripe; subsequent modules follow the same contract. A
module defines its typed operations, credential shape, idempotency behavior,
outbound request mapping, inbound webhook verification, redaction rules, and
contract-test fixtures. Deployment metadata selects and configures a compiled
module; it cannot upload code.

The runtime has two modes:

- durable activity — the normal mode. A process creates an activity job, then
  the connector worker executes it after commit and records a result;
- existing synchronous GraphQL Action — retained unchanged for legacy
  Hasura-compatible webhooks. It is not a workflow mechanism and must not be
  used to create a durable business side effect.

This makes create_checkout_session safe: a checkout_order process queues Stripe
work, persists the returned URL, and waits for a signature-verified Stripe
signal. It never invokes Stripe from an open order transaction.

## 2. Deployment metadata and registry

connectors.yaml names instances of modules that the binary already knows.

    - name: stripe
      module: stripe
      config:
        endpoint_identity: stripe_api_2025_06_30
        credential_identity: stripe_primary
        secret_key: { value_from_env: STRIPE_SECRET_KEY }
        webhook_secret: { value_from_env: STRIPE_WEBHOOK_SECRET }
        api_version: "2025-06-30.basil"
      operations:
        - name: create_checkout_session
          capacity:
            max_in_flight: 16
            rate_limit: { permits: 80, per: 1m, burst: 20 }

    - name: logistics_api
      module: http
      config:
        endpoint_identity: logistics_prod_eu_2026_07
        credential_identity: logistics_primary
        base_url: { value_from_env: LOGISTICS_BASE_URL }
        headers:
          - name: Authorization
            value_from_env: LOGISTICS_TOKEN
      operations:
        - name: create_shipment
          method: POST
          path: /v1/shipments
          success_statuses: [200, 201, 202]
          idempotency: { header: Idempotency-Key }
          capacity:
            max_in_flight: 8
            rate_limit: { permits: 20, per: 1s, burst: 8 }
            serialize_by: { input: order_id }

The compiled registry exposes a ConnectorDefinition with a module name,
semantic version, runtime ABI, JSON-schema-like configuration description,
operation input and output types, credential redaction paths, and webhook
verifier. Metadata validation rejects an unknown module, unavailable operation,
invalid environment-variable name, invalid config, duplicate instance, or
connector operation referenced by a process but not enabled here. Server
startup resolves variables and rejects a required missing value without
revealing a secret. endpoint_identity and credential_identity are required
non-secret deployment labels. A protocol-facing endpoint or credential-class
change must change its corresponding label, so revision review cannot miss it.

The http module has an allowlisted configured base_url; process input may
supply only a validated path, query, headers from the declared template, and
body fields. It cannot supply a host, scheme, port, raw URL, or arbitrary
header name. This prevents the connector from becoming an SSRF escape hatch.

## 3. Rust module contract

The server crate owns a registry and each module implements a narrow async
contract equivalent to:

    async fn execute(
        &self,
        operation: &str,
        input: serde_json::Value,
        context: ActivityContext,
    ) -> Result<ConnectorResult, ConnectorFailure>;

    fn verify_webhook(
        &self,
        request: VerifiedWebhookRequest,
    ) -> Result<InboundSignal, WebhookRejection>;

ActivityContext carries a stable idempotency key, deadline, trace ID, and
redacting logger. It does not carry database credentials or a mutable process
instance. The worker owns persistence of attempts and results; a module owns
only protocol translation.

Stripe sends the activity idempotency key in the Idempotency-Key header.
Webhook verification checks Stripe's signature against the unmodified raw
request body before JSON parsing. Connector logs store request and response
metadata with every configured credential and known sensitive response path
redacted.

## 4. Failure and test contract

`ConnectorErrorClass` is a closed activity-execution enum with exactly these
values: `transport`, `timeout`, `http_429`, `http_5xx`, `authentication`,
`validation`, `permanent`, and `invariant`. Every failed `execute` returns a
`ConnectorFailure { class: ConnectorErrorClass, code, safe_message,
retry_after }`; modules cannot return an ad-hoc class or a retry decision. The
process worker is the only owner of policy: `retry_on` accepts only
`transport`, `timeout`, `http_429`, and `http_5xx`; a matching failure is
retried only while attempts remain. Every other class, every retryable class
not selected by `retry_on`, and the worker-generated `retry_exhausted` outcome
are sent through the process activity's declared `on_error` routes and
mandatory fallback (Spec 005). `invariant` is always a non-retryable
activity failure and must therefore reach that routing contract. No connector
silently turns a non-2xx response into success.

Configuration failures are not `ConnectorErrorClass` values: static metadata
configuration fails `validate`/`migrate`, and unavailable required environment
values prevent the affected connector from starting before it can execute an
activity. Inbound webhook outcomes are likewise outside activity routing:
unknown instance, body-limit rejection, invalid signature, malformed verified
payload, duplicate provider event, unmatched or ambiguous correlation,
guard-false, and unexpected process state are bounded ingress/audit outcomes.
They may create zero or one durable process signal according to Spec 005; they
never enter an activity's `retry_on` or `on_error` table.

| Behavior | First failing test | Test double / reference |
| --- | --- | --- |
| HTTP host cannot come from input | metadata + module unit test | invalid URL template fixture |
| HTTP retry preserves idempotency key | process integration test | local recording HTTP server |
| Stripe checkout request shape | connector contract test | stripe-mock image or binary |
| Stripe non-2xx error mapping | connector contract test | stripe-mock error response |
| Stripe webhook signature failure | endpoint integration test | signed raw-body fixture |
| Duplicate Stripe event | process conformance fixture | fixed provider event ID |
| Credentials are absent from logs | unit test | synthetic secret sentinel |

Tests never call the live Stripe API. Their fixtures are Donat-owned or, where
an upstream test fixture is copied under a compatible license, retain the
source, commit, and notice next to the fixture.

## 5. Reference porting plan

| Upstream | Immutable revision | Files/behavior used | License and treatment |
| --- | --- | --- | --- |
| [stripe/openapi](https://github.com/stripe/openapi/tree/6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69) | 6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69 | endpoint schemas and request/response contract for enabled Stripe operations | MIT; schemas or generated artifacts may be imported only with the required notice and an explicit file list |
| [stripe/stripe-mock](https://github.com/stripe/stripe-mock/tree/3f370d112ba55a8a12c09b162547ba32f26b9693) | 3f370d112ba55a8a12c09b162547ba32f26b9693 (v0.201.0) | black-box HTTP contract server for tests | MIT; invoked as a test dependency, not compiled into Donat |
| [airbytehq/airbyte](https://github.com/airbytehq/airbyte/tree/32ec364b51e96f748e6aea28bbbac2dd9aac8bd9) Source Stripe | 32ec364b51e96f748e6aea28bbbac2dd9aac8bd9 | declarative connector acceptance-test ideas and edge cases | ELv2; behavior-only reference. No source or fixture is copied into Donat |
| Donat crates/server/src/action.rs, events.rs, and cron.rs | current Donat revision | webhook headers, timeout behavior, durable retry and log patterns | native extension reference |

The Stripe module is independently implemented in Rust using the table above.
Before any upstream code or fixture lands, its change must extend this table
with exact copied paths, destination paths, the preserved notice, and the
corresponding Donat tests. The same requirement applies to every later module.


## 6. Registry, configuration, and module boundary

The registry is compiled at build time in crates/server/src/connectors. Its
built-in entries are http and stripe. A module has a stable module name,
semantic version, runtime ABI, and version for every operation. A deployment
metadata entry selects an already compiled module and may enable only
operations that the module advertises. It cannot name a filesystem path,
shared library, container image, package URL, or untrusted code blob. The
process revision pins this module/operation tuple. A binary cannot claim an
activity whose pinned ABI or operation version it does not support.

The narrow Rust boundary is:

~~~rust
pub trait ConnectorModule: Send + Sync {
    fn definition(&self) -> ConnectorDefinition;
    fn validate_config(&self, config: &serde_json::Value) -> Result<ValidatedConfig, ConfigError>;
    fn validate_operation(&self, operation: &str, input_schema: &TypeShape)
        -> Result<(), ConfigError>;
    async fn execute(
        &self,
        operation: &str,
        input: serde_json::Value,
        context: ActivityContext,
    ) -> Result<ConnectorResult, ConnectorFailure>;
    fn verify_webhook(&self, raw: VerifiedWebhookRequest)
        -> Result<VerifiedInboundEvent, WebhookRejection>;
}

pub enum ConnectorErrorClass {
    Transport,
    Timeout,
    Http429,
    Http5xx,
    Authentication,
    Validation,
    Permanent,
    Invariant,
}

pub struct ConnectorFailure {
    pub class: ConnectorErrorClass,
    pub code: &'static str,
    pub safe_message: String,
    pub retry_after: Option<std::time::Duration>,
}

async fn execute_activity(
    module: &dyn ConnectorModule,
    operation: &str,
    input: serde_json::Value,
    context: ActivityContext,
) -> Result<ConnectorResult, ConnectorFailure>;
~~~

The worker, not the module, owns activity-job persistence, leases, retries,
and process events. A module receives immutable input, deadline, trace ID,
idempotency key, and a redacting logger. It does not receive a database pool,
a mutable process instance, an arbitrary role, or unfiltered HTTP request
headers.

value_from_env is the only secret form in connectors.yaml. Metadata validation
checks configuration shape and variable names without resolving a secret. At
server start, a required missing environment variable prevents that connector
instance from starting and reports only its variable name. Resolved values,
Authorization headers, Stripe keys, webhook secrets, and any configured
redaction path are removed from structured logs, transition logs, and
GraphQL errors.

A connector configuration fingerprint contains module and operation versions,
runtime ABI, enabled operations and capacity/serialization policies, API version, network policy,
endpoint_identity, credential_identity, non-secret literal configuration, and
environment-variable names. For HTTP, it additionally includes only a SHA-256
fingerprint of the resolved base URL, never its raw resolved value. It never
includes a resolved secret. Spec 005 includes this fingerprint in a process
definition revision. Rotating a secret changes runtime credentials without
serializing it into a deployment record; changing an endpoint, credential class,
capacity, serialization, or protocol-facing configuration creates a new process revision.

## 7. Declarative HTTP connector profile

The http module is generic transport, not an arbitrary request proxy. Every
enabled operation is declared at deploy time:

~~~yaml
- name: logistics_api
  module: http
  config:
    endpoint_identity: logistics_prod_eu_2026_07
    credential_identity: logistics_primary
    base_url: { value_from_env: LOGISTICS_BASE_URL }
    network_policy: private_allowed
    headers:
      - name: Authorization
        value_from_env: LOGISTICS_TOKEN
  operations:
    - name: create_shipment
      method: POST
      path: /v1/shipments/{input.order_id}
      path_parameters:
        order_id: uuid!
      query: []
      headers:
        - name: Idempotency-Key
          value: { activity: idempotency_key }
      body:
        order_id: { input: order_id }
        address: { input: address }
      success_statuses: [200, 201, 202]
      response:
        shipment_id: { json_pointer: /id, type: string! }
        tracking_url: { json_pointer: /tracking_url, type: string }
      idempotency: { header: Idempotency-Key }
      error_classification:
        http_5xx: [500, 502, 503, 504]
      capacity:
        max_in_flight: 8
        rate_limit: { permits: 20, per: 1s, burst: 8 }
        serialize_by: { input: order_id }
~~~

method is one of GET, POST, PUT, PATCH, or DELETE. path is an absolute path
with statically named, percent-encoded path parameters. It cannot contain a
scheme, authority, userinfo, fragment, dot-segment, or runtime-computed host.
Query keys, header names, JSON pointers, request body keys, success statuses,
and error-classification statuses are static metadata. Input may fill only a declared value
slot with a type-compatible value.

The client follows no redirects. It applies an operation deadline from the
process activity, limits a request body, response body, and raw webhook body to
1 MiB, and revalidates the resolved destination on every connection.
public_only is the default network policy; private_allowed is a deploy-time
opt-in for explicitly configured internal systems. Neither a GraphQL caller nor
a process event can select the policy, base URL, host, port, or DNS target.
This preserves useful internal HTTP integrations without turning process input
into an SSRF primitive.

An HTTP operation used by a durable activity must declare an idempotency
header. The validator rejects a durable process reference to an operation
without it. The same stable key is sent for every lease takeover and retry.
Every durable operation must also declare capacity.max_in_flight and a bounded
rate_limit. The process worker enforces these limits with a shared Postgres
reservation, not a per-worker semaphore; the connector module merely receives
an activity after that reservation is acquired.
An operation may additionally declare serialize_by for one typed scalar input
field. The worker derives a canonical, non-secret serialization key and permits
only one running activity for that connector-instance/operation/key across all
binaries. It is intended for a provider resource such as an order or customer,
not for a user-defined expression or an authorization decision.

## 8. Stripe Phase-1 module

The Stripe module exposes only create_checkout_session as an outbound
activity and checkout.session.completed as an inbound verified event in Phase
1. It is deliberately small; subscriptions, refunds, Connect, and arbitrary
Stripe resource access are separate future modules or operations.

create_checkout_session accepts a typed input containing mode, success_url,
cancel_url, client_reference_id as uuid, and one or more line_items with a Stripe price
identifier and positive quantity. It returns id, url, status, and expires_at.
It sends the process activity key as the Stripe Idempotency-Key header. The
input never contains secret_key or webhook_secret. The module serializes the
UUID in Stripe's string field and parses the verified client_reference_id back
to UUID before exposing it to a process correlation mapping.

The inbound verifier receives the exact raw body and all request headers before
any JSON parser runs. It verifies Stripe-Signature with the configured webhook
secret, extracts event.id as provider_event_id, and exposes only the typed
event name and permitted data fields to the process signal mapper. A signature
failure never stores an unverified payload in process state.

The configured api_version is a deployment pin. A metadata revision changes
when its value changes, preventing an old in-flight process from silently using
a new Stripe contract. Live Stripe API calls are forbidden in every automated
test.

## 9. Errors, retries, and observability

The first seven rows below enumerate all eight `ConnectorErrorClass`
activity-execution values: the HTTP status row contains `validation`,
`authentication`, and `permanent`, while the final activity row is the explicit
`invariant` class. The remaining rows are explicitly outside activity routing
and must not be converted into `retry_on` or `on_error` values.

| Condition | Connector classification | Worker behavior |
| --- | --- | --- |
| DNS, TLS, connection reset | transport | schedule only when retry_on includes transport |
| connector deadline or HTTP 408 | timeout | schedule only when retry_on includes timeout |
| HTTP 429 | http_429 | honor later Retry-After and retain key when listed |
| declared HTTP 5xx | http_5xx | schedule only when retry_on includes http_5xx |
| HTTP 400, 401, 403, 404, unsupported status | validation, authentication, or permanent | append typed failure event without implicit retry |
| malformed declared JSON response | validation | preserve redacted protocol diagnostic |
| module invariant violation | invariant | append typed failure event; mandatory process fallback remains available |
| module configuration invalid | outside activity: ConfigError | `validate`/`migrate` rejects it, or server startup refuses the instance |
| invalid inbound signature or body | outside activity: WebhookRejection | audit verification outcome; no process signal |
| duplicate verified provider event | outside activity: ingress outcome | return accepted response; no second signal |

ConnectorResult includes a typed success value or a redacted diagnostic with
classification, provider status, retry-after, and safe correlation IDs. It
does not contain raw secrets, unbounded bodies, or request authorization.
Metrics are keyed by connector instance, operation, classification, capacity
wait, and attempt; payload values are not metric labels.

A connector is callable only from a Spec 005 durable activity in Phase 1.
There is no generic GraphQL mutation such as call_http and no conversion of a
connector operation into a legacy synchronous Action. Existing Actions retain
their existing HTTP semantics and tests but are not reused as a connector
runtime.

## 10. Full contract-test matrix

| Test ID | Level | Required proof |
| --- | --- | --- |
| connector_registry_is_closed | server unit | unknown module/path/image is rejected by metadata validation |
| connector_env_is_redacted | server unit | missing name is reported; secret sentinel is absent from all errors/log JSON |
| connector_revision_fingerprint_is_complete | migrate validation | module ABI, operation version, identities, capacity, and HTTP endpoint digest change the revision |
| http_rejects_dynamic_authority | metadata unit | scheme, host, port, userinfo, fragment, dot segment, and dynamic header key fail |
| http_percent_encodes_path_value | connector unit | an input UUID/string cannot escape its declared path segment |
| http_disables_redirect | recording HTTP server | 3xx never causes a second outbound host request |
| http_bounds_payloads | connector unit | request, response, and webhook bodies over 1 MiB are rejected before binding |
| http_requires_idempotency_for_process | process metadata test | process reference to headerless operation fails validation |
| connector_capacity_is_worker_global | two-process integration | configured operation capacity holds across independent engine processes |
| connector_serialization_key_is_global | two-process integration | same configured provider-resource key never runs concurrently; different keys may run within capacity |
| stripe_checkout_contract | connector contract | request body/header and typed result match stripe-mock |
| stripe_retry_holds_idempotency_key | two-attempt stub | both requests carry exactly the same key |
| stripe_signature_precedes_json | endpoint integration | invalid signature with malformed body creates no process signal |
| stripe_duplicate_event_is_safe | process integration | same event.id creates one inbound row and one transition |
| connector_error_class_is_closed | connector plus process integration | only the eight declared classes reach activity retry/routing; invariant takes the declared failure path |
| connector_config_and_webhook_are_not_activity_failures | metadata plus endpoint integration | configuration prevents startup and inbound audit outcomes never enter retry_on/on_error |
| connector_error_never_leaks_secret | unit and conformance | body, header, and environment sentinels do not appear in response/log fixture |

The stripe-mock contract suite is a separately marked integration test and
never requires a Stripe account. A failure to start the local mock is a test
infrastructure failure, not a reason to fall back to live network calls.

## 11. Reference extraction ledger

| Upstream | Immutable source paths | License | Exact allowed use and Donat destination |
| --- | --- | --- | --- |
| stripe/openapi at 6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69 | openapi/spec3.json and openapi/fixtures3.json | MIT | enabled Checkout request and response shape; a copied schema or generated artifact requires its notice and path recorded beside crates/server/src/connectors/stripe.rs |
| stripe/stripe-mock at 3f370d112ba55a8a12c09b162547ba32f26b9693 | main_test.go, server/, spec/ | MIT | black-box contract server only; no Go source is compiled into Donat |
| airbytehq/airbyte Source Stripe at 32ec364b51e96f748e6aea28bbbac2dd9aac8bd9 | airbyte-integrations/connectors/source-stripe/manifest.yaml, acceptance-test-config.yml, integration_tests/, unit_tests/ | ELv2 | behavior, fixture categories, and acceptance-test ideas only; no Airbyte code or fixture is copied |
| Donat action, cron, and event modules | current repository paths named in Section 5 | Apache-2.0 | timeout/header resolution, durable retry logs, and exact existing webhook behavior |

Before a source file is copied or a generated artifact is checked in, the
implementation change must add a register entry with the upstream path,
checksum, license notice file, destination, reviewer, and the named Donat test
that first failed before the port.


## 12. Component ownership boundaries

| Area | Required ownership | Prohibited shortcut |
| --- | --- | --- |
| Metadata | crates/metadata connector declarations and type validation | accepting a raw URL or operation from GraphQL input |
| Registry and protocol code | crates/server/src/connectors modules compiled into donat | dynamic libraries, downloaded code, or child microservices |
| Durable invocation | Spec 005 activity worker | invoking a connector from command CTE or synchronous action path |
| Webhook ingress | axum connector route plus module verifier | parsing provider JSON before signature verification |
| HTTP safety | dedicated no-redirect reqwest client and resolved-destination policy | using the legacy Action client with arbitrary caller URL |
| Stripe proof | stripe-mock contract suite plus Donat-owned signed fixtures | live Stripe credentials in CI or developer tests |
| Provenance | reference-porting-register and THIRD_PARTY_NOTICES.md when needed | copying OpenAPI, Airbyte, or mock fixtures without a per-file record |

A new connector module starts as a Rust crate-local implementation and tests,
then receives registry admission only after its config schema, operation types,
error classification, redaction map, idempotency behavior, inbound verifier
when applicable, and reference ledger are all present.
