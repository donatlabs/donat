.PHONY: build test conformance db-up db-down db-logs conformance-backend \
	backend-runtime conformance-matrix perf perf-matrix perf-mixed run claude codex \
	petshop-up petshop-down petshop-system-tests wasm-core go-test \
	lending-up lending-down lending-system-tests \
	evals-verify-oracles evals-run evals-agent evals-mutants evals-control \
	evals-sweep evals-down evals-arm evals-compare

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

# Evals: can an agent build this application? Local only, and deliberately not
# in CI — a pipeline must never call a model. See evals/README.md.
#
# `evals-verify-oracles` is the one target here with no agent in it at all: it
# asks whether the tasks can tell a correct store from a plausible wrong one,
# which is the question that decides whether any later score means anything.
evals-verify-oracles:
	python3 evals/run.py verify-oracles $(TASK)

evals-run:
	@test -n "$(TASK)" || (echo 'usage: make evals-run TASK=<task> CANDIDATE=<oracle|anti/<name>|path>'; exit 2)
	python3 evals/run.py run $(TASK) $(or $(CANDIDATE),oracle)

# The mutant corpus: the checked-in Petshop with one business defect seeded at
# a time, judged by the store's own black-box suite. No agent, no prompts, no
# money — it measures what we would *notice*, which is what decides whether any
# later agent score is worth reading. Survivors are holes in the suite.
evals-mutants:
	python3 evals/mutants.py generate --limit $(or $(COUNT),200)

# The pristine store through the identical path. Anything red here is the
# stack, not a defect — and a sweep refuses to run until this is green, because
# a test that fails on a correct store kills every mutant it touches and reads
# exactly like detection.
evals-control:
	python3 evals/sweep.py --control

evals-sweep:
	python3 evals/sweep.py --workers $(or $(WORKERS),6) $(if $(LIMIT),--limit $(LIMIT)) $(if $(ONLY),--only $(ONLY))

# The only target that calls a model, and the only one that costs money. The
# agent is a process contract — prompt in, working tree out — so EVAL_AGENT_CMD
# swaps Claude Code for Codex or anything else without touching a task.
evals-agent:
	@test -n "$(TASK)" || (echo 'usage: make evals-agent TASK=<task> [ATTEMPTS=3] [SKILLS=plugin] [LABEL=name]'; exit 2)
	python3 evals/agent.py $(TASK) --attempts $(or $(ATTEMPTS),1) \
		$(if $(SKILLS),--skills $(SKILLS)) $(if $(LABEL),--label $(LABEL))

# Tuning a skill is a paired question — "is this edit better?" — and paired
# questions are answerable on a corpus this small, where absolute ones are not.
# Both arms run the same tasks with the same attempt count; `compare` reads
# them scenario by scenario, so an edit that moves an attempt from six of ten
# to nine of ten is visible, which it is not in a task-level rate at k=3.
#
#   make evals-arm TASK=001-… LABEL=bare
#   … edit plugins/donat/skills/…
#   make evals-arm TASK=001-… LABEL=v3 SKILLS=plugin
#   make evals-compare BEFORE=bare AFTER=v3
evals-arm:
	@test -n "$(TASK)" -a -n "$(LABEL)" || (echo 'usage: make evals-arm TASK=<task> LABEL=<arm> [SKILLS=plugin] [ATTEMPTS=3]'; exit 2)
	python3 evals/agent.py $(TASK) --attempts $(or $(ATTEMPTS),3) --label $(LABEL) \
		$(if $(SKILLS),--skills $(SKILLS))

evals-compare:
	@test -n "$(BEFORE)" -a -n "$(AFTER)" || (echo 'usage: make evals-compare BEFORE=<arm> AFTER=<arm>'; exit 2)
	python3 evals/compare.py $(BEFORE) $(AFTER)

evals-down:
	docker compose -f evals/docker-compose.yml down --volumes --remove-orphans

claude:
	claude --dangerously-skip-permissions --teammate-mode tmux

codex:
	codex --sandbox danger-full-access
