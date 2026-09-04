# Spec 010 — A command's tenant scopes the steps after it

Status: accepted. Decision:
`knowledgebase/declarative-saas/decisions/101-a-commands-tenant-once-resolved-scopes-everything-after-it.md`.
Origin: issue #57, the controlplane's service identity that resolves its
tenant in its first step and then needs to read and update inside it.

## 1. What is true today, verified at `4d5e4d3`

- **A command may take its tenant from a step.** `Command.tenant` is
  `establishes: {step, column}` or `from: {step, column}`
  (`crates/metadata/src/types.rs:636`). The write preset for every later
  insert is `CommandExecutionValue::StepColumn { cte, column }`
  (`crates/schema/src/plan_mutation.rs`, `command_tenant_assignment`), which
  sqlgen renders as `(SELECT <column> FROM <cte> LIMIT 1)`
  (`crates/sqlgen/src/lib.rs:3417`).
- **A scoped read after that step is refused at request time**
  (`plan_mutation.rs:1031`): "reads `t` scoped by the caller's tenant, but this
  command takes its own tenant from elsewhere". Conformance
  `a_scoped_read_after_the_tenant_step_is_refused` asserts it.
- **An update or a delete in such a command is refused at deploy**
  (`crates/metadata/src/tenancy.rs:717`), whatever its position.
- **The registry's serving gate is skipped** whenever `CheckTenant::Step`
  (`crates/schema/src/tenancy.rs:650`), i.e. for every write of an
  `establishes` or `from` command.
- **`CompareOp` has no arm that references a CTE** (`crates/ir/src/lib.rs:380`).
  `CompareColumnRel` is the nearest shape: a column compared to a scalar
  subquery over a related table.
- **The planner's tenant helpers take a `Session` and read the tenant claim**
  (`tenant_compare`, `tenant_via_relationship`, `registry_serving` in
  `crates/schema/src/tenancy.rs`), so they can only express the session arm.

## 2. Behaviour after this spec

Let `T` be the step a command's `tenant:` names, at index `k` in `steps`.

| Step position | Read of a tenanted table | Write |
|---|---|---|
| before `T` | refused at deploy (shared tables still fine) | refused at deploy |
| `T` itself | `from`: unscoped, as declared; `establishes`: the insert, no bound, no gate | — |
| after `T` | bounded by `T`'s tenant column, ANDed with the serving gate | insert: preset from `T`, check carries the gate; update/delete: predicate carries the bound, check carries the gate |

For `establishes` the serving gate is omitted after `T`: the registry row is
being written by this statement, inside a data-modifying CTE that later CTEs
cannot read, so gating on it would refuse every registration. The bound is
still applied. `TenantRef::Established` is that arm.

"`T`'s tenant column" is rendered as `<key> = (SELECT <column> FROM <cte of T>
LIMIT 1)`. When `T` is a `from` step with `require_found: false` and finds
nothing, the subquery is NULL, reads answer empty and writes are refused.

Session-scoped commands (no `tenant:` block) are byte-for-byte unchanged.

## 3. Changes

### 3.1 `crates/ir`

Add to `CompareOp`:

```rust
/// Column compared for equality to a single-row command step's column:
/// `col = (SELECT column FROM cte LIMIT 1)`. Produced by the tenancy layer
/// for a command whose tenant comes from a step; never from a client filter.
CompareStepColumn { cte: String, column: String },
```

### 3.2 `crates/sqlgen`

Render the arm in the `CompareOp` match (`lib.rs` ~1044):
`format!("{col} = (SELECT {} FROM {} LIMIT 1)", quote_ident(column), quote_ident(cte))`.
Add an insta test in `tests/commands.rs`: a `SelectMany` step whose `filter`
is `Compare { column: "tenant_id", op: CompareStepColumn { cte: "s1_invite",
column: "tenant_id" } }`, plus a `Insert` whose `check` is the registry
`Exists` built the same way.

### 3.3 `crates/schema/src/tenancy.rs`

```rust
pub(crate) enum TenantRef {
    Session,
    Step { cte: String, column: String },
    /// Bounds like `Step`, not gated: the registry row is in this statement.
    Established { cte: String, column: String },
}
```

- `fn tenant_op(&self, tenant: &TenantRef, session, path) -> Result<CompareOp>`:
  `Session` → `Eq(Scalar::Json(tenant_value(session)?))`;
  `Step` → `CompareStepColumn { cte, column }`.
- `tenant_compare`, `tenant_via_relationship`, `registry_serving`,
  `tenant_predicate` and `serving_gate` take `&TenantRef` and call
  `tenant_op`. As implemented, `tenant_predicate` and `serving_gate` gained
  the parameter themselves rather than growing `_from` twins: the compiler
  then forces every call site to state its arm instead of inheriting
  `Session` silently. Only `write_tenant_predicate` keeps a `Session`
  wrapper beside `write_tenant_predicate_from`.
- `write_tenant_predicate` likewise gains a `_from` form.
- `permission_predicate_full(ctx, session, apply_tenant: bool, apply_iam, path)`
  → `apply_tenant: Option<&TenantRef>` (`None` = unscoped).
  `permission_predicate_scoped(.., true, ..)` passes `Some(&TenantRef::Session)`.
