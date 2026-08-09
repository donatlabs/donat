# 001 — Multiplex live-query subscriptions

**Written against:** `768f89c`
**Kind:** design spec (a fork has to be chosen before anyone writes code)
**Effort:** Fork A — M. Fork B — L, and coupled to a refactor sqlgen already anticipates.

## What is true today

Every subscription runs its own poll loop. `crates/server/src/ws.rs:200-235`:

```rust
loop {
    let Some(poll_permit) = acquire_subscription_poll_permit(poll_permits.clone()).await
    else { break; };
    let response = gql::execute_preparsed_full(
        &state, &session, &payload, relay, &HeaderMap::new(), &subscription_doc,
    ).await.1;
    drop(poll_permit);
    if last.as_ref() != Some(&response) { /* … send … */ }
    tokio::time::sleep(SUBSCRIPTION_POLL_INTERVAL).await;
}
```

`SUBSCRIPTION_POLL_INTERVAL` is one second (`ws.rs:25`). Two limits bound the
whole process, both from `crates/server/src/main.rs`:
`DONAT_GRAPHQL_MAX_ACTIVE_SUBSCRIPTIONS` (default 1000) and
`DONAT_GRAPHQL_MAX_CONCURRENT_SUBSCRIPTION_POLLS` (default 16).

Nothing is shared between subscribers. A thousand clients watching the same
query issue a thousand full executions per second, through sixteen slots. For
that to keep up, each execution must finish in ≈16 ms; past that the loops
simply fall behind, and the symptom is latency that grows silently rather than
an error anyone sees. The advertised ceiling of 1000 active subscriptions is
therefore not a capacity statement — it is a count of loops the process is
willing to hold.

For comparison, Hasura batches ~100 similar subscriptions into a single SQL
statement, so the same thousand clients cost ten statements per second.

## The thing that makes this hard, and is easy to miss

Session variables are substituted **at plan time**, not at execution time.
`crates/schema/src/plan.rs:1563`:

```rust
/// The role's row filter as an IR predicate (session vars substituted).
```

and sqlgen renders what it receives as inlined literals
(`crates/sqlgen/src/lib.rs:8-10`: "Literals are inlined with strict quoting …
Parameterized execution can replace …").

So two subscribers on the *same* GraphQL document with the *same* variables,
differing only in `x-donat-user-id`, do not merely produce different results —
they produce **different SQL**. There is no shared statement to batch. This is
the whole difficulty, and any plan that does not name it will fail halfway
through implementation.

## Fork A — batch only identical plans

Group subscribers by a key of (document, variables, role, **the full session
variable map**), run one execution per group per interval, fan the response
out to every member.

- **Helps:** dashboards, public feeds, anything where many clients watch the
  same rows — every subscriber in the group is byte-identical anyway.
- **Does nothing for:** per-user subscriptions, where the session variable
  differs by definition. That is the common case and the one people hit.
- **Effort:** M. It is contained inside `ws.rs` plus a process-wide registry;
  no change to planning or sqlgen.
- **Risk:** low technically, high in framing. Shipping this as "subscriptions
  are multiplexed" would be untrue for the workload that motivated it, and the
  benchmark that looks good (N clients, one query) is exactly the workload
  that was never the problem.

## Fork B — cohort-parameterised execution

Lift session variables out of plan-time substitution for the subscription path
only: compile the plan once with the session variables left as parameters,
then execute one statement per cohort that takes a `VALUES` list of
`(subscriber_id, session_vars…)` and `LATERAL`-joins the plan per row, so one
statement returns one result row per subscriber.

- **Helps:** the real case. A thousand per-user subscriptions on one document
  become a handful of statements.
- **Costs:** the plan must stop substituting session variables, which means
  the IR has to carry a session-variable operand and sqlgen has to render it
  as a parameter. That is the parameterisation refactor sqlgen's own header
  already names as planned — this feature does not create that work, it needs
  it, and doing them together is cheaper than doing them apart.
- **Effort:** L.
- **Risk:** medium-high. It touches the boundary the whole engine is organised
  around. It must not weaken permissions: a cohort statement evaluates several
  callers' predicates in one execution, and a bug there leaks rows across
  users rather than merely returning wrong data.

There is precedent for the input this needs. `crates/schema/src/predicate.rs`
already has `collect_permission_session_uses` and a `PermissionSessionUse`
type, which computes exactly which session variables a permission depends on —
today used by the command compiler to publish a "closed session-variable
contract" (`crates/schema/src/commands.rs:2690`). A cohort key wants the same
information, so the analysis does not have to be invented.

## Recommendation

**Do not start with Fork A.** It is the cheaper half of the problem and it
buys the case nobody complained about, while making the metric look solved.

Fork B is the feature. Sequence it *with* the sqlgen parameterisation refactor
rather than before or after it: parameterised execution is the enabling change,
and it also removes the inlining caveat the security audit records.

If the parameterisation refactor is not going to happen this cycle, the honest
move is to **do neither**, and instead document the real subscription ceiling
and lower `DONAT_GRAPHQL_MAX_ACTIVE_SUBSCRIPTIONS` to a number the poll budget
can actually serve. A limit that cannot be met is worse than a smaller one.

## What a decision needs before code

1. Is the sqlgen parameterisation refactor in scope this cycle? Fork B's cost
   depends almost entirely on this answer.
2. What is the target? "1000 per-user subscriptions on one document at one
   second" is a specification; "faster subscriptions" is not.
3. Does streaming (cursor-based, forward-only) serve more of the real demand
   than live queries at lower cost? It is a different feature with a different
   contract and should be priced separately before committing to either.

## Verification a future implementation must meet

- `cargo test -p donat-server --lib ws::` and the conformance subscription
  suites (`cargo test -p donat-conformance --test subscriptions`) stay green.
- A new test proves cross-subscriber isolation directly: two subscribers in
  one cohort with different `x-donat-user-id` each receive only their own rows.
  This is the test that must exist before the feature is called done — the
  failure it guards against is a permission leak, not a performance
  regression.
- A load figure recorded under `benchmarks/perf/`: N subscribers, one
  document, statements per second at the database. The harness there already
  records per-phase server traces.
