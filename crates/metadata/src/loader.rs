//! Loader for the Donat v2 metadata *directory* format (version 3):
//!
//! ```text
//! metadata/
//! ├─ version.yaml                  # version: 3
//! └─ databases/
//!    ├─ databases.yaml             # sources; tables via `!include`
//!    └─ <source>/tables/
//!       ├─ tables.yaml             # list of `!include <table>.yaml`
//!       └─ public_author.yaml
//! ```
//!
//! `!include` paths are resolved relative to the directory of the file that
//! contains them, matching donat-cli behaviour.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_yaml::Value;

use crate::types::Metadata;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Yaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("invalid !include in {path}: expected a string path")]
    BadInclude { path: PathBuf },
    #[error("!include cycle detected at {path}")]
    IncludeCycle { path: PathBuf },
    #[error("unsupported metadata version {0} (only version 3 is supported)")]
    UnsupportedVersion(u32),
    #[error("invalid MCP metadata in {path}: {message}")]
    Mcp { path: PathBuf, message: String },
    #[error("invalid storage metadata ({path}): {message}")]
    Storage { path: PathBuf, message: String },
}

/// Load and fully resolve a metadata directory.
pub fn load_metadata_dir(dir: &Path) -> Result<Metadata, LoadError> {
    #[derive(Deserialize)]
    struct VersionFile {
        version: u32,
    }

    let version_path = dir.join("version.yaml");
    let version: VersionFile = parse_file(&version_path)?;
    if version.version != 3 {
        return Err(LoadError::UnsupportedVersion(version.version));
    }

    let databases_path = dir.join("databases").join("databases.yaml");
    let sources_value = load_yaml_resolved(&databases_path)?;
    let sources = serde_yaml::from_value(sources_value).map_err(|source| LoadError::Yaml {
        path: databases_path,
        source,
    })?;

    // Actions and their custom type system live together in `actions.yaml`,
    // which has two top-level keys: `actions:` (a list) and `custom_types:`
    // (a mapping). Both are optional. This mirrors the donat-cli export.
    let (actions, custom_types) = load_actions(dir)?;
    let mcp = load_mcp(dir)?;

    // Optional top-level sections, in the Donat v3 export layout. Each file
    // is a list (with `!include` allowed); absent files mean "none". This is
    // what lets the whole metadata surface boot from the filesystem with no
    // runtime admin/metadata API.
    let metadata = Metadata {
        version: version.version,
        sources,
        inherited_roles: load_section(dir, "inherited_roles.yaml")?,
        query_collections: load_section(dir, "query_collections.yaml")?,
        allowlist: load_section(dir, "allow_list.yaml")?,
        remote_schemas: load_section(dir, "remote_schemas.yaml")?,
        actions,
        custom_types,
        cron_triggers: load_section(dir, "cron_triggers.yaml")?,
        rest_endpoints: load_section(dir, "rest_endpoints.yaml")?,
        commands: load_section(dir, "commands.yaml")?,
        rules: load_section(dir, "rules.yaml")?,
        connectors: load_section(dir, "connectors.yaml")?,
        processes: load_section(dir, "flows.yaml")?,
        mcp,
        storage: load_section(dir, "storage.yaml")?,
    };
    validate_mcp_references(&metadata).map_err(|message| LoadError::Mcp {
        path: dir.join("mcp.yaml"),
        message,
    })?;
    validate_storage(&metadata).map_err(|message| LoadError::Storage {
        path: dir.join("storage.yaml"),
        message,
    })?;
    Ok(metadata)
}

