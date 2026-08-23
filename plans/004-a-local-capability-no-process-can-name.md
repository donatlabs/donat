# A local capability no Process can name

**Effort: M. Status: TODO — found while building `modules/notifications`.**

## What is wrong

The engine carries five local capabilities — `local.document`, `local.code`,
`local.image`, `local.ingest`, `local.recurrence` — and no deployment can use
any of them. A Process that names one is refused at `donat validate`:

```
inconsistency: processes[0].states[6].request.operation:
  connector operation `default.local.document.email.render` is not executable
```

The refusal is correct and the cause is two lines apart:

- `crates/server/src/connectors/mod.rs:1039-1044` skips every `local.*` instance
  when building the registry's `operation_specs` table, with a comment saying
  local capabilities are dispatched by `crate::local` instead. True at
  *execution* time.
- `crates/processes/src/lib.rs:2065-2077` resolves every request activity's
  operation through that same table at *compile* time, and a missing entry is
  "not executable".

So `LocalCapabilityRegistry` can run `email.render`, and the compiler will never
let a Process reach it.

## Why it has not surfaced

Nothing in the repository renders. `examples/petshop` has no `documents.yaml`,
there is no conformance module for `local.*` execution, and
`crates/metadata/tests/document_templates.rs` — the one test that names
`connector: local.document` — asserts only that the metadata *loads*, which it
does. The capability is covered from the metadata side and from the executor
side, and the seam between them is covered by neither.

By the standard `knowledgebase/declarative-saas/decisions/034-*` sets, this is
a defect of the same family it describes, inverted: not a declaration the
runtime ignores, but a runtime no declaration can reach.

## What closing it needs

The compiler wants two things from an operation that `LocalOperation`
(`crates/connectors/src/local/capability.rs:304-311`) does not carry: a typed
**input contract** and a typed **output contract**. It holds `id`, `version`,
`effect`, `bounds`, `units` and `run` — enough to execute, not enough to
type-check `{ state: render_email, field: html }`.

So the work is, in order:

1. **Declare the contracts.** Each local operation states its input and output
   as a `ValueContract`, beside the bounds it already declares. For
   `email.render` that is `{ template: string!, …declared template inputs }` in
   and `{ html: string!, text: string!, template: string!, template_hash:
   string! }` out — the last two are already in the product
   (`crates/connectors/src/local/document/email.rs:129-134`).
   The template-selected part of the input is per *template*, not per
   operation, which is the interesting part of the design: the contract has to
   be completed from `documents.yaml` at registry build, the way `twilio`'s
   declaration is completed from its Account SID
   (`crates/server/src/connectors/mod.rs:1064-1075`).
2. **Publish them.** `ConnectorRegistry::build` stops skipping `local.*` and
   registers one `OperationSpec` per declared operation, with a deployment
   fingerprint derived from the capability version plus, for the document
   capability, the template hashes it was completed from — which is what keeps
   `template_pin` meaningful.
3. **Cover the seam.** A conformance module that renders and asserts the bytes,
   so "loads" and "executes" stop being the only two things tested.

## What it unblocks

`modules/notifications` sends a subject and a body and lets the relay compose
the message. With this closed it renders the MJML the engine already carries,
and the module's mail contract gains `html` and `text` — see the comment at the
head of `modules/notifications/metadata/connectors.yaml`. The invoice, the
export and the calendar invitation that `local.document` was built for are in
the same position today.

## The fork to settle first

Whether a local operation's contract is **static** (declared once in Rust, with
template-selected fields typed as an open object) or **completed at build**
(the exact fields of the selected template, so a Process that passes the wrong
one is refused at `validate` rather than at the first render). The second is
what the rest of the system does and is worth more; it is also the reason this
is M rather than S, because the completion has to happen before the process
compiler runs and after `documents.yaml` is read.
