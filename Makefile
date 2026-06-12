build:
	cargo build

test:
	cargo test

# Native conformance suite (needs Postgres: postgis/postgis:16-3.4, default
# postgresql://postgres:postgres@127.0.0.1:15432/postgres, override via
# PG_URL). Spawns its own engine instances, one database per suite.
conformance:
	cargo build -p dist-server --bin dist-api
	cargo test -p dist-conformance

run:
	cargo run --bin dist-api -- --metadata-dir crates/metadata/tests/fixtures/metadata

claude:
	claude --dangerously-skip-permissions --teammate-mode tmux
