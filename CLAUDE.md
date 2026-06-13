# dist-api

A GraphQL engine over Postgres, compatible with the Hasura v2 surface
(metadata format, API shape), developed TDD-style against a native
conformance harness with Hasura-derived fixtures (`crates/conformance`).

## Tech Stack

Rust workspace (axum, tokio, serde, insta), Postgres 16 (postgis), native
conformance harness (`crates/conformance`).

## Layout

| Path | Purpose |
|---|---|
| `crates/metadata` | Hasura v2 metadata types + YAML directory loader (`!include`) |
| `crates/catalog` | Postgres introspection (pg_catalog) |
| `crates/schema` | Per-role GraphQL schema generation, introspection |
| `crates/ir` | Intermediate representation — the SQL-free boundary |
| `crates/sqlgen` | IR → one Postgres SQL statement (insta snapshot tests) |
| `crates/server` | axum server: `/v1/graphql` (+ws), relay, auth; `migrate`/`validate`. No runtime admin/`run_sql` API (deleted) |
| `crates/conformance` | Native conformance harness + Hasura-derived fixtures (the conformance source of truth) |
| `tests/hasura` | Legacy pytest harness (optional cross-check; safe to delete) |
| `knowledgebase/` | Design notes and ADRs (Obsidian-style, see `_index.md`) |
| `PLAN.md` | Architecture, milestones, decision log |

## Commands

| Task | Command |
|---|---|
| Build | `make build` |
| Unit/snapshot tests | `make test` (or `cargo test -p <crate>`) |
| Run with fixture metadata | `make run` (serves :8080) |
| Apply schema migrations (DDL) | `dist-api migrate --migrations-dir migrations` (refinery) |
| Validate metadata vs DB | `dist-api validate --metadata-dir <dir>` (non-zero exit on inconsistency) |
| Conformance suite | `make conformance` (or `cargo test -p dist-conformance [--test <module>]`) |
| Review snapshot changes | `cargo insta review` |
| Legacy pytest cross-check | `tests/hasura/run_suite.sh <selector>` (optional) |

The conformance harness needs Postgres (`postgis/postgis:16-3.4`) at
`PG_URL` (default `postgresql://postgres:postgres@127.0.0.1:15432/postgres`).
It builds/spawns the engine itself — REBUILD `cargo build -p dist-server
--bin dist-api` after engine changes before re-running conformance, the
harness uses the existing binary. One database per suite (`conf_<name>`),
parallel-safe. Conventions: `crates/conformance/PORTING.md`.

## The TDD Loop (how all engine work is done)

1. Engine-behavior changes start from a failing conformance case: a fixture
   in `crates/conformance/fixtures` + a call in `crates/conformance/tests/`.
2. Implement; add/adjust unit + insta tests in the touched crate.
3. `cargo build -p dist-server --bin dist-api && cargo test -p
   dist-conformance --test <module>` until green; then run the full
   conformance crate — suites share engine semantics and regress together.
4. Fixtures are ground truth (exact bodies, error codes, paths, status).
   Local fixture edits are allowed ONLY for documented known-diffs and must
   carry a `# dist-api:` comment (see fixtures/README.md).

Quirks to remember: some fixtures `!include` files as quoted strings;
legacy `$op` permission spellings are valid input; three insert fixtures
expect status 400 with bodies identical to our deliberate 200 — they are
patched copies with comments, do not "fix" the engine to 400. The legacy
pytest harness only WARNED on error-body mismatches; the native harness is
strict — pytest greenness is not evidence of exact conformance.

## Admin Role (Hasura parity)

**This engine implements the Hasura admin role** (the earlier "no admin
role" decision was reversed on 2026-06-13). `dist_schema::ADMIN_ROLE` =
"admin": a trusted request with no explicit `X-Hasura-Role` is admin — full
access to every tracked table, no row filter, no column/limit restriction,
all mutations (no check/preset), allowlist skipped. Trusted = valid
`X-Hasura-Admin-Secret`, or no admin secret configured (Hasura's
"no secret = admin"); an explicit role header still selects that role
(impersonation). Implemented in `plan.rs` (`TableCtx.is_admin`),
`plan_mutation.rs`/`v1.rs` (synthetic full mutation perms), `gql.rs`
(session + allowlist bypass), `ops.rs` (v1 admin session). Keep admin
behavior faithful to Hasura; don't silently weaken it.

## BLOCKING RULE: Knowledgebase First

Read relevant `knowledgebase/` files BEFORE analyzing, planning, or
implementing. `ls knowledgebase/` + `grep -ri "topic" knowledgebase/`;
check `knowledgebase/<domain>/decisions/` — ADRs explain *why*. Plans or
code written without this are invalid and must be redone. After work with
meaningful trade-offs, capture an ADR (template:
`knowledgebase/_templates/decision.md`).

## BLOCKING RULE: Judge Review After Every Commit

After EVERY `git commit`, dispatch the judge agent before starting the next
task. Not optional; "simple change" is not an excuse.

```
Agent(subagent_type="judge", run_in_background=true, prompt="REVIEW TASK: ...")
```

Input format in `.claude/agents/judge.md`. Continue only after ACCEPT; on
REJECT fix the issues first.

## Essential Rules

- **Repo content in English** (docs, comments, specs, ADRs). Chat language
  may differ; the repo never does.
- **Exact Hasura error shapes.** Error `code`/`path`/message text and HTTP
  status are part of the conformance contract — never invent error formats.
- **One SQL statement per operation** (M4 invariant): response JSON is
  assembled in Postgres (`json_build_object`/`json_agg`, correlated
  subqueries). Don't add row-by-row post-processing in Rust.
- **SQL injection safety.** sqlgen currently renders literals inline with
  strict quote-escaping helpers (parameterized execution is a planned
  refactor — see crates/sqlgen/src/lib.rs header). Never format user input
  into SQL except through those helpers.
- **insta snapshots are reviewed, never blind-accepted.** `cargo insta
  review` and read every diff; an unexplained snapshot change is a bug.
- **Full v2 metadata format** — metadata exported from existing Hasura
  projects must load without conversion.
- **Every change needs tests**: unit/insta in the touched crate AND the
  conformance crate green (`make conformance`) after rebuilding the engine
  binary.

## Agents

- `.claude/agents/judge.md` — two-stage quality gate (spec compliance →
  code quality → fresh verification). Mandatory after every commit.
- `.claude/agents/spec-writer.md` — researches the codebase + conformance
  fixtures and writes specs to `specs/NNN-<slug>.md`.
