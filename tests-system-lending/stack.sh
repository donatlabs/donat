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

PG_BASE="${LENDING_PG_BASE:-postgresql://postgres:postgres@127.0.0.1:15433}"
ENGINE_PORT="${LENDING_ENGINE_PORT:-8090}"
GO_PORT="${LENDING_GO_PORT:-8091}"
ENGINE_DB="${LENDING_ENGINE_DB:-lending_engine}"
GO_DB="${LENDING_GO_DB:-lending_go}"
ADMIN_SECRET="${LENDING_ADMIN_SECRET:-lending-secret}"

mkdir -p "$state"

engine_pid="$state/engine.pid"
engine_log="$state/engine.log"
go_pid="$state/go.pid"
go_log="$state/go.log"

createdb() {
  local name="$1"
  # `psql` is not assumed to be on the host: the database container has it.
  local container
  container="$(docker ps --filter "publish=${PG_BASE##*:}" --format '{{.Names}}' | head -1)"
  if [ -z "$container" ]; then
    echo "no postgres container publishing port ${PG_BASE##*:}" >&2
    exit 1
  fi
  docker exec "$container" psql -U postgres -q \
    -c "DROP DATABASE IF EXISTS $name" \
    -c "CREATE DATABASE $name" >/dev/null
}

migrate() {
  local url="$1"
  "$repo/target/debug/donat" --database-url "$url" migrate \
    --migrations-dir "$repo/migrations" >/dev/null
  "$repo/target/debug/donat" --database-url "$url" migrate \
    --migrations-dir "$example/migrations" >/dev/null
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
  echo "==> building the engine and the Go host from this working tree"
  cargo build --manifest-path "$repo/Cargo.toml" -p donat-server --bin donat
  ( cd "$repo" && make wasm-core >/dev/null )
  ( cd "$example" && CGO_ENABLED=0 go build -o "$state/lending-golang" . )

  local engine_url="$PG_BASE/$ENGINE_DB"
  local go_url="$PG_BASE/$GO_DB"

  echo "==> creating databases"
  createdb "$ENGINE_DB"
  createdb "$GO_DB"

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
    nohup "$repo/target/debug/donat" --metadata-dir "$example/metadata" \
      >"$engine_log" 2>&1 &
  echo $! >"$engine_pid"

  echo "==> serving the Go host on $GO_PORT"
  DATABASE_URL="$go_url" ADDR=":$GO_PORT" \
    nohup "$state/lending-golang" >"$go_log" 2>&1 &
  echo $! >"$go_pid"

  wait_for "http://127.0.0.1:$ENGINE_PORT" engine
  wait_for "http://127.0.0.1:$GO_PORT" "go host"
  echo "==> both stands are up"
  cmd_env
}

cmd_down() {
  for f in "$engine_pid" "$go_pid"; do
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
  tail -n 50 -F "$engine_log" "$go_log"
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
