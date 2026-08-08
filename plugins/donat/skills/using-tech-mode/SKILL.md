---
name: using-tech-mode
description: Use when working on a donat application with someone who reads the diff and runs the commands. Show files, commands and real output; verify with a refused request, not an assertion.
---

# Tech mode

Your partner reads code. Skip the translation layer, show the artifacts, and
hold yourself to evidence.

## What changes relative to analytics mode

| | Analytics | Tech |
|---|---|---|
| Interview | a scripted domain interview | ask the two or three things you actually need |
| Confirmation gate | a plain-language brief, confirmed before building | the access matrix, or the plan, stated in one paragraph |
| Artifacts shown | scenarios | `file:line`, diffs, exact commands |
| Failures | translated into domain terms | pasted verbatim |
| Verification | described as scenarios | the command and its output |

What does **not** change: the layer rule, the no-admin-role rule, and what
"done" means. Those are properties of the engine, not of the audience.

## Working shape

**1. Locate before you write.** Read the neighbouring files — two or three
existing table files, the nearest command in the same domain, the migrations
that created what you are touching. Match their conventions rather than
importing your own. `grep -rn 'role: <name>' metadata/` is the authoritative
answer to what a role can do; there is no central role registry.

**2. Decide the layer out loud.** One sentence, before writing: *"`quantity <= 20`
binds shoppers only, so it is a validator, not a CHECK."* Most defects in donat
applications are a rule in the wrong layer, and stating it is what catches it.

**3. Write the migration and the metadata in the same change** when one depends
on the other. A command whose guard relies on a unique constraint that no
migration creates is a race that will show up under load, not in review.

**4. Verify, in this order:**

```sh
donat migrate  --migrations-dir migrations
donat validate --metadata-dir metadata      # the compiler; red means stop
```

then the intended request, then **the refused one**. Paste both.

## Evidence rules

- **Run it.** Do not report a result you have not seen. If you could not run
  something, say which one and why.
- **Paste the real output**, including the parts that are noisy. A trimmed
  success is indistinguishable from a fabricated one.
- **A permission is proven by refusal.** The request that returns the caller's
  own rows proves nothing about isolation. The one asking for someone else's,
  returning empty, is the test.
- **`donat validate` green is necessary, not sufficient.** It checks that the
  metadata is consistent with the schema, not that the policy is what anyone
  intended.
- **Snapshots are reviewed, never blind-accepted.** An unexplained snapshot
  change is a bug, not noise.

## What to test

For permissions and validators:

1. the role sees its own rows;
2. another session's rows are **absent** — an empty list, not an error, so the
   record's existence is not disclosed;
3. a role with no permission gets the access-denied contract;
4. the validator's message comes back verbatim, with code `validation-failed`.

For commands and processes:

5. a replayed idempotency key returns the original result and writes nothing
   new;
6. a failing guard rolls back **every** step, not only its own;
7. the process reaches its terminal state, **and** its failure branches are
   reachable — script a provider stub to error and assert the `fail` code;
8. the journal says what you think: `donat process inspect --source <s>
   --instance <uuid>`, and `verify-history` for consistency.

## Reporting

Same skeleton every time:

1. **What changed**, with `file:line`.
2. **Evidence** — the command and its real output.
3. **What you could not verify**, named rather than omitted.
4. **One decision**, if you need one, with your recommendation attached.

Keep it dense. Your partner does not need the layer rule re-explained every
time — they need the file, the command, and the output.

- **Lead with the result.** No preamble, no restating the task.
- **Cut every sentence that survives its own deletion.**
- **Recommend, don't survey.** Two options with a pick beats four with none.
- **No "it is important to note", no apologising, no closing pleasantry.**

Friendly and terse are not in tension. "That approach races under load — here's
the version with the constraint" is both.

## Escalate rather than improvise

Say so plainly, rather than working around it, when:

- the change needs a permission bypass — it does not exist, and a shared secret
  or a service account is not a substitute;
- the metadata format cannot express the requirement declaratively;
- the schema contradicts the metadata and fixing it means a destructive
  migration;
- correctness depends on a constraint that would break existing rows.

Each of those is a decision for your partner, with the trade named. Silently
picking one is how a permission model drifts from the thing it was meant to be.
