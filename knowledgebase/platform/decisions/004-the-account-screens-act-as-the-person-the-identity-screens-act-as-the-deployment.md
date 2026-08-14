---
status: accepted
date: 2026-08-14
tags: [platform, admin-panel, identity, authorization]
---

# The account screens act as the person; the identity screens act as the deployment

## Context

The panel now has two sets of screens that both read and write accounts at the
same identity provider, and they look similar enough to invite being built the
same way.

The **Identity** screens (Users, Roles, Groups, Applications, …) manage other
people. They reach the provider through the engine, which holds the provider's
API key and exposes it as `idp_*` root fields behind an ordinary per-role
permission — [ADR 003](003-the-identity-adapter-ships-in-the-binary-and-grants-nothing.md).
Whoever holds that role can act on any account, which is the point of them.

The **account** screen manages your own. It replaces the provider's
`/auth/v1/account`, which is where the provider sends people from three
different places: an application that demands a second factor, an account it
wants updated before signing in, and its own menus.

The obvious move is to build the second on the first — the engine already has
the credential, the fields already exist, and `idp_user_update` would take the
change. Being obvious is the danger.

## Decision

The account screen talks to the provider **directly from the browser**, on the
person's own provider session cookie. It never goes through the engine.

An `AccountClient` separate from `IdpClient` carries this: same origin, same
proxy, different premise. `IdpClient` runs before anybody is signed in and is
written around a login that has not happened yet; `AccountClient` only exists
afterwards, and every call it makes is scoped by a cookie it cannot read,
choose, or widen.

The route sits **outside** the panel's guarded shell. Two of the three ways
somebody arrives are mid-login: there is a provider session and no engine one.
A screen that needed the engine's cookie would refuse exactly the people the
provider sent to it.

## Consequences

Nothing on this screen can reach an account other than the caller's own,
because the only thing it holds is that person's session. That is not a check
we wrote — there is no account id to tamper with that the provider would
honour, and no credential of the deployment's anywhere near it.

Had it gone through the engine, "change my password" would have been
`idp_user_update` with an id, and the id would have been the panel's to choose.
Getting that wrong once — a stale id, a copied handler, a role granted too
widely — is one account changing another's password. The engine's API key is
deliberately powerful, and the way to keep it safe is to keep it away from
requests that are about the caller.

The cost is a second HTTP client and a second idea of "signed in" in one
application, and a screen without the panel's chrome when it is reached
mid-login. Both are visible in the code rather than hidden, which is the right
side to err on.

This also means self-service works for deployments whose panel role cannot see
other accounts at all. The two surfaces are independent: a deployment can grant
the Identity screens to nobody and every person still manages their own
account.

## Alternatives considered

**Through the engine, with the caller's identity checked in a handler.** The
engine would have had to verify that the account being changed is the caller's,
from a token whose subject is the provider's user id. Correct, and one bug away
from not being — and the bug is silent.

**Inside the guarded shell, with a redirect for the mid-login case.** The
redirect would have to survive an engine session that does not exist yet. The
provider's own page has no such requirement, and neither should its
replacement.