/// Cross-check the table-local attachment declarations against the
/// deployment-wide `storage.yaml`. Everything here is refused at load time: a
/// declaration the engine cannot honour must stop the boot rather than
/// silently drop a file column.
fn validate_storage(metadata: &Metadata) -> Result<(), String> {
    use std::collections::HashMap;

    let storage = &metadata.storage;
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut needs_signing_secret = false;

    // Checked before the per-attachment loop so that a deployment that forgot
    // storage.yaml entirely is told exactly that, instead of being sent after
    // a backend name that could not have resolved either way.
    if storage.backends.is_empty() && metadata.attachments().next().is_some() {
        return Err("attachments are declared but storage.yaml has no backends".to_string());
    }

    // `donat.file_uploads` lives in the source's own database, and the file
    // routes and the collector each hold one connection to it. Attachments
    // spread across two sources would have their rows written to one database
    // and looked up in another, so the binding is refused while it is still
    // readable — the same answer the connector registry gives an ambiguous
    // source binding.
    let mut sources = metadata
        .attachments()
        .map(|a| a.source.to_string())
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    if sources.len() > 1 {
        return Err(format!(
            "attachments are declared on more than one source ({}), but the upload catalog \
             belongs to a single database",
            sources.join(", ")
        ));
    }

    for a in metadata.attachments() {
        let key = a.key();
        if seen.insert(key.clone(), ()).is_some() {
            return Err(format!("column {key} is declared as an attachment twice"));
        }

        let Some(source) = metadata.sources.iter().find(|s| s.name == a.source) else {
            return Err(format!("attachment {key}: unknown source '{}'", a.source));
        };
        // Signing, presigning, and the claim gate are all compiled as Postgres
        // SQL in this milestone. Refusing here is the honest answer; the other
        // backends would otherwise accept the declaration and never serve it.
        if !matches!(source.kind, crate::types::SourceKind::Postgres) {
            return Err(format!(
                "attachment {key}: file columns require a postgres source, but '{}' is {:?}",
                a.source, source.kind
            ));
        }

        // The store presigns the upload and the download, but the call the
        // client makes to report an upload finished is the engine's own and
        // carries no other proof.
        needs_signing_secret = true;
        match storage.backend(&a.attachment.backend) {
            // A public attachment is served from a stable URL that carries no
            // signature, so the deployment has to say where that URL is rooted.
            // On S3 there is no safe guess at all: the engine cannot know the
            // bucket is world-readable, and inventing an origin would publish
            // links that 403.
            Some(crate::types::StorageBackend::S3(s3))
                if a.attachment.public && s3.public_base_url.is_none() =>
            {
                return Err(format!(
                    "attachment {key} is public, but storage backend '{}' declares no \
                     public_base_url (the bucket's public origin or its CDN)",
                    a.attachment.backend
                ));
            }
            Some(crate::types::StorageBackend::S3(_)) => {}
            None => {
                return Err(format!(
                    "attachment {key}: unknown storage backend '{}'",
                    a.attachment.backend
                ));
            }
        }

        if a.attachment.max_bytes == 0 {
            return Err(format!(
                "attachment {key}: max_bytes must be greater than 0"
            ));
        }
        if let Some(bad) = a
            .attachment
            .media_types
            .iter()
            .find(|m| m.contains('*') || !m.contains('/'))
        {
            return Err(format!(
                "attachment {key}: '{bad}' is not an exact media type (wildcards are not accepted)"
            ));
        }
    }

    let mut names = HashSet::new();
    for backend in &storage.backends {
        if !names.insert(backend.name().to_string()) {
            return Err(format!(
                "storage backend '{}' is declared twice",
                backend.name()
            ));
        }
    }

    if needs_signing_secret && storage.signing.is_none() {
        return Err(
            "signing.secret is required whenever a table declares an attachment".to_string(),
        );
    }
    if let Some(signing) = &storage.signing {
        // A signature is verified against the day it was issued, and the
        // verifier recovers that day from the expiry. That only works while a
        // URL cannot outlive the day it was minted by more than a day.
        for (field, seconds) in [
            ("upload_ttl_seconds", signing.upload_ttl_seconds),
            ("download_ttl_seconds", signing.download_ttl_seconds),
        ] {
            if seconds == 0 || seconds > 86_400 {
                return Err(format!(
                    "signing.{field} must be between 1 and 86400 seconds, got {seconds}"
                ));
            }
        }
    }
    // A command writes tables directly, and its steps never pass the claim gate
    // that ordinary writes carry — so a command could point a file column at an
    // upload nobody verified. Until command steps can carry the gate, saying so
    // at deploy time is the only honest answer; the ordinary insert/update path
    // is where a file column is filled.
    for command in &metadata.commands {
        for step in &command.steps {
            for (table, column) in written_columns(step) {
                let key = format!("{}.{}.{column}", table.schema(), table.name());
                if seen.contains_key(&key) {
                    return Err(format!(
                        "command '{}' writes {key}, which is a declared file column. A file \
                         column is filled by an ordinary insert or update, whose claim gate \
                         proves the upload was verified and belongs to the caller",
                        command.name
                    ));
                }
            }
        }
    }

    let identity = &storage.identity.session_variable;
    if !identity.starts_with("x-donat-") && !identity.starts_with("x-hasura-") {
        return Err(format!(
            "identity.session_variable '{identity}' is not a session variable name"
        ));
    }
    if storage.gc.every_days == 0 {
        return Err("gc.every_days must be greater than 0".to_string());
    }
    Ok(())
}

