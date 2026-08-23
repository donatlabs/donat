.PHONY: build test conformance db-up db-down db-logs conformance-backend \
	backend-runtime conformance-matrix perf perf-matrix perf-mixed run claude codex \
	petshop-up petshop-down petshop-system-tests wasm-core go-test \
	lending-up lending-down lending-system-tests gate \
	setup-loop-infrastructure loop loop-status loop-off

build:
	cargo build

test:
	cargo test

# Rebuild the wasm core the Go SDK embeds. The blob is committed so that
# `go get` works without a Rust toolchain, so it must be regenerated here
# whenever any crate below `donat-wasm-core` changes.
# The path remapping is what makes the blob reproducible across machines, and
# that is what lets CI check the committed one against its sources: without it
# the wasm carries the builder's absolute paths in panic messages, so two
# correct builds differ and a byte comparison proves nothing. It also keeps a
# developer's home directory out of a shipped artifact. RUSTFLAGS replaces the
# config.toml rustflags rather than adding to them, so the getrandom backend
# has to be repeated here.
WASM_RUSTFLAGS = --cfg getrandom_backend="custom" \
	--remap-path-prefix=$(CURDIR)=/donat \
	--remap-path-prefix=$(HOME)/.cargo=/cargo

wasm-core:
	rustup target add wasm32-unknown-unknown
	RUSTFLAGS='$(WASM_RUSTFLAGS)' cargo build -p donat-wasm-core \
		--target wasm32-unknown-unknown --release
	cp target/wasm32-unknown-unknown/release/donat_wasm_core.wasm sdk/go/donat/wasm/core.wasm

# The Go host's own suite. CGO stays off: the SDK is `go get`-able and
# builds a static binary, which is the property wazero was chosen for.
go-test: wasm-core
	cd sdk/go && CGO_ENABLED=0 go vet ./... && CGO_ENABLED=0 go test ./...

# Both lending stands: the standalone engine and the Go host, from one
# metadata directory. See tests-system-lending/README.md.
lending-up:
	tests-system-lending/stack.sh up

lending-down:
	tests-system-lending/stack.sh down

# Black-box lending suite: every case against the standalone engine AND the Go
# host. A disagreement between them is the bug it exists to find.
lending-system-tests:
	@test -d tests-system-lending/.venv || python3 -m venv tests-system-lending/.venv
	@tests-system-lending/.venv/bin/pip install -q -r tests-system-lending/requirements.txt
	@cd tests-system-lending && eval "$$(./stack.sh env)" && .venv/bin/python -m pytest

# What CI's change gate will ask this branch to declare, before it asks.
# GATE_BASE is the branch the pull request targets; GATE_BODY is a file with
# the description you intend to write, so the markers can be checked locally.
GATE_BASE ?= main
gate:
	python3 scripts/check_change_gate.py --self-test
	python3 scripts/check_change_gate.py --base $(GATE_BASE) $(if $(GATE_BODY),--body-file $(GATE_BODY))

# The nightly loops: jobs that run unattended on this machine, each in a
# worktree of its own, and leave nothing behind but a pull request. See
# scripts/loop.sh and .claude/skills/<job>/SKILL.md; the job list is in
# scripts/loop-nightly.sh. State lives in ~/.local/state/donat-loops.
setup-loop-infrastructure:
	scripts/loop-setup.sh install

# Run one job now, the way the timer would: make loop JOB=fix-advisories
loop:
	@test -n "$(JOB)" || (echo 'usage: make loop JOB=<job>   (jobs: see scripts/loop-nightly.sh)'; exit 2)
	scripts/loop.sh $(JOB)

loop-status:
	scripts/loop-setup.sh status

loop-off:
	scripts/loop-setup.sh remove

# Native Postgres reference conformance suite. Spawns its own engine
# instances, one database per suite.
conformance:
	cargo build -p donat-server --bin donat
	@PG_URL="$${CONFORMANCE_PG_URL}" S3_URL="$(CONFORMANCE_S3_URL)" \
		cargo test -p donat-conformance -- --test-threads=4

