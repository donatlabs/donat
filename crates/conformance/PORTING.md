# Porting tests-py suites to the native harness

Source of truth: `tests/donat/tests-py` (vendored, git-ignored). Target:
one Rust `#[test]` per pytest class in `crates/conformance/tests/*.rs`,
fixtures copied under `crates/conformance/fixtures/` (same relative paths).

## Scope rules (CLAUDE.md governs)

- **No admin role.** Skip tests whose fixture sends no `X-Donat-Role`
  header (or an `admin` role), and multi-step fixtures' admin-only steps if
  the whole test depends on them. Mark every exclusion with a comment:
  `// <file>: no-role (admin) request — out of scope.`
- **Status-only known-diffs** (documented in tests/donat/COVERAGE.md): we
  return 200 where 3 old insert fixtures say 400 with byte-identical
  bodies. Patch the *copied* fixture to `status: 200` and add a YAML
  comment `# donat: Donat fixtures are inconsistent here; we return 200
  everywhere (see COVERAGE.md)`. Never patch anything else.

## Mapping a pytest class

1. Find the class in tests-py; note `dir()`, decorators, fixtures, and
   method ORDER (port in the same order — some multi-step fixtures depend
   on it).
2. `@pytest.mark.parametrize("transport", ['http', 'websocket'])` on the
   class → `Transport::Both` for methods taking `transport`; methods
   calling `check_query_f` WITHOUT the transport arg run `Transport::Http`.
3. Setup/teardown by fixture kind:
   - `per_class_tests_db_state` (default `setup_metadata_api_version` v1):
     `setup.yaml` → `/v1/query` once before all cases; `teardown.yaml` →
     `/v1/query` after.
   - `setup_metadata_api_version = "v2"`: order `pre_setup.yaml` →
     `/v1/metadata`, `schema_setup.yaml` → `/v2/query`, `setup.yaml` →
     `/v1/metadata`; teardown `teardown.yaml` → `/v1/metadata`,
     `schema_teardown.yaml` → `/v2/query`, `post_teardown.yaml` →
     `/v1/metadata`. Use `apply_if_exists` — files may be absent.
   - Mutation classes (`per_class_db_schema_for_mutation_tests` +
     `per_method_db_data_for_mutation_tests`): `schema_setup.yaml` once,
     then PER TEST: `values_setup.yaml` → run case → `values_teardown.yaml`
     (all `/v1/query` for the default backend); `schema_teardown.yaml` at
     the end.
4. `@pytest.mark.admin_secret` → `Suite::new(..).admin_secret("...")`.
   `@pytest.mark.hge_env('K', 'v')` → `.env("K", "v")`.
5. Engine flags some classes pass via hge-bin (e.g.
   `--stringify-numeric-types`) → `.arg(...)`.

## Module skeleton

```rust
use donat_conformance::{Suite, Transport};

#[test]
fn pytest_class_name_snake() {
    let s = Suite::new("unique_db_safe_name").start();
    s.setup_v1q("queries/<dir>/setup.yaml");
    s.check_query_f("queries/<dir>/case.yaml", Transport::Both);
    // ...same order as the pytest class...
    s.teardown_v1q("queries/<dir>/teardown.yaml");
}
```

Suite names must be unique across ALL modules (they become database names
`conf_<name>`; keep them short, snake_case).

## Workflow

1. Copy the fixture dir: `cp -R tests/donat/tests-py/queries/<dir>
   crates/conformance/fixtures/queries/<dir>` (create parents). Copy ONLY
   dirs the ported class needs, but copy them whole.
2. Write the module, run
   `cargo test -p donat-conformance --test <module>` until green.
3. A mismatch means EITHER a porting mistake (wrong setup endpoint, wrong
   order, missed exclusion) OR a real engine bug previously masked by
   shared-database state — pytest ran suites against one long-lived
   database, the native harness gives every suite a fresh one. Diagnose
   before patching anything; engine fixes go through the normal rules
   (never weaken a fixture to make it pass).

Postgres must be reachable (default
`postgresql://postgres:postgres@127.0.0.1:15432/postgres`, override via
`PG_URL`). Engine binary: `target/debug/donat` (auto-built).
