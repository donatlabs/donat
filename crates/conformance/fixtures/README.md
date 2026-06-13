# Conformance fixtures

YAML fixtures derived from Donat's `server/tests-py` test suite
(https://github.com/donat/graphql-engine, commit
`371d744e8a063fe348e291cc306f37973b11d1b8`), licensed under the Apache
License 2.0 — see LICENSE.donat. Local modifications are marked with
`# donat:` comments (currently: HTTP status corrections documented in
the conformance notes; admin-only fixtures are excluded from execution by
the test modules, not edited).

Executed by `crates/conformance/tests/*` via the harness in
`crates/conformance/src/lib.rs`. See ../PORTING.md for conventions.
