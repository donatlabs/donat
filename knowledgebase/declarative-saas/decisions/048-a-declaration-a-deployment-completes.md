---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A declaration a deployment completes, and the configuration that completes it

## Context

Nine hand-written connectors — Batch A (spec 012) and Batch F (spec 017) —
existed as green modules in `donat-connectors` with no way to deploy any of
them. Wiring them into the serving registry ran into two facts the existing
seam had no shape for.

The first is configuration. `ConnectorConfig` carried a fixed field set —
`base_url`, `headers`, `secret_key`, `webhook_secret`, `api_version`, `oauth2` —
that describes the declarative `http` connector and Stripe. It has one
`SecretRef` slot for a credential and no place at all for a non-secret
deploy-time value. Airtable needs a base ID, Twilio an Account SID, and every
AWS connector needs a Region plus a target (bucket, queue and its type, sending
identity) and *two or three* secrets, since AWS SigV4 signs with an access key
id, a secret access key, and optionally a session token.

The second is Twilio specifically. Spec 010 §11 makes the module table one
`&'static [&'static Connector]`, and eight of the nine connectors fit it
exactly. Twilio does not: its HTTP Basic username *is* the Account SID, the
SDK's `AuthPlan::basic` takes its username where the plan is built, and the same
SID is a path segment of every resource. A `&'static` declaration would have to
carry a placeholder username — which is a credential contract that does not
describe what reaches the wire, and the module's own documentation says so.

## Decision

Deploy-time configuration gets two additive metadata maps:
`config.settings` for non-secret values and `config.secrets` for further named
`SecretRef`s. Both are open in `donat-metadata`, which cannot know that
`aws_sqs` needs a `queue_type`, and **closed in the module that reads them**:
every connector declares its required and optional keys, a missing required key
is refused with its metadata path, and a key no module reads is refused rather
than ignored ([[034-a-declaration-the-runtime-ignores-is-a-defect]]). A
`SecretRef` in `config.secrets` is checked for availability by the same startup
pass that checks `secret_key`, so a missing one stops startup naming only the
variable.

The module table's key becomes a closed two-variant `ModuleDeclaration`:
`Static(&'static Connector)` for every module whose declaration is a constant,
and `PerDeployment { name, version, declare }` for Twilio, where `declare` is a
compiled `fn` from one already-validated `ConnectorInstance` to its declaration.
Metadata validation and instance compilation both run against the *resolved*
declaration, so a deployment is validated against exactly the connector it will
run. This is not a dynamic registry: the variant set is closed, the function is
a `fn` item in this binary, its only input is deploy-time metadata, and
everything a declaration decides — the operation set, every effect class, the
origin, the auth plan — is still fixed at compile time. One configured value
inside it is not.

The third decision is one of omission. A hand-written connector publishes **no**
catalog `OperationSpec` yet, because an SDK `Operation` exposes its id, version,
method, and effect class but not the path template, query bindings, success
statuses, or output pointers a spec is built from. Rather than duplicate those
in the server — a second description of one provider that could disagree with
the first — the seam publishes nothing to process compilation and says so. The
gate spec 010 §7 cares about is unaffected in the direction that matters: what
is published is a subset of what was admitted, and an inventory-only operation
is never admitted.

## Alternatives

| Option | Why Not |
|--------|---------|
| Add one typed field per connector to `ConnectorConfig` (`base_id`, `account_sid`, `region`, `bucket`, …) | The metadata crate would grow a field for every connector ever written, and `deny_unknown_fields` would make each one a breaking format change. The closed surface belongs where the module is, not where the format is |
| Carry the AWS access key in `secret_key` and the rest in `headers` | `headers` are HTTP headers the declarative connector sends; using them as a secret bag describes a request nobody makes |
| Give Twilio a placeholder Account SID so the table stays `&'static` | The placeholder becomes the Basic username in the published credential contract. Anything reading the table's auth plan would authenticate as nobody, and the table would be describing a request that is never sent |
| Make the whole table `fn(&ConnectorInstance) -> Connector` | Loosens every module to the weakest one. Eight declarations *are* constants and the table should keep saying so |
| Let Twilio's declaration be rebuilt at request time from the credential | Puts a declaration on the request path, which is the one thing "a connector is a declaration" exists to prevent |
| Duplicate each connector's request shape in `catalog.rs` to publish an `OperationSpec` | Two descriptions of one provider, and the second one is written by whoever last read the module. The projection belongs in the SDK, beside the declaration it is derived from |

## Consequences

Adding a connector is a module file, a table line, a section of deploy-time
rules, and a conformance fixture — and nothing in `state.rs` learns its name.
A deployment of any of the nine is refused before a listener opens for a missing
credential (naming the variable), a missing or hostile configuration value
(naming the metadata key), an operation this binary does not carry, an
inventory-only operation, or an operation whose class this deployment's own
target denies it ([[046-an-effect-class-can-depend-on-deploy-time-configuration]]).

Three costs are worth naming. Twilio's table entry carries its name and contract
version as constants rather than reading them off a declaration, so those two
could in principle disagree — they are the module's own `NAME` and `VERSION`
constants, which is the same source the declaration is built from, and the
module-table test asserts the entry is reachable under exactly that name. Each
instance holds a cloned `Connector` rather than a `&'static` one, which is one
allocation per instance at startup and buys one runtime type instead of two that
differ only in a lifetime. And until the catalog projection lands, these
connectors are deployable and executable through the registry but not yet
referenceable from a Process — which is a visible, tested gap rather than a
silent one.
