.PHONY: build test conformance db-up db-down db-logs conformance-backend \
	backend-runtime conformance-matrix perf perf-matrix perf-mixed run claude codex

build:
	cargo build

test:
	cargo test

# Native Postgres reference conformance suite. Spawns its own engine
# instances, one database per suite.
conformance:
	cargo build -p donat-server --bin donat
	@PG_URL="$${CONFORMANCE_PG_URL}" cargo test -p donat-conformance -- --test-threads=4

CONFORMANCE_COMPOSE ?= docker compose -f docker-compose.conformance.yml
CONFORMANCE_BACKENDS ?= postgres sqlite mysql clickhouse
CONFORMANCE_PG_URL ?= $(if $(PG_URL),$(PG_URL),postgresql://postgres:postgres@127.0.0.1:15432/postgres)
CONFORMANCE_MYSQL_URL ?= $(if $(MYSQL_URL),$(MYSQL_URL),mysql://root:root@127.0.0.1:13306/donat)
CONFORMANCE_CLICKHOUSE_URL ?= $(if $(CLICKHOUSE_URL),$(CLICKHOUSE_URL),http://donat:donat@127.0.0.1:18123)

# Export indirection variables so recipes never expand credential-bearing URLs
# into the command text printed by make (including `make -n`).
export CONFORMANCE_PG_URL
export CONFORMANCE_MYSQL_URL
export CONFORMANCE_CLICKHOUSE_URL

# Start all disposable external database services used by the backend matrix.
db-up:
	$(CONFORMANCE_COMPOSE) up -d --wait

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

claude:
	claude --dangerously-skip-permissions --teammate-mode tmux

codex:
	codex --sandbox danger-full-access
