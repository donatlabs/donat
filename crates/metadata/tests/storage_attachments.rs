//! Loading file attachments (spec 008): the deployment-wide `storage.yaml`
//! half, the table-local `attachments:` half, and every refusal that must stop
//! the boot rather than silently drop a file column. No database needed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use donat_metadata::{LoadError, StorageBackend, load_metadata_dir};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "donat_metadata_storage_{tag}_{}_{}",
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

const STORAGE_YAML: &str = "\
backends:
  - name: media
    kind: s3
    bucket: donat-media
    region: eu-central-1
    access_key_id: { value_from_env: DONAT_S3_KEY }
    secret_access_key: { value_from_env: DONAT_S3_SECRET }
signing:
  secret: { value_from_env: DONAT_FILE_SIGNING_SECRET }
";

const PET_TABLE: &str = "\
table:
  name: pet
  schema: public
attachments:
  - column: photo
    backend: media
    max_bytes: 5242880
    media_types: [image/png, image/jpeg]
insert_permissions:
  - role: customer
    permission:
      columns: [photo]
      check: {}
select_permissions:
  - role: customer
    permission:
      columns: \"*\"
      filter: {}
";

/// Build a metadata directory. `kind` is the source kind, `table` the single
/// tracked table's YAML, and `storage` the optional storage.yaml body.
fn build(tag: &str, kind: &str, table: &str, storage: Option<&str>) -> PathBuf {
    let dir = tempdir(tag);
    write(&dir, "version.yaml", "version: 3\n");
    write(
        &dir,
        "databases/databases.yaml",
        &format!(
            "\
- name: default
  kind: {kind}
  configuration:
    connection_info:
      database_url:
        from_env: DONAT_GRAPHQL_DATABASE_URL
  tables: \"!include default/tables/tables.yaml\"
"
        ),
    );
    write(
        &dir,
        "databases/default/tables/tables.yaml",
        "- \"!include public_pet.yaml\"\n",
    );
    write(&dir, "databases/default/tables/public_pet.yaml", table);
    if let Some(storage) = storage {
        write(&dir, "storage.yaml", storage);
    }
    dir
}

fn storage_error(result: Result<donat_metadata::Metadata, LoadError>) -> String {
    match result {
        Err(LoadError::Storage { message, .. }) => message,
        Err(other) => panic!("expected a storage error, got {other}"),
        Ok(_) => panic!("expected a storage error, but the metadata loaded"),
    }
}

#[test]
fn attachment_is_declared_on_the_table_and_resolves_to_a_backend() {
    let dir = build("ok", "postgres", PET_TABLE, Some(STORAGE_YAML));
    let metadata = load_metadata_dir(&dir).unwrap();

    let attachments: Vec<_> = metadata.attachments().collect();
    assert_eq!(attachments.len(), 1);
    let a = attachments[0];
    assert_eq!(a.source, "default");
    assert_eq!(a.key(), "public.pet.photo");
    assert_eq!(a.attachment.column, "photo");
    assert_eq!(a.attachment.max_bytes, 5_242_880);
    assert!(a.attachment.allows_media_type("image/png"));
    assert!(!a.attachment.allows_media_type("application/pdf"));

    assert!(metadata.attachment("public.pet.photo").is_some());
    assert!(metadata.attachment("public.pet.avatar").is_none());

    match metadata.storage.backend("media") {
        Some(StorageBackend::S3(s3)) => {
            assert_eq!(s3.bucket, "donat-media");
            assert_eq!(s3.access_key_id.value_from_env, "DONAT_S3_KEY");
            assert!(!s3.path_style);
            assert!(s3.endpoint.is_none());
        }
        other => panic!("expected an s3 backend, got {other:?}"),
    }
}

#[test]
fn collector_windows_default_to_one_day() {
    let dir = build("gc_default", "postgres", PET_TABLE, Some(STORAGE_YAML));
    let metadata = load_metadata_dir(&dir).unwrap();
    assert_eq!(metadata.storage.gc.every_days, 1);
    assert_eq!(metadata.storage.gc.pending_ttl_days, 1);
    assert_eq!(metadata.storage.gc.orphan_grace_days, 1);
}

