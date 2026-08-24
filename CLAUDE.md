# donat

A GraphQL engine over Postgres, compatible with the Donat v2 surface
(metadata format, API shape), developed TDD-style against a native
conformance harness with Donat-derived fixtures (`crates/conformance`).

## Who This Is For

**donat is for an analyst working with an AI agent, not for a developer.**
The person says what the business needs; the agent declares it — schema,
rules, permissions, processes, integrations, and tests — in SQL migrations
and YAML, and nothing else. Nobody writes code, and nobody should have to
read code to check the result. So the measure of every engine feature is two
questions: can an agent produce the right declaration from a plain sentence
on the first try, and can the analyst read what came out and see their own
requirement in it? A feature that needs a script, a service, a Rust file, or
a developer's eye on the application side is not finished. What the agent
reads before it acts — the skills in `plugins/donat/skills` — and what the
engine says when a declaration is wrong — `validate`, `donat test`, the
error bodies — are the product's surface as much as the API is. Rust is the
engine's language only: `crates/*` and their tests are about the engine,
never a stand-in for what should have been declarable. `examples/petshop` is
the proof — a whole store, tests included, with not one line of code.

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
| `crates/storage` | File attachments: the resolved S3-compatible store and the URL signing shared by planner and server |
| `crates/server` | axum server: `/v1/graphql` (+ws), relay, `/api/rest` (RESTified endpoints), `/mcp` (MCP server), auth; `migrate`/`validate`. No runtime admin/`run_sql` API (deleted) |
| `crates/testkit` | Test stand + the `*_test.yaml` runner behind `donat test`; the stubs and matching `crates/conformance` shares |
| `crates/conformance` | Native conformance harness + fixtures (the conformance source of truth) |
| `apps/ui` | Platform UI (`@refinest/*` + React) over the engine's GraphQL. Its own npm project, outside the Cargo workspace and `make test`. Not an admin surface — an ordinary role rendered; see `knowledgebase/platform/decisions/001-*` |
| `modules/notifications` | Shipped domain module: an inbox, opt-out, email delivery and a digest, as migrations plus ordinary metadata. No engine code — see its README and `knowledgebase/declarative-saas/decisions/097-*` |
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
| An application's own tests (`*_test.yaml` beside its metadata) | `make app-test` (`APP_DIR=examples/petshop`) or `donat test --app-dir <app>`; in cargo, `cargo test -p donat-conformance --test petshop_yaml` |
| Review snapshot changes | `cargo insta review` |
| Format and lint gates (CI blocks on both) | `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` |
| What the change gate will ask this branch to declare | `make gate` (`GATE_BASE=<target branch>`, `GATE_BODY=<file holding the PR description>`) |
| Inspect one Process instance | `donat process inspect --source <name> --instance <uuid>` (read-only) |
| Check one instance's history | `donat process verify-history --source <name> --instance <uuid>` (read-only, non-zero exit on inconsistency) |
| Authorize an OAuth2 connector instance | `donat connector authorize --source <name> --instance <name>` (deploy-time only; needs `DONAT_CREDENTIAL_KEY`) |
| Inspect / remove stored credentials | `donat connector credentials list --source <name>` (read-only, non-zero exit when a configured instance has none), `... credentials revoke --source <name> --instance <name> --subject <id>` |

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

**This engine has no admin role, and no admin secret.** Only classic explicit
roles work — every data access goes through an explicit per-role permission.
There is no permission-bypass role and no admin-over-HTTP surface at all: the
runtime admin/management API (`run_sql`, metadata mutation) was deleted, the
admin DATA role (the `ADMIN_ROLE` permission bypass) was removed, and
`DONAT_GRAPHQL_ADMIN_SECRET` with it.

**A role is established by a verified JWT or an authentication hook, and by
nothing else.** No header names one — `X-Donat-Role` only *picks* between roles
a token already granted. A request nothing authenticated runs as
`DONAT_GRAPHQL_UNAUTHORIZED_ROLE` when one is set and is refused otherwise, and
a deployment configuring none of the three refuses to boot. The engine may
serve the login itself (`DONAT_OIDC` → `/auth/login`, `/auth/callback`), which
stores no users and issues no tokens: it carries a token from the provider that
issued it to the cookie this engine verifies. Any diff that re-introduces an
admin role, a shared secret, a permission bypass, or a header that grants a
role must be rejected. The change gate (`make gate`, CI job `change-gate`)
rejects the retired names mechanically — `ADMIN_ROLE`,
`DONAT_GRAPHQL_ADMIN_SECRET`, `X-Donat-Admin-Secret` in engine sources,
`run_sql` in `crates/server/src` — so engine code does not spell them even in
a comment. Configuration is deploy-time: `migrate` (DDL) + YAML metadata at
boot.

