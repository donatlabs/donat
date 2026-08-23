# An inherited role grants the tables and not the commands

**Effort: S–M. Status: TODO — found while adopting `modules/notifications` into `examples/petshop`.**

## What is wrong

`inherited_roles` carries table permissions and silently drops command
permissions. A deployment that writes

```yaml
- role_name: support
  role_set: [support, notification_scheduler]
```

gets half of what that line says.

Verified against a running Petshop stand, asking the engine's own schema:

```
support:                reads notification_pending_digest = True
                        can call flush_notification_digests = False
notification_scheduler: reads notification_pending_digest = True
                        can call flush_notification_digests = True
```

`notification_pending_digest` is granted to `notification_scheduler` by a
`select_permission`, and `flush_notification_digests` is granted to it by the
command's own `permissions:` list. The inheritance reached the first and not the
second.

## Why it matters

It is not that the behaviour is unsafe — it errs closed, and
`declarative-saas/decisions/019-command-only-table-permissions` establishes that
commands are a separate resolution plane that generic schema generation ignores.
The problem is that `inherited_roles` reads like set union and is not, and
nothing says so:

- A deployment adopting a module and inheriting its role gets the module's
  reads and none of its actions, and finds out when the call it expected to work
  returns "field not found".
- The reverse audit is worse. Someone asking "who can call this command?" gets
  the right answer only if they know to ignore inheritance — and someone asking
  "what does this role set grant?" cannot answer from the two files in front of
  them.

`examples/petshop` hit exactly this: `support` was given
`notification_scheduler` to run a digest sweep by hand, could read the backlog,
and could not sweep. The store's role matrix caught it
(`tests-system/tests/test_role_matrix.py`), which is the only reason it is
written down rather than shipped.

## What closing it needs

The fork to settle first is **which way it should go**, and it is a real fork:

1. **Make inheritance total.** `role_set` unions command permissions too. It is
   what the declaration looks like it means. It also widens what a role can
   *do* rather than only what it can see, so every existing `inherited_roles`
   entry in every deployment silently gains commands on upgrade — which is a
   permission change nobody asked for and the worst kind to ship quietly.
2. **Make it explicitly partial.** Keep the behaviour and refuse the ambiguity:
   `donat validate` warns — or refuses — when an inherited role set names a role
   that owns commands, telling the operator to grant those directly. Nothing
   changes for anyone already deployed, and the surprise moves to deploy time.

Option 2 is the smaller and safer change and is probably right; option 1 is what
a reader expects. Either way the fix is incomplete without documenting the rule
next to `inherited_roles` itself, because today the only way to learn it is to
run the query above.

## How to reproduce

Grant a role a command, inherit that role into another, and ask the engine what
the second role may call:

```graphql
query { __type(name: "mutation_root") { fields { name } } }
```

The command is absent. Ask the same about `query_root` and the inherited table
is there.
