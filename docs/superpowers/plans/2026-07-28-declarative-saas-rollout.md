# Declarative SaaS Runtime Rollout Implementation Plan

> **For Codex:** Execute every checkbox in order. Each implementation slice
> starts RED, ends GREEN, and is committed separately. Preserve focused test
> evidence for every slice; run one independent code review only after the
> complete cohesive feature range is ready for merge, handoff, or readiness.

**Goal:** Add a declarative SaaS automation layer to the existing Donat data
plane without duplicating Hasura-compatible views, permissions, CRUD, REST,
or GraphQL semantics. Operators declare rules, commands, connector instances,
and durable processes in YAML; the one Donat Rust binary validates, compiles,
and executes them.

**Architecture:** The dependency-free `donat-rules` crate defines the shared
typed expression language. `commands.yaml` turns explicit, role-authorized
data mutations into one Postgres statement per GraphQL root. Compiled
connector modules own HTTP and provider contracts, while `processes.yaml`
persists an at-least-once journal and drives commands and connector activities
through short, leased database transactions. All definitions are immutable at
runtime: `donat migrate --metadata-dir` reconciles deploy-time state and the
serving binary only reads metadata and journal rows.

**Tech stack:** Rust workspace, Axum, Tokio, Postgres 16, serde YAML,
`reqwest`, `insta`, native conformance harness, CEL v0.25.2 as a behaviour
reference only.

## Scope and non-negotiable invariants

- The normal Donat data plane remains authoritative. No feature creates a
  second view DSL, CRUD API, permission system, or metadata-management API.
- Every request and internal command runs under an explicit classic role. No
  `admin` role, implicit privilege, `X-Donat-Admin-Secret` bypass, or
  user-controlled `run_as_role` exists.
- A Postgres command compiles to exactly one statement for each GraphQL root;
  the result is assembled as `json` in Postgres. Commands are Postgres-only in
  this release and must be absent from non-Postgres schemas.
- The serving binary never applies DDL or reconciles desired state. Migration
  and metadata validation are deploy-time subcommands.
- Processes use at-least-once delivery: commit a claim/lease before external
  I/O, accept a result only for the active lease token, distinguish
  schedule-to-start from start-to-close timeout, and make every connector
  request idempotent at its provider boundary with a logical activity key that
  never changes between retries or lease takeovers.
- Every active process revision pins its runtime ABI, command fingerprints,
  connector module/operation versions, endpoint/credential identities, and
  non-secret configuration fingerprint. A binary that cannot support a pinned
  revision is fenced from claiming it; deployments drain compatible workers.
- Connector configuration contains stable identities and environment variable
  names only. Resolved secret values never appear in logs, metadata, process
  definition revisions, errors, or catalog fingerprints. HTTP base URLs enter
  a fingerprint only as a digest.
- Connector capacity and rate limits are coordinated through Postgres across
  every binary instance, never a per-worker in-memory semaphore. An operation
  may also declare a typed same-resource serialization key enforced by the same
  durable reservation mechanism.
- A process can be recovered or cancelled only through a typed declarative
  command signal and declared transition. There is no generic process-admin
  API, CLI, role bypass, or mutable runtime definition.
- Operational visibility is read-only: a redacted CLI timeline and offline
  history verifier may read pinned journal data, but never invoke a command or
  connector, mutate history, or publish an HTTP management route.
- New behaviour begins with a native conformance fixture and focused test;
  fixture response bodies, GraphQL codes, paths, and statuses are contracts.

## Delivery order

```text
Rules crate + metadata validation
        |
        v
Commands: catalog relation kind -> schema/IR -> one-statement SQL
        |
        +--------------------------+
        v                          v
Compiled connector registry     Process metadata/journal/worker
        |                          |
        +------------+-------------+
                     v
       Stripe Checkout activity + signed inbound completion
                     |
                     v
       Full deploy/upgrade/retry/idempotency conformance run
```

Execute the four detailed plans in this exact order:

1. [Rules](2026-07-28-declarative-rules.md)
2. [Commands](2026-07-28-declarative-commands.md)
3. [In-binary connectors](2026-07-28-in-binary-connectors.md)
4. [Durable processes](2026-07-28-declarative-processes.md)

The process plan deliberately comes last because its worker consumes the
typed rule evaluator, starts commands under a fixed role, and invokes the
connector registry. It also contains the explicit cross-plan activation of a
command's `start_process` effect: Commands implements the bounded effect type
and core idempotency first; Processes adds the durable outbox table and enables
that effect only once a pinned process definition can receive it. The connector
plan establishes the registry and HTTP safety boundary before process work
binds jobs to it.

## Deliberate Phase-1 deferrals

- No BPMN import/export, visual workflow editor, dynamic fan-out/join, child
  workflows, automatic saga generation, or generic replay/reset. The state
  machine must first prove its revision and activity contracts on a small
  deterministic grammar.
- No generic runtime workflow GraphQL/REST/MCP query, cancel, retry, or
  operator recovery API. Customer-visible state belongs in ordinary tracked
  business tables; recovery is a declared command signal under an explicit
  role. The read-only CLI described above is diagnosis, not a management API.
- No activity heartbeat protocol, OAuth/vault connector manager, dynamic
  plugin loading, or connector marketplace. Phase 1 uses bounded HTTP calls,
  timers/signals for long interactions, env-provided secrets, and compiled
  modules.
