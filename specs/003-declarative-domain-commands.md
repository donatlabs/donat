# Spec 003 — Declarative domain commands

Status: proposed. This is a new Donat extension, deliberately layered above
the existing Hasura-compatible data plane. It does not replace tracked tables,
views, CRUD mutations, permissions, relationships, REST, MCP, or Actions.

## 1. Goal and non-goals

A domain command is a named, typed, role-authorized business operation such as
`create_order`, `approve_invoice`, or `cancel_subscription`. A command can
atomically change multiple relations without an application-specific webhook
or a separately deployed service.

Phase 1 supports Postgres sources only. A command compiles to **one Postgres
statement**: writable CTEs perform the changes and the final `SELECT` assembles
the response JSON. This preserves the engine's one-statement/no-N+1 invariant.

The following are explicitly out of scope:

- arbitrary SQL, stored-procedure bodies, JavaScript, WASM, or user-supplied
  Rust;
- HTTP, connector calls, email, or any other external I/O in a transaction;
- writes to views, materialized views, foreign tables, or partitioned-table
  parents in Phase 1;
- changing metadata or permissions at runtime.

## 2. Metadata surface

`commands.yaml` is a top-level metadata section. The loader adds
`commands: Vec<Command>` to `Metadata` and preserves the directory-format
convention used by `cron_triggers.yaml`.

```yaml
- name: create_order
  source: default
  permissions:
    - role: customer
  arguments:
    customer_id: uuid!
    lines: "[CreateOrderLine!]!"
  guards:
    - rule: order_request_is_well_formed
      with:
        lines: { arg: lines }
      message: order request is not valid
  steps:
    - name: order
      insert:
        table: public.orders
        object:
          customer_id: { arg: customer_id }
          status: { literal: draft }
        returning: [id, customer_id, status]
    - name: lines
      insert_many:
        table: public.order_lines
        for_each: { arg: lines }
        object:
          order_id: { step: order, column: id }
          sku: { item: sku }
          quantity: { item: quantity }
        returning: [id, sku, quantity]
  result:
    order_id: { step: order, column: id }
    status: { step: order, column: status }
    lines: { step: lines }
```

`arguments` names GraphQL scalars or input objects already declared in custom
types. `object` values are only: `arg`, `item`, `step`, `literal`, and a named
rule expression. They are not string interpolation.

Phase 1 operations are `select_one`, `insert`, `insert_many`, `update`,
`delete`, and `assert`. Each step receives an explicit name; a reference can only point to a
previously declared step. `update` and `delete` require a primary-key equality
predicate built from arguments or prior step output. This prevents accidental
set-wide mutations while still covering normal SaaS commands.

## 3. Authorization and relation safety

The schema exposes a command as a generated top-level GraphQL mutation only
for a role named in `permissions`. Command permission is an additional gate;
it never bypasses the existing table permission of that role. Metadata
validation proves that every command operation is permitted by the corresponding
insert, select, update, or delete permission and that all referenced columns
are allowed.

When a process invokes a command, its transition names an explicit `run_as_role`.
That role must also appear in the command's `permissions` and must have all
required table permissions. It is a normal classic role, not an implicit
workflow identity or a permission bypass.

The catalog currently discovers relation kinds `r`, `v`, `m`, `f`, and `p` but
does not retain the kind in `TableInfo`. This spec requires retaining it before
commands exist. Only `r` (ordinary tables) may be a Phase-1 write target.
Views remain first-class query targets through existing tracking, but cannot
become an accidental command write target merely because Postgres happens to
consider a view updatable.

Every generated command mutation is executed with the request's explicit
`X-Donat-Role`. There is no command-owner role, no admin fallback, and no
permission bypass.

## 4. Compilation and execution contract

Metadata loading validates type names, source names, relations, columns,
step-reference order, operation permissions, and rule names. `donat validate`
reports all invalid command entries before serving begins.

Each validated command has a canonical definition fingerprint: source, typed
arguments/result, steps, rule AST hashes, idempotency policy, and effect
shapes. A process revision stores the fingerprint of every command it can
invoke. The process worker resolves that pinned command definition, while
current explicit table permissions still apply at execution time; a later
permission revocation must fail safely rather than revive an old privilege.
`migrate --metadata-dir` rejects removal or incompatible replacement of a
command definition referenced by a non-terminal process instance.

