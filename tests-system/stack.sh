#!/usr/bin/env bash
# Raise (or drop) a Petshop stand for the black-box system tests.
#
#   tests-system/stack.sh up      # database + providers + this branch's engine
#   tests-system/stack.sh down    # stop the engine, remove the containers
#   tests-system/stack.sh logs    # tail the engine log
#   tests-system/stack.sh env     # the variables the suite reads
#   tests-system/stack.sh provision  # top the demo warehouse back up
#   tests-system/stack.sh up-fast    # a second stand whose declared periods
#                                    # run in seconds, for the time-based
#                                    # branches (see fast_metadata.py)
#   tests-system/stack.sh down-fast
#
# The deploy model is the one the example documents: `donat migrate` applies the
# engine's own DDL and the store's, a second `migrate` deploys the durable
# Process revisions, and only then does the engine serve. The engine runs from
# this working tree — a published image would test someone else's build.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
example="$repo/examples/petshop"
state="$here/.stack"
pidfile="$state/engine.pid"
logfile="$state/engine.log"

PETSHOP_PORT="${PETSHOP_PORT:-8080}"
PETSHOP_PG_URL="${PETSHOP_PG_URL:-postgresql://postgres:postgres@127.0.0.1:15434/petshop_system}"
PETSHOP_PROVIDERS_URL="${PETSHOP_PROVIDERS_URL:-http://127.0.0.1:8099}"
PETSHOP_JWT_KEY="${PETSHOP_JWT_KEY:-petshop-dev-jwt-key-change-me-32bytes+}"
PETSHOP_ADMIN_SECRET="${PETSHOP_ADMIN_SECRET:-petshop-secret}"
PETSHOP_BASE_URL="${PETSHOP_BASE_URL:-http://127.0.0.1:${PETSHOP_PORT}}"
FAST_PORT="${PETSHOP_FAST_PORT:-8081}"
FAST_PG_URL="${PETSHOP_FAST_PG_URL:-postgresql://postgres:postgres@127.0.0.1:15434/petshop_fast}"
STAND_METADATA="$state/metadata"
FAST_METADATA="$state/metadata-fast"
FAST_PID="$state/engine-fast.pid"
FAST_LOG="$state/engine-fast.log"
PETSHOP_FAST_BASE_URL="${PETSHOP_FAST_BASE_URL:-http://127.0.0.1:${FAST_PORT}}"
PETSHOP_FAST_PROVIDERS_URL="${PETSHOP_FAST_PROVIDERS_URL:-http://127.0.0.1:8098}"

compose() { docker compose -f "$here/docker-compose.yml" "$@"; }

# A non-login shell (CI step, editor terminal) may not have rustup on PATH.
if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
  PATH="$HOME/.cargo/bin:$PATH"
fi

engine_env() {
  # Connectors resolve every endpoint and credential from the environment; an
  # unset variable fails the deploy rather than the first request that needs it.
  export DONAT_GRAPHQL_DATABASE_URL="$PETSHOP_PG_URL"
  export PETSHOP_PAYMENT_BASE_URL="$PETSHOP_PROVIDERS_URL"
  export PETSHOP_PAYMENT_API_TOKEN="petshop-payment-token"
  export PETSHOP_TAX_BASE_URL="$PETSHOP_PROVIDERS_URL"
  export PETSHOP_TAX_API_TOKEN="petshop-tax-token"
  export DONAT_MOCK_CARRIER_BASE_URL="$PETSHOP_PROVIDERS_URL"
  export DONAT_MOCK_CARRIER_TOKEN="petshop-carrier-token"
  export PETSHOP_NOTIFICATION_BASE_URL="$PETSHOP_PROVIDERS_URL"
  export PETSHOP_NOTIFICATION_API_TOKEN="petshop-notification-token"
  export PETSHOP_PAYOUT_BASE_URL="$PETSHOP_PROVIDERS_URL"
  export PETSHOP_PAYOUT_API_TOKEN="petshop-payout-token"
  # File attachments: the object store the engine presigns against, and the
  # secret those signatures are made with.
  export PETSHOP_S3_KEY="petshopminio"
  export PETSHOP_S3_SECRET="petshopminiosecret"
  export PETSHOP_FILE_SIGNING_SECRET="petshop-file-signing-secret"
  # Durable-queue tuning, forwarded when the caller sets it. Unset is the
  # engine's own answer: half the source's connection pool.
  [ -n "${DONAT_PROCESS_TRANSITION_CONCURRENCY:-}" ] &&
    export DONAT_PROCESS_TRANSITION_CONCURRENCY
  return 0
}

engine_is_up() {
  curl -fsS -m 3 -X POST "$PETSHOP_BASE_URL/v1/graphql" \
    -H 'content-type: application/json' \
    -d '{"query":"{ __typename }"}' >/dev/null 2>&1
}