/// Load the optional MCP presentation layer. Unlike the other top-level
/// sections this file is a mapping, not a list, so it gets a dedicated loader.
fn load_mcp(dir: &Path) -> Result<crate::types::McpMetadata, LoadError> {
    let path = dir.join("mcp.yaml");
    if !path.exists() {
        return Ok(Default::default());
    }
    let value = load_yaml_resolved(&path)?;
    let mut mcp: crate::types::McpMetadata = if value.is_null() {
        Default::default()
    } else {
        serde_yaml::from_value(value).map_err(|source| LoadError::Yaml {
            path: path.clone(),
            source,
        })?
    };
    // Presence switches MCP into the explicit publication mode even when the
    // mapping is empty. An operator may deliberately use an empty mapping to
    // publish no tools.
    mcp.mark_configured();
    validate_mcp(&mcp).map_err(|message| LoadError::Mcp { path, message })?;
    Ok(mcp)
}

fn validate_mcp(mcp: &crate::types::McpMetadata) -> Result<(), String> {
    if mcp.resources.schema.enabled {
        return Err("MCP schema resources are not supported".to_string());
    }
    let mut names = HashSet::new();
    for tool in &mcp.tools {
        if tool.name.is_empty() {
            return Err("tool name must not be empty".to_string());
        }
        if !names.insert(tool.name.as_str()) {
            return Err(format!("duplicate tool name '{}'", tool.name));
        }
        let source_count = usize::from(tool.source.saved_query.is_some())
            + usize::from(tool.source.action.is_some());
        if source_count != 1 {
            return Err(format!(
                "tool '{}' must declare exactly one of source.saved_query or source.action",
                tool.name
            ));
        }
        if tool.permissions.is_empty() {
            return Err(format!(
                "tool '{}' must declare at least one role",
                tool.name
            ));
        }
    }
    for table_tool in &mcp.table_tools {
        for operation in &table_tool.operations {
            if operation.name.is_empty() {
                return Err("table tool name must not be empty".to_string());
            }
            if !names.insert(operation.name.as_str()) {
                return Err(format!("duplicate tool name '{}'", operation.name));
            }
            if operation.permissions.is_empty() {
                return Err(format!(
                    "table tool '{}' must declare at least one role",
                    operation.name
                ));
            }
        }
    }
    Ok(())
}

/// Verify that the publication layer names real GraphQL entrypoints before
/// booting the server. Publishing a broken tool is worse than rejecting the
/// deployment because MCP clients discover it before they can learn it fails.
fn validate_mcp_references(metadata: &Metadata) -> Result<(), String> {
    for tool in &metadata.mcp.tools {
        if let Some(source) = &tool.source.saved_query {
            let found = metadata
                .query_collections
                .iter()
                .find(|collection| collection.name == source.collection)
                .is_some_and(|collection| {
                    collection
                        .definition
                        .queries
                        .iter()
                        .any(|query| query.name == source.query)
                });
            if !found {
                return Err(format!(
                    "tool '{}' references unknown saved query '{}.{}'",
                    tool.name, source.collection, source.query
                ));
            }
        }
        if let Some(action_name) = &tool.source.action {
            let Some(action) = metadata
                .actions
                .iter()
                .find(|action| action.name == *action_name)
            else {
                return Err(format!(
                    "tool '{}' references unknown action '{}'",
                    tool.name, action_name
                ));
            };
            if action_output_has_relationships(
                &metadata.custom_types,
                &action.definition.output_type,
                &mut HashSet::new(),
            ) {
                return Err(format!(
                    "tool '{}' references action '{}' with unsupported output relationships",
                    tool.name, action_name
                ));
            }
        }
    }
    for table_tool in &metadata.mcp.table_tools {
        let tracked = metadata
            .sources
            .iter()
            .flat_map(|source| &source.tables)
            .any(|entry| {
                entry.table.schema() == table_tool.table.schema()
                    && entry.table.name() == table_tool.table.name()
            });
        if !tracked {
            return Err(format!(
                "MCP table tool references untracked table '{}.{}'",
                table_tool.table.schema(),
                table_tool.table.name()
            ));
        }
    }
    Ok(())
}

fn action_output_has_relationships(
    custom_types: &crate::types::CustomTypes,
    type_: &str,
    ancestors: &mut HashSet<String>,
) -> bool {
    let name = type_.trim_matches(|ch| matches!(ch, '[' | ']' | '!'));
    let Some(object) = custom_types
        .objects
        .iter()
        .find(|object| object.name == name)
    else {
        return false;
    };
    if !ancestors.insert(object.name.clone()) {
        return false;
    }
    let has_relationship = !object.relationships.is_empty()
        || object
            .fields
            .iter()
            .any(|field| action_output_has_relationships(custom_types, &field.type_, ancestors));
    ancestors.remove(&object.name);
    has_relationship
}

