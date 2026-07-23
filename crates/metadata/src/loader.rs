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
        mcp,
    };
    validate_mcp_references(&metadata).map_err(|message| LoadError::Mcp {
        path: dir.join("mcp.yaml"),
        message,
    })?;
    Ok(metadata)
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
            return Err(format!("tool '{}' must declare at least one role", tool.name));
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
            let Some(action) = metadata.actions.iter().find(|action| action.name == *action_name)
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
        let tracked = metadata.sources.iter().flat_map(|source| &source.tables).any(|entry| {
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
    let Some(object) = custom_types.objects.iter().find(|object| object.name == name) else {
        return false;
    };
    if !ancestors.insert(object.name.clone()) {
        return false;
    }
    let has_relationship = !object.relationships.is_empty()
        || object.fields.iter().any(|field| {
            action_output_has_relationships(custom_types, &field.type_, ancestors)
        });
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

/// Load an optional top-level list section (`!include`-resolved). Returns an
/// empty vec when the file is absent or blank.
fn load_section<T: serde::de::DeserializeOwned>(
    dir: &Path,
    file: &str,
) -> Result<Vec<T>, LoadError> {
    let path = dir.join(file);
    if !path.exists() {
        return Ok(vec![]);
    }
    let value = load_yaml_resolved(&path)?;
    if value.is_null() {
        return Ok(vec![]);
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
