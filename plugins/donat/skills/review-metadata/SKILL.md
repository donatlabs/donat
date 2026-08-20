---
name: review-metadata
description: Use when the user runs /donat:review-metadata, or asks to review, audit or check a donat metadata directory — finds permission holes, constraints in the wrong layer, unbounded steps and unreachable process branches.
argument-hint: [path to metadata dir, or a file]
allowed-tools: [Read, Glob, Grep, Bash]
---

Review the donat metadata in $ARGUMENTS (default: `metadata/`).

Use the `donat-metadata-reviewer` agent if it is available; otherwise do the
review inline. Load `donat-tables-and-permissions`, `donat-validators`,
`donat-commands`, `donat-processes` and `donat-connectors` as needed.

Report findings ranked by severity, each with a `file:line`, what goes wrong,
and the concrete input or sequence that triggers it. **Do not report style.**
If nothing is wrong, say so plainly rather than manufacturing findings.

Look for, in this order:

**Access**
- A `filter: {}` or `columns: "*"` that grants more than the role's purpose —
  especially `"*"` on a table whose columns will grow.
- An unbounded permission with no `unbounded:` reason. If the directory sets
  `unbounded_permissions: declared` the loader catches it; if it does not, list
  them anyway — that list is the review. Say which look deliberate and which
  look forgotten, and remember that a non-empty static filter like
  `{status: {_eq: 'paid'}}` is just as unbounded as `{}`.
- An `unbounded:` reason that is *wrong* rather than missing — `operator` on a
  role that is a person's own data, or `worker` on a role people log into.
  The engine checks that a reason is present and not stale; whether it is the
  right reason is what a reviewer is for.
- An insert `check` that does not bind the row to the caller's session, where a
  preset plus `columns: []` would make forgery inexpressible.
- An update with a `check` that merely repeats its `filter`, so a row can be
  edited into a state the role could not have created.
- Any role named `admin`, or any comment describing a permission as a bypass.
  There is no admin role; this is a blocking finding.
- A published MCP tool or REST endpoint whose role has no matching table
  permission.

**Tenancy**, where `tenancy.yaml` exists
- A permission bounded only by tenant on a table whose rows belong to someone
  *inside* the tenant — a seller's orders, a member's documents. The tenant
  predicate is satisfied and the rows are still not theirs.
- A `shared: read_only` exemption on a table some role can write, or a table in
  `exempt:` that should have been in `keys:` — putting the registry there
  publishes every tenant to every tenant.
- A command writing before the step its `tenant:` comes from, and any
  `tenant: unscoped` step: each is a deliberate hole and should read like one.
- A cross-tenant foreign key that is not composite, so the database allows a
  reference the predicate would refuse.

**Layers**
- A role-shaped rule in a database CHECK, which binds writers it should not.
- A universal rule in a `validate` list, which any other role bypasses.
- A validator reading a nullable column without `not_null:` or `when_present:`
  (a deploy failure), or a `when_present:` assumed to carry to the next entry
  (it does not).

**Commands**
- A read-then-assert pair where a per-row `check:` would close the race.
- A guard whose correctness needs a unique constraint that no migration
  creates.
- A batch step with no `maximum_rows` / `maximum_items`.
- A state-changing command with no `idempotency` key.
- More than one command declaring `start_process` for the same process.

**Processes**
- A `wait` with no `deadline`/`on_timeout`.
- A `request` error class with no route, or a `fallback` pointing at a happy
  path.
- A `when` whose `default` is a success state rather than a `fail`.
- A provider mutation with no read-only `lookup_*` counterpart, so a timeout
  has nowhere to go.
- `for_each` without `max_items` or `max_concurrency`.

**Connectors**
- A credential written as a literal instead of `value_from_env`.
- A mutating operation with no `provider_idempotent` evidence.
- `retry_on` including `validation` or `authentication`.
- `maximum_redirects` above 0, or a response field with no size bound.
- A credential header missing from `redaction`.

Finish by running `donat validate --metadata-dir <dir>` if a migrated database
is reachable, and report its output verbatim — several of the above are things
it catches for free.
