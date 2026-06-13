# donat

A GraphQL engine over Postgres, compatible with the Donat v2 surface
(metadata format, API shape), developed TDD-style against a native
conformance harness with Donat-derived fixtures (`crates/conformance`).

## Tech Stack

Rust workspace (axum, tokio, serde, insta), Postgres 16 (postgis), native
conformance harness (`crates/conformance`).

## Layout

| Path | Purpose |
|---|---|
| `crates/metadata` | Donat v2 metadata types + YAML directory loader (`!include`) |
| `crates/catalog` | Postgres introspection (pg_catalog) |
| `crates/schema` | Per-role GraphQL schema generation, introspection |
| `crates/ir` | Intermediate representation — the SQL-free boundary |
| `crates/sqlgen` | IR → one Postgres SQL statement (insta snapshot tests) |
| `crates/server` | axum server: `/v1/graphql` (+ws), relay, `/api/rest` (RESTified endpoints), `/mcp` (MCP server), auth; `migrate`/`validate`. No runtime admin/`run_sql` API (deleted) |
| `crates/conformance` | Native conformance harness + fixtures (the conformance source of truth) |
| `knowledgebase/` | Design notes and ADRs (Obsidian-style, see `_index.md`) |
| `PLAN.md` | Architecture, milestones, decision log |

## Commands

| Task | Command |
|---|---|
| Build | `make build` |
| Unit/snapshot tests | `make test` (or `cargo test -p <crate>`) |
| Run with fixture metadata | `make run` (serves :8080) |
| Apply schema migrations (DDL) | `donat migrate --migrations-dir migrations` (refinery) |
| Validate metadata vs DB | `donat validate --metadata-dir <dir>` (non-zero exit on inconsistency) |
| Conformance suite | `make conformance` (or `cargo test -p donat-conformance [--test <module>]`) |
| Review snapshot changes | `cargo insta review` |

The conformance harness needs Postgres (`postgis/postgis:16-3.4`) at
`PG_URL` (default `postgresql://postgres:postgres@127.0.0.1:15432/postgres`).
It builds/spawns the engine itself — REBUILD `cargo build -p donat-server
--bin donat` after engine changes before re-running conformance, the
harness uses the existing binary. One database per suite (`conf_<name>`),
parallel-safe. Conventions: `crates/conformance/PORTING.md`.

## The TDD Loop (how all engine work is done)

1. Engine-behavior changes start from a failing conformance case: a fixture
   in `crates/conformance/fixtures` + a call in `crates/conformance/tests/`.
2. Implement; add/adjust unit + insta tests in the touched crate.
3. `cargo build -p donat-server --bin donat && cargo test -p
   donat-conformance --test <module>` until green; then run the full
   conformance crate — suites share engine semantics and regress together.
4. Fixtures are ground truth (exact bodies, error codes, paths, status).
   Local fixture edits are allowed ONLY for documented known-diffs and must
   carry a `# donat:` comment (see fixtures/README.md).

Quirks to remember: some fixtures `!include` files as quoted strings;
legacy `$op` permission spellings are valid input; three insert fixtures
expect status 400 with bodies identical to our deliberate 200 — they are
patched copies with comments, do not "fix" the engine to 400. The legacy
pytest harness only WARNED on error-body mismatches; the native harness is
strict — pytest greenness is not evidence of exact conformance.

## BLOCKING RULE: No Admin Role

**This engine has no admin role.** Only classic explicit roles work — every
data access goes through an explicit per-role permission. There is no
permission-bypass role and no admin-over-HTTP surface at all: the runtime
admin/management API (`run_sql`, metadata mutation) was deleted, and the
admin DATA role (the `ADMIN_ROLE` permission bypass) was removed too. A
trusted request with no `X-Donat-Role` is denied ("x-donat-role header is
required"); `X-Donat-Admin-Secret` is API-level auth only. Any diff that
re-introduces an admin role or permission bypass must be rejected.
Configuration is deploy-time: `migrate` (DDL) + YAML metadata at boot.

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
- **Exact Donat error shapes.** Error `code`/`path`/message text and HTTP
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
- **Full v2 metadata format** — metadata exported from existing Donat
  projects must load without conversion.
- **Every change needs tests**: unit/insta in the touched crate AND the
  conformance crate green (`make conformance`) after rebuilding the engine
  binary.

## Agents

- `.claude/agents/judge.md` — two-stage quality gate (spec compliance →
  code quality → fresh verification). Mandatory after every commit.
- `.claude/agents/spec-writer.md` — researches the codebase + conformance
  fixtures and writes specs to `specs/NNN-<slug>.md`.
