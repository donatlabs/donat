<!--
Append this section to the AGENTS.md of a repository that builds an
application on donat. Codex reads AGENTS.md automatically; it has no plugin
format, so this file plus the prompts in ./prompts are the Codex equivalent of
the Claude Code plugin's skills and slash commands.
-->

## Working on this donat application

This application is declared, not coded. One Rust binary (donat) plus Postgres
serves GraphQL, REST and MCP from versioned SQL migrations and a directory of
YAML metadata. There is no request-path application code to write.

### The layer rule

| Layer | Owns | Changed by |
|---|---|---|
| SQL migrations | tables, columns, foreign keys, unique constraints, `CHECK`, indexes — anything binding **every** writer | `donat migrate`, versioned files |
| YAML metadata | table visibility, per-role row filters and column masks, validators, rules, commands, processes, connectors, REST/MCP surfaces | edited under `metadata/`, loaded at boot |

The test: **does this rule bind every writer, or one role?** `quantity > 0` is
a database `CHECK`. `quantity <= 20` for shoppers is a per-role `validate`
entry. Putting either in the other layer is a defect — one blocks legitimate
writers, the other is bypassed by any other role.

### Blocking rules

- **There is no admin role.** Not disabled — absent. Every access, including
  the ones a process makes, resolves through an explicit per-role permission.
  If a design needs "something that sees everything", that is an ordinary role
  with explicit permissions and a reason. Never propose or add an admin role or
  a permission bypass.
- **There is no runtime configuration API.** No `run_sql`, no metadata mutation
  over HTTP. If a change is not in a migration or in `metadata/`, it does not
  happen.
- **Declare it, or take the next tier down — never code around it.** The order
  is: (1) a migration or a metadata declaration, which covers almost
  everything; (2) if it truly cannot be declared, a named typed function
  registered against a declared action or event trigger, resolved in-process
  and checked at boot — not a service; (3) only then a written request for a
  developer to build a separate service. A loose script, a PL/pgSQL trigger
  carrying a domain rule, a view that decides who sees what, or a client-side
  check are all the same failure: the rule has left the permission model and
  your partner can no longer read it. Tier 2 needs the engine embedded in a Go
  program; on the standalone server it collapses into tier 3, so establish the
  host before promising it.
- A role comes from a verified JWT or an authentication hook, and from
  nothing else. No header names one: `X-Donat-Role` only picks between roles a
  token already granted, and there is no shared secret in this engine.
- Secrets in metadata are always `value_from_env:`, never literals.
- **Talk in the partner's language; write the repository in English.** Comments,
  metadata descriptions, scripts, docs and commit messages are English whatever
  language the conversation is in — a repository is read by people who were not
  in the room. The one exception is copy an end user will see, such as a
  validator's message, which follows the product.
- **Never accept a secret in a message.** Identifiers and public keys can be
  pasted; anything a provider calls secret, private or signing is pasted by the
  partner into a place you point at, never sent to you.

### The loop

```sh
donat migrate  --migrations-dir migrations   # DDL first
donat validate --metadata-dir metadata       # metadata against the real schema
donat serve
```

`validate` is the compiler. Run it after every metadata change and treat a red
result the way you would treat a failed build. Never run it before `migrate` —
it would check against the old schema and pass for the wrong reason.

### Verification standard

A permission is only proven by the request it refuses. Any change to
permissions, validators, commands or processes is unfinished until there is
evidence that the *wrong* role, the *wrong* session or the *wrong* value is
turned away — with the real output pasted, not asserted.

### Working mode

Decide before answering, and say which you picked:

- **Analytics mode** — your partner owns the domain, not the codebase. Interview
  in business terms, write the schema and metadata yourself, report results as
  scenarios. Never paste YAML, file paths or errors at them.
- **Tech mode** — your partner reads the diff and runs the commands. Show
  `file:line`, exact commands and real output.

When the signals are mixed, ask once, then stay in the chosen mode.

The modes differ in vocabulary, artifacts and confirmation gate. They do not
differ on the layer rule, the no-admin-role rule, or the verification standard.

In both modes: lead with the answer, cut every sentence that survives its own
deletion, recommend rather than survey, and size the ceremony to the stakes — a
one-field change is not an interview. Friendly means warm and direct, never
padded and never soft on the truth.

### Reference

Detailed patterns live in `~/.codex/donat/skills/*/SKILL.md` (installed by
`plugins/donat/codex/install.sh`):

| Topic | Skill |
|---|---|
| how to work at all — mode, priority, red flags | `using-donat` |
| interviewing a non-technical domain owner | `using-analytics-mode` (+ its `references/`) |
| working with an engineer | `using-tech-mode` |
| which primitive a requirement becomes; how to escalate | `declaring-not-coding` |
| login, users, SSO, mapping token claims to roles | `donat-authentication` |
| schedules, row-change hooks, inbound webhooks, actions | `donat-automation` |
| an inbox, an email, an opt-out, a digest | `donat-notifications` |
| the engine embedded in a Go program; in-process functions | `donat-embedded-go` |
| the platform's screens as resource configs, not components | `donat-platform-ui` |
| layer model, directory layout, deploy pipeline | `donat-app-architecture` |
| migrations, naming, what the database owns | `donat-schema-and-migrations` |
| tracking, relationships, filter vs check, `_exists` | `donat-tables-and-permissions` |
| per-role value validators, nullability | `donat-validators` |
| types, rules, decision tables | `donat-rules` |
| commands: steps, guards, idempotency, effects | `donat-commands` |
| durable processes: waits, timers, fan-out, ambiguity | `donat-processes` |
| connectors: contracts, bounds, retries, idempotency evidence | `donat-connectors` |
| saved operations, REST endpoints, MCP tools | `donat-api-surfaces` |
| file attachments and the object store | `donat-file-attachments` |
| running, env, health probes, testing | `donat-deploy-and-verify` |

The worked reference application is `examples/petshop` in the donat repository.
When a pattern is ambiguous, read the file the skill points at — the example is
the specification.