## BLOCKING RULE: Knowledgebase First

Read relevant `knowledgebase/` files BEFORE analyzing, planning, or
implementing. `ls knowledgebase/` + `grep -ri "topic" knowledgebase/`;
check `knowledgebase/<domain>/decisions/` — ADRs explain *why*. Plans or
code written without this are invalid and must be redone. After work with
meaningful trade-offs, capture an ADR (template:
`knowledgebase/_templates/decision.md`).

## Quality Review at Feature Completion

There is no per-commit review gate. Each TDD slice must still have its focused
test evidence and the required suite verification. Run one independent code
review for the complete, cohesive feature range before it is merged, handed
off, or declared ready. Address material findings with a regression test and
fresh verification before completion.

Every material finding lands on the lowest rung that can hold it, and the pull
request says which: a test or fixture first; then a mechanical gate
(`scripts/check_*.py`, clippy, CI); then a skill in `plugins/donat/skills`;
then an ADR. A lesson that only becomes a sentence in a document is read by
whoever reads it, and the mistake is made again. The model does not learn
between sessions — the repository does, and only on the rungs that refuse.

## When CI Is Red

Classify before touching anything:

- **Flake** — failed with no code change, or passes on rerun. Record the test;
  change neither code nor timeout. Conformance tests that wait on time
  (`sleep` in `event_triggers`, `file_attachments`; the `await` steps of
  `*_test.yaml` files) are the
  usual source. A flake seen twice is a task about the *wait* — wait on the
  event, not on the clock — never a bigger `sleep`.
- **Regression** — fails deterministically after a change. The TDD loop.
- **Infrastructure** — container, registry, runner. Rerun; nothing to fix here.

Three attempts on one failure, then stop and write down what was tried and why
it did not work. A fourth attempt that nobody asked for is how a wrong fix
gets merged.

Never a way to make CI green: `cargo insta accept` without reading every diff,
editing an existing fixture to match the engine, raising a `sleep`,
`#[ignore]`, deleting the test, excusing an advisory. Each is occasionally the
right change, so the change gate does not forbid them — it asks for the reason
in the pull request description, one line per kind, `gate:<kind> <reason>`.
`make gate` prints the lines it is missing. A new fixture or snapshot is free;
rewriting an existing one is what needs a reason.

A change under `plugins/donat/skills/` names a measurement that the edit helps
a skill — a paired benchmark arm, where a corpus to run one exists — or says why
it needs none. A skill with a wrong rule in it makes every later agent worse and
never cleans itself up.

## Essential Rules

- **Repo content in English** (docs, comments, specs, ADRs). Chat language
  may differ; the repo never does.
- **Exact Donat error shapes.** Error `code`/`path`/message text and HTTP
  status are part of the conformance contract — never invent error formats.
- **One SQL statement per operation** (M4 invariant): response JSON is
  assembled in Postgres (`json_build_object`/`json_agg`, correlated
  subqueries). Don't add row-by-row post-processing in Rust. Documented
  carve-out: SQLite *mutations* fold one DML statement's `RETURNING` rows in
  the Rust executor, because SQLite forbids DML inside a CTE/subquery (so it
  cannot aggregate `RETURNING` in SQL). The "one statement per root, no N+1"
  core still holds; see `knowledgebase/multi-backend/decisions/003-*`. Postgres
  mutations and all SQLite queries keep full in-database assembly.
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
- **An application's tests are declarations beside the thing they test.** A
  `*_test.yaml` sits next to the metadata file it exercises
  (`public_orders.yaml` → `public_orders_test.yaml`), never in Rust; `donat
  test` runs them. A table that grants a role something has a test beside it
  that proves the refusal (`scripts/check_app_tests.py`, CI). See
  `knowledgebase/engineering/decisions/002-*`.
- **The toolchain is pinned** in `rust-toolchain.toml`, because `cargo fmt
  --check` and `clippy -D warnings` are CI gates and both change meaning
  between releases. Bumping it is a deliberate commit that carries whatever
  reformatting the new toolchain wants.
- **A background loop must be drainable.** Wait with
  `donat_server::shutdown::idle`, never a bare `tokio::time::sleep`: a loop
  that cannot observe the shutdown token cannot be drained on `SIGTERM`, which
  is what a rolling deployment needs (see
  `knowledgebase/operations/decisions/001-*`).

## Agents

- `.claude/agents/spec-writer.md` — researches the codebase + conformance
  fixtures and writes specs to `specs/NNN-<slug>.md`.
