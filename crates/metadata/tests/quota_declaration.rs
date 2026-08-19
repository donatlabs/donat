//! Plan entitlements, and the two ways a ceiling stops being one.
//!
//! A limit is only a limit if every path that creates a row moves the counter,
//! and only if the tenant cannot change the numbers the limit is read from.
//! Both are decided here, before a deployment starts.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use donat_metadata::{LoadError, load_metadata_dir};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "donat_metadata_quota_{tag}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

const TENANCY: &str = "\
source: default
variable: X-Donat-Tenant-Id
key: tenant_id
registry:
  table: { schema: public, name: store }
  key: id
  status: { column: status, serving: [active] }
keys:
  - { table: { schema: public, name: store }, key: id }
exempt:
  - table: { schema: public, name: plan }
    shared: read_only
";

const QUOTAS: &str = "\
source: default
counters:
  table: { schema: public, name: usage }
  tenant: { column: tenant_id }
limits:
  table: { schema: public, name: plan }
  key: { column: code }
  via: { table: { schema: public, name: store }, column: plan_code }
entitlements:
  - name: products
    counter: product_count
    maximum: max_products
    consumes:
      - table: { schema: public, name: product }
";

/// `product` is the counted table; the rest are the machinery the ceiling is
/// read from. `extra` appends to the product table's declaration.
fn build(tag: &str, product_extra: &str, quotas: &str) -> PathBuf {
    let dir = tempdir(tag);
    write(&dir, "version.yaml", "version: 3\n");
    write(
        &dir,
        "databases/databases.yaml",
        "\
- name: default
  kind: postgres
  configuration:
    connection_info:
      database_url:
        from_env: DONAT_GRAPHQL_DATABASE_URL
  tables: \"!include default/tables/tables.yaml\"
",
    );
    write(
        &dir,
        "databases/default/tables/tables.yaml",
        "- \"!include public_store.yaml\"\n\
         - \"!include public_plan.yaml\"\n\
         - \"!include public_usage.yaml\"\n\
         - \"!include public_product.yaml\"\n",
    );
    for (file, name) in [
        ("public_store", "store"),
        ("public_plan", "plan"),
        ("public_usage", "usage"),
    ] {
        write(
            &dir,
            &format!("databases/default/tables/{file}.yaml"),
            &format!(
                "table: {{ schema: public, name: {name} }}\n\
                 select_permissions:\n\
                 \x20 - role: staff\n\
                 \x20   permission:\n\
                 \x20     columns: \"*\"\n\
                 \x20     filter: {{}}\n"
            ),
        );
    }
    write(
        &dir,
        "databases/default/tables/public_product.yaml",
        &format!(
            "table: {{ schema: public, name: product }}\n\
             select_permissions:\n\
             \x20 - role: staff\n\
             \x20   permission:\n\
             \x20     columns: \"*\"\n\
             \x20     filter: {{}}\n{product_extra}"
        ),
    );
    write(&dir, "tenancy.yaml", TENANCY);
    write(&dir, "quotas.yaml", quotas);
    dir
}

fn quota_error(result: Result<donat_metadata::Metadata, LoadError>) -> String {
    match result {
        Err(LoadError::Quotas { message, .. }) => message,
        Err(other) => panic!("expected a quota error, got {other}"),
        Ok(_) => panic!("expected a quota error, but the metadata loaded"),
    }
}

#[test]
fn a_ceiling_over_ordinary_writes_loads() {
    let dir = build(
        "ok",
        "insert_permissions:\n\
         \x20 - role: staff\n\
         \x20   permission:\n\
         \x20     columns: \"*\"\n\
         \x20     check: {}\n",
        QUOTAS,
    );
    let metadata = load_metadata_dir(&dir).unwrap();
    let quotas = metadata.quotas.expect("quotas loaded");
    assert_eq!(quotas.entitlements[0].name, "products");
}

/// The hole this rule exists for: a command's steps carry no counter, so a row
/// written through one crosses the ceiling without moving it — and a tenant
/// that notices picks that path every time.
#[test]
fn a_counted_table_may_not_be_written_by_a_command() {
    let dir = build(
        "command_writer",
        "command_insert_permissions:\n\
         \x20 - role: staff\n\
         \x20   permission:\n\
         \x20     columns: \"*\"\n\
         \x20     check: {}\n",
        QUOTAS,
    );
    let message = quota_error(load_metadata_dir(&dir));
    assert!(
        message.contains("carry no counter")
            && message.contains("cross the ceiling without moving it"),
        "unexpected message: {message}"
    );
}

/// A ceiling a tenant can move is not a ceiling.
#[test]
fn the_tables_a_ceiling_is_read_from_are_not_a_tenants_to_write() {
    for (tag, table) in [("usage", "public_usage"), ("plan", "public_plan")] {
        let dir = build(&format!("writable_{tag}"), "", QUOTAS);
        let path = dir.join(format!("databases/default/tables/{table}.yaml"));
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(
            "update_permissions:\n\
             \x20 - role: staff\n\
             \x20   permission:\n\
             \x20     columns: \"*\"\n\
             \x20     filter: {}\n\
             \x20     check: {}\n",
        );
        std::fs::write(&path, body).unwrap();

        let result = load_metadata_dir(&dir);
        let message = match result {
            Err(LoadError::Quotas { message, .. }) => message,
            Err(LoadError::Tenancy { message, .. }) => message,
            other => panic!("expected a refusal for {tag}, got {other:?}"),
        };
        assert!(
            message.contains("lift its own limit") || message.contains("side channel"),
            "unexpected message for {tag}: {message}"
        );
    }
}

/// A ceiling nothing consumes is fiction, and fiction in a limits table is how
/// a plan quietly stops meaning anything.
#[test]
fn an_entitlement_that_nothing_consumes_is_refused() {
    let quotas = QUOTAS.replace(
        "    consumes:\n      - table: { schema: public, name: product }\n",
        "",
    );
    let dir = build("no_consumer", "", &quotas);
    let message = quota_error(load_metadata_dir(&dir));
    assert!(
        message.contains("nothing consumes it"),
        "unexpected message: {message}"
    );
}
