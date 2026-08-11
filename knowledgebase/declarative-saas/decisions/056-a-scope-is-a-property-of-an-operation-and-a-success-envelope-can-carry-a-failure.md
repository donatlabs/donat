---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A scope is a property of an operation, and a success envelope can carry a failure

## Context

Batch C (spec 014) is the first batch whose providers all authenticate with
authorization-code OAuth2, so it is the first real consumer of
[[041-a-credential-the-engine-writes-is-still-not-an-admin-api]] and
[[043-the-credential-seam-refuses-before-it-sends]]. Wiring four Google
Workspace connectors through that machinery surfaced three things the existing
decisions had no shape for.

The first is scopes. `config.oauth2.scopes` was, until now, an opaque list a
deployment wrote and `donat connector authorize` asked the provider for. Nothing
checked it against anything, because nothing knew what an operation needed.
Google's discovery documents do know, exactly and per method — and they publish
*alternatives* rather than a single scope: `spreadsheets.values.get` is admitted
under any of five, from `spreadsheets.readonly` up to the whole of `drive`. A
declaration that named "the scope" of an operation would be wrong for four of
the five.

The second is that the SDK had no credential application for a stored token. Its
closed `AuthPlan` set covers deploy-time material — an API key, a bearer secret,
HTTP Basic, client-credentials OAuth2, AWS SigV4 — and every one of them reads a
value a deployment configured. A Google connector configures none. The obvious
workaround, declaring `AuthPlan::bearer()` and handing it a placeholder secret,
is the thing [[048-a-declaration-a-deployment-completes]] refused for Twilio's
Account SID: a credential contract that does not describe what reaches the wire.

The third is Google's own habit of answering `200` with a failure inside.
Calendar's `freeBusy` reply carries `calendars.<id>.errors[]`; Drive's `FileList`
carries `incompleteSearch`. Both are documented, and both are decodable through
the declared output pointers into something that looks exactly like a complete
answer.

## Decision

**A scope requirement is a property of an operation, and it is a set of
alternatives with a least member.** `ScopeRequirement::documented(least,
accepted)` carries every scope Google's discovery document lists for that method
plus the smallest one a deployment could hold. A deployment is checked in both
directions: an enabled operation no declared scope authorizes is refused with
its metadata path and the least scope that would satisfy it, and a declared
scope no enabled operation is authorized by is refused as a grant nothing uses
([[034-a-declaration-the-runtime-ignores-is-a-defect]] applied to a permission).
"Scope sets are per operation group" is then not a mechanism but a consequence:
the group *is* the enabled set, and the requirement is its union, so a
deployment that enables only reads is never asked for a write scope and cannot
hold one by accident.

Two refinements keep that rule honest. Google's OpenID Connect scopes —
`openid`, `email`, `profile`, and the two `userinfo.*` — are never surplus,
because they grant no API access and are how a token response names the account
an operator reads. And the check runs *twice*, against two different things:
metadata validation compares the declaration to the enabled operations before a
listener opens, and `CredentialRuntime::validate_stored_credentials` compares
the stored grant to the declaration at the same moment. The second is the one
that catches a deployment that widened its metadata without re-authorizing, and
it is a startup failure for the reason spec 011 §7 already gave for a missing
credential: an activity discovering it later is a worse version of the same
failure.

**The SDK gains one auth plan that configures nothing.**
`AuthPlan::oauth2_authorization_code()` declares no credential field at all, so
`CredentialSpec::for_plan` produces an empty field list and startup asks a Google
instance for no secret. Applying it without a token is a refusal rather than an
unauthenticated request, and the value it accepts must be a non-empty
`Bearer …` — the complete `Authorization` header the credential lifecycle
produced, because that lifecycle owns the scheme name as well as the token. This
is what makes ADR 043's "there is deliberately no path in which the header is
merely absent" structural for a hand-written connector: the unauthenticated
`execute` path cannot render a Google request at all, because the plan has
nothing to apply.