# Every engine serving this stand's metadata, whatever wrote the pid file. A
# stale pid file once left an older build holding the port while a new one
# exited silently — and the suite then tested the wrong binary.
engine_pids() {
  # Anchored: the fast stand's directory starts with this one's name, and an
  # unanchored match would take both stands down together — leaving the
  # time-based suites to skip themselves on the next run.
  pgrep -f "target/debug/donat --metadata-dir $STAND_METADATA\$" 2>/dev/null || true
}

cmd_up() {
  mkdir -p "$state"
  if [ -n "$(engine_pids)" ]; then
    echo "an engine is already serving this stand (pid $(engine_pids | tr '\n' ' ')); run 'stack.sh down' first" >&2
    exit 1
  fi

  echo "==> database, object storage and mock providers"
  # The bucket initializer is a one-shot container: `--wait` reads its exit as
  # a failure, so the long-running services are waited for and it is run after.
  compose up -d --wait postgres minio mock-providers
  compose up minio-init >/dev/null 2>&1

  echo "==> building the engine from this working tree"
  cargo build --manifest-path "$repo/Cargo.toml" -p donat-server --bin donat
  binary="$repo/target/debug/donat"

  echo "==> pointing object storage at the published ports"
  python3 "$here/fast_metadata.py" "$example/metadata" "$STAND_METADATA" --rehost

  engine_env
  echo "==> applying DDL (engine schema, then the store's)"
  "$binary" migrate --migrations-dir "$repo/migrations"
  "$binary" migrate --migrations-dir "$example/migrations"

  echo "==> deploying the durable Process revisions"
  "$binary" migrate --migrations-dir "$repo/migrations" \
    --metadata-dir "$STAND_METADATA" --source default

  cmd_provision

  echo "==> serving on port $PETSHOP_PORT"
  # The suite signs its own tokens with this key; there is no admin role, so a
  # token always names the one role its request runs as.
  DONAT_PORT="$PETSHOP_PORT" \
  DONAT_METADATA_DIR="$STAND_METADATA" \
  DONAT_GRAPHQL_ADMIN_SECRET="$PETSHOP_ADMIN_SECRET" \
  DONAT_GRAPHQL_UNAUTHORIZED_ROLE="anonymous" \
  DONAT_GRAPHQL_JWT_SECRET="{\"type\":\"HS256\",\"key\":\"$PETSHOP_JWT_KEY\"}" \
  RUST_LOG="${RUST_LOG:-donat=info}" \
    nohup "$binary" --metadata-dir "$STAND_METADATA" >"$logfile" 2>&1 &
  echo $! >"$pidfile"

  for _ in $(seq 1 60); do
    if engine_is_up; then
      echo "==> up: $PETSHOP_BASE_URL (log: $logfile)"
      cmd_env
      return 0
    fi
    if ! kill -0 "$(cat "$pidfile")" 2>/dev/null; then
      echo "engine exited during start-up; last lines:" >&2
      tail -30 "$logfile" >&2
      exit 1
    fi
    sleep 1
  done
  echo "engine did not answer within 60s; last lines:" >&2
  tail -30 "$logfile" >&2
  exit 1
}

cmd_down() {
  for pid in $(engine_pids) $(cat "$pidfile" 2>/dev/null); do
    kill "$pid" 2>/dev/null || true
  done
  for _ in $(seq 1 20); do
    [ -z "$(engine_pids)" ] && break
    sleep 0.5
  done
  for pid in $(engine_pids); do
    kill -9 "$pid" 2>/dev/null || true
  done
  rm -f "$pidfile"
  # Postgres, MinIO and the provider stand-ins are shared with the fast stand.
  # Tearing them down while it is serving takes its database and its scripted
  # providers with it, and its suites then fail on a stand that looks up.
  if pgrep -f "target/debug/donat --metadata-dir $FAST_METADATA\$" >/dev/null 2>&1; then
    echo "==> down (containers left up: the fast stand is still serving)"
  else
    compose down --volumes --remove-orphans
    echo "==> down"
  fi
}

cmd_logs() { tail -n "${2:-100}" -f "$logfile"; }

# Opening stock for the demo warehouse. Stand setup, not a test action: the
# example seeds a few units, and its per-location inventory cannot be received
# through any API surface (see provision.sql).
cmd_provision() {
  echo "==> provisioning the warehouse"
  compose exec -T postgres psql -v ON_ERROR_STOP=1 -q -U postgres -d petshop_system \
    <"$here/provision.sql"
}