Schema compilation produces a role-specific mutation field and a typed output
object. Planning lowers the command into a new SQL-free IR node. `sqlgen`
lowers that node to one parameter-safe Postgres statement with writable CTEs;
the final CTE emits the result object as JSON. No Rust row-by-row assembly is
allowed. An `assert` or guard that evaluates false rolls back the statement and
returns the engine's normal GraphQL execution error envelope with the command
field path; the native conformance fixture freezes its exact code and body.

The command transaction may write a narrowly typed internal process outbox row
only after Spec 005's journal exists. It must be another CTE in the same
statement, never a post-commit best-effort side effect. The process worker
converts that outbox row into a pinned process event in its own short
transaction.

## 5. Test-first acceptance contract

Each behavior begins as a conformance case under
`crates/conformance/fixtures/commands/` and a call in
`crates/conformance/tests/commands.rs`, before implementation.

| Behavior | First failing test | Regression proof |
| --- | --- | --- |
| Role is allowed and creates order plus lines atomically | `create_order_customer` | both relations contain rows and GraphQL result is exact |
| Guard rejects | `create_order_guard_denied` | no row is written; exact GraphQL error body is frozen |
| A later CTE fails | `create_order_rolls_back` | neither order nor line is visible |
| Missing table permission | metadata validation fixture | `donat validate` fails before server startup |
| View used as a target | metadata validation fixture | validation identifies the command and relation |
| SQL generation | `crates/sqlgen` insta test | reviewed single-statement snapshot contains ordered CTEs |

The TDD loop is: add the failing native fixture; add the crate-level IR and
snapshot test; implement the smallest compiler path; rebuild the `donat`
binary; run the focused conformance suite; then run `make conformance`.

## 6. Reference porting plan

This feature has no source-equivalent upstream to copy. The command IR and SQL
compiler are independently written because the required one-statement Postgres
semantics and Donat permission model are project-specific.

The existing code is the primary implementation reference:

| Reference | Revision | Use | Port policy |
| --- | --- | --- | --- |
| `crates/ir`, `crates/sqlgen`, and mutation planner in this repository | current Donat revision | IR boundaries, SQL literal safety, insta review practice | native extension, no external code |
| Hasura-compatible table mutation conformance fixtures already in `crates/conformance` | current Donat revision | role and GraphQL result conventions | fixtures are extended, not copied from an external project |

Every future source-level port must add its URL, immutable commit, file list,
license, required notices, Rust target file, and an `upstream fixture -> Donat
test` mapping to this table before code is imported. Only Apache-2.0,
MIT, or BSD-compatible source is eligible. Non-permissive sources may inform
behavior but contribute neither code nor copied fixtures.


## 7. Normative command surface

### 7.1 Scope and names

A command name must be a valid GraphQL name and be unique across commands,
Actions, tracked-function mutations, and generated table mutations for the
same role. The GraphQL field name is exactly the metadata name; no implicit
snake_case or camelCase conversion occurs. A command belongs to exactly one
source, and the source must have kind Postgres. Commands cannot read or write a
second source in Phase 1.

Each write target must be both tracked in that source and present in the
catalog as relation kind r. A target must have a non-empty primary key. The
catalog change that preserves relation kind is therefore a prerequisite of this
feature and applies to every supported catalog implementation; non-Postgres
catalogs may report their native kind but are rejected as command sources in
this phase.

The canonical arguments form is a list, so descriptions and a deterministic
schema order are preserved:

~~~yaml
arguments:
  - name: customer_id
    type: uuid!
  - name: lines
    type: "[CreateOrderLine!]!"
  - name: request_id
    type: uuid!
~~~

The mapping form shown in the earlier example is accepted only as YAML
shorthand and is normalized to this list before validation. Duplicate argument
names, GraphQL-reserved names, and types not known to the role schema are
deployment errors.

### 7.2 Generated GraphQL contract

For command create_order the role schema contains a mutation field with one
argument per declared command argument:

~~~graphql
mutation CreateOrder($customer: uuid!, $lines: [CreateOrderLine!]!, $request: uuid!) {
  create_order(customer_id: $customer, lines: $lines, request_id: $request) {
    order_id
    status
    lines { id sku quantity }
  }
}
~~~

The compiler synthesizes one output object named CreateOrderResult and, for
each step exposed as a row or row array, a stable generated row type such as
CreateOrderLinesRow. Result field order is metadata declaration order.
Selections can contain only declared result fields; a command never exposes an
unlisted table column merely because the calling role may select that column
through ordinary GraphQL CRUD.