- Decision tables support only first and unique policies, named condition maps,
  stable row IDs, and metadata test cases. Full DMN, hit-policy variants, and
  a decision-table UI remain separate product work.

## Shared implementation contract

Every implementation plan must preserve the following public boundaries.

```rust
// Metadata is only deserialized from the deploy-time directory.
pub struct Metadata {
    pub rules: RulesMetadata, // one rules.yaml wrapper: rules + decision_tables
    pub commands: Vec<CommandDefinition>,
    pub connectors: Vec<ConnectorInstance>,
    pub processes: Vec<ProcessDefinition>,
    // existing Donat metadata fields remain unchanged
}

// Rules are compiled once and shared by commands and the process worker.
pub struct RuleCatalog { /* typed ASTs, bindings, decision tables */ }

// Commands are GraphQL mutation roots only for allowed explicit roles.
pub enum MutationRoot {
    Command { alias: String, command: CommandMutation },
    // existing variants
}

// Connector modules are compiled into the server binary.
pub trait ConnectorModule: Send + Sync {
    fn definition(&self) -> ConnectorDefinition;
    fn validate_config(&self, config: &ConnectorConfig) -> Result<(), ConnectorConfigError>;
    fn validate_operation(&self, operation: &ConnectorOperation) -> Result<(), ConnectorConfigError>;
    async fn execute(
        &self,
        operation: ValidatedOperation,
        request: ConnectorRequest,
    ) -> Result<ConnectorSuccess, ConnectorFailure>;
    fn verify_webhook(&self, request: &InboundRequest) -> Result<VerifiedWebhook, WebhookRejection>;
}

pub enum ConnectorErrorClass {
    Transport, Timeout, Http429, Http5xx,
    Authentication, Validation, Permanent, Invariant,
}

// Only execute failures use this class. Configuration and inbound-webhook
// outcomes never enter a process activity retry_on/on_error route.
pub struct ConnectorFailure {
    pub class: ConnectorErrorClass,
    pub code: &'static str,
    pub safe_message: String,
}
```

The exact concrete types may be refined while implementing the relevant plan,
but each refinement must retain these authority boundaries and update the
corresponding spec before code changes.

## Deploy and compatibility gates

- `load_metadata_dir` accepts an absent new file as an empty section, so all
  existing metadata directories remain valid without conversion.
- `validate --metadata-dir` reports all static inconsistencies before serving:
  duplicate names, invalid references, role/permission gaps, unavailable
  relation kinds, invalid rule typing, connector config, and process graph
  errors.
- `migrate --metadata-dir` applies user DDL migrations, built-in `donat`
  catalog migrations, event-trigger reconciliation, then declarative SaaS
  definition reconciliation. No reconciliation is called by `serve`.
- A process instance stores a pinned definition revision. Reconciliation may
  register a later revision and retire future starts; it cannot rewrite active
  instance semantics.
- Upgrade tests must prove an active old revision completes under its stored
  definition, that a start after deployment uses the new revision, and that an
  incompatible binary cannot claim a revision it cannot execute.
- Migration refuses to remove or incompatibly replace a command/catalog
  dependency still referenced by a non-terminal process revision. Database
  migrations remain backward compatible until those instances complete.

## Reference-port admission gate

The only approved upstream behaviour references are listed in
[`knowledgebase/declarative-saas/reference-porting-register.md`](../../../knowledgebase/declarative-saas/reference-porting-register.md).
Before copying or porting any implementation, record all of the following in a
commit:

1. immutable upstream URL, commit SHA, exact source path, and file SHA-256;
2. licence and required attribution in `THIRD_PARTY_NOTICES.md` when source is
   copied or substantially derived;
3. destination module and the narrowed behaviour being ported;
4. a failing local unit, snapshot, or conformance test that defines Donat's
   contract before the port;
5. a human review of the upstream delta and the generated local diff.

The source tree is a reference, not a dependency or licence shortcut. Port
only the smallest independently tested algorithm. Do not import an upstream
runtime, workflow server, Stripe SDK, CEL evaluator, code generator, or
microservice.

## Commit and review protocol

After each completed implementation slice:

1. Inspect the staged diff and run the focused RED/GREEN commands stated in
   the relevant plan.
2. Commit only that slice.
3. Continue to the next independently testable slice when its focused evidence
   is green; a commit is not a review boundary.
4. Before merging, handing off, or declaring the complete cohesive feature
   range ready, request one independent code review over the full range,
   including the changed contract, files, and fresh verification evidence.
   Address material findings with a failing regression test and fresh
   verification before completion.

## Final acceptance sequence

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace --exclude donat-conformance`
- [ ] `cargo build -p donat-server --bin donat`
- [ ] `docker compose -f docker-compose.conformance.yml up -d --wait`
- [ ] `cargo test -p donat-conformance --test rules`
- [ ] `cargo test -p donat-conformance --test commands`
- [ ] `cargo test -p donat-conformance --test connectors`
- [ ] `cargo test -p donat-conformance --test processes -- --test-threads=1`
- [ ] `make conformance`
- [ ] Review every SQL snapshot with `cargo insta review`; accept only
  intentional one-statement SQL changes.
- [ ] Run `git diff --check origin/main...HEAD` and obtain one independent
  code review over the full diff and fresh verification evidence.
