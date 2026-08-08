# Lending system tests

Black-box tests for the checked-in [lending example](../examples/lending-golang),
driven the way a member drives a library: over HTTP, as one of its roles,
against services that are already running. Nothing here imports the engine.

They exist to answer a question the unit tests cannot: the same YAML is served
by the **standalone Rust engine** and by a **Go application embedding the
compiled core**, and those two must not disagree. Every case runs against both
stands, and a failure names which host was wrong.

That is the whole claim of the embedded SDK — that `Planner::plan` and sqlgen
are reused verbatim, so the SQL is the engine's SQL. This suite is what would
notice if it stopped being true.

## Running

```bash
tests-system-lending/stack.sh up          # database, the engine, the Go host
python3 -m venv tests-system-lending/.venv
tests-system-lending/.venv/bin/pip install -r tests-system-lending/requirements.txt

cd tests-system-lending && eval "$(./stack.sh env)" && .venv/bin/python -m pytest
                                          # or, from the repo root: make lending-system-tests

tests-system-lending/stack.sh down        # when finished
```

`stack.sh` builds both binaries **from this working tree** — a published image
would test somebody else's build — applies the platform's migrations, then the
example's, then publishes the Process revisions, regenerates the snapshot the
Go host embeds, and serves each stand against its own database so one cannot
observe the other's rows.

It raises a third process: an engine against the Go stand's database whose only
job is the durable runtime loop. The Go host originates Process work and does
not carry it forward, so this is the deployment shape the SDK documents — and
running it here is what lets the suite prove the durable outcome rather than
assert it. The two stands reach the same follow-up row by different routes.

The engine stand serves a copy of the metadata with handler-less actions
removed (`engine_metadata.py`): those are resolved in-process by a function the
embedding program registered, and `donat-server` refuses to mount a field it
could never answer. Everything the suite compares — every command, rule,
permission and table — is byte-identical.

With neither `LENDING_ENGINE_URL` nor `LENDING_GO_URL` set the whole suite
skips. A run with no service must not look like a passing run, and must not
look like a broken library either.

| Variable | Meaning |
|---|---|
| `LENDING_ENGINE_URL` | the standalone engine stand; unset skips that half |
| `LENDING_GO_URL` | the Go host stand; unset skips that half |
| `LENDING_ADMIN_SECRET` | sent as `X-Donat-Admin-Secret`; the engine only honours `X-Donat-*` headers on a trusted request, and the Go host ignores it |
| `LENDING_PG_BASE` | the database server, default `…@127.0.0.1:15433` |
| `LENDING_ENGINE_PORT`, `LENDING_GO_PORT`, `LENDING_DRIVER_PORT` | listen ports, default 8090, 8091 and 8092 |

## How the tests talk to the library

**Roles, not authority.** The engine has no admin role, so every request runs
as one explicit role and carries the identity that role acts on. A member's
token cannot reach another member's loan by adding a header — the command
permission's filter is what refuses it, inside the statement.

**Nothing recomputes the answer.** Each test states a rule the YAML declares
and then asks the service to break it. The borrowing limit lives in
`rules.yaml`; this suite only checks that it is enforced. A test that computed
the expected result in Python would be testing itself.

**Fixtures per test, not a shared reset.** Each test gets a fresh member, so
the stands could share a database if an operator pointed them at one, and a
failed test cannot strand another test's rows.
