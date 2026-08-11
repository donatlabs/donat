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
    #[error("invalid cron trigger metadata ({path}): {message}")]
    CronTriggers { path: PathBuf, message: String },
    #[error("invalid document template metadata ({path}): {message}")]
    Templates { path: PathBuf, message: String },
    #[error("invalid media metadata ({path}): {message}")]
    Media { path: PathBuf, message: String },
    #[error("invalid ingest metadata ({path}): {message}")]
    Ingest { path: PathBuf, message: String },
    #[error("invalid recurrence metadata ({path}): {message}")]
    Recurrence { path: PathBuf, message: String },
    #[error("invalid process metadata ({path}): {message}")]
    Processes { path: PathBuf, message: String },
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
    let mut metadata = Metadata {
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
        templates: load_templates(dir)?,
        media: load_section(dir, "media.yaml")?,
        ingest_schemas: load_ingest_schemas(dir)?,
        recurrence: load_section(dir, "recurrence.yaml")?,
    };
    // A pin is derived, so a declared one is refused here rather than
    // overwritten by the stamp below — before anything else reads the process.
    if let Some(message) = declared_template_pin(&metadata) {
        return Err(LoadError::Processes {
            path: dir.join("flows.yaml"),
            message,
        });
    }
    // Templates are read before anything validates them, and the pin is
    // stamped before anything hashes a process: after this point the loaded
    // metadata carries the exact material a definition revision is taken over.
    resolve_html_paths(&mut metadata);
    stamp_template_pins(&mut metadata);
    stamp_ingest_pins(&mut metadata);
    let template_errors = crate::documents::validate_document_templates(&metadata);
    if !template_errors.is_empty() {
        return Err(LoadError::Templates {
            path: dir.join("documents.yaml"),
            message: template_errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        });
    }
    let ingest_errors = crate::ingest::validate_ingest_schemas(&metadata);
    if !ingest_errors.is_empty() {
        return Err(LoadError::Ingest {
            path: dir.join("ingest.yaml"),
            message: ingest_errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        });
    }
    let media_errors = crate::media::validate_media_declarations(&metadata);
    if !media_errors.is_empty() {
        return Err(LoadError::Media {
            path: dir.join("media.yaml"),
            message: media_errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        });
    }
    let recurrence_errors = crate::recurrence::validate_recurrence_declarations(&metadata);
    if !recurrence_errors.is_empty() {
        return Err(LoadError::Recurrence {
            path: dir.join("recurrence.yaml"),
            message: recurrence_errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        });
    }
    validate_mcp_references(&metadata).map_err(|message| LoadError::Mcp {
        path: dir.join("mcp.yaml"),
        message,
    })?;
    validate_storage(&metadata).map_err(|message| LoadError::Storage {
        path: dir.join("storage.yaml"),
        message,
    })?;
    validate_cron_triggers(&metadata).map_err(|message| LoadError::CronTriggers {
        path: dir.join("cron_triggers.yaml"),
        message,
    })?;
    Ok(metadata)
}

