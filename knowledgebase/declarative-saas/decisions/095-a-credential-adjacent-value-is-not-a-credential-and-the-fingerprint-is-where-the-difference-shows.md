---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A credential-adjacent value is not a credential, and the fingerprint is where the difference shows

## Context

Several providers in this programme send **two** values on every request and
call both of them "your credentials". Twilio puts its Account SID in the HTTP
Basic username beside the auth token; Freshdesk puts the API key in the username
and a dummy password beside it
([[064-a-credentials-scheme-and-its-username-are-the-providers]]); WooCommerce
sends a consumer key beside a consumer secret; Trello sends two values on the
query string and neither authenticates alone
([[066-a-credential-can-be-two-query-parameters-and-an-account-is-a-compiled-path-prefix]]).

Spec 028 asks for two more, and they are the clearest instances of the shape:
Harvest sends `Authorization: Bearer $ACCESS_TOKEN` **and**
`Harvest-Account-Id: $ACCOUNT_ID`, and BambooHR puts the API key in the Basic
*username* with a fixed password. The question spec 028 §3 poses is not "how do
we send them" — the SDK already has plans for both — but **which of the two
values is a secret**, and what follows from the answer.

The programme had answered it case by case and never written the rule down. The
existing answers disagree in a way that looks arbitrary until the reason is
stated: Twilio's Account SID is `FieldClassification::NonSecret`, WooCommerce's
consumer key is a `config.settings` value, and Trello's API key is a
`config.secrets` entry — three providers, three different homes for "the other
half of the credential".

## Decision

**A value is a secret when possessing it changes what a caller can do. A value
that only says *which* account a request is about is deploy-time configuration,
and it goes in `config.settings`.**

Harvest's account id fails the test in both directions and so it is not a
secret: Harvest prints it in its own Developers page beside the token it issues,
and holding it authorizes nothing — a request carrying the account id and no
token is rejected. It is `config.settings` material, declared on the credential
contract as `FieldClassification::NonSecret` so that startup can still refuse an
instance that configured neither, and the same value is compiled into every
operation's `Harvest-Account-Id` header by a declaration a deployment completes
([[048-a-declaration-a-deployment-completes]]).

Trello's API key passes the test and is a secret: it identifies the
*application*, it is what a rate limit and an audit are keyed on, and Trello's
own advice is to treat the pair as a pair. The distinction is not "is it half of
something the provider calls a credential" — it is "does holding it let you do
anything".

**The configuration fingerprint is where the difference becomes observable, and
that is the property this ADR asks a batch to prove.**
`provider_configuration_fingerprint` hashes `config.settings` **by value** and
`config.secrets`/`config.secret_key` **by environment variable name**. So:

* the non-secret half *decides* the fingerprint — pointing a pinned operation at
  another account makes it a different pinned operation, which is correct,
  because it is a different thing to do;
* the secret half cannot reach it at all — rotating the value behind the same
  variable leaves the fingerprint byte-for-byte identical, which is also
  correct, because rotating a token does not change what the operation does.

That gives the split a *machine-checkable* consequence rather than a naming
convention. `the_harvest_account_id_is_fingerprinted_and_its_token_is_not` is
the proof: it asserts both halves at once, plus the negative half — that a
startup refusal of the account id names `config.settings.account_id` and
discloses no resolved secret.

**A non-secret value still gets a grammar and a deploy-time refusal.** Being
public is not being harmless: the account id travels in a header, so a value
carrying `\r\n` would be a request this connector did not write, and a mistyped
account is a request against somebody else's data. `harvest::validate_account_id`
refuses anything that is not the numeric identifier Harvest publishes, the
declaration is refused where it is built, and the conformance fixture
`harvest_startup.yaml` makes the refusal the case an operator actually meets.

**And it stays out of the projection.** A value a deployment configures is never
a Process input: `harvest::ACCOUNT_ID` and `harvest::USER_AGENT` join the
reserved-name list in `crates/connectors/tests/projection.rs`, beside Twilio's
Account SID and Basecamp's account prefix, so no operation can publish either as
a slot a Process fills.

## Alternatives

| Option | Why Not |
|--------|---------|
| Put the account id in `config.secrets` too, on the grounds that it travels with a credential | It would be redacted out of every diagnostic, and "which account did this activity reach" is exactly the question an operator has when a timesheet lands in the wrong place. It would also leave the fingerprint unable to distinguish two instances that differ only in the account they point at — two different pinned operations hashing the same |
| Leave the account id out of the declared credential contract entirely, since only `settings` fills it | `CredentialSpec::admits` is what makes startup answer "is this instance complete" by name before a listener opens. A field nothing declares is a field nothing can refuse the absence of, and the failure would surface as a provider `401` on the first activity attempt instead |
| Take the account id from operation input, since it is not a secret | A connector whose account came from input is a connector a Process could aim at another tenant. Non-secret is not the same as caller-chosen, and this is the refusal [[066-a-credential-can-be-two-query-parameters-and-an-account-is-a-compiled-path-prefix]] already made for a path prefix |
| Skip the grammar check because the value is public | It is a header value. `\r\n` in it is a second header field, and a typo in it is another account's data. Publicness bounds the *disclosure* cost, not the *injection* cost |
| Hash secret values into the fingerprint too, so rotation is visible | The fingerprint is a non-secret deployment identity that is logged and compared; putting a resolved secret into it would put a secret everywhere the fingerprint goes. Rotation is a credential-lifecycle event, and it is not supposed to invalidate a pinned operation |

## Consequences

The programme has a stated rule for the value that sits beside a credential, and
one proof shape that any batch meeting the pattern can copy: assert that the
non-secret half decides the fingerprint, that the secret half cannot move it,
and that a refusal of the first never prints the second. Harvest is the worked
example; Twilio's Account SID and WooCommerce's consumer key are the same call
made before the rule was written, and Trello's API key is the counter-example
that shows the test has two answers.

The cost is that the rule rests on a judgement — "does holding it let you do
anything" — which a reviewer applies per provider and no type enforces. That is
the same kind of judgement `ReadOnlyAssertion` and `NoIdempotencyEvidence`
already rest on ([[042-the-effect-gate-admits-evidence-not-methods]]), and the
fingerprint test is what makes a *wrong* answer visible rather than silent: a
value misfiled as a secret stops changing the fingerprint, and a value misfiled
as configuration starts appearing in one.