/// Load `actions.yaml`, which carries both the action list and the custom
/// type system. Returns empties when the file is absent or blank.
fn load_actions(
    dir: &Path,
) -> Result<(Vec<crate::types::ActionEntry>, crate::types::CustomTypes), LoadError> {
    #[derive(Deserialize, Default)]
    struct ActionsFile {
        #[serde(default)]
        actions: Vec<crate::types::ActionEntry>,
        #[serde(default)]
        custom_types: crate::types::CustomTypes,
    }

    let path = dir.join("actions.yaml");
    if !path.exists() {
        return Ok(Default::default());
    }
    let value = load_yaml_resolved(&path)?;
    if value.is_null() {
        return Ok(Default::default());
    }
    let parsed: ActionsFile =
        serde_yaml::from_value(value).map_err(|source| LoadError::Yaml { path, source })?;
    Ok((parsed.actions, parsed.custom_types))
}

/// Load an optional top-level section (`!include`-resolved). Returns the
/// section's default value when the file is absent or blank.
fn load_section<T: serde::de::DeserializeOwned + Default>(
    dir: &Path,
    file: &str,
) -> Result<T, LoadError> {
    let path = dir.join(file);
    if !path.exists() {
        return Ok(T::default());
    }
    let value = load_yaml_resolved(&path)?;
    if value.is_null() {
        return Ok(T::default());
    }
    serde_yaml::from_value(value).map_err(|source| LoadError::Yaml { path, source })
}

fn parse_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, LoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml::from_str(&text).map_err(|source| LoadError::Yaml {
        path: path.to_path_buf(),
        source,
    })
}

/// Parse a YAML file and recursively splice every `!include`.
fn load_yaml_resolved(path: &Path) -> Result<Value, LoadError> {
    load_yaml_tracked(path, &mut HashSet::new())
}

/// `seen` holds the include chain currently being resolved (canonicalized
/// paths) so a file that transitively includes itself errors instead of
/// recursing until the stack overflows.
fn load_yaml_tracked(path: &Path, seen: &mut HashSet<PathBuf>) -> Result<Value, LoadError> {
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(key.clone()) {
        return Err(LoadError::IncludeCycle {
            path: path.to_path_buf(),
        });
    }
    let value: Value = parse_file(path)?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let resolved = resolve_includes(value, base, path, seen);
    seen.remove(&key);
    resolved
}

fn resolve_includes(
    value: Value,
    base: &Path,
    file: &Path,
    seen: &mut HashSet<PathBuf>,
) -> Result<Value, LoadError> {
    match value {
        // donat-cli writes includes as plain quoted strings: "!include foo.yaml"
        Value::String(s) if s.starts_with("!include ") => {
            let rel = s["!include ".len()..].trim();
            load_yaml_tracked(&base.join(rel), seen)
        }
        // ...but accept the genuine YAML-tag form too: !include foo.yaml
        Value::Tagged(tagged) if is_include_tag(&tagged.tag) => {
            let rel = tagged.value.as_str().ok_or_else(|| LoadError::BadInclude {
                path: file.to_path_buf(),
            })?;
            load_yaml_tracked(&base.join(rel), seen)
        }
        Value::Mapping(map) => {
            let mut out = serde_yaml::Mapping::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, resolve_includes(v, base, file, seen)?);
            }
            Ok(Value::Mapping(out))
        }
        Value::Sequence(seq) => seq
            .into_iter()
            .map(|v| resolve_includes(v, base, file, seen))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Sequence),
        other => Ok(other),
    }
}

fn is_include_tag(tag: &serde_yaml::value::Tag) -> bool {
    tag.to_string().trim_start_matches('!') == "include"
}

/// The (table, column) pairs one command step writes.
///
/// Only the write steps are listed: reads, decisions and projections cannot put
/// a value in a column.
fn written_columns(
    step: &crate::types::CommandStep,
) -> Vec<(&crate::types::QualifiedTable, &String)> {
    use crate::types::CommandStepOperation as Op;

    match &step.operation {
        Op::Insert { insert } => insert
            .object
            .keys()
            .map(|column| (&insert.table, column))
            .collect(),
        Op::InsertMany { insert_many } => insert_many
            .object
            .keys()
            .map(|column| (&insert_many.table, column))
            .collect(),
        Op::InsertWhen { insert_when } => insert_when
            .object
            .keys()
            .map(|column| (&insert_when.table, column))
            .collect(),
        Op::Update { update } => update
            .set
            .keys()
            .map(|column| (&update.table, column))
            .collect(),
        Op::UpdateWhen { update_when } => update_when
            .set
            .keys()
            .map(|column| (&update_when.table, column))
            .collect(),
        Op::UpdateMany { update_many } => update_many
            .set
            .keys()
            .map(|column| (&update_many.table, column))
            .collect(),
        _ => vec![],
    }
}
