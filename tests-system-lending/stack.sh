#!/usr/bin/env bash
# Raise (or drop) the two lending stands the black-box suite compares.
#
#   tests-system-lending/stack.sh up     # database, the engine, the Go host
#   tests-system-lending/stack.sh down   # stop both, keep the database
#   tests-system-lending/stack.sh env    # the variables the suite reads
#   tests-system-lending/stack.sh logs   # tail both logs
#
# Two stands, one metadata directory. The standalone Rust engine and a Go
# application embedding the compiled core serve the same YAML, and the suite
# runs every case against both — a difference between them is the bug this
# suite exists to find.
#
# One exception, and only one: an action declared without a `handler` is
# resolved in-process by a function the embedding program registered, and
# `donat-server` refuses to start rather than mount a field it could never
# answer. The engine stand is therefore given a copy with those actions
# removed (see engine_metadata.py). Everything the suite actually compares —
# every command, rule, permission and table — is byte-identical.
#
# Each stand gets its own database so a test on one cannot observe the other's
# rows. The deploy model is the platform's: `donat migrate` for the engine's
# own catalog, a second `donat migrate` for the example's tables, and only then
# does anything serve. Both binaries are built from this working tree, because
# a published image would test somebody else's build.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
example="$repo/examples/lending-golang"
state="$here/.stack"
engine_metadata="$state/metadata-engine"

PG_BASE="${LENDING_PG_BASE:-postgresql://postgres:postgres@127.0.0.1:15433}"
ENGINE_PORT="${LENDING_ENGINE_PORT:-8090}"
GO_PORT="${LENDING_GO_PORT:-8091}"
DRIVER_PORT="${LENDING_DRIVER_PORT:-8092}"
ENGINE_DB="${LENDING_ENGINE_DB:-lending_engine}"
GO_DB="${LENDING_GO_DB:-lending_go}"
ADMIN_SECRET="${LENDING_ADMIN_SECRET:-lending-secret}"
# The metadata declares an attachment, so both stands need the same storage
# secrets. The Go host defaults them in main.go; the engine reads them from the
# environment, so the same values are exported for it here — a stand signing
# URLs with a different key than its twin would compare two different
# deployments.
export LENDING_S3_KEY="${LENDING_S3_KEY:-minioadmin}"
export LENDING_S3_SECRET="${LENDING_S3_SECRET:-minioadmin}"
export LENDING_SIGNING_SECRET="${LENDING_SIGNING_SECRET:-dev-signing-secret}"

mkdir -p "$state"

engine_pid="$state/engine.pid"
engine_log="$state/engine.log"
go_pid="$state/go.pid"
go_log="$state/go.log"
driver_pid="$state/driver.pid"
driver_log="$state/driver.log"

