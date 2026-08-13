---
type: decision
status: accepted
date: 2026-08-11
features:
  - "[[platform]]"
---

# The identity adapter ships in the binary, and grants nothing

## Context

[[001-the-admin-panel-is-a-role-rendered-not-a-surface-the-engine-grows]] says
the panel lives outside the engine and **the engine grows nothing** for it. It
also says where a deployment's people are the identity provider's accounts, the
panel reaches them through an *action* — donat proxying that provider's REST
API into GraphQL, with the credential in the engine and access an ordinary
per-role permission.

Building that produced two things at once. The action mechanism gained what it
needed to stand in front of an API written for somebody else — Donat v2
`request_transform`, its Kriti templates ported and checked against Kriti's own
corpus — and the petshop example gained forty lines of `actions.yaml`
describing one provider's user API: three fields, two custom types, one
credential, one role.

Those forty lines are the problem. They are the same in every deployment,
because they describe *the provider*, not the deployment. Copying them is how
they end up subtly different; asking an operator to write them is asking for a
correct Kriti template as a condition of having a Users screen at all. The
alternative — a panel that ships with a Users screen pointing at fields nothing
serves — is worse, because it fails at the first click rather than at
configuration.

## Decision

**The declaration ships in the binary; the deployment names three things.**
`crates/server/src/idp_admin.yaml` holds the actions and custom types for one
named provider (Rauthy), and `idp_admin.rs` fills in what is a deployment's to
say — where the provider is, the key to reach it with, and the one role allowed
to use it — from `DONAT_OIDC`:

```json
{ "…": "…", "admin_key": "API-Key donat$…", "admin_role": "support" }
```

Configure those and twenty-nine fields exist — the people, the roles, groups,
attributes, scopes and applications the provider decides about them, the
addresses it is refusing and the sessions it is honouring. Configure nothing
and they do not exist at all — not empty, absent.

Where the line falls is set by the provider, not by us: everything its API key
can reach is served, and what answers only to an admin session (its password
policy, its terms, its key-value store) is not, because this engine holds a key
rather than somebody's password. Its own API keys refuse to be managed by
another API key, which is its rule and a good one.

**This is the connector bargain, applied to identity.** This repository already
ships adapters for named providers as code: sixty-nine connectors, each a
declaration in the binary, with a generic `http` module for everything else.
An identity provider's admin API is the same kind of fact — it belongs to the
provider, not to the deployment — so it belongs in the same place. A provider
that is not this one is still an ordinary `actions.yaml`, and a deployment that
declares a field of the same name keeps its own: the built-in module is a
default, and a metadata file is a deliberate statement.

**It grants nothing, and that is checked.** The fields are visible to exactly
the role the configuration names, through the same per-role permission every
other field has; the role is still established only by a verified token or an
authentication hook ([[api-surfaces/decisions/013-a-role-is-established-by-a-verified-token-or-a-hook-and-by-nothing-else]]).
Two conformance cases hold the line: one asserts the accounts are served to the
named role with the credential attached and are *absent for another role the
same token grants*, the other asserts they are absent entirely until a key is
configured.

**The panel's default follows.** `apps/admin` no longer guesses at a `users`
table when a stand declares no people; it renders these fields, because these
are the ones the engine serves. A deployment whose people are rows says so, and
that declaration replaces all of it.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep it in each deployment's `actions.yaml` (what we built first) | Correct, and what a second provider still does. But it is the same forty lines everywhere, it makes a Users screen conditional on writing a Kriti template correctly, and every copy is a chance to differ from the others. |
| Built-in GraphQL fields resolved in Rust, with no metadata at all | Faster and smaller — and it would put a hand-written resolver beside a permission system that every other field goes through. Going through actions means these fields are checked, shaped and permission-gated by exactly the code all fields are, and the declaration stays readable as YAML. |
| Make the panel talk to the provider directly | Puts an admin credential in a browser. Not a trade-off; just wrong. |
| A general "identity" surface in the engine — accounts, sessions, tokens | This is the shape the no-admin-role rule exists to prevent. What ships is an adapter for one provider's API behind ordinary fields, and the next request after "let it manage sessions" is "let it manage roles", which is administration by another name. |

## Consequences

A deployment gets a working Users screen from two settings, and its metadata
directory stays about *its* application. The engine's surface grows by nothing
a role cannot already reach: no route, no bypass, no field visible to anyone
the configuration did not name.

What we pay is honest: the engine now contains knowledge of one identity
provider's REST API. When Rauthy changes that API, this binary is wrong until
it is updated, and the symptom will be an action error rather than a
configuration error. The mitigation is the same as for connectors — the
declaration is small, it is versioned with the code, and a deployment can
override it by declaring the field itself.

It also means `platform/001`'s "the engine grows nothing" is now qualified
rather than absolute. The qualification is this: the engine grows **adapters**,
which are facts about third parties, and does not grow **surfaces**, which are
powers. Anything that would let a caller do something no role could already do
belongs on the other side of that line.
