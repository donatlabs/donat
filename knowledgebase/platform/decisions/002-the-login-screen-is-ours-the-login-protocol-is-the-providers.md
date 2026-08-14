---
type: decision
status: accepted
date: 2026-08-11
features:
  - "[[platform]]"
---

# The login screen is ours, the login protocol is the provider's

## Context

[[001-the-admin-panel-is-a-role-rendered-not-a-surface-the-engine-grows]]
settled that the panel holds no credential and that the engine's relying party
sends an operator to the deployment's identity provider to sign in. That works,
and it leaves one screen in the product looking like someone else's software —
the first screen anyone sees.

Three attempts at fixing that failed, each for a reason worth keeping:

- **Theme the provider's page.** Rauthy serves a per-client theme
  (`scripts/idp-theme.mjs` maps our palette onto it), which recolours the page
  but cannot change its layout, its wording or its shape.
- **A first-party email-and-password form.** It could only reach the provider
  from the browser, which its `allowed_origins` refuse; and the grant that
  would have made it work — the OAuth password grant — cannot prompt for a
  second factor, offer a passkey or start a recovery, and skips the provider's
  own rate limiting and proof of work. Built, demonstrated failing against a
  real provider, removed.
- **Fork the provider's frontend.** 263 Svelte components, of which the login
  page alone is 861 lines, in a framework this repository does not otherwise
  use.

What made a fourth attempt possible was reading the provider rather than
guessing at it. Its login page is not part of its protocol: it is an ordinary
client of its own HTTP API, and that API is pinned by the provider's own
integration tests — `src/bin/tests/handler_auth.rs`, 1211 lines, run against a
live backend on two databases, with a case literally commented *"simulate UI
login"* that performs the exact sequence a login page performs. Its frontend,
by contrast, has no tests at all: `frontend/tests/test.js` is six lines of
untouched SvelteKit scaffold asserting an `h1` reading "Welcome to SvelteKit".

## Decision

**The panel renders the login screen; the provider keeps the login protocol.**
A new route (`/idp/authorize`) is the destination of the engine's authorization
redirect — `DONAT_OIDC.authorization_endpoint` points at it instead of at the
provider's page — and it carries the request it receives through to the
provider's unchanged endpoints: `POST /oidc/session` for the session and its
CSRF token, `POST /pow` for a proof-of-work challenge, `POST /oidc/authorize`
for the attempt, and then the `Location` the provider answers with, which
carries the code to the engine's `/auth/callback`.

Nothing about OpenID Connect moves. The engine is still the relying party and
still the only holder of the PKCE verifier; the code is still exchanged server
to server; the panel still never sees a token. What changed is which document
draws the form.

**The provider is reached on the panel's origin.** nginx proxies `/auth/v1/` to
it, and the provider's own public URL is configured as the panel's address.
This is not a trick to defeat a check — it is the reverse-proxy deployment the
provider documents, and it is what makes its `__Host-` session cookie and its
`Origin` comparison work rather than something to be worked around.

**Only the screens we can finish.** Password login is one endpoint; a passkey,
a terms update and a forced enrolment are separate protocols. Those hand over
to the provider's own page — still one proxied request away, carrying the same
authorization request — so those accounts sign in correctly rather than almost.
`research-what-a-platform-needs` already recorded that almost-right is worse
than absent; this is that rule applied to ourselves.

> **Amended 2026-08-14.** The rule stands; what changed is what we can finish.
> A passkey is now signed on our own screen and new terms are read on it, and
> the reset link an email carries lands on a page of ours because the engine
> rewrites the provider's URL to it. The two remaining answers — an application
> demanding a second factor the account has not got, and an account the
> provider wants updated first — were never login screens: the provider's own
> page points at its account page for both, and ours points at the account
> screen described in
> [004](004-the-account-screens-act-as-the-person-the-identity-screens-act-as-the-deployment.md).
> Nothing was half-implemented to get there; each protocol was read off the
> provider's own frontend and pinned by tests before it was written.

**The proof of work is ported, not called.** It is the one part that must run
in the browser and cannot be a request. `src/idp/pow.ts` is the `spow`
algorithm in TypeScript — find the smallest counter whose
`SHA-256(challenge + counter)` opens with the demanded number of zero bits —
rather than the provider's WebAssembly build of the same crate, which would put
a Rust toolchain and `wasm-pack` inside an npm project deliberately kept
outside the Cargo workspace. It is roughly ten times slower, so it runs in a
worker and starts when the page opens rather than when the button is pressed;
a difficulty this implementation could not finish is refused with a message
instead of a hung tab.

**The provider's admin screens are not ported at all.** Users, clients, roles,
groups, scopes and sessions are resources, and this panel already renders
resources — through the engine's GraphQL, under an ordinary per-role
permission, per [[001-the-admin-panel-is-a-role-rendered-not-a-surface-the-engine-grows]].
Hand-porting that half of the provider's frontend would duplicate the panel's
whole reason to exist.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep sending operators to the provider's page | Still supported, still the default for every provider that is not Rauthy. But it is the first screen of the product, and it is the one screen no amount of theming makes ours. |
| Fork the provider's frontend and restyle it | 263 components in a framework this repository does not use, with no tests to port and none to inherit — and a permanent merge burden against a project that ships often. |
| Serve our page *at* the provider's own authorize path (nginx branching on the request method) | Needs no engine configuration, but hides a routing decision inside a web-server rule where nobody would look for it. The engine already names where the browser is sent; naming our page there is the same fact written where it belongs. |
| Vendor the provider's WebAssembly proof-of-work module | Legally fine (Apache-2.0) and ten times faster, but it drags `wasm-pack` and a Rust toolchain into the panel's build for one function whose algorithm is thirty lines. Reconsider if the difficulty a deployment needs outgrows what JavaScript can solve. |
| Implement the passkey and terms screens too | Each is a protocol of its own (WebAuthn, a terms-acceptance code), and a partly-working passkey screen locks out exactly the operators who took security most seriously. |

## Consequences

The panel's first screen now looks like the panel. The provider keeps
everything that makes it worth having — password policy, rate limiting, proof
of work, passkeys, recovery, its own audit trail — because every one of those
still runs where it always did.

What we pay is a coupling to one provider's HTTP API. It is narrow (five
endpoints, in `src/idp/`), it is versioned by tests we can read, and it is
opt-in per deployment — but it is a coupling, and a provider that is not Rauthy
uses the default shape until someone writes its equivalent. The port also means
this repository now contains an implementation of someone else's algorithm; if
`spow` changes, our login stops working and the symptom will be a refused
proof of work.

The deployment gains a requirement that reads oddly until it is understood: the
identity provider's public URL must be the panel's origin. Written down here,
in `apps/admin/README.md` and in the nginx template, because it is the kind of
constraint that looks like a mistake to whoever inherits it.
