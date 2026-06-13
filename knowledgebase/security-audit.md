---
type: reference
created: 2026-06-13
---

# Security & dependency audit (2026-06-13)

Threat model: dist-api is an **internal microservice component** (sidecar /
behind a mesh / trusted network). TLS termination and edge auth are assumed
to live in front of it. Findings are ranked for that model; "network-edge"
items are downgraded accordingly.

## Resolution status (2026-06-13)

- **Admin/`run_sql` fail-open (findings 1b, 2): RESOLVED by removal.** The
  entire runtime admin/management API was deleted and the admin role removed
  — there is no admin-over-HTTP surface and no permission-bypass role at all.
- **Deep-nesting stack-overflow DoS (#1): FIXED.** `gql::query_too_deep`
  rejects queries past `MAX_QUERY_DEPTH` (100) before the recursive parser
  runs, on `/v1/graphql` and the ws path.
- **`resolve_url_template` panic (#3): FIXED.** Rewritten to be panic-free
  (anchors `}}` after `{{`) and to substitute all occurrences.
- **Non-constant-time secret compare (#4): FIXED.** `gql::ct_eq` is used for
  the `X-Hasura-Admin-Secret` check in `resolve_session`.
- **Metadata `!include` cycle → overflow: FIXED.** The loader tracks the
  include chain and returns `LoadError::IncludeCycle`.
- **Aggregate-function injection in `order_by` (SEC-01): FIXED (2026-06-13).**
  The relationship-aggregate `order_by` path interpolated an *unvalidated*
  function name into SQL; it is now checked against the shared
  `AGGREGATE_COLUMN_OPS` allowlist. See the correction under "SQL generation"
  below — this audit's original "injection-safe" verdict had missed it.
- **`cargo audit`: ADDED to CI** (`rustsec/audit-check`); current scan shows
  no vulnerabilities (the yanked `time` transitive dep was bumped).
- **Still open (non-security / by design):** serde_yaml deprecated (tech
  debt); graphql-parser unmaintained (DoS now mitigated by the depth guard);
  no `tower_http::catch_panic` layer (the fatal overflow path is gone; only
  non-fatal handler panics remain, which drop a single connection); TLS
  off by design (internal). Remote-schema-from-YAML needs boot-time upstream
  introspection (a feature gap, not security) — see PLAN.md.

The original ranked findings below are kept for history.

## SQL generation — verdict: injection-safe (one miss, since fixed)

> **Correction (2026-06-13):** the "no injection" verdict below missed one
> path. The aggregate function name in an `order_by` over a *relationship
> aggregate* was **not** validated against the allowlist — it flowed from a
> GraphQL object key straight into sqlgen's `format!("{function}(..)")`
> (`crates/sqlgen/src/lib.rs:455`), letting a client invoke arbitrary
> single-arg SQL functions (e.g. `pg_sleep`). GraphQL's Name grammar
> (`[A-Za-z_][A-Za-z0-9_]*`) blocked quote/semicolon breakout, so the impact
> was arbitrary-function-call (DoS / info-disclosure), not statement
> breakout; the v1 JSON `order_by` path does not handle relationship
> aggregates and was unaffected. Fixed in `plan.rs::parse_order_by` by
> validating against the shared `AGGREGATE_COLUMN_OPS` allowlist (the same
> one the `_aggregate_fields` selection path already used), with regression
> tests in `crates/schema/tests/planner.rs`.

Audited every literal/identifier path in `crates/sqlgen`. No GraphQL-path
injection found:
- All user-controlled values (query variables, session variables, where /
  order_by / limit / insert / update args) are rendered through
  `scalar_sql` → `quote_lit` (`'` doubled) or `quote_ident` (`"` doubled).
  Correct under `standard_conforming_strings = on` (PG default).
- All identifiers (table/column/alias names) come from catalog introspection
  or are validated against it at plan time — unknown columns error out.
- The raw `format!("{name}(...)")` interpolations must come from trusted
  sources: aggregate op (validated against the shared `AGGREGATE_COLUMN_OPS`
  allowlist — note the `order_by`-relationship-aggregate path originally
  missed this check; see the correction above), PostGIS function (fixed
  `st_op(...)` constants), computed-field function (operator-defined metadata
  = trust-equivalent to DDL).
- Session-variable values become `Scalar::Json` → `quote_lit`; never used as
  identifiers.

Caveat (not security): literals are **inlined**, not bound as `$N`
parameters (`crates/sqlgen/src/lib.rs` header calls parameterized execution
a future refactor). Safe today via escaping, but: (a) defeats PG statement
plan caching → perf cost on hot paths; (b) generated SQL logged at
`debug` (`gql.rs:454`, `521`, `ops.rs:221`) contains raw user data — keep
debug logging off in prod.

## Ranked findings

### 1. Deep nesting → stack overflow (DoS) — FIX (matters internally)
`graphql_parser::parse_query` (recursive descent) and the planner's
relationship recursion have **no depth limit**. axum's default 2MB body cap
still allows ~hundreds-of-thousands of nesting levels in one request —
enough to overflow the stack. A stack overflow in Rust **aborts the whole
process**, so a single malformed/buggy-client query kills the service for
everyone. Fix: reject queries past a max depth (e.g. 30–50) before/while
parsing, and a max-aliases/complexity cap.

### 1b. Admin role widened fail-open to the WHOLE data plane — UPDATE (2026-06-13)
After the Hasura admin role landed, "no admin secret configured" now means
**every no-role request is admin** on `/v1/graphql` too, not just the
metadata API. Verified live: new binary, no secret, `{ __typename }` with no
role → `{"data":...}` (full access); `run_sql` with no auth → executes
arbitrary SQL. With a secret set it is fail-CLOSED (verified: no-secret
request → access-denied; valid secret + no role → admin). This is faithful
Hasura behavior ("no admin secret = everything is admin"), but it means: any
shared/networked deployment MUST set `--admin-secret` (or be strictly
network-isolated), otherwise all data and arbitrary SQL are exposed
unauthenticated. Before the admin change a no-role request was denied, so
this is a real posture change to flag in deploy docs.

### 2. `run_sql` / metadata API is fail-OPEN — FIX or document hard
`/v1/query`, `/v2/query`, `/v1/metadata` (includes `run_sql` = arbitrary
SQL, and `clear_metadata`) are gated only by `check_admin_secret`, which
returns `Ok(())` when no `admin_secret` is configured (`main.rs:216`). So
**with no `--admin-secret`, anyone who reaches the port runs arbitrary SQL**
— bypassing all role permissions. The `make run` fixture mode and the
conformance harness run secret-less by design. For any shared deployment:
require `admin_secret` for these routes (fail-closed), or guarantee network
isolation as the boundary. Today the boundary is implicit.

### 3. `resolve_url_template` can panic — FIX (cheap)
`crates/server/src/remote.rs`: substitutes only the first `{{VAR}}`, and an
input where `}}` precedes `{{` slices `raw_url[start+2..end]` with
`start > end` → panic. Reachable from remote-schema metadata. Also a
functional bug (multiple templates in one URL). (Already in PLAN.md
known-issues.)

### 4. Non-constant-time admin-secret compare — LOW (internal)
`check_admin_secret` uses `provided == Some(expected)` (`main.rs:222`).
Timing side-channel; negligible on a trusted network, trivially fixed with
`subtle`/`constant_time_eq` when this ever faces less-trusted callers.

### 5. Panic safety on the request path — MEDIUM
No `CatchPanic` tower layer; ~27 `unwrap()/expect()` across
gql/ops/remote/plan. A handler panic drops that connection (process
survives under default unwind), but #1 (stack overflow) is the exception —
that one is fatal. Adding `tower_http::catch_panic` would convert handler
panics into 500s and is cheap insurance.

### Not security gaps under this model (by design)
- **No TLS**: reqwest built `default-features=false` (no TLS backend) and
  Postgres uses `NoTls`. Acceptable for a trusted-network internal service.
  Consequence to know: `jwk_url`, remote-schema, and auth-webhook calls work
  over `http://` only — an `https://` JWKS/remote URL will fail to connect.
  Revisit if any of these must reach an external `https` endpoint.
- Edge authn/z, rate limiting, CORS, request timeout: assumed handled by the
  fronting layer; none are configured here.

## Library assessment

| Crate | Ver | Verdict |
|---|---|---|
| serde / serde_json (`preserve_order`) | 1 | ✓ correct & fast; preserve_order is required for response key ordering |
| **serde_yaml** | 0.9.34 **+deprecated** | ⚠ archived/unmaintained (dtolnay). Works; migrate to `serde_yaml_ng` or `serde_norway` eventually. Used in metadata loader + fixtures. |
| **graphql-parser** | 0.4.1 | ⚠ effectively unmaintained; parser-only (no validation/depth limit → finding #1). Reasonable for now; `async-graphql-parser` is the maintained alternative if we need validation/limits. |
| tokio | 1 (full) | ✓ (could trim features later) |
| axum | 0.8 | ✓ current |
| tokio-postgres / deadpool-postgres | 0.7 / 0.14 | ✓ solid & fast (NoTls by choice) |
| jsonwebtoken | 9 | ✓ current; per-alg `Validation` prevents alg-confusion; rejects `none` |
| reqwest | 0.12 | ✓ version; ⚠ no TLS feature (intentional, see above) |
| base64 / clap / thiserror / anyhow / tracing | current | ✓ |
| tungstenite / postgres (sync) / insta | — | ✓ test/harness-only |

No `cargo audit` advisory scan was run (cargo-audit not installed); worth
adding to CI. No pinned crate shows a known-RUSTSEC issue at these versions
from manual review, but automated scanning should confirm.

## Suggested priority
1. Query depth/complexity limit (#1) — only fatal DoS.
2. Fail-closed `run_sql`/metadata or documented network boundary (#2).
3. `resolve_url_template` panic fix (#3) + `catch_panic` layer (#5).
4. Later: constant-time compare, serde_yaml migration, `cargo audit` in CI.