CONFORMANCE_COMPOSE ?= docker compose -f docker-compose.conformance.yml
CONFORMANCE_BACKENDS ?= postgres sqlite mysql clickhouse
CONFORMANCE_PG_URL ?= $(if $(PG_URL),$(PG_URL),postgresql://postgres:postgres@127.0.0.1:15432/postgres)
CONFORMANCE_MYSQL_URL ?= $(if $(MYSQL_URL),$(MYSQL_URL),mysql://root:root@127.0.0.1:13306/donat)
CONFORMANCE_CLICKHOUSE_URL ?= $(if $(CLICKHOUSE_URL),$(CLICKHOUSE_URL),http://donat:donat@127.0.0.1:18123)
# File attachments need an S3-compatible store; the compose file runs MinIO.
CONFORMANCE_S3_URL ?= $(if $(S3_URL),$(S3_URL),http://127.0.0.1:19000)

# Export indirection variables so recipes never expand credential-bearing URLs
# into the command text printed by make (including `make -n`).
export CONFORMANCE_PG_URL
export CONFORMANCE_MYSQL_URL
export CONFORMANCE_CLICKHOUSE_URL

# Start every disposable external service the conformance harness uses: the
# database matrix, and the object store file attachments require. Bucket setup
# is a separate step because a one-shot container cannot satisfy `--wait`.
db-up:
	$(CONFORMANCE_COMPOSE) up -d --wait
	$(CONFORMANCE_COMPOSE) run --rm minio-init

db-down:
	$(CONFORMANCE_COMPOSE) down --remove-orphans

db-logs:
	$(CONFORMANCE_COMPOSE) logs --tail=200

# Run the shared backend contract for one selected backend. The service must
# already be available; SQLite uses its in-process target.
conformance-backend:
	@test -n "$(BACKEND)" || (echo 'usage: make conformance-backend BACKEND=<postgres|sqlite|mysql|clickhouse>'; exit 2)
	cargo build -p donat-server --bin donat
	@CONF_BACKEND=$(BACKEND) \
	PG_URL="$${CONFORMANCE_PG_URL}" \
	MYSQL_URL="$${CONFORMANCE_MYSQL_URL}" \
	CLICKHOUSE_URL="$${CONFORMANCE_CLICKHOUSE_URL}" \
	cargo test -p donat-conformance --lib
	@CONF_BACKEND=$(BACKEND) \
	PG_URL="$${CONFORMANCE_PG_URL}" \
	MYSQL_URL="$${CONFORMANCE_MYSQL_URL}" \
	CLICKHOUSE_URL="$${CONFORMANCE_CLICKHOUSE_URL}" \
	cargo test -p donat-conformance --test backend_matrix -- --test-threads=4 --nocapture

# Run the live MySQL and ClickHouse server-path tests. Unlike an ordinary
# workspace test, this target requires the compose services and therefore
# fails if either configured backend is unavailable.
backend-runtime:
	@DONAT_EXTERNAL_DB_TESTS=1 \
	MYSQL_URL="$${CONFORMANCE_MYSQL_URL}" \
	CLICKHOUSE_URL="$${CONFORMANCE_CLICKHOUSE_URL}" \
	cargo test -p donat-server \
		--test mysql_e2e \
		--test mysql_runtime \
		--test mysql_mutations \
		--test clickhouse_runtime -- --include-ignored --nocapture

# Run the shared contract once for every registered backend. External services
# are started once and suite databases remain isolated per backend/test.
conformance-matrix:
	trap '$(MAKE) db-down' EXIT INT TERM; \
	$(MAKE) db-up || exit $$?; \
	for backend in $(CONFORMANCE_BACKENDS); do \
		$(MAKE) conformance-backend BACKEND=$$backend || exit $$?; \
	done; \
	$(MAKE) backend-runtime

# Local bottleneck investigation only: records measurements and never applies
# pass/fail thresholds. SQLite is self-contained; external backends use
# PERF_DATABASE_URL + PERF_METADATA_DIR.
perf:
	BACKEND="$${BACKEND:-sqlite}" benchmarks/perf/run.sh

# Local-only backend comparison. External backends use
# PERF_<BACKEND>_DATABASE_URL and PERF_<BACKEND>_METADATA_DIR so a matrix
# cannot accidentally combine a URL with metadata for another backend.
perf-matrix:
	benchmarks/perf/matrix.sh

# Local-only mixed-source workload. Its metadata and query must describe the
# participating sources explicitly; this target never assumes a topology.
perf-mixed:
	benchmarks/perf/mixed.sh

run:
	cargo run --bin donat -- --metadata-dir crates/metadata/tests/fixtures/metadata

# Black-box system tests for the checked-in Petshop example. The stand runs the
# engine built from this working tree; see tests-system/README.md.
petshop-up:
	tests-system/stack.sh up
	tests-system/stack.sh up-fast

petshop-down:
	tests-system/stack.sh down-fast
	tests-system/stack.sh down

# Both stands, because the deadline branches skip themselves when the fast one
# is not addressed — and a skipped branch reads exactly like a passing one.
petshop-system-tests:
	@test -d tests-system/.venv || python3 -m venv tests-system/.venv
	@tests-system/.venv/bin/pip install -q -r tests-system/requirements.txt
	@cd tests-system && eval "$$(./stack.sh env)" && .venv/bin/python -m pytest

# The UI (apps/ui) is its own npm project, not part of the Cargo
# workspace — `make test` does not reach it, so these targets are how it is
# run and checked. Point VITE_DONAT_GRAPHQL_URL at an engine first (see
# apps/ui/.env.example).
admin:
	cd apps/ui && npm install && npm run dev

admin-test:
	cd apps/ui && npm install && npm run typecheck && npm test

claude:
	claude --dangerously-skip-permissions --teammate-mode tmux

codex:
	codex --sandbox danger-full-access

# Fill .env with fresh secrets.
#
# .env.example ships every name and no value: a committed file with working
# secrets in it is a shared secret. This makes the values, once, on the machine
# that will use them. It refuses to overwrite an existing .env — those secrets
# are already in a database somewhere.
.PHONY: env
env:
	@test ! -f .env || { echo ".env exists; delete it first if you mean to replace it"; exit 1; }
	@python3 scripts/generate-env.py > .env
	@echo "wrote .env — sign in as operator@example.com with:"
	@grep DONAT_ADMIN_PASSWORD .env

# The same thing as a file rule, so `up` below can depend on it. It runs only
# when `.env` is absent, which is the same refusal as `env`'s, expressed by
# make rather than by a test.
.env:
	@python3 scripts/generate-env.py > .env
	@echo "no .env, so wrote one."

# The whole stack, from nothing.
#
# There is no step here where somebody types a value. Every credential this
# deployment uses is generated on the machine that will use it, because each is
# a secret two programs use to recognise each other — not something a person
# chooses, remembers or should ever see. The one exception is printed at the
# end, because it is the one a person actually types.
.PHONY: up
up: .env
	# `--remove-orphans` because this stack lost a service: the panel used to be
	# its own nginx container, and the engine serves it now. Without this, the
	# old container is still running, still holding the port, and the new engine
	# cannot bind — which is what upgrading looks like without it.
	docker compose up -d --build --remove-orphans
	@echo
	@port=$$(sed -n 's/^DONAT_ADMIN_PORT=//p' .env); echo "open http://localhost:$${port:-5180}"
	@echo "sign in as operator@example.com with:"
	@grep DONAT_ADMIN_PASSWORD .env

.PHONY: down
down:
	docker compose down