**A `2xx` that carries a failure is a failure, and the class is the failure's,
not the status's.** Every Google operation's decode runs a guard between parsing
the body and reading the declared output pointers. Two guards are Google's own
documented shapes — a non-empty `errors[]` under any `freeBusy` calendar or
group, and `incompleteSearch: true` on a Drive listing — and each maps to
exactly one class, with `permanent` for the reasons Google says to expect but
has not published. The third guard is **this workspace's own rule and is
recorded as such**: a `2xx` body carrying Google's canonical top-level `error`
object is refused rather than decoded. It costs a legitimate success nothing —
no response schema of any operation these four connectors declare has a
top-level `error` property, the only schema in the four discovery documents that
does being Drive's long-running `Operation`, which none of them returns — and it
closes the one shape in which a provider failure could be extracted into an
activity's output as though it were data.

The reasoning behind refusing rather than reporting is the SDK's own, already
written down for pagination: a truncated aggregate is indistinguishable from a
complete one downstream. A `freeBusy` answer missing one calendar and a Drive
listing missing an unknown number of files are the same problem, and a Process
that received either as a success would make a decision on evidence it has no
way to know was partial.

## Alternatives

| Option | Why Not |
|--------|---------|
| Declare one scope per operation instead of Google's alternative set | Wrong for four of five Sheets scopes and eight of nine Drive ones. A deployment already holding `drive` would be told to authorize `drive.metadata.readonly`, which it would never use |
| Check the declared scopes only, never the stored grant | Misses the case the check exists for: metadata widened after `donat connector authorize` ran. The CLI already refuses to *write* a short grant, so the declaration is the only thing that can drift |
| Accept a surplus scope silently | A grant nothing uses is a permission the deployment did not need and cannot see it did not need. Refusing it is ADR 034 applied to authority rather than to configuration |
| Refuse Google's OpenID Connect scopes as surplus too | They are how a token response carries a `sub`, which is what `donat connector credentials list` prints. Refusing them would push operators to `--subject` for no benefit |
| Put the scope table in the SDK as `OperationBuilder::scopes` | Scopes are provider facts, and provider facts live in the module that reads the provider's documentation. The SDK would gain a field only Google-shaped connectors fill |
| Declare `AuthPlan::bearer()` with a placeholder secret | [[048-a-declaration-a-deployment-completes]] in miniature: the published credential contract would describe a request that is never sent, and the unauthenticated path would send `Bearer ` |
| Reuse `AuthPlan::oauth2_client_credentials` for the stored token | It declares a token origin and a client id/secret, none of which a Google connector has — the token endpoint is deployment metadata, resolved by `crate::credentials`. The declaration would be describing a flow the engine does not perform |
| Report `incompleteSearch` as an output field instead of refusing | It is then a field a Process may forget to check, and the failure mode is a wrong answer rather than a failed activity. The SDK already made the opposite choice for a truncated page |
| Give the partial failures their own `ConnectorErrorClass` | The set is closed because Processes route on it ([[043-the-credential-seam-refuses-before-it-sends]]). These map onto classes that already exist and mean the right thing |
| Skip the fail-closed `error`-in-`2xx` rule as undocumented | The rule costs nothing and closes a shape that would otherwise decode a provider's failure into an activity output. It is recorded as ours precisely so a reviewer is not misled into thinking Google published it |

## Consequences

Four Google Workspace connectors are deployable and executable end to end, which
is most of what spec 011 was built for. A deployment of one holds exactly the
Google scopes its enabled operations need, and both ways of getting that wrong —
too few declared, too many declared, or a stored grant that no longer covers the
declaration — stop startup with the metadata path or the missing scope named.

Three costs are worth naming. The scope tables are hand-transcribed from
Google's discovery documents and nothing checks them against Google at build
time; a scope list that changes upstream becomes a startup refusal for a
deployment that would have worked, which is the safe direction but is still a
maintenance obligation, and `crates/connectors/tests/google_*.rs` asserts only
that every declared operation has a table entry whose least member is one of its
accepted ones. The fail-closed envelope rule is a rule about a shape rather than
about a documented behaviour, so a Google API that one day returns a top-level
`error` object as part of a legitimate success would be refused by this engine
until the guard learned about it. And `google_drive.file.download` composes its
output — base64 of the response body, its length, and its media type — because
the SDK's response contract is JSON and Drive's `alt=media` is not; that is
bounded by the SDK's 1 MiB response ceiling, and a larger file is a typed
`validation` failure rather than a truncated one.
