//! The declared surface of every connector, written out for the schema audit.
//!
//! Every other test in this crate answers "does the connector do what we told
//! it to". None of them can answer "did we tell it the right thing", because
//! the local stub's expectations were written from the same reading of the
//! provider's documentation as the connector itself — a misread path or a
//! misspelled query parameter is invisible to both halves at once.
//!
//! The only material that can settle that without a credential is the
//! provider's *own published API schema*, which
//! [[037-connectors-are-written-by-hand-against-provider-documentation]] admits
//! as ground truth alongside its prose documentation. This test does the half
//! that belongs in Rust: it reads the declarations back and writes them out as
//! JSON. `scripts/check_provider_schemas.py` does the other half — fetching
//! each published schema and reporting the operations it cannot account for.
//!
//! The split is deliberate. Comparing against a schema means reaching the
//! network, and a unit test that reaches the network is a test that fails when
//! a provider's documentation site does. Dumping is hermetic and always runs;
//! the comparison is a tool run on demand.

mod declarations_support;

use std::collections::BTreeMap;

use declarations_support::executable_operations;
use serde_json::{Value as JsonValue, json};

/// Where the dump is written when `DONAT_DECLARATIONS_OUT` names a path.
///
/// Without the variable this test still runs and still asserts the dump is
/// well formed — it just keeps the result to itself, so an ordinary
/// `cargo test` neither writes into the working tree nor skips a check.
const OUT_VAR: &str = "DONAT_DECLARATIONS_OUT";

fn declarations() -> JsonValue {
    let mut modules = Vec::new();
    for (module, operations) in executable_operations() {
        let mut entries = Vec::new();
        for operation in operations {
            // The projection is the declaration's own published read-back, so
            // the audit compares the same description a Process binds against
            // rather than a second one assembled here.
            let projection = operation.project();
            let mut query: Vec<&str> = projection.query().iter().map(|entry| entry.key()).collect();
            query.sort_unstable();
            query.dedup();
            entries.push(json!({
                "id": projection.id(),
                "method": projection.method(),
                "path_template": projection.path_template(),
                "query": query,
                "effect": projection.effect_class().map(|class| format!("{class:?}")),
            }));
        }
        entries.sort_by(|left, right| {
            let key = |value: &JsonValue| {
                (
                    value["path_template"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    value["method"].as_str().unwrap_or_default().to_owned(),
                )
            };
            key(left).cmp(&key(right))
        });
        modules.push(json!({ "module": module, "operations": entries }));
    }
    modules.sort_by(|left, right| left["module"].as_str().cmp(&right["module"].as_str()));
    json!({ "modules": modules })
}

/// The dump names every executable operation exactly once, and every entry
/// carries the three things the audit compares.
///
/// An operation whose path template is empty, or whose id repeats inside its
/// own module, would silently narrow the audit — the comparison would run,
/// report nothing for that operation, and read as coverage.
#[test]
fn the_declared_surface_dumps_completely() {
    let dump = declarations();
    let modules = dump["modules"].as_array().expect("modules is an array");
    assert!(
        modules.len() >= 50,
        "the shared list lost connectors: only {} modules",
        modules.len()
    );

    let mut total = 0usize;
    for module in modules {
        let name = module["module"].as_str().expect("a module name");
        let operations = module["operations"].as_array().expect("an operation array");
        assert!(
            !operations.is_empty(),
            "`{name}` contributes no executable operation to the audit"
        );

        let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
        for operation in operations {
            let id = operation["id"].as_str().expect("an operation id");
            assert!(
                seen.insert(id, ()).is_none(),
                "`{name}.{id}` appears twice; the audit would report one and hide the other"
            );

            let path = operation["path_template"]
                .as_str()
                .expect("a path template");
            assert!(
                path.starts_with('/'),
                "`{name}.{id}` declares `{path}`, which no schema path can match"
            );
            assert!(
                !operation["method"].as_str().unwrap_or_default().is_empty(),
                "`{name}.{id}` declares no method"
            );
            total += 1;
        }
    }
    assert!(
        total >= 300,
        "the audit would cover only {total} operations; the list is truncated"
    );

    if let Some(path) = std::env::var_os(OUT_VAR) {
        let rendered = serde_json::to_string_pretty(&dump).expect("the dump serializes");
        std::fs::write(&path, rendered).expect("the dump is written");
    }
}
