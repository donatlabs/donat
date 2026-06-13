---
type: reference
created: 2026-06-13
---

# Security & dependency audit (2026-06-13)

Threat model: dist-api is an **internal microservice component** (sidecar /
behind a mesh / trusted network). TLS termination and edge auth are assumed
to live in front of it. Findings are ranked for that model; "network-edge"
items are downgraded accordingly.

## SQL generation — verdict: injection-safe

Audited every literal/identifier path in `crates/sqlgen`. No GraphQL-path
injection found:
- All user-controlled values (query variables, session variables, where /
  order_by / limit / insert / update args) are rendered through
  `scalar_sql` → `quote_lit` (`'` doubled) or `quote_ident` (`"` doubled).
  Correct under `standard_conforming_strings = on` (PG default).
- All identifiers (table/column/alias names) come from catalog introspection
  or are validated against it at plan time — unknown columns error out.
- The three raw `format!("{name}(...)")` interpolations are from trusted
  sources: aggregate op (fixed `COLUMN_OPS` allowlist), PostGIS function
  (fixed `st_op(...)` constants), computed-field function (operator-defined
  metadata = trust-equivalent to DDL).
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
