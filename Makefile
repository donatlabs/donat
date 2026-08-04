.PHONY: build test conformance db-up db-down db-logs conformance-backend \
	backend-runtime conformance-matrix perf perf-matrix perf-mixed run claude codex \
	petshop-up petshop-down petshop-system-tests wasm-core go-test

build:
	cargo build

test:
	cargo test

# Rebuild the wasm core the Go SDK embeds. The blob is committed so that
# `go get` works without a Rust toolchain, so it must be regenerated here
# whenever any crate below `donat-wasm-core` changes.
wasm-core:
	rustup target add wasm32-unknown-unknown
	cargo build -p donat-wasm-core --target wasm32-unknown-unknown --release
	cp target/wasm32-unknown-unknown/release/donat_wasm_core.wasm sdk/go/donat/wasm/core.wasm

# The Go host's own suite. CGO stays off: the SDK is `go get`-able and
# builds a static binary, which is the property wazero was chosen for.
go-test:
	cd sdk/go && CGO_ENABLED=0 go vet ./... && CGO_ENABLED=0 go test ./...

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

claude:
	claude --dangerously-skip-permissions --teammate-mode tmux

codex:
	codex --sandbox danger-full-access
