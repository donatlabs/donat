---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# Rotation is a write, and a continuation is a destination

## Context

Batch D (spec 015) is the second consumer of the OAuth2 credential lifecycle
([[041-a-credential-the-engine-writes-is-still-not-an-admin-api]],
[[043-the-credential-seam-refuses-before-it-sends]]) and the first whose
provider *rotates*. Its four connectors share one origin,
`https://graph.microsoft.com`, and one credential shape, so the batch was
expected to be Batch C with different paths. Reading Microsoft's own published
material turned three of its assumptions over.

The spec's premise was that "the identity platform returns a new refresh token
on every exchange **and invalidates the previous one**, so a non-atomic write
loses the account". Microsoft publishes the first half and explicitly denies the
second: "Refresh tokens replace themselves with a fresh token upon every use.
The Microsoft identity platform doesn't revoke old refresh tokens when used to
fetch new access tokens. Securely delete the old refresh token after acquiring a
new one."

The spec's second premise was that `@odata.nextLink` is "the sharpest
origin-escape surface in the whole plan". That one held, and reading the same
documentation found its exact mirror image one page away: `GET
/me/drive/items/{item-id}/content` "Returns a `302 Found` response redirecting
to a preauthenticated download URL", whose sample `Location` is on
`b0mpua-by3301.files.1drv.com` and for which Microsoft publishes no host
allowlist at all.

And the SDK could not spell a Microsoft request. Every OData system query option
begins with `$`, which `validate_query_key` refused, and Microsoft's own guidance
closes the escape hatch: "On the *v1.0* endpoint, the `$` prefix is optional for
only a subset of APIs. **For simplicity, always include `$` across all
versions**."

## Decision

**The rotation proof stands on the commit, not on the provider's forgiveness.**
`<name>_rotation_survives_crash` drives a real Microsoft connector instance
through `refresh::access_token` with `abort_before_commit`, which drops the
transaction after the provider exchange and before the commit — exactly what a
worker dying at that instant does — and then asserts three things in order: the
exchange really happened and its answer was lost, the row is byte-identical
afterwards and is not marked unusable, and the *next* attempt, made through the
serving registry with the deployment's own metadata, refreshes with the token
still in the row and commits one rotation. The token stub models what Microsoft
publishes and nothing more: it issues a new refresh token on every exchange and
it does not revoke the presented one.

That pairing is the decision, because it would be easy to read the outcome as
"rotation is fine here". It is not. Microsoft's non-revocation is what makes the
crash *survivable*; the engine's commit-before-use is what makes it *correct*. A
provider that rotated and revoked — which the OAuth 2.0 specification explicitly
permits, and which Microsoft quotes at itself: "The authorization server MAY
revoke the old refresh token after issuing a new refresh token to the client" —
would destroy the account on any implementation that used a token it had not
committed. The property under test is therefore the transaction, not the
provider: a stub that revoked the presented token would fail the recovery
assertions here, and the reason would be the provider's contract rather than
this engine's write, which is exactly the distinction worth being able to see.

**`offline_access` is declared and not required.** It is the precondition for
having a refresh token at all — "your app must explicitly request the
`offline_access` scope, to receive refresh tokens" — so a Microsoft deployment
that omits it is authorized once and can never refresh. It is nevertheless *not*
a metadata requirement, because Microsoft also publishes "If any delegated
permission is granted, offline_access is implicitly granted", and an implicit
grant is one a token response need not name. `donat connector authorize` refuses
to write a row whose granted set does not cover the declared one, so demanding
`offline_access` in `config.oauth2.scopes` would refuse a complete
authorization — a failure the operator could not fix. It is therefore a protocol
scope that is never surplus, declared in every fixture, and documented in the
module and here.

