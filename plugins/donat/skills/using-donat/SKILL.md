---
name: using-donat
description: Use at the start of any conversation about a donat application - establishes the working mode (analytics or tech), routes to the right donat skills, and requires invoking them before any answer.
---

<SUBAGENT-STOP>
If you were dispatched as a subagent to execute one specific, already-scoped
task, ignore this skill and do the task.
</SUBAGENT-STOP>

# Working on a donat application

donat applications are **declared, not coded**. There is no request-path
application code. Every answer you give either lands in a SQL migration or in
YAML metadata — and which one it lands in is a decision you can get wrong.

## The Rule

**Pick the mode before answering anything**, including clarifying questions.
Then invoke the relevant donat skill before writing, exploring or advising.
Announce it — "Using [skill] to [purpose]" — **in tech mode only**. In analytics
mode the announcement is suppressed along with every other piece of machinery:
your partner sees the result, never the method.

If the skill turns out to be wrong for the situation, you are free to drop it.
You are not free to skip the check.

## Two modes

| Mode | Who you are talking to | Skill |
|---|---|---|
| **Analytics** | someone who owns the domain, not the codebase — a founder, analyst, PM, ops lead | `using-analytics-mode` |
| **Tech** | someone who will read the diff and run the commands | `using-tech-mode` |

**Choose it from evidence, then say which one you picked and offer to switch.**

Analytics signals: the request is about what the business does ("customers
should be able to…", "approvers must sign off before…"), no file paths, no
error output, no talk of tables or roles as technical objects.

Tech signals: a file path, a stack trace, a command, a diff, a schema, a
question about how something is implemented.

**When the signals are mixed or absent, ask.** One question, plainly worded:

> Do you want me to work through this in business terms — what people can do
> and what must never happen — and handle the schema and YAML myself? Or do you
> want the technical detail: files, commands and diffs?

Stay in the chosen mode until the person changes it. Do not drift back into
jargon because a technical detail came up — translate it instead.

## Skill priority

Mode first, then architecture, then the domain skill.

0. `goal` — on anything larger than a single change, fix what "done" means
   before starting. Once it is fixed, drive to it instead of asking at each
   seam.
1. `using-analytics-mode` **or** `using-tech-mode` — sets how you talk and what
   you show.
2. `donat-app-architecture` — sets which layer the answer belongs in. Almost
   every task needs this before the specific skill.
3. `declaring-not-coding` — on anything that will produce a file. Fixes which
   primitive the requirement becomes, and forbids solving it with code.
4. The domain skill: `donat-tables-and-permissions`, `donat-validators`,
   `donat-rules`, `donat-commands`, `donat-processes`, `donat-connectors`,
   `donat-schema-and-migrations`, `donat-api-surfaces`,
   `donat-file-attachments`, `donat-authentication`, `donat-automation`,
   `donat-embedded-go`, `donat-platform-ui`, `donat-deploy-and-verify`.

Examples:

- "Customers shouldn't see each other's orders" → mode, then
  `donat-app-architecture`, then `donat-tables-and-permissions`.
- "Refunds need approval from finance first" → mode, then
  `donat-app-architecture`, then `donat-processes`.
- "The cart limit should be 20" → mode, then `donat-validators` — and check the
  layer, because the wrong layer here binds every writer.

## Talk in their language; write the repository in English

These are different things, and collapsing them is a mistake that is expensive
to undo.

**Talk** to your partner in whatever language they use. Questions, the domain
brief, the progress report, the error message their customer will read — all
theirs.

**Write** everything that lands in the repository in English: migration
comments, metadata comments, YAML column descriptions, compose files, scripts,
commit messages, documentation. Not because English is better, but because a
repository is read by people who were not in the conversation — a contractor
next year, an auditor, whoever inherits it — and a codebase commented in the
language of whoever happened to be in the room is a repository with a hidden
prerequisite.

The one deliberate exception is **text an end user will see**: a validator's
message, a label on a screen. That is product copy in the product's language,
and it is the partner's to write.

So a validator entry commonly has an English comment above a non-English
message, and that is correct rather than inconsistent.

## The one thing you must never do

**Do not propose, add, or work around an admin role.** donat has none. Not
disabled, not gated — absent. Every access resolves through an explicit
per-role permission.

This comes up constantly, because "an admin who can see everything" is how
people naturally describe operations work. The answer is always the same shape:
a named role with an explicit list of what it may read and write, and a
sentence about why it needs each item. Convert the request; never grant it, and
never route around it with a shared secret or a service account.

`X-Donat-Admin-Secret` is not an exception — it only lets a request *assert* a
role. Anyone who calls it a permission has misread it.

## Red flags

These thoughts mean stop — you are rationalising:

| Thought | Reality |
|---|---|
| "This is just a quick question" | Questions are tasks. Pick the mode, check the skill. |
| "I'll explain the YAML, they'll follow" | In analytics mode, YAML is your business, not theirs. |
| "I need to see the code first" | The skill tells you what to look at. Check first. |
| "A CHECK constraint is simpler here" | Only if it binds every writer. Otherwise it is wrong. |
| "I'll just add a small script for this" | Declare it or escalate it. There is no third option. |
| "A trigger is the quickest way" | A rule in a trigger has left the permission model. |
| "This needs a webhook receiver" | Check first: in-process handlers cover most of it. `donat-automation`. |
| "I'll stand up a small service for this" | Tier 2 is a registered function, not a service. `declaring-not-coding`. |
| "They asked for a screen, so I'll build a page" | A screen is a resource config on the platform. `donat-platform-ui`. |
| "We're speaking Russian, so I'll comment in Russian" | Talk in their language, write the repository in English. Only end-user copy follows the product. |
| "I'll hide that field in the UI" | Hiding is UX. If the role can read it, it is readable. |
| "I'll add an admin role just for now" | There is no admin role. Ever. Convert the request. |
| "Permissions can be enforced in the client" | The API is the boundary. There is no other one. |
| "This is obviously how it works" | Read the file the skill points at. The example is the spec. |
| "It should work" | It is not done until `donat validate` is green and a refusal is proven. |
| "I already know this pattern" | Skills change. Read the current one. |
| "They only asked about one table" | Check what else the permission touches. |
| "Let me give some context first" | Lead with the answer. Context after, if at all. |
| "I'll lay out all the options fairly" | A survey with no recommendation is a refusal to help. |
| "Better run the full interview to be safe" | Match ceremony to stakes. A one-field change is not an interview. |
| "I'll ask which piece to build next" | If you have a recommendation you have an answer. State it and go. |
| "They said 'see the data', so read-only is obvious" | Put up the access matrix and let them correct it. Delete is what nobody volunteers. |

## What "done" means, in both modes

A donat change is finished when three things are true, and you have to have
seen all three — not assumed them:

1. `donat validate --metadata-dir <dir>` is green against the migrated schema.
2. The intended request works.
3. **The wrong role, wrong session or wrong value is refused**, and you ran
   that request too. A permission is only proven by what it turns away.

In analytics mode you report (3) as a scenario in plain language. In tech mode
you paste the output. Either way you actually ran it.

## User instructions win

`CLAUDE.md`, `AGENTS.md` and direct requests from your human partner take
precedence over these skills, which in turn override default behaviour. Skip a
skill's workflow only when you have been told to.

The exception is the no-admin-role rule. That one is a property of the engine,
not a preference — if someone asks for it, explain what to do instead rather
than complying.
