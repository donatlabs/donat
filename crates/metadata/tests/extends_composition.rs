//! Composing one metadata directory onto another.
//!
//! This exists so that "the base is unchanged" can be structural rather than
//! aspirational: an overlay adds a platform layer on top of a business domain
//! without a copy of the domain to drift, and `git diff` against the base is
//! the proof. Refusing to override is the important half — an overlay that
//! could quietly replace a base permission would make every audit of the base
//! meaningless, because you would have to read both directories to know what
//! is served.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use donat_metadata::{LoadError, load_metadata_dir};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "donat_metadata_extends_{tag}_{}_{}",
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

/// A metadata directory tracking `tables`, with optional extra top-level files.
fn dir(root: &Path, tables: &[(&str, &str)], extra: &[(&str, &str)]) {
    write(root, "version.yaml", "version: 3\n");
    write(
        root,
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
    let index = tables
        .iter()
        .map(|(file, _)| format!("- \"!include {file}.yaml\"\n"))
        .collect::<String>();
    write(root, "databases/default/tables/tables.yaml", &index);
    for (file, body) in tables {
        write(root, &format!("databases/default/tables/{file}.yaml"), body);
    }
    for (name, body) in extra {
        write(root, name, body);
    }
}

fn table(name: &str) -> String {
    format!(
        "\
table: {{ schema: public, name: {name} }}
select_permissions:
  - role: staff
    permission:
      columns: \"*\"
      filter: {{}}
"
    )
}

fn extends_error(result: Result<donat_metadata::Metadata, LoadError>) -> String {
    match result {
        Err(LoadError::Extends { message, .. }) => message,
        Err(other) => panic!("expected an extends error, got {other}"),
        Ok(_) => panic!("expected an extends error, but the metadata loaded"),
    }
}

#[test]
fn an_overlay_adds_the_bases_tables_to_its_own() {
    let root = tempdir("compose");
    let base = root.join("base");
    let overlay = root.join("overlay");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&overlay).unwrap();

    let product = table("product");
    let tenant = table("tenant");
    dir(&base, &[("public_product", &product)], &[]);
    dir(
        &overlay,
        &[("public_tenant", &tenant)],
        &[("extends.yaml", "extends:\n  - path: ../base\n")],
    );

    let metadata = load_metadata_dir(&overlay).unwrap();
    assert_eq!(metadata.sources.len(), 1, "sources merge by name");
    let mut names: Vec<String> = metadata.sources[0]
        .tables
        .iter()
        .map(|entry| entry.table.to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["public.product", "public.tenant"]);

    // The base is untouched on disk and still loads on its own — the property
    // the whole mechanism exists for.
    let alone = load_metadata_dir(&base).unwrap();
    assert_eq!(alone.sources[0].tables.len(), 1);
}