#[test]
fn signing_ttls_default_and_can_be_overridden() {
    let dir = build("ttl", "postgres", PET_TABLE, Some(STORAGE_YAML));
    let signing = load_metadata_dir(&dir).unwrap().storage.signing.unwrap();
    assert_eq!(signing.upload_ttl_seconds, 900);
    assert_eq!(signing.download_ttl_seconds, 300);

    let overridden =
        format!("{STORAGE_YAML}  upload_ttl_seconds: 60\n  download_ttl_seconds: 30\n");
    let dir = build("ttl_over", "postgres", PET_TABLE, Some(&overridden));
    let signing = load_metadata_dir(&dir).unwrap().storage.signing.unwrap();
    assert_eq!(signing.upload_ttl_seconds, 60);
    assert_eq!(signing.download_ttl_seconds, 30);
}

#[test]
fn metadata_without_storage_yaml_has_no_attachments() {
    let table = "table:\n  name: pet\n  schema: public\n";
    let dir = build("absent", "postgres", table, None);
    let metadata = load_metadata_dir(&dir).unwrap();
    assert!(metadata.storage.is_empty());
    assert_eq!(metadata.attachments().count(), 0);
}

#[test]
fn unknown_backend_is_refused() {
    let table = PET_TABLE.replace("backend: media", "backend: nope");
    let dir = build("unknown_backend", "postgres", &table, Some(STORAGE_YAML));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(
        message.contains("public.pet.photo") && message.contains("nope"),
        "unexpected message: {message}"
    );
}

#[test]
fn attachments_without_any_backend_are_refused() {
    let dir = build("no_backends", "postgres", PET_TABLE, Some("backends: []\n"));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(message.contains("no backends"), "unexpected: {message}");
}

#[test]
fn a_column_declared_twice_is_refused() {
    let table = format!("{PET_TABLE}{}", "");
    let table = table.replace(
        "    media_types: [image/png, image/jpeg]\n",
        "    media_types: [image/png, image/jpeg]\n  - column: photo\n    backend: media\n    max_bytes: 10\n",
    );
    let dir = build("twice", "postgres", &table, Some(STORAGE_YAML));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(message.contains("twice"), "unexpected: {message}");
}

#[test]
fn a_wildcard_media_type_is_refused() {
    let table = PET_TABLE.replace("[image/png, image/jpeg]", "[\"image/*\"]");
    let dir = build("wildcard", "postgres", &table, Some(STORAGE_YAML));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(message.contains("wildcards"), "unexpected: {message}");
}

#[test]
fn zero_max_bytes_is_refused() {
    let table = PET_TABLE.replace("max_bytes: 5242880", "max_bytes: 0");
    let dir = build("zero_max", "postgres", &table, Some(STORAGE_YAML));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(message.contains("max_bytes"), "unexpected: {message}");
}

#[test]
fn an_attachment_without_a_signing_secret_is_refused() {
    // The store presigns upload and download URLs, but the call reporting an
    // upload finished is answered by the engine and carries no other proof.
    let storage = "\
backends:
  - name: media
    kind: s3
    bucket: donat-media
    region: eu-central-1
    access_key_id: { value_from_env: DONAT_S3_KEY }
    secret_access_key: { value_from_env: DONAT_S3_SECRET }
";
    let dir = build("no_secret", "postgres", PET_TABLE, Some(storage));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(message.contains("signing.secret"), "unexpected: {message}");
}

#[test]
fn a_non_postgres_source_is_refused() {
    let dir = build("sqlite", "sqlite", PET_TABLE, Some(STORAGE_YAML));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(message.contains("postgres source"), "unexpected: {message}");
}

#[test]
fn a_zero_gc_interval_is_refused() {
    let storage = format!("{STORAGE_YAML}gc:\n  every_days: 0\n");
    let dir = build("zero_gc", "postgres", PET_TABLE, Some(&storage));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(message.contains("every_days"), "unexpected: {message}");
}