# A second stand serving the same store with its declared periods rewritten to
# seconds. Deadlines and dunning delays are days in the shipped metadata, so the
# branches behind them cannot be reached on the ordinary stand at all.
cmd_up_fast() {
  mkdir -p "$state"
  if [ -f "$FAST_PID" ] && kill -0 "$(cat "$FAST_PID")" 2>/dev/null; then
    echo "the fast stand is already running (pid $(cat "$FAST_PID"))" >&2
    exit 1
  fi
  echo "==> rewriting declared periods"
  python3 "$here/fast_metadata.py" "$example/metadata" "$FAST_METADATA"

  compose up -d --wait postgres minio mock-providers mock-providers-fast
  compose up minio-init >/dev/null 2>&1
  cargo build --manifest-path "$repo/Cargo.toml" -p donat-server --bin donat
  binary="$repo/target/debug/donat"

  docker compose -f "$here/docker-compose.yml" exec -T postgres \
    psql -q -U postgres -c "SELECT 1 FROM pg_database WHERE datname = 'petshop_fast'" \
    | grep -q 1 || docker compose -f "$here/docker-compose.yml" exec -T postgres \
        psql -q -U postgres -c "CREATE DATABASE petshop_fast"

  engine_env
  # Its own providers, so a scripted answer here is never claimed by the
  # ordinary stand's durable work.
  PETSHOP_PROVIDERS_URL="$PETSHOP_FAST_PROVIDERS_URL" engine_env
  export DONAT_GRAPHQL_DATABASE_URL="$FAST_PG_URL"
  "$binary" migrate --migrations-dir "$repo/migrations"
  "$binary" migrate --migrations-dir "$example/migrations"
  "$binary" migrate --migrations-dir "$repo/migrations" \
    --metadata-dir "$FAST_METADATA" --source default
  compose exec -T postgres psql -v ON_ERROR_STOP=1 -q -U postgres -d petshop_fast \
    <"$here/provision.sql"

  DONAT_PORT="$FAST_PORT" \
  DONAT_METADATA_DIR="$FAST_METADATA" \
  DONAT_GRAPHQL_ADMIN_SECRET="$PETSHOP_ADMIN_SECRET" \
  DONAT_GRAPHQL_UNAUTHORIZED_ROLE="anonymous" \
  DONAT_GRAPHQL_JWT_SECRET="{\"type\":\"HS256\",\"key\":\"$PETSHOP_JWT_KEY\"}" \
  RUST_LOG="${RUST_LOG:-donat=info}" \
    nohup "$binary" --metadata-dir "$FAST_METADATA" >"$FAST_LOG" 2>&1 &
  echo $! >"$FAST_PID"

  for _ in $(seq 1 60); do
    if curl -fsS -m 2 -X POST "$PETSHOP_FAST_BASE_URL/v1/graphql" \
         -H 'content-type: application/json' -d '{"query":"{ __typename }"}' >/dev/null 2>&1; then
      echo "==> fast stand up: $PETSHOP_FAST_BASE_URL (log: $FAST_LOG)"
      echo "export PETSHOP_FAST_BASE_URL=$PETSHOP_FAST_BASE_URL"
      echo "export PETSHOP_FAST_PROVIDERS_URL=$PETSHOP_FAST_PROVIDERS_URL"
      return 0
    fi
    kill -0 "$(cat "$FAST_PID")" 2>/dev/null || { tail -30 "$FAST_LOG" >&2; exit 1; }
    sleep 1
  done
  tail -30 "$FAST_LOG" >&2
  exit 1
}

cmd_down_fast() {
  if [ -f "$FAST_PID" ]; then
    kill "$(cat "$FAST_PID")" 2>/dev/null || true
    rm -f "$FAST_PID"
  fi
  pkill -f "target/debug/donat --metadata-dir $FAST_METADATA" 2>/dev/null || true
  # Symmetric with `down`: whichever stand goes last takes the shared
  # containers — and the databases — with it. Otherwise the order the two are
  # stopped in decides whether the next `up` starts from a clean store, and a
  # suite run against a stand carrying an earlier run's work is not a fresh run.
  if pgrep -f "target/debug/donat --metadata-dir $STAND_METADATA\$" >/dev/null 2>&1; then
    echo "==> fast stand down (containers left up: the ordinary stand is still serving)"
  else
    compose down --volumes --remove-orphans
    echo "==> fast stand down"
  fi
}

cmd_env() {
  cat <<EOF
export PETSHOP_BASE_URL=$PETSHOP_BASE_URL
export PETSHOP_PROVIDERS_URL=$PETSHOP_PROVIDERS_URL
export PETSHOP_JWT_KEY=$PETSHOP_JWT_KEY
EOF
  # The time-based suites skip themselves when the fast stand is not addressed,
  # so a stand that is up but unnamed reads as a green run that never ran them.
  if curl -fsS -m 2 -X POST "$PETSHOP_FAST_BASE_URL/v1/graphql" \
    -H 'content-type: application/json' -d '{"query":"query { __typename }"}' \
    >/dev/null 2>&1; then
    cat <<EOF
export PETSHOP_FAST_BASE_URL=$PETSHOP_FAST_BASE_URL
export PETSHOP_FAST_PROVIDERS_URL=$PETSHOP_FAST_PROVIDERS_URL
EOF
  fi
}

case "${1:-}" in
  up) cmd_up ;;
  up-fast) cmd_up_fast ;;
  down-fast) cmd_down_fast ;;
  provision) cmd_provision ;;
  down) cmd_down ;;
  logs) cmd_logs "$@" ;;
  env) cmd_env ;;
  *)
    echo "usage: $0 {up|up-fast|provision|down|down-fast|logs|env}" >&2
    exit 2
    ;;
esac
