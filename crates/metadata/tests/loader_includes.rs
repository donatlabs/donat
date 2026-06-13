//! Unit tests for the metadata directory loader: `!include` resolution
//! (real YAML tag and the quoted-string spelling hasura-cli writes),
//! nesting, relative-path semantics, and error cases. No database needed:
//! each test builds a metadata directory in a unique temp dir.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use dist_metadata::{LoadError, load_metadata_dir};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Unique scratch directory per test (std::env::temp_dir + pid + counter).
fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dist_metadata_loader_{tag}_{}_{}",
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

const VERSION_3: &str = "version: 3\n";

const AUTHOR_TABLE: &str = "\
table:
  name: author
  schema: public
select_permissions:
  - role: user
    permission:
      columns: \"*\"
      filter: {}
";

fn databases_yaml(tables_value: &str) -> String {
    format!(
        "\
- name: default
  kind: postgres
  configuration:
    connection_info:
      database_url:
        from_env: HASURA_GRAPHQL_DATABASE_URL
  tables: {tables_value}
"
    )
}

#[test]
fn include_as_quoted_string_hasura_cli_quirk() {
    // hasura-cli writes includes as plain strings: tables: "!include x.yaml"
    let dir = tempdir("string_include");
    write(&dir, "version.yaml", VERSION_3);
    write(
        &dir,
        "databases/databases.yaml",
        &databases_yaml("\"!include default/tables/tables.yaml\""),
    );
    write(
        &dir,
        "databases/default/tables/tables.yaml",
        "- \"!include public_author.yaml\"\n",
    );
    write(&dir, "databases/default/tables/public_author.yaml", AUTHOR_TABLE);

    let md = load_metadata_dir(&dir).expect("metadata should load");
    assert_eq!(md.version, 3);
    assert_eq!(md.sources.len(), 1);
    assert_eq!(md.sources[0].tables.len(), 1);
    assert_eq!(md.sources[0].tables[0].table.to_string(), "public.author");
}

#[test]
fn include_as_real_yaml_tag() {
    // The genuine YAML-tag form: tables: !include x.yaml
    let dir = tempdir("tag_include");
    write(&dir, "version.yaml", VERSION_3);
    write(
        &dir,
        "databases/databases.yaml",
        &databases_yaml("!include default/tables/tables.yaml"),
    );
    write(
        &dir,
        "databases/default/tables/tables.yaml",
        "- !include public_author.yaml\n",
    );
    write(&dir, "databases/default/tables/public_author.yaml", AUTHOR_TABLE);

    let md = load_metadata_dir(&dir).expect("metadata should load");
    assert_eq!(md.sources[0].tables.len(), 1);
    assert_eq!(md.sources[0].tables[0].table.to_string(), "public.author");
    assert_eq!(
        md.sources[0].tables[0].select_permissions[0].role,
        "user"
    );
}

#[test]
fn nested_includes_resolve_relative_to_each_including_file() {
    // databases.yaml -> tables/tables.yaml -> sub/author.yaml: each hop's
    // path is relative to the file that contains the include, not the root.
    let dir = tempdir("nested");
    write(&dir, "version.yaml", VERSION_3);
    write(
        &dir,
        "databases/databases.yaml",
        &databases_yaml("\"!include default/tables/tables.yaml\""),
    );
    write(
        &dir,
        "databases/default/tables/tables.yaml",
        "- \"!include sub/public_author.yaml\"\n",
    );
    write(
        &dir,
        "databases/default/tables/sub/public_author.yaml",
        AUTHOR_TABLE,
    );

    let md = load_metadata_dir(&dir).expect("metadata should load");
    assert_eq!(md.sources[0].tables.len(), 1);
    assert_eq!(md.sources[0].tables[0].table.to_string(), "public.author");
}

#[test]
fn include_string_with_extra_whitespace_is_trimmed() {
    let dir = tempdir("trim");
    write(&dir, "version.yaml", VERSION_3);
    write(
        &dir,
        "databases/databases.yaml",
        &databases_yaml("\"!include   default/tables/tables.yaml\""),
    );
    write(
        &dir,
        "databases/default/tables/tables.yaml",
        "- \"!include public_author.yaml\"\n",
    );
    write(&dir, "databases/default/tables/public_author.yaml", AUTHOR_TABLE);

    let md = load_metadata_dir(&dir).expect("metadata should load");
    assert_eq!(md.sources[0].tables.len(), 1);
}

#[test]
fn missing_include_target_is_io_error_with_path() {
    let dir = tempdir("missing");
    write(&dir, "version.yaml", VERSION_3);
    write(
        &dir,
        "databases/databases.yaml",
        &databases_yaml("\"!include default/tables/tables.yaml\""),
    );
    // default/tables/tables.yaml intentionally absent.

    let err = load_metadata_dir(&dir).expect_err("must fail on missing include");
    match err {
        LoadError::Io { path, .. } => {
            assert!(
                path.ends_with("databases/default/tables/tables.yaml"),
                "unexpected error path: {}",
                path.display()
            );
        }
        other => panic!("expected Io error, got {other:?}"),
    }
}