**A permission has two spellings and no case.** Microsoft documents
`scope=User.Read` and `https://graph.microsoft.com/User.Read` as the same grant,
and its own pages write both `Mail.Read` and `mail.read`, so
`PermissionRequirement` strips the resource identifier and compares
ASCII-case-insensitively. The rest is [[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]
unchanged: a requirement is a set of alternatives with a least member, the group
is the enabled set, and a deployment is checked in both directions.

**A continuation is a destination and a download URL is data, and the difference
is the declaration's.** `@odata.nextLink` goes through one constructor,
`microsoft_graph::next_link`, which is `Pagination::next_uri_in_body` — the SDK's
plan for a body-carried destination, resolved against the compiled origin and
refused with `connector_pagination_cross_origin` when it lands on another host,
scheme, or port, with no request made. `microsoft_onedrive.file.download`
declares the alternative Microsoft publishes on the same page — `GET
/drive/items/{item-ID}?select=id,@microsoft.graph.downloadUrl` — and emits the
pre-authenticated URL as a declared *output*. The `302` form is not declarable
here at all, because the SDK admits only `2xx` as a success and never follows a
redirect, and that is the right answer rather than a limitation: a connector
whose origin is compiled cannot be allowed to follow a provider onto a host the
provider does not publish.

**The SDK widens twice, and both times a provider forced it.** `$` is admitted
in a query key, because every OData system query option begins with one and
Microsoft publishes no alternative a connector may rely on; the boundary is
unchanged, since a key still cannot carry `=`, `&`, `%`, whitespace, or a
binding, so it can no more end itself or start a second parameter than `.` or
`-` can.

The second is sharper, and it is a correctness fix rather than an expressiveness
one. Microsoft puts two arguments *inside a function call in the path* —
`search(q='<search-text>')` and `range(address='<address>')` — and publishes no
other spelling of either. Percent-encoding alone is not containment there:
a receiver decodes `%27` back to `'` before it parses the expression, so a value
carrying a quote would end the literal and the rest of it would be read as
syntax. `OperationBuilder::odata_literal_path_param` therefore doubles the quote
first — OData's own escape — and then applies the same `NON_ALPHANUMERIC`
encoding every other path value gets. The two are complementary and neither is
sufficient: doubling keeps the value inside the literal, encoding keeps it inside
the segment. This is not an origin escape either way — `%2F` is still not a path
separator — but `'My Sheet'!A1:B2` and `O'Brien` are both ordinary values, and a
connector that mangled them would have been wrong before it was unsafe. Both
widenings are [[047-the-sdk-widens-where-a-provider-forced-it-and-nowhere-else]]
applied once more.

## Alternatives

| Option | Why Not |
|--------|---------|
| Model the token stub as revoking the presented refresh token, per the spec's premise | It would be testing a provider Microsoft documents itself not to be. The fixture would encode a behaviour no reader could find in the published material, which is the one thing [[037-connectors-are-written-by-hand-against-provider-documentation]] forbids |
| Prove the rotation through `crates/server/src/credentials` alone | `oauth_rotation_is_atomic` already does, and spec 015 asks for the proof *through a real Microsoft connector*: the recovery attempt here runs through `ConnectorRegistry::execute` with this deployment's metadata, so the declaration, the registry, the credential runtime, and the row lock are all in the path |
| Require `offline_access` in `config.oauth2.scopes` | Microsoft grants it implicitly and its token response need not echo it, so `donat connector authorize` would refuse a grant that is in fact complete, and the operator would have no way to authorize the connector at all |
| Treat `offline_access` as an ordinary permission with its own requirement | No operation is authorized *by* it; it grants no API access. It would then be surplus on every deployment that declared it, which is the opposite of what it is |
| Compare declared permissions as exact strings | Microsoft publishes two spellings as one grant and writes both cases itself. Exact comparison would refuse a correct deployment for choosing the spelling on the protocol page rather than the one on the permissions page |
| Declare `microsoft_onedrive.file.download` as the `302` and follow the `Location` | The destination is on a host Microsoft does not publish and cannot promise. The SDK follows no redirect by construction, and a connector with a compiled origin that made an exception for one operation would have no origin |
| Declare the `@odata.nextLink` walk as `Pagination::token_in_body` | It would spend an absolute URL as a percent-encoded query value, which is not what Microsoft's continuation is; the walk would silently ask for page one forever |
| Keep `worksheet.update_range` as spec 015 §2's provisional `NM` | Microsoft publishes it as a `PATCH` with partial-merge semantics and publishes no repeat-safety for it. It fails the gate by method and by evidence, and its own best-practice page says the workbook's state is unknowable after a failure |
| Admit `event.create` as `ProviderIdempotent::ExplicitKey` on `transactionId` | Microsoft publishes the binding, the scope, and the deduplicating purpose — and no retention window. A key a provider may have already forgotten is not one a durable send horizon can fit inside ([[042-the-effect-gate-admits-evidence-not-methods]]) |
| Let an operation take `workbook-session-id` from input | A session id is a handle to another call's state, and a non-persistent session's "changes made by the API aren't saved to the source location". A Process could then silently throw its own writes away |
| Let an operation take `prefer: bypass-shared-lock` from input | It overrides somebody else's coauthoring lock. That is a decision a declaration makes once and a reviewer reads, not one a caller makes per request |
| Use the unprefixed OData spelling (`select`, `top`) instead of widening the SDK | Microsoft supports it on v1.0 for "only a subset of APIs", never says which, and tells clients to "always include `$`" |
| Refuse a value containing `'` in an OData function argument instead of doubling it | `'My Sheet'!A1:B2` is what Microsoft's own A1 notation looks like when a sheet name has a space, and `O'Brien` is a search term. Refusing them would make a correct request undeployable to buy a containment that doubling already gives |
| Escape the quote in the connector module rather than in the SDK | The module does not render; `Operation::plan_request` does. An escape applied in `crates/server`'s runtime would not be the one the connector tests exercise, which is two descriptions of one request |

## Consequences

Four Microsoft 365 connectors are deployable and executable end to end, and the
programme's last unimplemented specification is closed. Forty operations are
declared across them, of which three are executable mutations — the three
`DELETE`s — and sixteen are inventory-only with their evidence in
`providers/INVENTORY.md`. `microsoft_teams` publishes no executable mutation at
all, which is a fair summary of what Microsoft documents about repeating a Teams
write.

Three costs are worth naming. The permission tables are hand-transcribed from
per-operation reference pages and nothing checks them against Microsoft at build
time; a permission list that changes upstream becomes a startup refusal for a
deployment that would have worked, which is the safe direction and still a
maintenance obligation. The three `NaturalMethod` deletes rest on the fixed
identity of a documented request rather than on a published statement about the
repeat — Microsoft publishes no such statement for any of them — and both the
modules and `INVENTORY.md` say so in as many words, so a reviewer can see
exactly what the class is standing on. And the `Prefer: IdType="ImmutableId"`
that `microsoft_outlook` declares on every operation naming an Outlook item id
is a shape decision a deployment cannot change: it is the right one, because
Microsoft documents that ids change when an item moves and that the header
applies per request, but a deployment holding ids captured without it would have
to re-read them.
