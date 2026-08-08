# donat plugin

Skills and patterns for **building an application on donat** — the declarative
GraphQL/REST/MCP engine over Postgres. It teaches the metadata format, the
layer rule that decides what goes in SQL and what goes in YAML, and the
conventions the reference application (`examples/petshop`) is built on.

This is for people writing an application *on* donat. Contributing to the
engine itself is covered by the repository's own `CLAUDE.md` / `AGENTS.md`.

## Install — Claude Code

```
/plugin marketplace add donatlabs/donat
/plugin install donat@donat
```

Or, from a local checkout:

```
/plugin marketplace add /path/to/donat
/plugin install donat@donat
```

Skills load themselves when relevant; the slash commands are available
immediately.

## Install — Codex

The plugin ships a `.codex-plugin/plugin.json` manifest pointing at the same
`skills/` directory, which is the convention other multi-harness plugins use
(superpowers ships `.claude-plugin`, `.codex-plugin`, `.cursor-plugin` and
`.kimi-plugin` side by side over one skills tree).

If your Codex install does not pick that up, everything also works through
Codex's two documented extension points — `AGENTS.md`, which it reads
automatically, and `~/.codex/prompts/*.md`, which become slash commands:

```sh
plugins/donat/codex/install.sh
cat plugins/donat/codex/AGENTS.donat.md >> /path/to/your-app/AGENTS.md
```

That copies the skills to `~/.codex/donat/skills/` and five prompts to
`~/.codex/prompts/`. Uninstall with `install.sh --uninstall`.

The fallback path is flatter than the Claude Code one: the skills become files
an agent is told to read, rather than material the harness loads on relevance.

## What is in it

Everything is a skill, in the `skills/<name>/SKILL.md` layout that Anthropic's
own `example-plugin` recommends for new plugins over the legacy `commands/*.md`
files. Four set how the agent works, fifteen are model-invoked knowledge, and
five carry `argument-hint` and are invoked by name as slash commands.

### Mode skills

`using-donat` is the entry point, in the shape of superpowers' `using-superpowers`:
it fires at the start of a donat conversation, picks the mode, routes to the
right skill, and carries the red-flag table.

| Skill | For |
|---|---|
| `using-donat` | Every donat conversation. Picks the mode, sets priority, defines what "done" means |
| `using-analytics-mode` | Someone who owns the domain but not the codebase — a founder, analyst, PM or ops lead. A domain interview sized to the stakes, a plain-language brief to confirm, results as scenarios. References: `interview.md`, `domain-brief.md`, `talking.md` |
| `using-tech-mode` | Someone who reads the diff and runs the commands. Files, commands and real output |
| `declaring-not-coding` | Every build task. Every requirement becomes a declaration or a written request for a developer — never a script, service, trigger or client-side check |

The two modes differ in vocabulary, artifacts and confirmation gate. They do
**not** differ on the layer rule, the no-admin-role rule, or what counts as
done — those are properties of the engine, not of the audience.

Both carry the same anti-waffle discipline: lead with the answer, a fixed reply
skeleton, recommend rather than survey, and a ceremony gate so a one-field
change never triggers a half-hour interview. The gate, the declared output
shape and the named-failures table are borrowed from
[UditAkhourii/adhd](https://github.com/uditakhourii/adhd), which uses them to
keep an expensive technique from firing on cheap problems.

### Knowledge skills

| Skill | Covers |
|---|---|
| `donat-app-architecture` | **Start here.** The layer rule (migrations vs metadata), directory layout, the deploy pipeline, and the rules that are never negotiable |
| `donat-schema-and-migrations` | Timestamped refinery migrations, what the database owns, constraints a command depends on |
| `donat-tables-and-permissions` | Tracking, relationships, `filter` vs `check`, session variables, column masks, presets, correlated `_exists` |
| `donat-validators` | Per-role value validators with one message per condition; `not_null` and `when_present` |
| `donat-rules` | Types, named rules, first-match decision tables, test cases that run at deploy time |
| `donat-commands` | Steps in one transaction, every step kind and value reference, guards, idempotency, process effects |
| `donat-processes` | Durable state machines: connector requests, waits on verified signals, timers, branching, bounded fan-out, the ambiguity pattern |
| `donat-connectors` | HTTP provider contracts: typed responses, success contracts, error classes, bounds, retry, capacity, redaction, idempotency evidence |
| `donat-api-surfaces` | Saved operations, RESTified endpoints, MCP tools |
| `donat-file-attachments` | File columns, the object store, the request/upload/complete flow |
| `donat-authentication` | Login, users and SSO: donat verifies tokens and never issues them — provider choice, claim mapping, and the default-role trap |
| `donat-automation` | Cron triggers, event triggers, verified inbound webhooks and actions — and which of them need no receiver at all |
| `donat-embedded-go` | The engine inside a Go program (wasm core, no cgo): in-process action functions, event handlers, `ExecuteTx`, and what the embedded host refuses |
| `donat-admin-ui` | An admin panel as TypeScript resource configs, derived from the metadata — screens, forms and nav generated, never hand-written |
| `donat-deploy-and-verify` | migrate/validate/serve, environment, health probes and drain, what to test |

### Task skills (slash commands)

| Command | Does |
|---|---|
| `/donat:new-app` | Scaffold migrations, metadata and a compose stand, stopping for confirmation on the access matrix |
| `/donat:add-table` | Migration, tracking, relationships, per-role permissions, validators |
| `/donat:add-command` | A domain command with guards, idempotency and any process effect |
| `/donat:add-process` | A durable process with routed errors, deadlines and terminals |
| `/donat:review` | Review a metadata directory for permission holes and misplaced constraints |

### Agent

`donat-metadata-reviewer` — reads a metadata directory, builds the role × table
access matrix, and reports ranked findings with concrete failure scenarios. It
reads; it does not edit.

## The two things worth knowing before anything else

**The layer rule.** Does this bind *every* writer, or *one role*? `quantity > 0`
is a database `CHECK` in a migration. `quantity <= 20` for shoppers is a
per-role `validate` entry. In the wrong layer, one blocks legitimate writers and
the other is bypassed by any other role.

**There is no admin role.** Not disabled, not gated — absent. Every access
resolves through an explicit per-role permission, including the ones a durable
process makes on your behalf. Any design that needs a bypass is a design to
revisit, and the plugin will say so rather than work around it.

## Source

Everything here is derived from the repository it ships in — `examples/petshop`
(11 durable processes, 73 commands, 60 rules, 5 connectors, files, REST and
MCP), `examples/petshop-rest`, `examples/petshop-mcp`, the ADRs under
`knowledgebase/`, and the conformance suites in `crates/conformance` that hold
all of it to its contract.

Where a pattern here is ambiguous, read the file the skill points at. The
example is the specification.

Apache-2.0, same as the engine.