createdb() {
  local name="$1"
  # Terminate anything still connected before dropping. A stand left running
  # from a previous `up` holds sessions, and DROP DATABASE refuses while they
  # exist — which used to fail the recreate and then quietly let the suite run
  # against the *old* stands, reporting a pass for code it never loaded.
  local kill_sql="SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '$name' AND pid <> pg_backend_pid()"
  # A CI runner has psql; a developer machine often only has the database in a
  # container. Try the direct client first and fall back rather than assuming
  # either, because a stand that cannot create its database fails much later
  # and much less clearly.
  #
  # Two -c flags in each call, never one string: psql wraps a multi-statement
  # -c in a single transaction, and DROP DATABASE cannot run inside one.
  if command -v psql >/dev/null 2>&1; then
    psql "$PG_BASE/postgres" -q -v ON_ERROR_STOP=1 \
      -c "$kill_sql" \
      -c "DROP DATABASE IF EXISTS $name" \
      -c "CREATE DATABASE $name" >/dev/null
    return
  fi

  local container
  container="$(docker ps --filter "publish=${PG_BASE##*:}" --format '{{.Names}}' | head -1)"
  if [ -z "$container" ]; then
    echo "no psql on PATH and no postgres container publishing port ${PG_BASE##*:}" >&2
    exit 1
  fi
  docker exec "$container" psql -U postgres -q -v ON_ERROR_STOP=1 \
    -c "$kill_sql" \
    -c "DROP DATABASE IF EXISTS $name" \
    -c "CREATE DATABASE $name" >/dev/null
}

migrate() {
  local url="$1"
  "$repo/target/debug/donat" --database-url "$url" migrate \
    --migrations-dir "$repo/migrations" >/dev/null
  "$repo/target/debug/donat" --database-url "$url" migrate \
    --migrations-dir "$example/migrations" >/dev/null
  # The third step of the platform's deploy model: publish the Process
  # revisions. A command that starts a Process writes a journal row whose
  # foreign key names the revision, so without this the borrow fails on the
  # constraint rather than on anything a caller did.
  DONAT_GRAPHQL_DATABASE_URL="$url" "$repo/target/debug/donat" \
    --database-url "$url" migrate --migrations-dir "$repo/migrations" \
    --metadata-dir "$engine_metadata" --source default >/dev/null
}

wait_for() {
  local url="$1" name="$2"
  for _ in $(seq 1 60); do
    if curl -fsS "$url/healthz" >/dev/null 2>&1; then return 0; fi
    sleep 0.5
  done
  echo "$name did not answer at $url" >&2
  return 1
}

cmd_up() {
  # `up` is idempotent: a stand left from a previous run is stopped rather than
  # left to serve stale code beside a suite that believes it is fresh.
  cmd_down >/dev/null

  echo "==> building the engine and the Go host from this working tree"
  cargo build --manifest-path "$repo/Cargo.toml" -p donat-server --bin donat
  ( cd "$repo" && make wasm-core >/dev/null )
  ( cd "$example" && CGO_ENABLED=0 go build -o "$state/lending-golang" . )

  local engine_url="$PG_BASE/$ENGINE_DB"
  local go_url="$PG_BASE/$GO_DB"

  echo "==> creating databases"
  createdb "$ENGINE_DB"
  createdb "$GO_DB"

  echo "==> preparing the engine stand's metadata"
  # The suite's venv has PyYAML; a bare interpreter may not, so prefer it when
  # the suite has already been installed and fall back otherwise.
  local python="python3"
  [ -x "$here/.venv/bin/python" ] && python="$here/.venv/bin/python"
  "$python" "$here/engine_metadata.py" "$example/metadata" "$engine_metadata" >/dev/null

  echo "==> applying DDL (the platform's catalog, then the library's)"
  migrate "$engine_url"
  migrate "$go_url"

  echo "==> regenerating the core config the Go host embeds"
  DONAT_GRAPHQL_DATABASE_URL="$go_url" "$repo/target/debug/donat" \
    --database-url "$go_url" dump-core-config \
    --metadata-dir "$example/metadata" --out "$example/core-config.json" >/dev/null
  ( cd "$example" && CGO_ENABLED=0 go build -o "$state/lending-golang" . )

  echo "==> serving the engine on $ENGINE_PORT"
  DONAT_PORT="$ENGINE_PORT" \
  DONAT_GRAPHQL_DATABASE_URL="$engine_url" \
  DONAT_GRAPHQL_ADMIN_SECRET="$ADMIN_SECRET" \
  RUST_LOG="${RUST_LOG:-donat=info}" \
    nohup "$repo/target/debug/donat" --metadata-dir "$engine_metadata" \
      >"$engine_log" 2>&1 &
  echo $! >"$engine_pid"

  # The Go host originates durable work but does not carry it forward, so its
  # database gets an engine whose only job is the runtime loop. This is the
  # deployment shape the SDK documents, and running it here is what makes the
  # suite prove it rather than assert it.
  echo "==> driving the Go stand's Processes with an engine on $DRIVER_PORT"
  DONAT_PORT="$DRIVER_PORT" \
  DONAT_GRAPHQL_DATABASE_URL="$go_url" \
  DONAT_GRAPHQL_ADMIN_SECRET="$ADMIN_SECRET" \
  RUST_LOG="${RUST_LOG:-donat=info}" \
    nohup "$repo/target/debug/donat" --metadata-dir "$engine_metadata" \
      >"$driver_log" 2>&1 &
  echo $! >"$driver_pid"

  echo "==> serving the Go host on $GO_PORT"
  DONAT_DATABASE_URL="$go_url" DONAT_PORT="$GO_PORT" \
  DONAT_CORE_CONFIG="$example/core-config.json" \
    nohup "$state/lending-golang" >"$go_log" 2>&1 &
  echo $! >"$go_pid"

  wait_for "http://127.0.0.1:$ENGINE_PORT" engine
  wait_for "http://127.0.0.1:$DRIVER_PORT" "process driver"
  wait_for "http://127.0.0.1:$GO_PORT" "go host"
  echo "==> both stands are up"
  cmd_env
}

cmd_down() {
  for f in "$engine_pid" "$go_pid" "$driver_pid"; do
    [ -f "$f" ] || continue
    kill "$(cat "$f")" 2>/dev/null || true
    rm -f "$f"
  done
  echo "==> stands stopped"
}

cmd_env() {
  echo "export LENDING_ENGINE_URL=http://127.0.0.1:$ENGINE_PORT"
  echo "export LENDING_GO_URL=http://127.0.0.1:$GO_PORT"
  echo "export LENDING_ADMIN_SECRET=$ADMIN_SECRET"
}

cmd_logs() {
  tail -n 50 -F "$engine_log" "$go_log" "$driver_log"
}

case "${1:-}" in
  up) cmd_up ;;
  down) cmd_down ;;
  env) cmd_env ;;
  logs) cmd_logs ;;
  *)
    echo "usage: $0 {up|down|env|logs}" >&2
    exit 2
    ;;
esac