#[test]
fn a_ttl_longer_than_a_day_is_refused() {
    let storage = format!("{STORAGE_YAML}  download_ttl_seconds: 90000\n");
    let dir = build("long_ttl", "postgres", PET_TABLE, Some(&storage));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(
        message.contains("download_ttl_seconds") && message.contains("86400"),
        "unexpected: {message}"
    );
}

#[test]
fn a_command_writing_a_file_column_is_refused() {
    // A command step never passes the claim gate, so it could point a file
    // column at an upload nobody verified. The ordinary insert/update path is
    // where a file column is filled.
    let dir = build("command_write", "postgres", PET_TABLE, Some(STORAGE_YAML));
    write(
        &dir,
        "commands.yaml",
        "\
- name: set_photo
  source: default
  arguments:
    - name: photo
      type: uuid
  steps:
    - name: write
      update:
        table: public.pet
        where:
          id: { arg: photo }
        set:
          photo: { arg: photo }
",
    );
    let message = storage_error(load_metadata_dir(&dir));
    assert!(
        message.contains("set_photo") && message.contains("public.pet.photo"),
        "unexpected: {message}"
    );
}

#[test]
fn a_public_attachment_on_s3_needs_a_public_base_url() {
    // The engine cannot know a bucket is world-readable, and inventing an
    // origin would publish links that 403.
    let table = PET_TABLE.replace(
        "    media_types: [image/png, image/jpeg]",
        "    media_types: [image/png, image/jpeg]\n    public: true",
    );
    let dir = build("public_no_base", "postgres", &table, Some(STORAGE_YAML));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(message.contains("public_base_url"), "unexpected: {message}");

    // With one declared, it loads.
    let storage = STORAGE_YAML.replace(
        "    secret_access_key: { value_from_env: DONAT_S3_SECRET }",
        "    secret_access_key: { value_from_env: DONAT_S3_SECRET }\n    public_base_url: https://cdn.example.com",
    );
    let dir = build("public_with_base", "postgres", &table, Some(&storage));
    let metadata = load_metadata_dir(&dir).expect("public attachment loads");
    assert!(
        metadata
            .attachment("public.pet.photo")
            .unwrap()
            .attachment
            .public
    );
}

#[test]
fn an_identity_variable_must_be_a_session_variable() {
    let storage = format!("{STORAGE_YAML}identity:\n  session_variable: user_id\n");
    let dir = build("identity", "postgres", PET_TABLE, Some(&storage));
    let message = storage_error(load_metadata_dir(&dir));
    assert!(
        message.contains("session variable"),
        "unexpected: {message}"
    );
}

#[test]
fn limits_and_identity_default_without_being_written() {
    let dir = build("limit_defaults", "postgres", PET_TABLE, Some(STORAGE_YAML));
    let storage = load_metadata_dir(&dir).unwrap().storage;
    assert_eq!(storage.limits.pending_uploads_per_session, 20);
    assert_eq!(storage.limits.uploads_per_minute_per_session, 60);
    assert_eq!(storage.identity.session_variable, "x-donat-user-id");
    assert!(storage.cors.is_empty());
}

#[test]
fn attachments_on_two_sources_are_refused() {
    // The upload catalog lives in one database, and the file routes hold one
    // connection to it. Rows written to one source and looked up in another
    // would simply not be found, so the binding is refused while it is still
    // readable.
    let dir = tempdir("two_sources");
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
- name: second
  kind: postgres
  configuration:
    connection_info:
      database_url:
        from_env: DONAT_SECOND_DATABASE_URL
  tables: \"!include second/tables/tables.yaml\"
",
    );
    for source in ["default", "second"] {
        write(
            &dir,
            &format!("databases/{source}/tables/tables.yaml"),
            "- \"!include public_pet.yaml\"\n",
        );
        write(
            &dir,
            &format!("databases/{source}/tables/public_pet.yaml"),
            PET_TABLE,
        );
    }
    write(&dir, "storage.yaml", STORAGE_YAML);

    let message = storage_error(load_metadata_dir(&dir));
    assert!(
        message.contains("more than one source"),
        "unexpected: {message}"
    );
}