/// Check the timezone half of a cron declaration. A schedule that names a zone
/// the engine cannot resolve, or names one without saying what it does at a
/// DST transition, would otherwise boot and then fire at a time nobody chose —
/// so it stops the boot instead (ADR-034: a declaration the runtime ignores is
/// a defect).
///
/// The cron expression itself is not parsed here: `croner` lives in the
/// serving binary, and this crate stays free of it.
fn validate_cron_triggers(metadata: &Metadata) -> Result<(), String> {
    for trigger in &metadata.cron_triggers {
        match (&trigger.timezone, &trigger.dst) {
            (Some(zone), Some(_)) => {
                if zone.parse::<chrono_tz::Tz>().is_err() {
                    return Err(format!(
                        "cron trigger '{}': '{zone}' is not an IANA timezone name \
                         (for example Europe/Berlin, UTC)",
                        trigger.name
                    ));
                }
            }
            (Some(zone), None) => {
                return Err(format!(
                    "cron trigger '{}' is declared in timezone '{zone}' but has no `dst` \
                     policies; a wall-clock schedule must say what it does at the local \
                     time a DST transition skips and at the one it repeats",
                    trigger.name
                ));
            }
            (None, Some(_)) => {
                return Err(format!(
                    "cron trigger '{}' declares `dst` policies but no `timezone`; a UTC \
                     schedule has no DST transitions, so the policies would never be read",
                    trigger.name
                ));
            }
            (None, None) => {}
        }
    }
    Ok(())
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

/// Load `documents.yaml` and freeze every template it declares.
///
/// "Freeze" is the whole of spec 019 §3's file-resolution rule: the source and
/// every declared include are read here, keyed by a virtual path rooted at the
/// source's own directory, and nothing reads the filesystem again. A renderer
/// therefore cannot be made to open a file, because by the time it runs there
/// is no path left to open — only a map.
fn load_templates(dir: &Path) -> Result<Vec<crate::documents::DocumentTemplate>, LoadError> {
    #[derive(Deserialize, Default)]
    struct DocumentsFile {
        #[serde(default)]
        templates: Vec<crate::documents::DocumentTemplate>,
    }

    let path = dir.join("documents.yaml");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let value = load_yaml_resolved(&path)?;
    if value.is_null() {
        return Ok(Vec::new());
    }
    let parsed: DocumentsFile =
        serde_yaml::from_value(value).map_err(|source| LoadError::Yaml {
            path: path.clone(),
            source,
        })?;

    let mut templates = parsed.templates;
    for template in &mut templates {
        freeze(dir, template).map_err(|message| LoadError::Templates {
            path: path.clone(),
            message,
        })?;
    }
    Ok(templates)
}

/// Read one template's file set and record its hash.
fn freeze(dir: &Path, template: &mut crate::documents::DocumentTemplate) -> Result<(), String> {
    let source = safe_relative(dir, &template.source).ok_or_else(|| {
        format!(
            "template `{}`: `{}` escapes the metadata directory",
            template.name, template.source
        )
    })?;
    let root = source
        .parent()
        .ok_or_else(|| {
            format!(
                "template `{}`: `{}` has no directory",
                template.name, template.source
            )
        })?
        .to_path_buf();

    let mut files = std::collections::BTreeMap::new();
    let entry = virtual_path(&root, &source).ok_or_else(|| {
        format!(
            "template `{}`: `{}` is not inside its own directory",
            template.name, template.source
        )
    })?;
    files.insert(entry.clone(), read_template_file(&source, &template.name)?);

    for include in &template.includes {
        let included = safe_relative(dir, include).ok_or_else(|| {
            format!(
                "template `{}`: include `{include}` escapes the metadata directory",
                template.name
            )
        })?;
        // The template's own directory is the whole of its world. An include
        // that reaches outside it is refused here, while the declaration is
        // still readable, rather than becoming a render-time file error.
        let key = virtual_path(&root, &included).ok_or_else(|| {
            format!(
                "template `{}`: include `{include}` is outside the template's directory",
                template.name
            )
        })?;
        if files
            .insert(key.clone(), read_template_file(&included, &template.name)?)
            .is_some()
        {
            return Err(format!(
                "template `{}`: `{key}` is included twice",
                template.name
            ));
        }
    }

    // A spreadsheet or calendar layout is YAML in the metadata directory and
    // JSON by the time the renderer sees it: the renderer parses its own
    // layout, and this workspace keeps exactly one YAML reader.
    if template.kind.is_layout() {
        for text in files.values_mut() {
            let value: serde_yaml::Value = serde_yaml::from_str(text)
                .map_err(|error| format!("template `{}`: {error}", template.name))?;
            let json = serde_json::to_value(&value)
                .map_err(|error| format!("template `{}`: {error}", template.name))?;
            *text = serde_json::to_string(&json)
                .map_err(|error| format!("template `{}`: {error}", template.name))?;
        }
    }

    template.entry = entry;
    template.files = files;
    // Last, and over the whole template: the hash pins the declaration that
    // says how the file set is executed as well as the set itself.
    template.content_hash = crate::documents::content_hash(template);
    Ok(())
}

/// Resolve each template's HTML input paths against the deployment's type
/// system, once the whole of it is loaded.
///
/// This is spec 019 §4's "a field carrying HTML must be declared as such",
/// answered here rather than in the renderer: what comes out is a set of dotted
/// paths, and the renderer escapes everything that is not in it.
fn resolve_html_paths(metadata: &mut Metadata) {
    let types = metadata.custom_types.clone();
    for template in &mut metadata.templates {
        template.html_paths = template
            .inputs
            .iter()
            .flat_map(|(name, declared)| crate::documents::html_paths(&types, name, declared))
            .collect();
    }
}

fn read_template_file(path: &Path, template: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| {
        format!(
            "template `{template}`: cannot read {}: {error}",
            path.display()
        )
    })
}

/// Join a declared, metadata-relative path without letting it leave the
/// directory it is relative to.
fn safe_relative(base: &Path, declared: &str) -> Option<PathBuf> {
    let candidate = Path::new(declared);
    if candidate.is_absolute() {
        return None;
    }
    let mut resolved = base.to_path_buf();
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(part) => resolved.push(part),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(resolved)
}

/// The path a file is known by inside the template's frozen set: `/` plus its
/// path relative to the template's own directory.
fn virtual_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut out = String::new();
    for component in relative.components() {
        let std::path::Component::Normal(part) = component else {
            return None;
        };
        out.push('/');
        out.push_str(part.to_str()?);
    }
    (!out.is_empty()).then_some(out)
}