/// The important half. An overlay that could replace a base permission would
/// make every audit of the base meaningless.
#[test]
fn a_table_declared_in_both_directories_is_refused_rather_than_overridden() {
    let root = tempdir("collide");
    let base = root.join("base");
    let overlay = root.join("overlay");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&overlay).unwrap();

    let product = table("product");
    dir(&base, &[("public_product", &product)], &[]);
    dir(
        &overlay,
        &[("public_product", &product)],
        &[("extends.yaml", "extends:\n  - path: ../base\n")],
    );

    let message = extends_error(load_metadata_dir(&overlay));
    assert!(
        message.contains("table `public.product` is tracked in both directories"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_command_declared_in_both_directories_is_refused() {
    let root = tempdir("commands");
    let base = root.join("base");
    let overlay = root.join("overlay");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&overlay).unwrap();

    let product = table("product");
    let tenant = table("tenant");
    let command = "\
- name: place_order
  source: default
  permissions:
    - role: staff
";
    dir(
        &base,
        &[("public_product", &product)],
        &[("commands.yaml", command)],
    );
    dir(
        &overlay,
        &[("public_tenant", &tenant)],
        &[
            ("extends.yaml", "extends:\n  - path: ../base\n"),
            ("commands.yaml", command),
        ],
    );

    let message = extends_error(load_metadata_dir(&overlay));
    assert!(
        message.contains("command `default.place_order` is declared in both directories"),
        "unexpected message: {message}"
    );
}

/// Two answers to one question is the thing configuration must never have.
#[test]
fn one_deployment_has_one_storage_declaration() {
    let root = tempdir("storage");
    let base = root.join("base");
    let overlay = root.join("overlay");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&overlay).unwrap();

    let storage = "\
backends:
  - name: media
    kind: s3
    bucket: donat-media
    region: eu-central-1
    access_key_id: { value_from_env: DONAT_S3_KEY }
    secret_access_key: { value_from_env: DONAT_S3_SECRET }
";
    let product = table("product");
    let tenant = table("tenant");
    dir(
        &base,
        &[("public_product", &product)],
        &[("storage.yaml", storage)],
    );
    dir(
        &overlay,
        &[("public_tenant", &tenant)],
        &[
            ("extends.yaml", "extends:\n  - path: ../base\n"),
            ("storage.yaml", storage),
        ],
    );

    let message = extends_error(load_metadata_dir(&overlay));
    assert!(
        message.contains("both directories declare storage"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_directory_that_extends_itself_is_refused_rather_than_looping() {
    let root = tempdir("cycle");
    let a = root.join("a");
    let b = root.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();

    let product = table("product");
    let tenant = table("tenant");
    dir(
        &a,
        &[("public_product", &product)],
        &[("extends.yaml", "extends:\n  - path: ../b\n")],
    );
    dir(
        &b,
        &[("public_tenant", &tenant)],
        &[("extends.yaml", "extends:\n  - path: ../a\n")],
    );

    let message = extends_error(load_metadata_dir(&a));
    assert!(
        message.contains("extends itself"),
        "unexpected message: {message}"
    );
}

/// Tenancy is declared by the platform layer and applies to the base's tables
/// too — that is the entire point of composing them.
#[test]
fn tenancy_declared_by_the_overlay_governs_the_bases_tables() {
    let root = tempdir("tenancy");
    let base = root.join("base");
    let overlay = root.join("overlay");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&overlay).unwrap();

    let product = table("product");
    let tenant = table("tenant");
    dir(&base, &[("public_product", &product)], &[]);
    dir(
        &overlay,
        &[("public_tenant", &tenant)],
        &[
            ("extends.yaml", "extends:\n  - path: ../base\n"),
            (
                "tenancy.yaml",
                "\
source: default
variable: X-Donat-Tenant-Id
key: tenant_id
registry:
  table: { schema: public, name: tenant }
  key: id
  status: { column: status, serving: [active] }
keys:
  - { table: { schema: public, name: tenant }, key: id }
",
            ),
        ],
    );

    let metadata = load_metadata_dir(&overlay).unwrap();
    let tenancy = metadata.tenancy.expect("tenancy loaded");
    // The base's table is governed although the base knows nothing about it.
    assert_eq!(
        tenancy.table_scope(&donat_metadata::QualifiedTable::Name("product".into())),
        donat_metadata::TableScope::Key("tenant_id")
    );
}

/// The declaration is validated against the *merged* table list, so an
/// exemption naming a base table resolves rather than looking untracked.
#[test]
fn the_declaration_is_checked_against_the_merged_table_list() {
    let root = tempdir("merged_validation");
    let base = root.join("base");
    let overlay = root.join("overlay");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&overlay).unwrap();

    let product = table("product");
    let tenant = table("tenant");
    dir(&base, &[("public_product", &product)], &[]);
    dir(
        &overlay,
        &[("public_tenant", &tenant)],
        &[
            ("extends.yaml", "extends:\n  - path: ../base\n"),
            (
                "tenancy.yaml",
                "\
source: default
variable: X-Donat-Tenant-Id
key: tenant_id
registry:
  table: { schema: public, name: tenant }
  key: id
  status: { column: status, serving: [active] }
keys:
  - { table: { schema: public, name: tenant }, key: id }
exempt:
  - table: { schema: public, name: product }
    shared: read_only
",
            ),
        ],
    );

    // `product` is the base's table and `staff` only reads it, so this loads.
    let metadata = load_metadata_dir(&overlay).unwrap();
    assert!(
        metadata
            .tenancy
            .expect("tenancy")
            .is_shared(&donat_metadata::QualifiedTable::Name("product".into()))
    );
}

/// Two overlays reaching one base is a diamond, not a cycle. Merging the base
/// twice would refuse the second copy for declaring everything the first
/// already had — which reads as a collision and is not one.
#[test]
fn a_base_two_overlays_share_is_composed_once() {
    let root = tempdir("diamond");
    let base = root.join("base");
    let middle = root.join("middle");
    let top = root.join("top");
    for dir in [&base, &middle, &top] {
        std::fs::create_dir_all(dir).unwrap();
    }

    let product = table("product");
    let tenant = table("tenant");
    let review = table("review");
    dir(&base, &[("public_product", &product)], &[]);
    dir(
        &middle,
        &[("public_tenant", &tenant)],
        &[("extends.yaml", "extends:\n  - path: ../base\n")],
    );
    dir(
        &top,
        &[("public_review", &review)],
        &[(
            "extends.yaml",
            "extends:\n  - path: ../middle\n  - path: ../base\n",
        )],
    );

    let metadata = load_metadata_dir(&top).unwrap();
    let mut names: Vec<String> = metadata.sources[0]
        .tables
        .iter()
        .map(|entry| entry.table.to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["public.product", "public.review", "public.tenant"],
        "the shared base was composed more than once"
    );
}

fn bounds_error(result: Result<donat_metadata::Metadata, LoadError>) -> String {
    match result {
        Err(LoadError::PermissionBounds { message, .. }) => message,
        Err(other) => panic!("expected a permission-bounds error, got {other}"),
        Ok(_) => panic!("expected a permission-bounds error, but the metadata loaded"),
    }
}

/// A table whose unbounded permission names its reason, so the base itself is
/// clean and the overlay is the only thing left to refuse.
fn declared_table(name: &str) -> String {
    format!(
        "\
table: {{ schema: public, name: {name} }}
select_permissions:
  - role: staff
    permission:
      columns: \"*\"
      filter: {{}}
      unbounded: operator
"
    )
}

/// A base that requires its unbounded permissions to declare themselves keeps
/// requiring it once something is composed on top.
///
/// This is the one section where the merge takes the stricter of two answers
/// instead of refusing the pair. Everywhere else an overlay supplying a second
/// answer is an error, because configuration must never have to choose between
/// two — but here the two answers are not peers: one is a guarantee and the
/// other is its absence, and an overlay that could drop the guarantee would let
/// a composition serve what the base refused to.
#[test]
fn an_overlay_cannot_relax_the_base_requirement() {
    let base = tempdir("bounds_base");
    dir(
        &base,
        &[("public_order", &declared_table("order"))],
        &[("permissions.yaml", "unbounded_permissions: declared\n")],
    );
    let overlay = tempdir("bounds_overlay");
    dir(
        &overlay,
        &[("public_invoice", &table("invoice"))],
        &[(
            "extends.yaml",
            &format!("extends:\n  - path: {}\n", base.display()),
        )],
    );

    // The overlay says nothing about the policy, and its own table is
    // unbounded with no reason given.
    let error = bounds_error(load_metadata_dir(&overlay));
    assert!(
        error.contains("public.invoice.select_permissions[staff]"),
        "the overlay's own table escaped the base's requirement: {error}"
    );
    assert!(error.contains("does not bound"), "{error}");
}

/// And the base's tables are still held to it, which is what makes composing
/// safe to review: reading the base tells you what the base serves.
#[test]
fn the_bases_own_tables_are_still_held_to_it() {
    let base = tempdir("bounds_base_own");
    dir(
        &base,
        &[("public_order", &table("order"))],
        &[("permissions.yaml", "unbounded_permissions: declared\n")],
    );
    let error = bounds_error(load_metadata_dir(&base));
    assert!(
        error.contains("public.order.select_permissions[staff]"),
        "{error}"
    );
}

/// A base two overlays share, where one of them is itself a base.
///
/// `A extends [C, B]` and `B extends C` is an ordinary diamond. Loading it in
/// that order composes C into A first, and the "load a base once" set then
/// makes B's own `extends: C` a no-op — so B is loaded and validated *without*
/// the directory it declares it needs. The same three directories written
/// `[B, C]` load fine, which is the tell: the answer depends on the order the
/// list happens to be in.
#[test]
fn a_base_that_is_also_a_base_is_loaded_with_its_own() {
    let c = tempdir("diamond_c");
    dir(&c, &[("public_shared", &table("shared"))], &[]);

    // B needs C for more than tables: its tenancy names C's registry, so B
    // validated without C is B refused.
    let b = tempdir("diamond_b");
    dir(
        &b,
        &[("public_middle", &table("middle"))],
        &[
            (
                "extends.yaml",
                &format!("extends:\n  - path: {}\n", c.display()),
            ),
            (
                "tenancy.yaml",
                "\
source: default
variable: X-Donat-Tenant-Id
key: tenant_id
registry:
  table: { schema: public, name: shared }
  key: id
  status: { column: status, serving: [active] }
keys:
  - { table: { schema: public, name: shared }, key: id }
",
            ),
        ],
    );

    // The order that trips it: the shared base first, the directory that also
    // needs it second.
    let a = tempdir("diamond_a");
    dir(
        &a,
        &[("public_top", &table("top"))],
        &[(
            "extends.yaml",
            &format!(
                "extends:\n  - path: {}\n  - path: {}\n",
                c.display(),
                b.display()
            ),
        )],
    );

    let loaded = load_metadata_dir(&a).expect("the diamond composes");
    let tables: Vec<String> = loaded.sources[0]
        .tables
        .iter()
        .map(|entry| entry.table.name().to_string())
        .collect();
    for wanted in ["top", "middle", "shared"] {
        assert!(
            tables.iter().any(|name| name == wanted),
            "`{wanted}` is missing from the composition: {tables:?}"
        );
    }
    assert_eq!(
        tables.iter().filter(|name| *name == "shared").count(),
        1,
        "the shared base was merged twice: {tables:?}"
    );
}
