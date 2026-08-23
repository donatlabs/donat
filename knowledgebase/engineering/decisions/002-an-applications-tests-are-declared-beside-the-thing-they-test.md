---
type: decision
status: accepted
date: 2026-08-23
features:
  - "[[engineering]]"
---

# An application's tests are declared beside the thing they test

## Context

A donat application is declared — SQL migrations and YAML metadata — and the
whole discipline of `declaring-not-coding` is that a requirement never becomes
a script. Its tests were the exception. The petshop example was tested from
`crates/conformance/tests/petshop.rs` and `petshop_process.rs`: 1,800 lines of
Rust driving the harness's `Suite` builder, a `ProviderStub`, raw `postgres`
polling of `donat.process_*`, and GraphQL assembled with `format!`. An
application author who had written no Rust to build the application had to
write Rust to test it, against an API that existed to port pytest fixtures.
A reviewer saw a permission in one diff and its test, if any, in another
crate.

Three things were already true and pointed the same way. The conformance
fixture format (`url`, `headers`, `query`, `response`) is a declarative test
of one request, and the petshop's own cases were written in it. Decision
tables carry `test_cases` in the metadata itself, checked by `validate`. And
the metadata loader follows only known file names and `!include`, so a file
placed next to a metadata file is invisible to the engine unless the metadata
names it.

## Decision

**A test is a `*_test.yaml` file beside the metadata file it exercises**, the
way Go keeps `_test.go` beside the source: `public_orders.yaml` →
`public_orders_test.yaml`, `flows/checkout-payment.yaml` →
`flows/checkout-payment_test.yaml`, `commands/checkout/prepare-checkout-quote.yaml`
→ `…_test.yaml`. The file's subject is a convention for people and for the
gate; the runner does not interpret it. `donat.test.yaml` at the application
root holds the application's side of a stand — metadata, migrations, the
engine environment with `${providers}` for the runner's provider stub. Where
Postgres is and which binary runs are the machine's side and come from flags
or the environment.

**The same runner serves two entry points.** `donat test` in the shipped
binary, so an application author needs nothing but the image; and the
conformance crate's `petshop_yaml` test binary, so CI runs an example's tests
with no container stand. The runner lives in a new leaf crate,
`donat-testkit`, with the pieces a conformance suite and an application test
both need — the authentication hook, the provider stub, fixture loading,
response matching, migrations — moved there from `donat-conformance`, which
re-exports them. `Suite` and `Running` did not move: they carry the
fixture-era machinery (in-memory metadata accumulation, `/v1/query`
interception, four backends) that an application test, whose metadata is on
disk, does not need. The stand is ~250 lines and boots in the production
order: the engine's migrations, the application's, then the Process revisions.

**One database and one engine per test case.** The waits an application test
expresses — "this process reached a terminal state", "this table received a
row" — are keyed by process or table, not by instance; two cases in one
database would see each other. The cost equals what the 22 Rust `#[test]`s
already paid, and files run in parallel.

**The step vocabulary is the minimal set the petshop's 22 tests needed**, and
nothing more: `url` (the fixture shape, compared exactly), `sql` with `expect`
rows, an `error` class, or `capture`; `as`; `graphql` with a subset `expect`;
`providers`; `await` on `terminal`, `receptive`, `row` or `held`; `calls`;
`hold` / `release`. Values a test learns as it runs are captured by name and
referenced as `${name}`; a whole-string reference keeps the captured type.
Sugar that no test needed — `seed: {table: rows}`, subject-derived
`select`/`insert`, `run: {command}` — was left out, to be added when a test
asks for it and with its own test.

**`expect` is a subset match; a fixture's `response` stays exact.** A
conformance fixture pins a contract byte for byte and a changed byte is a
bug. An application test asserts a behaviour, and a column added tomorrow
should not fail it. Arrays keep exact length — a list of one row is a claim
about how many rows there are.

**A table that grants a role something has a test beside it that proves the
refusal.** `scripts/check_app_tests.py` (CI) refuses a permission-bearing
table file without a sibling `_test.yaml` holding a case; a baseline lists
the tables untested when the gate arrived and only shrinks. The gate sees
files: tables declared inline in `tables.yaml` are covered by
`tables_test.yaml` as one unit, which is weaker than one test per table and
is the honest statement of what it checks.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep writing application tests in Rust against `Suite` | The application author writes no Rust to build the application; requiring it to test one is the `declaring-not-coding` rule broken at the last step, and the test is never in the same diff as the thing it tests. |
| A `tests/` directory per application instead of files beside the subject | Works, and is what every framework does by default; but the reviewer then reads the permission and its test in two places, and the gate has to map names across directories instead of checking a sibling. |
| Move `Suite`/`Running` into the shared crate and build the runner on them | Drags four backends, in-memory metadata and the admin-API interception into the shipped binary, for a runner whose metadata is already on disk. |
| One database per file, seeds per case | Faster, but every `await` would need an instance id the test does not have, and cases would have to be written to tolerate each other's rows. A `CREATE DATABASE … TEMPLATE` optimisation is available later without changing a test. |
| Exact matching for `expect`, as for fixtures | Every generated id and every added column becomes a test edit; application tests would drift toward `@any` everywhere, which is weaker than a subset. |
| A richer vocabulary up front (`seed`, `select`, `run`, `@uuid`) | Every step that no test uses is a step whose semantics were never checked by one. |

## Consequences

The petshop's 22 Rust tests are 23 YAML cases in 21 files beside their
subjects, and `petshop.rs` / `petshop_process.rs` are gone; two assertions
that were about the metadata rather than the running application moved to
`crates/server/tests/petshop_candidate.rs`, which already loads both. A new
`_test.yaml` that nobody lists in `petshop_yaml.rs` fails a test rather than
going unrun. `make app-test` and `donat test --app-dir examples/petshop` run
the same files.

What this costs: `donat-server` now depends on `donat-testkit`, which brings
the blocking `postgres` client into the binary, and the `test` subcommand runs
its runner on a plain thread because that client and `reqwest::blocking`
refuse a tokio worker. A stand boots in ~8 s, so a file of several cases is
several stands; parallelism across files keeps a 20-file run under a minute.
The `sleep`-free `await` steps are the new home of the flake rule in
`CLAUDE.md`: a wait that expires is a task about the wait, never a bigger
deadline.

Ten petshop tables with permissions still have no test beside them; the
baseline in `examples/petshop/untested-tables.txt` names them, and the gate
fails the day one of them gains a test without leaving the list.
