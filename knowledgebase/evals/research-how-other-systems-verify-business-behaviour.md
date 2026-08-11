---
type: research
status: draft
date: 2026-08-09
---

# How other systems prove a declared application behaves correctly

Companion to [[research-how-to-build-an-evals-framework]], written to answer a
narrower question: §4.7 of that note proposes scenarios, an invariant library
and anti-oracles for measuring business behaviour. **Is that how anyone else
does it?**

Mostly yes — every element has an established ancestor in workflow engines,
distributed-systems testing, declarative-infrastructure tooling and payments.
The useful output of this research is not validation, though; it is the seven
concrete deltas in §9, each of which is something a mature system does that
our plan did not.

## 1. Camunda — coverage over the process model

Camunda's community tooling attacks exactly our problem: given a declared
process graph, how do you know your tests exercised the business?

`bpmn-driven-testing` lets an author **select paths through the BPMN model as
test cases** and generates the test code from the model, so the test names flow
nodes rather than restating them; a breaking model change (a user task becoming
a service task) surfaces at design time as a compile error.
`camunda-bpm-process-test-coverage` computes and asserts a **node and
sequence-flow coverage ratio** per test method and class, and renders the
covered graph as HTML.

**Transfer.** donat compiles a Process to a real graph
(`CompiledProcessCatalog`), so state and transition coverage is computable for
free, per attempt. Note the subtlety: coverage of the *oracle's* graph tells us
nothing about a candidate with a different design. Coverage of the
**candidate's own graph** is the meaningful measure — it answers "did our
scenarios reach what this application actually built", and an unexercised
transition is precisely where a business defect survives. Report it; do not
score it. A coverage number in the reward would push authors toward tests that
walk states rather than tests that assert outcomes.

## 2. Temporal — the history is the artefact, and time is an input

Two practices are directly relevant. **Replay testing**: recorded event
histories are re-executed against the current workflow definition in CI, and
replay succeeds only if the definition is still deterministically compatible
with the history — this is how Temporal users guard against non-determinism.
**Time skipping**: the test environment is an in-memory server that fast-
forwards time, so a workflow with a 30-day timer is testable in milliseconds.

**Transfer.** donat already owns both halves and uses neither in testing.
`donat process verify-history` exists as a read-only diagnostic that exits
non-zero when an instance's recorded history is internally inconsistent — that
is a free, design-independent invariant that the eval can run over every
instance a scenario touched. And `tests-system/stack.sh up-fast` is our time
skipping, with the sharp edge already documented: without the fast stand every
deadline branch skips itself and a run that tested none of them looks green.

## 3. AWS Step Functions — the mocked world is a data file

Step Functions Local takes a `MockConfigFile.json` that declares, per state
machine, named **test cases** mapping states to **mocked responses**, so an
execution path is exercised without calling any integrated service.

**Transfer.** This validates the `world:` field being data rather than code.
But it also shows the trap to avoid: SFN keys its mocks **by state name**,
which is legitimate when the author owns the state machine and fatal when the
agent does. Our mocks must be keyed by **connector operation** — a name fixed
by the connector catalog, not by the candidate's design. Same expressive power,
design-independent.

## 4. Jepsen, FoundationDB, Antithesis — faults are the test

Jepsen injects process and network faults while checking linearizability.
FoundationDB went further and made the simulation *be* the implementation: a
deterministic harness that runs the real database at ~10× speed, injects
crashes and partitions, and verifies invariants across thousands of parallel
timelines. Antithesis generalised that into a platform, and Jepsen now
integrates with it via seeded, reproducible randomness. The reported bugs are
instructive — Antithesis caught a cache-coherence bug on the first run of a
delete-then-get workload *under fault injection*, a defect no happy-path suite
would ever produce.

**Transfer, honestly stated.** We cannot do deterministic simulation: we run a
real engine against a real Postgres with real mock providers. What we can take
is (a) the doctrine that faults are the test rather than an afterthought, and
(b) the **reproducibility half** — every world gets a seed, and the seed plus
the provider script is recorded with the attempt, so a failure can be replayed.
And the honest consequence: because we script faults rather than search for
them, we do not get the "found a bug nobody imagined" property. That is exactly
why anti-oracles (§6) carry more weight for us than they would for a project
with a simulator.

## 5. Specification by Example / BDD — ambiguity is removed before the build

Example mapping workshops exist to surface ambiguity in a requirement *before*
anyone implements it, turning a vague rule into concrete Given/When/Then
examples that read the same to a developer, a tester and an analyst; the
resulting feature files become living documentation and give traceability from
requirement to test execution.

**Transfer.** This is the cheapest known countermeasure to the failure that
kills benchmark corpora — 38.3% of SWE-bench samples were flagged as
underspecified. A task author who writes the example map first will discover
that "retry transient failures" does not say what happens after the last
attempt, and will fix the *prompt* rather than discovering it later as a
mysterious spread in agent scores. Our rule→assertion traceability matrix is
the same artefact under another name.

## 6. Mutation testing — the suite is itself under test

The established answer to "do my tests actually catch anything" is fault
seeding: generate mutants, measure the ratio killed. The framing in the
literature is that mutation score measures test *quality* rather than quantity,
and that it surfaces the false confidence that line coverage produces.

**Transfer, with one deliberate divergence.** Classic mutation testing mutates
the implementation automatically, thousands of syntactic mutants at a time.
We cannot: the implementation is metadata an agent wrote in a shape we do not
control, and a random syntactic mutation of it will usually just fail
`validate` — caught by the gate, proving nothing about the scenario suite. So
we invert it: a small number of **hand-authored semantic anti-oracles at the
design level**, each a deployable, validate-clean application carrying one
plausible business defect. Fewer mutants, each worth reviewing.

