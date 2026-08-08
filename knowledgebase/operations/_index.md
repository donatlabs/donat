# Operations — deploying and running the engine

> How the engine behaves as a process rather than as a compiler: what bounds a
> request, how it connects to its database, how it stops, and what an operator
> can see and do without a permission bypass.

**Status: in progress (August 2026).** The decisions here came out of an audit
of the engine as a deployed product rather than as a library, and they mostly
close gaps between the internal-component threat model recorded in
[[security-audit]] (June 2026) and the public image the README now documents.

## Decisions

- [[decisions/001-bounded-and-drainable-by-default]] — a statement ceiling on
  every pooled session, a deadline on every request-response surface (but not
  on websocket upgrades), a panic that becomes a response, and a `SIGTERM`
  that drains the HTTP server and every background worker instead of cutting
  them off
- [[decisions/002-the-engine-speaks-tls-to-postgres]] — one shared TLS
  connector replaces the hard-coded `NoTls` at every connection site, and an
  unusable `DONAT_GRAPHQL_JWT_SECRET` stops the boot instead of silently
  disabling token verification
- [[decisions/003-a-replica-announces-its-own-readiness]] — stopping happens in
  two phases, so a balancer is told before it is true; `/readyz` reports the
  first phase and neither probe touches the database
- [[decisions/004-the-other-end-of-a-socket-is-not-trusted-to-behave]] — every
  upstream response is read against a ceiling and abandoned past it, and a
  pooled connection is proved alive before it is handed out
- [[decisions/005-the-deploy-gate-and-the-log-are-for-the-operator]] —
  `validate` resolves the credentials `serve` will need, and
  `DONAT_LOG_FORMAT=json` gives a collector something to read

## Related

- [[declarative-saas/decisions/002-durable-process-operational-contracts]] —
  owns the operator contract for durable Processes: the drain requirement the
  shutdown work implements, and the two read-only CLI diagnostics
  (`donat process inspect`, `donat process verify-history`) that are the only
  permitted operator entry points
- [[security-audit]] — the ranked security findings and their resolution