/// The first process state that declares a `template_pin`, described.
///
/// A pin is derived from the template the activity selects, so a written one is
/// a defect: it claims a revision the deployment did not earn. The field
/// deserializes — it has to, because the engine reads its own persisted
/// definitions back through these very types — so this walk, not
/// `deny_unknown_fields`, is what keeps a pin out of a deployment's hands.
fn declared_template_pin(metadata: &Metadata) -> Option<String> {
    use crate::types::{ProcessForEachState, ProcessStateOperation};

    for process in &metadata.processes {
        for state in &process.states {
            let declared = match &state.operation {
                ProcessStateOperation::Request { request } => request.template_pin.is_some(),
                ProcessStateOperation::ForEach { for_each } => {
                    matches!(
                        for_each.as_ref(),
                        ProcessForEachState::Request { request, .. } if request.template_pin.is_some()
                    )
                }
                _ => false,
            };
            if declared {
                return Some(format!(
                    "process `{}.{}` state `{}` declares `template_pin`, which is derived from the template it selects and cannot be written",
                    process.source, process.name, state.id
                ));
            }
        }
    }
    None
}

/// Stamp `<template>@<hash>` onto every `local.document` activity.
///
/// The stamp is derived from the declaration the activity already made, so it
/// adds no configuration; what it adds is that the process's serialized
/// definition — and therefore its revision — changes when the template's bytes
/// change.
fn stamp_template_pins(metadata: &mut Metadata) {
    use crate::documents::{DOCUMENT_CAPABILITY, template_pin};
    use crate::types::{ProcessForEachState, ProcessStateOperation, ProcessValue};

    let pins: std::collections::BTreeMap<String, String> = metadata
        .templates
        .iter()
        .map(|template| (template.name.clone(), template_pin(template)))
        .collect();
    fn pin_for(
        pins: &std::collections::BTreeMap<String, String>,
        connector: &str,
        input: &std::collections::BTreeMap<String, ProcessValue>,
    ) -> Option<String> {
        if connector != DOCUMENT_CAPABILITY {
            return None;
        }
        let ProcessValue::Literal {
            literal: serde_json::Value::String(name),
        } = input.get("template")?
        else {
            return None;
        };
        pins.get(name).cloned()
    }

    for process in &mut metadata.processes {
        for state in &mut process.states {
            match &mut state.operation {
                ProcessStateOperation::Request { request } => {
                    request.template_pin = pin_for(&pins, &request.connector, &request.input);
                }
                ProcessStateOperation::ForEach { for_each } => {
                    if let ProcessForEachState::Request { request, .. } = for_each.as_mut() {
                        request.template_pin = pin_for(&pins, &request.connector, &request.input);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Load an optional top-level section (`!include`-resolved). Returns the
/// section's default value when the file is absent or blank.
/// Load `ingest.yaml`, whose one top-level key is `schemas:`.
///
/// Unlike a document template there is nothing to freeze: a schema *is* its
/// declaration, so what the loader reads is what the reader runs on.
fn load_ingest_schemas(dir: &Path) -> Result<Vec<crate::ingest::IngestSchema>, LoadError> {
    #[derive(Deserialize, Default)]
    struct IngestFile {
        #[serde(default)]
        schemas: Vec<crate::ingest::IngestSchema>,
    }

    let path = dir.join("ingest.yaml");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let value = load_yaml_resolved(&path)?;
    if value.is_null() {
        return Ok(Vec::new());
    }
    let parsed: IngestFile =
        serde_yaml::from_value(value).map_err(|source| LoadError::Yaml { path, source })?;
    Ok(parsed.schemas)
}

/// Stamp `<schema>@<hash>` onto every `local.ingest` activity.
///
/// The same mechanism, and the same reason, as the document template pin: a
/// column's declared type decides what an import means, so editing one changes
/// the revision of every process that imports with it — with no change to the
/// process compiler and no new entry in the dependency closure.
fn stamp_ingest_pins(metadata: &mut Metadata) {
    use crate::ingest::{INGEST_CAPABILITY, schema_pin};
    use crate::types::{ProcessForEachState, ProcessStateOperation, ProcessValue};

    let pins: std::collections::BTreeMap<String, String> = metadata
        .ingest_schemas
        .iter()
        .map(|schema| (schema.name.clone(), schema_pin(schema)))
        .collect();
    fn pin_for(
        pins: &std::collections::BTreeMap<String, String>,
        connector: &str,
        input: &std::collections::BTreeMap<String, ProcessValue>,
    ) -> Option<String> {
        if connector != INGEST_CAPABILITY {
            return None;
        }
        let ProcessValue::Literal {
            literal: serde_json::Value::String(name),
        } = input.get("schema")?
        else {
            return None;
        };
        pins.get(name).cloned()
    }

    for process in &mut metadata.processes {
        for state in &mut process.states {
            match &mut state.operation {
                ProcessStateOperation::Request { request } => {
                    if let Some(pin) = pin_for(&pins, &request.connector, &request.input) {
                        request.template_pin = Some(pin);
                    }
                }
                ProcessStateOperation::ForEach { for_each } => {
                    if let ProcessForEachState::Request { request, .. } = for_each.as_mut()
                        && let Some(pin) = pin_for(&pins, &request.connector, &request.input)
                    {
                        request.template_pin = Some(pin);
                    }
                }
                _ => {}
            }
        }
    }
}

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