Two known hazards come along with the technique. **Equivalent mutants** — a
mutant that is actually correct — become, for us, an anti-oracle whose "defect"
is really a legitimate alternative design; it must be caught in review, or the
task starts punishing correct work. And a mutation score is gameable by
authoring easy mutants, so the anti-oracle set is a review artefact, not a
number anyone optimises.

## 7. Terraform and OPA — assertions over declared config, and policies with
their own tests

Terraform's built-in test framework runs HCL `run` blocks (plan or apply) with
assertions, and — notably — an `expect_failures` attribute that passes when the
named objects *do* report an issue. Conftest evaluates OPA policies against
`terraform plan` JSON, and ships `conftest verify` so the policies themselves
have unit tests: "so you're confident the gate fails for the right reasons".

**Transfer.** Two things. `expect_failures` is the shape of a **negative task**
— an application handed to the agent broken, where the expected outcome is a
specific diagnostic before the fix and none after. And "the gate must fail for
the right reasons" is the same principle in two places for us: the validator's
rules need positive and negative fixtures (the validate RFC already says so),
and the eval's invariant library needs anti-oracles. Neither a linter nor an
invariant is trustworthy until something has proven it can fail.

## 8. Payments — our hardest task class is an industry-standard problem

Stripe's own documentation states the premise of the ambiguity task class
almost verbatim: "in many cases, the success of the operation is ambiguous from
the perspective of the client", a connection terminating mid-exchange being the
example. Their mechanism is an idempotency key under which the **status code
and body of the first request are saved and replayed**, regardless of whether
it succeeded. Separately, test clocks let an integration simulate time moving
forward so that subscription state changes and webhooks fire on demand.

**Transfer.** The mock provider should behave the way a real provider does:
store the response per idempotency key and replay it, so that "charged once" is
observable from the provider's evidence log rather than inferred. That single
behaviour is what makes the fourth scenario of
`checkout-ambiguous-payment-001` a real test instead of a wish.

## 9. What this changes in our plan

Seven concrete deltas, each taken from a system above:

1. **Run `donat process verify-history` over every instance a scenario
   touched** and fail the attempt on a non-zero exit. Free, design-independent,
   currently unused outside operator diagnostics. *(Temporal, §2)*
2. **Record state and transition coverage of the candidate's own compiled
   graph** per attempt; report it, never score it. *(Camunda, §1)*
3. **Key mocks by connector operation, never by state name.** *(Step
   Functions, §3)*
4. **Seed and record every world** — provider script plus seed — so a failing
   attempt is replayable. *(Antithesis/Jepsen, §4)*
5. **Add a crash-and-restart world.** `tests-system` has races, provider
   failures and deadlines, but nothing kills the engine mid-Process. Durable
   execution is the central claim, and it is the one claim currently untested.
   *(Jepsen/FDB, §4)*
6. **Require an example map in each task's README**, written before the
   prompt is finalised; unresolved ambiguity found there is fixed in the
   prompt. *(Specification by Example, §5)*
7. **Support negative tasks** — a broken application in, a named diagnostic
   expected before the fix — in the same `gates:` grammar. *(Terraform
   `expect_failures`, §7)*

And one thing to keep doing that this research confirms: nobody credible scores
business correctness with a model judge, and nobody scores it by comparing
structure to a reference. Every system above scores observable behaviour under
a controlled world, and tests its own tests by seeding faults.

## Sources

- [bpmn-driven-testing — paths through a BPMN model as test cases](https://github.com/camunda-community-hub/bpmn-driven-testing)
- [camunda-bpm-process-test-coverage — node and sequence-flow coverage](https://github.com/Miragon/camunda-bpm-process-test-coverage)
- [Testing Entire Process Paths (Camunda)](https://camunda.com/blog/2020/10/testing-entire-process-paths/)
- [Temporal — testing suite, replay and time skipping](https://docs.temporal.io/develop/go/best-practices/testing-suite)
- [Replay testing to avoid non-determinism in Temporal workflows](https://www.bitovi.com/blog/replay-testing-to-avoid-non-determinism-in-temporal-workflows)
- [Step Functions Local — mocked service integrations](https://docs.aws.amazon.com/step-functions/latest/dg/sfn-local-test-sm-exec.html)
- [Jepsen — distributed systems safety research](https://jepsen.io/)
- [What's the big deal about Deterministic Simulation Testing?](https://notes.eatonphil.com/2024-08-20-deterministic-simulation-testing.html)
- [FoundationDB paper — simulation testing](https://www.foundationdb.org/files/fdb-paper.pdf)
- [Antithesis-driven testing, and Jepsen's integration](https://sqlsync.dev/posts/antithesis-driven-testing/)
- [Specification by Example / example mapping (Cucumber)](https://cucumber.io/blog/bdd/better-requirements-by-harnessing-the-power-of-exa/)
- [Selecting Fault Revealing Mutants — mutation adequacy](https://arxiv.org/pdf/1803.07901)
- [Mutation testing with PIT — test quality, not quantity](https://javapro.io/2026/01/21/test-your-tests-mutation-testing-in-java-with-pit/)
- [Terraform tests — run blocks, assertions, expect_failures](https://developer.hashicorp.com/terraform/language/tests)
- [Conftest + OPA for Terraform policy testing](https://oneuptime.com/blog/post/2026-02-23-how-to-use-conftest-with-terraform-for-policy-testing/view)
- [Stripe — designing robust APIs with idempotency](https://stripe.com/blog/idempotency)
- [Stripe — idempotent requests reference](https://docs.stripe.com/api/idempotent_requests)
- [Stripe test clocks](https://stripe.dev/blog/test-clocks-how-we-made-it-easier-to-test-stripe-billing-integrations)
