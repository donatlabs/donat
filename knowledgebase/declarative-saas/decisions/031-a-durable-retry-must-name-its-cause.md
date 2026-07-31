---
type: decision
status: accepted
date: 2026-07-31
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A durable retry must name its cause in the log

## Context

A Process that cannot make a transition does not fail: it retries. That is the
point of a durable runtime, and it is also why an unactionable failure is worse
here than anywhere else — the instance sits `running`, its activity jobs all
report `succeeded`, and the only evidence is a log line repeating every 250ms.

Three such lines said nothing usable while bringing up the Petshop modules:

- The transition consumer logged `error = %error`, printing only the outermost
  `anyhow` context. "validating Process output state" — with the actual cause,
  `unknown field 'allocation_id'`, dropped.
- A Process command that violated a database constraint reported
  `SQLSTATE 23514`, which cannot distinguish one check constraint from another
  in a command that writes six tables.
- A command assertion that rejected without a declared message said "command
  assertion rejected", naming neither the step nor the rule.

Each cost a diagnostic round trip that the log should have answered.

## Decision

Where a failure is retried rather than surfaced, the log names the specific
thing that refused. The transition consumer prints the whole error chain
(`{error:#}`). A Process command database failure names the relation and
constraint alongside the SQLSTATE. An assertion without a declared message
identifies its step and its rule.

Command *response* bodies are unchanged. A command statement embeds request
values, so a PostgreSQL primary message can disclose them; the opaque
`data-exception` body stays exactly as it is, and the cause goes to the log
instead — which previously recorded nothing at all for that case.

The log records the primary message and never `detail()`. This is a deliberate
line rather than a claim that the log is value-free: a primary message can
quote one offending literal (`invalid input syntax for type integer: "abc"`),
while `detail()` prints whole key tuples (`Key (email)=(alice@example.com)
already exists`). One quoted literal is the price of a diagnosable type error;
a key tuple is another party's data and stays out of both surfaces.

## Alternatives

| Option | Why Not |
|--------|---------|
| Return the cause to the caller | A command statement's error text can carry request values and relation details. The opacity is deliberate. |
| Leave it to the operator to reproduce with a debugger | The failure is inside a durable retry loop against committed state; there is nothing to attach to. |
| Record the cause in the durable journal instead | The journal holds outcomes that are part of the contract; a compile-time or constraint mismatch is an operator concern, not a Process fact. |

## Consequences

An operator reading the engine log can tell which declaration is wrong without
instrumenting the engine.

The assertion message is different from the other two: it is user-visible when
no message is declared, and a step name and a rule name are *not* otherwise
readable — neither appears in the generated GraphQL schema. Naming them is
still right, because a rejected assertion is a validation outcome and telling
an authorized caller which rule refused their input is what lets them fix it.
What it discloses is static metadata naming, never data or another party's
rows. Declaring a `message` remains the way to keep those names private.