A scalar result field has the GraphQL scalar of its referenced catalog column
or declared literal. A step reference is an object for select_one, insert,
update, and delete, and a list of objects for insert_many. Every object field is named in
that step's returning list. A result field referring to a zero-row update or
delete is null only when the step explicitly sets require_affected: false;
the default is require_affected: true and produces a rejected command.

### 7.3 Values, predicates, and cardinality

A value reference is one of the following closed forms:

| Form | Meaning | Validation |
| --- | --- | --- |
| argument | a declared command argument | GraphQL type is assignable to target column |
| item | a field of the current insert_many item | allowed only inside that step; field type is assignable |
| step | a previously completed, declared returning column | source step precedes destination and has compatible cardinality |
| literal | a metadata literal | parsed and cast at deploy time against the column type |
| rule | a named Spec 004 expression | result type is assignable and all parameter bindings are explicit |

No form permits a SQL fragment, identifier, JSON path, or string template.
update and delete accept only an exact primary-key predicate, represented as a
map containing every primary-key column. A command may assert an arbitrary
Spec 004 boolean expression over its declared bindings, but it cannot add a
free-form WHERE clause. This is the Phase-1 guardrail against accidental
set-wide writes.

A command or process may accept a decision value only into an exactly typed data destination
or map a declared enum at deploy time to fixed action/state targets; they
never bind a generic output to role/permission/command/connector identifiers.

select_one requires a complete primary-key predicate and returns exactly one
row when require_found is true. insert writes exactly one object. insert_many accepts exactly one declared list
argument, rejects an empty list unless allow_empty: true is set, and preserves
input order in its returned list. update and delete affect at most one row
because of their complete primary-key predicate.

## 8. Idempotency and durable hand-off

A command that starts a process must declare idempotency. Other commands may
opt in, but the absence of this block means that a client retry re-executes the
command.

~~~yaml
idempotency:
  key: { argument: request_id }
  scope:
    - { session_variable: x-donat-user-id }
    - { argument: customer_id }
  retention: 30d
effects:
  - start_process:
      process: checkout_order
      input:
        order_id: { step: order, column: id }
      idempotency_key: { argument: request_id }
~~~

scope is an ordered list of declared scalar arguments and explicitly named
session variables. It is serialized canonically and hashed; raw session values
and the key never appear in logs. The validator rejects secret-looking
variables and mutable JSON values as idempotency scope components.

The deploy-time migration creates donat.command_invocations with the unique
key (command_name, scope_hash, key), an input_fingerprint, canonical full
result JSON, and expiry time. In the command's one statement, an insert-or-lock
CTE claims the key, the write CTEs execute only for the first claim, and the
canonical result plus every process outbox request is committed in the same
statement. A repeat with the same fingerprint returns the stored canonical
result, projected to the caller's GraphQL selection in SQL. A repeat with a
different fingerprint returns the normal GraphQL error envelope with code
validation-failed, the command field path, and message
"idempotency key was reused with different input".

A process-start effect inserts one Spec 005 process-start request only on the
first command execution. Its key is unique per command invocation and effect
position and it stores the statically compiled active process revision. The
command cannot enqueue a connector job itself; the process worker uses that
stored revision, never a newer active revision, after it consumes the committed
request.

A second, deliberately narrow effect resumes a declared waiting state or
delivers a declared cancellation signal to a non-terminal process. It is the
only Phase-1 way a human/domain approval or cancellation reaches a process;
there is no generic process-management endpoint:

~~~yaml
effects:
  - signal_process:
      process: checkout_order
      signal: approval_recorded
      correlate:
        order_id: { argument: order_id }
      payload:
        approved_by: { session_variable: x-donat-user-id }
      idempotency_key: { argument: request_id }
~~~

The target process must declare the exact signal name, correlation fields, and
payload type in a `wait_for_command` state or its optional cancellation block.
`signal_process` is emitted only by an ordinary command under its explicit
role. It writes an append-only, idempotently deduplicated process-signal request
in the same command CTE; the process worker later appends the event and applies
only the declared transition. It cannot requeue, rewrite, or otherwise
administer a process.

## 9. Error and rollback contract

Command precondition failure, missing required affected row, invalid
idempotency reuse, and a rule result other than boolean true are business
rejections. They return the existing PlanError wire shape:

~~~json
{
  "errors": [{
    "extensions": {
      "path": "$.selectionSet.create_order",
      "code": "validation-failed"
    },
    "message": "customer is not allowed to order"
  }]
}
~~~

A new deploy-time donat.raise_graphql_error helper serializes this already
established shape for a command CTE. The GraphQL database-error translator
accepts its validated payload in addition to the existing permission-check
payload. Raw PostgreSQL messages, SQL text, and connector details are never
returned to the caller.

