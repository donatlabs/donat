---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A deployment's URL grammar is declared where deployments are refused, not inherited from the SDK

## Context

The declarative `http` module now compiles a deployment's operation into an SDK
request node, so a deployment and a hand-written connector describe one request
in one vocabulary (ADR 049). The module kept its own deploy-time checks —
`validate_path_template`, the query-name check in `HttpOperationBuilder::build`
— and the SDK's checks ran again underneath them.

Three rules therefore started refusing metadata that had always loaded, and none
of the three was decided:

* a path binding spent in more than one segment —
  `/v1/orgs/{input.org}/repos/{input.org}` — is refused by the SDK's "a path
  binding appears more than once", although the SDK's own `render_path` renders
  repeats perfectly well;
* a literal `@` anywhere in a path is refused as userinfo;
* a query key may carry only `[A-Za-z0-9_\-.\[\]$]`, so a key with `:`, `+` or
  `,` is refused.

The failure is a boot failure: `from_metadata` runs when the registry is built,
so a deployment that upgraded engines would stop starting, with a message
written for somebody authoring a connector in Rust.

## Decision

**What a deployment may declare is decided in the module that refuses
deployments, and it is written there.** `validate_path_template` and the query
key rule state the whole grammar in this module's own words, so a deployment is
never refused by a message the SDK's builder wrote for a different author, and
the two cannot drift without this module's test failing.

**A repeated binding is translated, not refused.** It is the same value spent
twice — the deployment's spelling, not a different contract — and refusing it is
a compatibility break with nothing on the other side of the ledger.
`sdk_path_bindings` gives the first occurrence the input's own name and every
later one a distinct slot, and the extra slots are filled from the one declared
input at render, inside `prepare_request`. The operation's declared inputs are
taken from the metadata names, so nothing a Process binds or a caller sends
moves, and the bytes on the wire are the ones the deployment wrote.

**The `@` and the query key set are adopted deliberately, and they narrow the
declarative surface.** One engine should have one answer to "what may a
connector request's URL contain", and that answer is widened once, in the SDK,
on provider evidence (ADR 047). A second, wider grammar available only to
deployments would be a hole with a different name, and it would be one nobody
decided. Neither restriction can be translated the way the repeated binding can:
percent-encoding `@` or a query key changes what the provider receives, and a
request that silently differs from the one the deployment declared is worse than
a refusal it can read.

This is a narrowing, and it is recorded here rather than discovered at boot. The
module's semantic version is `0.1.0` and no engine has shipped the SDK-compiled
declarative request node yet, so the narrowing lands before there is anything to
be compatible with. It is deliberately *not* spelled as a bump of
`DECLARATIVE_REQUEST_SHAPE_VERSION`: that version identifies the request shape
this module renders and reaches every declarative operation's published
identity, and no admitted operation renders differently than it did — a bump
would move every declarative process to a new revision to describe a change that
alters nothing they do. A deployment that needs `/users/@me` or a `:` in a query
key is provider evidence, and the answer to it is one SDK widening, for
everybody, with the provider sentence attached.

## Alternatives

| Option | Why Not |
|--------|---------|
| Let the SDK's checks be the deployment's rule | The declarative surface then changes whenever the SDK does, in words aimed at a Rust author. A rule nobody wrote is a rule nobody can read, and this is exactly how three restrictions arrived unnoticed |
| Refuse the repeated binding too, for one grammar | The SDK's rule there is hygiene for a hand-written declaration — a name written twice is a typo — not a URL property. The renderer already handles repeats; refusing them buys nothing and stops a running deployment |
| Percent-encode `@` (or a query key) into the SDK's spelling | `%40` and `@` are equivalent only after a normalization query parsers do not perform. The request would silently stop being the one the deployment declared, which is the failure mode a refusal exists to prevent |
| Give the declarative module its own renderer, so its grammar stays wide | Two renderers for one request is two places for an encoding bug, in the code that decides what leaves this engine. The SDK's renderer is the one that has been reasoned about |
| Widen the SDK for `@` and the wider query key set now | Neither has a provider sentence attached yet, and ADR 047's rule is that the SDK widens where a provider forced it and nowhere else. When one does, the widening is one change and every connector gets it |
| Bump `DECLARATIVE_REQUEST_SHAPE_VERSION` to mark the narrowing | It versions the rendered shape and enters every declarative operation's published identity. Nothing admitted renders differently, so the bump would be revision churn describing a change to what is *refused* |

## Consequences

Metadata that deployed before keeps deploying, with one documented exception in
each direction of the URL: a literal `@` in a path and a query key outside the
closed set are now refused at load, by name, at the field that declared them.
Both refusals are this module's, and both are one SDK widening away from being
admitted again if a provider asks for them.

The repeated binding costs one `Vec<(String, String)>` on the compiled
operation, empty for every operation that spends each binding once, and one
input clone per request for the operations that do not. The synthetic slots are
invisible outside `prepare_request`: they are not declared inputs, not part of
the request fingerprint, and not something a caller can send.