#[test]
fn include_tag_with_non_string_value_is_bad_include() {
    let dir = tempdir("bad_tag");
    write(&dir, "version.yaml", VERSION_3);
    write(
        &dir,
        "databases/databases.yaml",
        &databases_yaml("!include [not, a, string]"),
    );

    let err = load_metadata_dir(&dir).expect_err("must reject non-string !include");
    match err {
        LoadError::BadInclude { path } => {
            assert!(path.ends_with("databases/databases.yaml"));
        }
        other => panic!("expected BadInclude, got {other:?}"),
    }
}

#[test]
fn unsupported_metadata_version_is_rejected() {
    let dir = tempdir("version");
    write(&dir, "version.yaml", "version: 2\n");

    let err = load_metadata_dir(&dir).expect_err("version 2 must be rejected");
    match err {
        LoadError::UnsupportedVersion(v) => assert_eq!(v, 2),
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
    assert_eq!(
        err.to_string(),
        "unsupported metadata version 2 (only version 3 is supported)"
    );
}

#[test]
fn missing_version_file_is_io_error() {
    let dir = tempdir("no_version");
    let err = load_metadata_dir(&dir).expect_err("empty dir must fail");
    match err {
        LoadError::Io { path, .. } => assert!(path.ends_with("version.yaml")),
        other => panic!("expected Io error, got {other:?}"),
    }
}

// --- Top-level sections boot from the filesystem (no admin API needed) ---

/// Minimal valid metadata dir (version + one source/table) plus whatever
/// extra top-level files the caller writes.
fn base_dir(tag: &str) -> PathBuf {
    let dir = tempdir(tag);
    write(&dir, "version.yaml", VERSION_3);
    write(
        &dir,
        "databases/databases.yaml",
        &databases_yaml("\"!include default/tables/tables.yaml\""),
    );
    write(
        &dir,
        "databases/default/tables/tables.yaml",
        "- \"!include public_author.yaml\"\n",
    );
    write(&dir, "databases/default/tables/public_author.yaml", AUTHOR_TABLE);
    dir
}

#[test]
fn top_level_sections_absent_default_to_empty() {
    let dir = base_dir("sections_absent");
    let md = load_metadata_dir(&dir).expect("metadata should load");
    assert!(md.inherited_roles.is_empty());
    assert!(md.query_collections.is_empty());
    assert!(md.allowlist.is_empty());
    assert!(md.remote_schemas.is_empty());
}

#[test]
fn inherited_roles_load_from_filesystem() {
    let dir = base_dir("inherited");
    write(
        &dir,
        "inherited_roles.yaml",
        "- role_name: manager\n  role_set: [user, auditor]\n",
    );
    let md = load_metadata_dir(&dir).expect("metadata should load");
    assert_eq!(md.inherited_roles.len(), 1);
    assert_eq!(md.inherited_roles[0].role_name, "manager");
    assert_eq!(md.inherited_roles[0].role_set, vec!["user", "auditor"]);
}

#[test]
fn query_collections_and_allow_list_load_from_filesystem() {
    let dir = base_dir("collections");
    write(
        &dir,
        "query_collections.yaml",
        "\
- name: ops
  definition:
    queries:
      - name: q1
        query: \"query { author { id } }\"
",
    );
    // Hasura's filename is allow_list.yaml; it maps to Metadata.allowlist.
    write(&dir, "allow_list.yaml", "- collection: ops\n");
    let md = load_metadata_dir(&dir).expect("metadata should load");
    assert_eq!(md.query_collections.len(), 1);
    assert_eq!(md.query_collections[0].name, "ops");
    assert_eq!(md.query_collections[0].definition.queries.len(), 1);
    assert_eq!(md.allowlist.len(), 1);
    assert_eq!(md.allowlist[0].collection, "ops");
}

#[test]
fn remote_schemas_load_from_filesystem_with_include() {
    let dir = base_dir("remotes");
    // The list itself may be an !include, like hasura-cli emits.
    write(&dir, "remote_schemas.yaml", "\"!include remote_schemas/schemas.yaml\"\n");
    write(
        &dir,
        "remote_schemas/schemas.yaml",
        "\
- name: countries
  definition:
    url: http://countries.example/graphql
    forward_client_headers: true
",
    );
    let md = load_metadata_dir(&dir).expect("metadata should load");
    assert_eq!(md.remote_schemas.len(), 1);
    assert_eq!(md.remote_schemas[0].name, "countries");
    assert_eq!(
        md.remote_schemas[0].definition.url.as_deref(),
        Some("http://countries.example/graphql")
    );
}

#[test]
fn blank_section_file_is_treated_as_empty() {
    let dir = base_dir("blank_section");
    write(&dir, "inherited_roles.yaml", "");
    let md = load_metadata_dir(&dir).expect("metadata should load");
    assert!(md.inherited_roles.is_empty());
}

#[test]
fn include_cycle_is_detected_not_overflow() {
    // a.yaml includes b.yaml includes a.yaml -> cycle error, not stack overflow.
    let dir = base_dir("cycle");
    write(&dir, "query_collections.yaml", "\"!include a.yaml\"\n");
    write(&dir, "a.yaml", "\"!include b.yaml\"\n");
    write(&dir, "b.yaml", "\"!include a.yaml\"\n");
    let err = load_metadata_dir(&dir).expect_err("cycle must error");
    assert!(
        matches!(err, LoadError::IncludeCycle { .. }),
        "expected IncludeCycle, got {err:?}"
    );
}
