---
type: decision
status: accepted
date: 2026-08-24
features:
  - "[[notifications]]"
  - "[[application-tests]]"
---

# A module proves itself on its own stand, and a deployment example proves its seams

## Context

`engineering/decisions/002` settled where an application's tests live: a
`*_test.yaml` beside the metadata file it exercises, never in Rust. It was
written about `examples/petshop`, which is an application.

`modules/notifications` is not an application — it is a metadata directory an
application adopts (`097`). Its first test suite was 30 Rust cases in
`crates/conformance/tests/notifications.rs`, which stood the module up by
merging its declarations into a fixture through a harness helper
(`adopt_metadata_module`) and then edited the merged metadata in memory
(`tune_metadata`) for the two cases that needed a deployment's choices rather
than the module's defaults.

That worked and it was the wrong shape twice over. A module whose tests are Rust
is a module an analyst cannot read the tests of, which is the whole audience
this repo is for. And a test that reaches into loaded metadata to change a
decision-table row is testing a deployment nobody can see: the row it edited
exists in no file, so the thing proven is not the thing anyone ships.

## Decision

**A shipped module is tested as the application it almost is.** The module
carries its own `donat.test.yaml`; `metadata/` is the metadata dir, `migrations/`
its schema, and `testdata/migrations/` the one thing a module cannot ship — a
recipient binding, played by the smallest users table that satisfies the
contract. Every case is a `*_test.yaml` beside the declaration it exercises, and
`scripts/check_app_tests.py` runs over the module's tables the same way it runs
over a store's.

**What a deployment decides is proven by a deployment.** Two of the module's
promises are seams rather than behaviours: the mail connector can be replaced,
and the email escalation can be turned on. Both are things an adopter does in
its own files, so both are tested in
`modules/notifications/examples/deployment` — a second stand that adopts the
module by `!include`, brings its own sender, and declares a real delay. It is
simultaneously the worked adoption checklist, which no amount of README prose
had been.

**What only an adopter can see stays with the adopter.** That
`inherited_roles` carries a module's table permissions and not its command ones
(`plans/009`) is a fact about adopting, not about the module, and it is pinned
in `examples/petshop/metadata/inherited_roles_test.yaml` — beside the
declaration that causes it.

Two harness changes fell out, and both were gaps rather than conveniences.
`await` gained a `sql` shape — poll a query until its rows match — because
`await.row` waits for a table's *first* row and `await.terminal` reads a single
process instance, and a module whose subject is a log with one row per channel
per dispatch can express neither. And the per-case template database is now
keyed by which application it belongs to as well as by its content: the stale-
template reclaim dropped every template that was not the current one, which was
right for a run with one application and destroyed a sibling application's
in-progress build as soon as there were two.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep the Rust suite | It is the only test suite in the repo an analyst cannot read, for the one component whose entire premise is that no code is needed. |
| Test the module only through `examples/petshop` | The store exercises what the store uses. Opt-out, the digest sweep, the claim race and the delivery log's failure modes are not a pet shop's business, and a module tested only through one adopter fails in the second one. |
| Ship the escalation as the module's default so its own stand can test it | A module that delays every message by default decides something the deployment did not ask for. The delay is a deployment's answer and it needed a deployment to answer it. |
| A metadata overlay in `donat.test.yaml` (patch the metadata a stand loads) | It would put the "edit the loaded metadata" hole back, one layer down, and every test using it would again prove something no file says. |

## Consequences

The module's 30 behaviours are 30 declarative cases across nine files, and the
cargo entry (`crates/conformance/tests/notifications.rs`) asserts that no test
file goes unrun, so a new case cannot be added and forgotten. `make conformance`
covers all three stands; `make app-test APP_DIR=…` runs one.

The cost is a second stand to keep alive, and one case that waits on a duration:
the "skipped" outcome needs the recipient to read the bell before the escalation
fires, which is a window and not a state. It uses the longest row the deployment
declares (twenty seconds) so a busy machine does not lose the race, and it is
the only test in the repo whose green depends on a clock.

Modules that follow this one inherit the shape: a stand of your own, a
deployment example for every seam you promise, and nothing about adoption tested
anywhere but in an adopter.
