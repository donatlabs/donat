Review a donat metadata directory. Arguments: $ARGUMENTS (default `metadata/`)

Read first, as needed:
- `~/.codex/donat/skills/donat-tables-and-permissions/SKILL.md`
- `~/.codex/donat/skills/donat-validators/SKILL.md`
- `~/.codex/donat/skills/donat-commands/SKILL.md`
- `~/.codex/donat/skills/donat-processes/SKILL.md`
- `~/.codex/donat/skills/donat-connectors/SKILL.md`

Build a role × table matrix of what each role may read and write **before**
judging anything — most real findings are only visible there. Then cross-check
`metadata/` against `migrations/`.

Report findings ranked by severity, each with `file:line`, one sentence on the
defect, and the concrete role/input/sequence that triggers it. No style
findings. If nothing is wrong, say so rather than manufacturing findings.

**Blocking**: an admin-like role or any permission written as a bypass; a row
filter letting one caller reach another's rows; a credential as a literal
instead of `value_from_env`; a published REST/MCP surface whose role has no
matching permission.

**High**: an insert `check` not binding the row to the caller's session where a
preset would; an update `check` identical to its `filter`; a read-then-assert
race where a per-row `check:` belongs; a command guard relying on a unique
constraint no migration creates; a validator on a nullable column with no
`not_null:`/`when_present:`; a state-changing command with no idempotency key;
a mutating connector operation with no `provider_idempotent` evidence, or a
retry window exceeding declared key retention; a `wait` with no deadline; a
`request` error class with no route; a provider mutation with no read-only
lookup.

**Medium**: `columns: "*"` on a growing table or `filter: {}` broader than the
role's purpose; an unbounded batch or `for_each`; a universal rule expressed as
a per-role validator, or a role-specific rule as a database CHECK;
`retry_on` including `validation`/`authentication`; a credential header missing
from `redaction`; `maximum_redirects` above 0; a decision table with no test
case for its fallback row.

Finish by running `donat validate --metadata-dir <dir>` if a migrated database
is reachable, and paste its output verbatim.