Any rejection or database error rolls back all command CTE writes, including
the idempotency claim and process outbox request. Existing table permission checks still
raise their established permission-error response. Metadata errors occur before
the server publishes the snapshot and use donat validate output; they are not
runtime GraphQL errors.

## 10. Required metadata validation matrix

| Invalid declaration | Required validation outcome |
| --- | --- |
| command source is not Postgres | identifies command and source kind |
| target is a view or has no primary key | identifies command, step, and relation kind |
| command name collides in a role schema | identifies both conflicting fields and role |
| value references a later or unknown step | identifies command, step, and reference |
| update/delete omits a primary-key column | identifies the missing column |
| role lacks a referenced table or column permission | identifies role, operation, and relation |
| result field has no derivable GraphQL type | identifies result field and source value |
| process effect names a missing process or has incompatible input | identifies command, effect, and process field |
| process signal names an undeclared waiting signal or has incompatible payload/correlation | identifies command, effect, process state, and field |
| idempotency key/scope is not deterministic | identifies the offending component |
| a role used by process run_as_role is absent from command permissions | identifies process state, command, and role |
| active process revision references a removed/incompatible command definition | migrate rejects the deployment before publishing metadata |

## 11. Expanded test-first matrix

In addition to the tests in Section 5, the first implementation series must
add the following tests before the corresponding compiler code:

| Test ID | Layer | Required assertion |
| --- | --- | --- |
| command_schema_is_role_specific | schema unit | allowed role sees field and output type; disallowed role does not |
| command_result_selection_is_projected | conformance | replayed idempotency result contains only the requested GraphQL selection |
| command_idempotency_replay | conformance | same key/input writes once and creates one process-start request |
| command_idempotency_conflict | conformance | different input with same scope/key returns exact frozen error |
| command_guard_rolls_back | conformance | denied command leaves user tables and donat.command_invocations unchanged |
| command_update_requires_row | conformance | missing PK row is rejected and no later CTE runs |
| command_start_is_atomic | integration | order, invocation, and process-start request are visible together or none are |
| command_signal_is_atomic | integration | command result and exactly one correlated process-signal request commit together |
| command_sql_is_single_statement | sqlgen insta | one reviewed Postgres statement; no semicolon-separated second statement |
| command_never_targets_view | catalog plus metadata unit | relkind v is preserved then rejected |

## 12. Reference extraction ledger

The native references in Section 6 are used at these concrete boundaries:

| Donat reference | Reused behavior, not copied text | Required new test location |
| --- | --- | --- |
| crates/metadata/src/loader.rs load_section pattern | optional top-level YAML loading and include handling | crates/metadata tests for absent, list, and include commands.yaml |
| crates/schema/src/plan.rs PlanError | validation-failed wire shape and path convention | crates/schema command planner tests |
| crates/ir and crates/sqlgen mutation pipeline | SQL-free plan followed by reviewed SQL snapshot | crates/ir and crates/sqlgen command tests |
| crates/server/src/gql.rs database error translation | safe conversion of structured SQL rejection to GraphQL JSON | crates/server gql unit test |
| crates/conformance/tests/actions.rs | explicit-role fixture setup and exact body checks | crates/conformance/tests/commands.rs |

No external source code is selected for this compiler. The implementation is a
native Rust design; the ledger exists so a later port cannot be smuggled in
without provenance and TDD coverage.


## 13. Component ownership boundaries

| Area | Required ownership | Prohibited shortcut |
| --- | --- | --- |
| Metadata | crates/metadata types, loader, and validation | parsing command YAML in server request handlers |
| Relation safety | crates/catalog relation kind plus catalog tests | treating an updatable view as a table based on a failed write |
| Role schema and plan | crates/schema role-specific command field and SQL-free command IR | dispatching commands through an Action webhook |
| SQL | crates/sqlgen Postgres CTE renderer and insta snapshots | multiple client-side statements or Rust response assembly |
| Runtime error conversion | crates/server GraphQL dispatcher and structured SQL error translator | leaking PostgreSQL text or inventing a separate HTTP error envelope |
| Deploy-time objects | migrations plus migrate/validate paths | creating command tables or helper functions at serve boot |
| Behavior proof | crates/conformance commands suite | unit-only proof of transaction or role behavior |

The command compiler may reuse existing mutation-expression utilities only after
they are extended to carry a command path and structured rejection. It must not
make the generic CRUD mutation planner aware of command YAML by stringly typed
special cases.
