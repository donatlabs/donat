---
type: decision
status: accepted
date: 2026-07-29
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Command replay identity is qualified by source and explicit role

## Context

A command name is local to a metadata source. The schema may therefore expose
two commands with the same name to disjoint roles, and those commands may
write different physical tables. V3 and V4 keyed the completed-invocation
journal and first-executor claim by `(command_name, scope_hash, key)`. Two
source-local commands with the same name and request key could consequently
replay one another's result or suppress a legitimate write.

The role is also part of the executable command contract: permissions are
compiled for the explicit request role, and this engine has no admin or
permission-bypass role. Replaying a result produced under another role would
cross that boundary even when source and command name matched.

Existing V3/V4 rows contain neither source nor role. A migration cannot infer
their owner without inventing authorization context, and treating them as new
qualified rows could execute an already completed domain write twice.

## Decision

The compiled command catalog retains its metadata source. Planning constructs
a resolved IR identity containing source, command name, and the explicit
session role. SQLgen serializes that tuple with a versioned, length-prefixed
encoding and uses it in both the V3 result journal and V4 claim primary keys:
`(command_identity, scope_hash, key)`. The display `command_name` remains as
diagnostic metadata but no longer owns election or replay uniqueness.

V5 is a deploy-time migration. It adds the non-null identity column, replaces
both primary keys, and moves existing rows into an explicit
`legacy-unqualified:<hex-command-name>` namespace. It does not guess a source
or role. Before a qualified claim, the generated command statement checks both
legacy tables for the same command name, hashed scope, and key. A match raises
the existing structured `validation-failed` P0D01 envelope before claim
election or any domain DML. The response exposes neither the key nor the scope.

Legacy rows remain fail-closed until normal retention cleanup removes them or
an operator resolves them out of band. Expiry alone is not proof of ownership:
the engine must not silently trade an attribution ambiguity for a possible
duplicate external business effect.

## Alternatives

| Option | Why Not |
| --- | --- |
| Keep command name as the replay identity | Command names are source-local and permissions are role-specific; the key can cross both boundaries. |
| Qualify by source but not role | A canonical result produced under one explicit permission set could replay under another. |
| Infer source and role for V3/V4 rows during migration | The stored data does not contain either value, so any attribution would be fabricated. |
| Treat every legacy row as a miss | A post-upgrade retry could repeat an already completed domain write. |
| Store raw scope values to disambiguate later | Broadens retained request data and is unnecessary; the existing canonical scope hash is sufficient for fail-closed matching. |

## Consequences

Same-named commands on distinct sources and disjoint roles can use the same
request key independently, while retries remain isolated to one executable
command identity. The IR and SQL snapshots carry an explicit authorization
identity, and both durable tables share exactly the same uniqueness boundary.

Upgrades preserve every prior result and claim without pretending to know its
owner. A retry that intersects an unattributable legacy row is deliberately
unavailable rather than duplicated; operators can inspect retention and clean
legacy rows through deployment operations. The serving binary still performs
no runtime DDL, emits one Postgres statement per command, and gains no admin
surface or auxiliary service.
