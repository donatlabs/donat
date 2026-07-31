---
type: decision
status: accepted
date: 2026-07-31
features:
  - "[[declarative-saas]]"
  - "[[006-in-binary-connectors]]"
---

# Connector egress reachability is a network concern, not an engine setting

## Context

The HTTP and Stripe connectors classified every resolved destination address
and refused any host that was not globally reachable: loopback, private LAN
ranges, link-local, and the IANA IPv6 special-purpose prefixes. The rule
existed because connector base URLs come from metadata, so it treated a
declaration aimed at internal infrastructure as an SSRF primitive.

The rule also made two legitimate cases impossible. A deployment whose
providers genuinely live on an internal network could not call them at all, and
no black-box test could exercise an outbound activity, because a test host only
has loopback and private addresses. Outbound connector behavior — retry,
stable idempotency keys, lease takeover, capacity — was therefore provable only
in crate-local tests, through a `#[cfg(test)]` escape hatch that the shipped
binary did not have.

## Decision

The engine enforces no destination reachability policy. Which hosts a
deployment may reach is decided by its network layer — egress rules, firewall,
or VPC configuration — where the operator already expresses that intent for
every other outbound connection the process makes.

The address classifier, its IANA prefix tables, and the `NetworkPolicy` type
are removed from both connector modules. What remains is the part that is an
engine invariant rather than a network policy: the configured host is re-resolved
immediately before connecting, and the connected peer must be one of the
addresses that resolution returned. One request stays on one resolved host, so
a name cannot resolve to one address for validation and another for transport.

A `network_policy` declaration in connector metadata is now rejected as an
unsupported setting, exactly as the Stripe module already rejected it. Silently
accepting a policy field the engine does not implement would be worse than
either enforcing or refusing it.

Nothing about caller reach changes. No GraphQL caller, process event, or role
can select a base URL, host, port, or DNS target; those remain deploy-time
configuration, so process input still cannot aim the engine at a destination it
was not configured to call.

## Alternatives

| Option | Why Not |
| --- | --- |
| Keep public-only with no escape | Blocks internal providers entirely and leaves outbound activity behavior unprovable outside crate-local tests. |
| Add a deploy-time `allow_private_destinations` flag | Keeps an application-level duplicate of a network-level control, and the safe default is the one that breaks real internal deployments. |
| Keep the policy and test outbound activities against a public host | Makes the test suite depend on external network reachability and a third party's availability. |
| Drop peer pinning along with the policy | Pinning is not a reachability rule: without it one request could validate one address and connect to another. |
| Accept and ignore `network_policy` in metadata | Silently ignoring a security-shaped declaration misleads the operator who wrote it. |

## Consequences

A misconfigured or malicious `base_url` can now reach anything the deployment's
network permits, including a cloud metadata endpoint, if egress is unrestricted.
That risk moves to the deployment: hardening guidance belongs with the egress
configuration rather than in the engine, and the engine no longer implies a
protection an operator might not otherwise apply.

Outbound connector activities become testable end to end against a local
provider stub, so retry, idempotency-key stability, and takeover can be proven
through the real binary instead of only in crate-local tests.
