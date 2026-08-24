# A deployment that adopts the module

The module's own stand tests what ships. This one is a *deployment*: it adopts
the module by `!include`, and then changes the two things a deployment is meant
to change.

- **Its own sender.** `connectors/own-mail.yaml` replaces the shipped mail
  contract with a differently shaped one — another path, another set of field
  names on the wire, another response pointer. Nothing in the module's flows or
  commands is touched, which is what makes it a swap rather than a fork.
- **The escalation turned on.** `rules/email-delay.yaml` replaces the shipped
  "not at all" with a real wait, so the email arrives only if the recipient did
  not look at the bell first.

So it is two things at once: the worked answer to "how do I adopt this", and the
proof that those two seams are real. Reading its `metadata/` top to bottom is
the adoption checklist — every list an application has to join is here, with
nothing else in the way.

```bash
make app-test APP_DIR=modules/notifications/examples/deployment
```

The delays are seconds where a real deployment would say minutes — five for an
ordinary escalation, twenty for a reminder. The reminder's row is the one the
`skipped` test acts inside: it is the only test in the repo whose green depends
on a clock, so it takes the widest window this deployment declares.