- `write_permission_filter_bounded(.., bound_by_session: bool, ..)` →
  `tenant: Option<&TenantRef>`; `Some` ANDs `write_tenant_predicate_from`.
- `CheckTenant`:
  ```rust
  Session,                 // bound repeated, gate from session
  SessionBoundElsewhere,   // gate from session
  Step { cte, column },    // gate from the step; bound is the preset / the predicate
  Establishing,            // the establishing insert itself: nothing
  ```
  `write_check_expression` applies the gate for `Session`,
  `SessionBoundElsewhere` and `Step`, via `serving_gate_from`.

### 3.4 `crates/schema/src/plan_mutation.rs`

- `CommandTenantSource` gains `fn tenant_ref(&self) -> Option<TenantRef>`
  (`Session` → `Session`; `Step` → `Step`; `Creating` | `Pending` → `None`)
  and `fn check_tenant(&self) -> CheckTenant`.
- Replace `tenant_bound_by_session: bool` at every call site (11) with the
  `Option<TenantRef>` / `CheckTenant` the source yields.
- The refusal at 1031 is kept only for `tenant_ref() == None` (before the
  establishing step, or a scoped read placed before the `from` step) and its
  message becomes: "step `{name}` reads `{table}`, which is scoped by a
  tenant, but this command's tenant is not resolved until step `{T}` runs.
  Move it after that step, or read a table `tenancy.yaml` marks shared." It is
  a belt for the deploy-time check in 3.5.
- `SelectOne` passes `if step_scoped { tenant_ref } else { None }`; `SelectMany`
  passes `tenant_ref`.

### 3.5 `crates/metadata/src/tenancy.rs`

Replace the blanket update/delete refusal (line ~717) with an order-aware one
over the same loop: for every step with index `< k` that is a write
(`insert`, `insert_many`, `insert_when`, `update`, `update_many`,
`update_when`, `delete`, `allocate_many`), push

> command `{c}` takes its tenant from step `{T}`, but step `{s}` — an
> `{kind}` — runs before it, so the row it writes would belong to nobody.
> Move it after `{T}`.

Reads before `k` are left to the planner (3.4): the validator does not know
which tables are shared without the tenancy `table_scope`, which it does have —
so also refuse a `select_one`/`select_many` before `k` whose table scope is
`Key`/`ScopeVia` unless the step is itself `T` or declared `unscoped`:

> command `{c}` takes its tenant from step `{T}`, but step `{s}` reads
> `{table}`, a tenanted table, before it. Move the read after `{T}`.

### 3.6 Conformance — `crates/conformance/tests/tenancy.rs`

Onboarding suite changes:

- `ONBOARDING_SQL`: `invite` gains `redeemed boolean NOT NULL DEFAULT false`;
  a second invite `('unguessable-alpha-token', 'tenant-alpha')`; two `member`
  rows for `person-both` in alpha and beta.
- `join_and_peek` is no longer malformed: after the `from` step it
  `select_many` members `by: {user_id: {arg: user_id}}` and returns
  `peeked: {step: peek}` (a row set). Test renamed
  `a_read_after_the_tenant_step_is_bounded_by_that_tenant`: called by a
  tenantless `joiner` for `person-both`, the list holds exactly one row and it
  is `tenant-beta`.
- New command `redeem_invite`: `from: invite`; step `mark` is an `update` on
  `invite` `by: {token: {arg: other_token}}`, `set: {redeemed: true}`,
  `require_affected: true`. Test
  `an_update_after_the_tenant_step_cannot_reach_another_tenants_row`: with
  `other_token` = the beta token it succeeds; with `other_token` = the alpha
  token it is refused (no row in scope), and the alpha invite stays
  unredeemed.
- Test `an_invitation_into_a_store_the_registry_stopped_serving_is_refused`:
  suspend `tenant-beta`, `accept_invite` with the beta token → `errors`.
- Existing `an_invitation_read_outside_the_tenant_scopes_the_write_that_follows`
  and `a_command_can_establish_the_tenant_it_writes_into` stay green.

### 3.7 Metadata tests — `crates/metadata/tests/tenancy_declaration.rs`

- `a_write_before_the_tenant_step_is_refused`: `ACCEPT` with an `insert`
  placed before the `invite` step → message contains "runs before it".
- `an_update_after_the_tenant_step_is_accepted`: `ACCEPT` plus an `update`
  after the `invite` step loads.

### 3.8 Docs

- `plugins/donat/skills/donat-multitenancy/SKILL.md` §"Commands and
  processes": reads and writes after the tenant step are scoped by it; before
  it they are refused at deploy.
- `knowledgebase/declarative-saas/_index.md`: list ADR 101.
- ADR 097 consequences: add a pointer that the "cannot reference another CTE"
  limitation is lifted by 101.

## 4. Out of scope

- The JWT configuration list (issue #57 Feature 1) — its own ADR.
- `tenant: unscoped` on `select_many`, or on a `select_one` keyed by
  anything but a unique constraint.
- A `from` step that is anything other than a `select_one`.
