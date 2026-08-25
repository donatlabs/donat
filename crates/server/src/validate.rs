//! Read-only deploy-time metadata validation.
//!
//! Validation always introspects one real database. The selected-source entry
//! point never reuses that catalog for another metadata source; this is the
//! same source boundary used by Process reconciliation.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use donat_metadata::{Metadata, SourceKind};
use donat_schema::{
    CompiledCommandCatalog, PlanError, compile_command_source_catalog, finalize_command_effects,
    validate_command_source_catalog,
};

use crate::connectors::ConnectorRegistry;
use crate::processes::{
    CompiledProcessCatalog, CompiledSourceProcessCatalog, build_process_effect_contract_catalog,
    compile_process_source_catalog,
};

/// The credentials `serve` will resolve at boot, checked here instead.
///
/// `validate` is the documented gate between `migrate` and serving, so a
/// deployment that passes it expects to start. Without this it could pass on
/// metadata that is perfectly consistent with the database and still fail
/// seconds later on an environment variable nobody set — the single most
/// common deploy mistake, found in the one place least able to report it well.
///
/// Only the registries that *resolve* secrets are built. Neither reaches the
/// network: this answers "is the deployment configured", not "is the provider
/// up", which is not a question a deploy-time gate should block on.
fn missing_deployment_secrets(metadata: &Metadata) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(error) = ConnectorRegistry::build(metadata) {
        problems.push(format!("connector configuration: {error}"));
    }
    if let Err(error) = donat_storage::StorageRegistry::build(metadata) {
        problems.push(format!("storage configuration: {error}"));
    }
    problems
}

pub struct SourceProcessDeployment {
    pub source_catalog: donat_catalog::Catalog,
    pub processes: CompiledSourceProcessCatalog,
}

/// Preserve the pre-source-selection validation API for existing callers.
///
/// This mode is appropriate only when the caller already knows that all
/// Postgres metadata refers to the supplied database (the historic harness
/// uses one source). New deployment code must use
/// [`check_source_consistency`].
pub async fn check_consistency(database_url: &str, metadata_dir: &Path) -> Result<Vec<String>> {
    check_consistency_inner(database_url, metadata_dir, None).await
}

/// Validate exactly one selected Postgres source against its own database.
pub async fn check_source_consistency(
    database_url: &str,
    metadata_dir: &Path,
    source_name: &str,
) -> Result<Vec<String>> {
    check_consistency_inner(database_url, metadata_dir, Some(source_name)).await
}

/// Rebuild the already validated source-local Process candidate for
/// deployment reconciliation. The returned catalog was introspected from the
/// selected database and is never reused for another metadata source.
pub async fn compile_source_process_deployment(
    database_url: &str,
    metadata_dir: &Path,
    source_name: &str,
) -> anyhow::Result<SourceProcessDeployment> {
    let metadata = donat_metadata::load_metadata_dir(metadata_dir)
        .with_context(|| format!("loading metadata from {}", metadata_dir.display()))?;
    let selected_metadata = select_metadata(&metadata, Some(source_name))?;
    let rules = crate::state::compile_rule_catalog(&metadata)
        .map_err(|error| anyhow::anyhow!("{}: {}", error.path, error.message))?;
    let connectors = ConnectorRegistry::build(&selected_metadata)?;
    let (client, connection) = tokio_postgres::connect(database_url, crate::pgtls::connector())
        .await
        .context("connecting to selected source for Process deployment")?;
    let connection = tokio::spawn(connection);
    let source_catalog = donat_catalog::introspect(&client)
        .await
        .context("introspecting selected source for Process deployment")?;
    connection.abort();
    let commands =
        compile_command_source_catalog(&metadata, source_name, &source_catalog, &rules, true)
            .map_err(|error| anyhow::anyhow!("{}: {}", error.path, error.message))?;
    let processes =
        compile_process_source_catalog(&metadata, source_name, &commands, &rules, &connectors)
            .map_err(|error| anyhow::anyhow!("{}: {}", error.path, error.message))?;
    Ok(SourceProcessDeployment {
        source_catalog,
        processes,
    })
}

async fn check_consistency_inner(
    database_url: &str,
    metadata_dir: &Path,
    selected_source: Option<&str>,
) -> Result<Vec<String>> {
    let metadata = donat_metadata::load_metadata_dir(metadata_dir)
        .with_context(|| format!("loading metadata from {}", metadata_dir.display()))?;
    let validation_metadata = select_metadata(&metadata, selected_source)?;

    // Connector shape errors are independent of a database and therefore
    // fail before any connection attempt.
    let mut problems = crate::state::validate_connector_metadata(&validation_metadata)
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    problems.extend(missing_deployment_secrets(&validation_metadata));
    if !problems.is_empty() {
        return Ok(problems);
    }

    let rule_catalog = match crate::state::compile_rule_catalog(&metadata) {
        Ok(catalog) => Some(catalog),
        Err(error) => {
            push_plan_error(&mut problems, error);
            None
        }
    };

    let (client, connection) = tokio_postgres::connect(database_url, crate::pgtls::connector())
        .await
        .context("connecting to database for validate")?;
    let connection = tokio::spawn(connection);
    let catalog = donat_catalog::introspect(&client)
        .await
        .context("introspecting database")?;
    connection.abort();

    if let Some(rule_catalog) = rule_catalog.as_ref() {
        match selected_source {
            Some(source_name) => {
                validate_selected_executable_catalog(
                    &metadata,
                    &validation_metadata,
                    source_name,
                    &catalog,
                    rule_catalog,
                    &mut problems,
                );
            }
            None => {
                let catalogs = metadata
                    .sources
                    .iter()
                    .filter(|source| source.kind == SourceKind::Postgres)
                    .map(|source| (source.name.clone(), catalog.clone()))
                    .collect::<HashMap<_, _>>();
                for error in
                    donat_schema::validate_command_catalog(&metadata, &catalogs, rule_catalog, true)
                {
                    push_plan_error(&mut problems, error);
                }
            }
        }
    }

    validate_tracked_objects(&validation_metadata, &catalog, &mut problems);

    // The tenant column a table is *assumed* to carry is the one thing the
    // metadata loader cannot check, and the one whose absence would be a 500
    // on the first query instead of a refusal to deploy.
    //
    // Only against the database that was actually introspected. Mapping every
    // Postgres source to this one catalog would check a tenanted source
    // against somebody else's tables — and the failure that matters here is
    // the false negative: a table reported as carrying a tenant column
    // because a different database has one.
    let postgres_sources = validation_metadata
        .sources
        .iter()
        .filter(|source| source.kind == SourceKind::Postgres)
        .map(|source| source.name.clone())
        .collect::<Vec<_>>();
    match (selected_source, postgres_sources.as_slice()) {
        (Some(name), _) => {
            let catalogs = HashMap::from([(name.to_string(), catalog.clone())]);
            for error in donat_schema::validate_tenancy_catalog(&validation_metadata, &catalogs) {
                push_plan_error(&mut problems, error);
            }
        }
        (None, [only]) => {
            let catalogs = HashMap::from([(only.clone(), catalog.clone())]);
            for error in donat_schema::validate_tenancy_catalog(&validation_metadata, &catalogs) {
                push_plan_error(&mut problems, error);
            }
        }
        (None, _) if metadata.tenancy.is_some() => problems.push(
            "tenancy is declared and this deployment has more than one Postgres source, so \
             `validate` cannot tell which database to check it against. Re-run with `--source \
             <name>`."
                .to_string(),
        ),
        (None, _) => {}
    }

    // Inherited-role mutation permission conflicts are evaluated only for the
    // selected source view, never against an introspection snapshot borrowed
    // from another source.
    let planner = donat_schema::Planner::new(&validation_metadata, &catalog);
    for (role, table, kind) in planner.mutation_permission_conflicts() {
        problems.push(format!(
            "inherited role \"{role}\": conflicting {kind} permission on table \"{table}\""
        ));
    }
    problems.extend(planner.validator_problems(&validation_metadata.commands));

    Ok(problems)
}

fn select_metadata(metadata: &Metadata, source_name: Option<&str>) -> Result<Metadata> {
    let Some(source_name) = source_name else {
        return Ok(metadata.clone());
    };
    let source = metadata
        .sources
        .iter()
        .find(|source| source.name == source_name)
        .ok_or_else(|| anyhow::anyhow!("source `{source_name}` was not found in metadata"))?;
    if source.kind != SourceKind::Postgres {
        anyhow::bail!("source `{source_name}` is not Postgres");
    }

    let mut selected = metadata.clone();
    selected.sources.retain(|source| source.name == source_name);
    selected
        .commands
        .retain(|command| command.source == source_name);
    selected
        .processes
        .retain(|process| process.source == source_name);
    Ok(selected)
}

fn validate_selected_executable_catalog(
    metadata: &Metadata,
    validation_metadata: &Metadata,
    source_name: &str,
    catalog: &donat_catalog::Catalog,
    rules: &donat_rules::RuleCatalog,
    problems: &mut Vec<String>,
) {
    let command_diagnostics =
        validate_command_source_catalog(metadata, source_name, catalog, rules, true);
    if !command_diagnostics.is_empty() {
        for error in command_diagnostics {
            push_plan_error(problems, error);
        }
        return;
    }
    let commands = match compile_command_source_catalog(metadata, source_name, catalog, rules, true)
    {
        Ok(commands) => commands,
        Err(error) => {
            push_plan_error(problems, error);
            return;
        }
    };

    // ConnectorRegistry is also the compiler catalog: Process revisions pin
    // its exact ABI descriptors and non-secret deployment fingerprints. The
    // selected metadata view gives the otherwise implicit connector binding
    // exactly one Postgres source.
    let connectors = match ConnectorRegistry::build(validation_metadata) {
        Ok(connectors) => connectors,
        Err(error) => {
            problems.push(format!("connectors: {error}"));
            return;
        }
    };
    let processes = match compile_process_source_catalog(
        metadata,
        source_name,
        &commands,
        rules,
        &connectors,
    ) {
        Ok(processes) => processes,
        Err(error) => {
            push_plan_error(problems, error);
            return;
        }
    };

    let process_catalog = CompiledProcessCatalog::single_source(source_name.to_owned(), processes);
    let effects = match build_process_effect_contract_catalog(&process_catalog) {
        Ok(effects) => effects,
        Err(error) => {
            push_plan_error(problems, error);
            return;
        }
    };
    let command_catalog = CompiledCommandCatalog::single_source(source_name.to_owned(), commands);
    if let Err(error) = finalize_command_effects(command_catalog, &effects) {
        push_plan_error(problems, error);
    }
}

fn validate_tracked_objects(
    metadata: &Metadata,
    catalog: &donat_catalog::Catalog,
    problems: &mut Vec<String>,
) {
    for source in &metadata.sources {
        if source.kind != SourceKind::Postgres {
            continue;
        }
        for entry in &source.tables {
            let (schema, name) = (entry.table.schema(), entry.table.name());
            if catalog.table(schema, name).is_none() {
                problems.push(format!(
                    "tracked table \"{schema}.{name}\" does not exist in the database"
                ));
                continue;
            }
            // A file column is an ordinary uuid column, and metadata alone
            // cannot know whether it exists or what type it has. This is the
            // one place that can check, so a declaration against a missing or
            // wrongly typed column fails the deploy rather than the first
            // upload.
            for attachment in &entry.attachments {
                match catalog
                    .table(schema, name)
                    .and_then(|table| table.columns.iter().find(|c| c.name == attachment.column))
                {
                    Some(column) if column.sql_type() == "uuid" => {}
                    Some(column) => problems.push(format!(
                        "attachment \"{schema}.{name}.{}\" must be a uuid column, but it is {}",
                        attachment.column,
                        column.sql_type()
                    )),
                    None => problems.push(format!(
                        "attachment \"{schema}.{name}.{}\" references a column that does not exist",
                        attachment.column
                    )),
                }
            }
            for computed_field in &entry.computed_fields {
                let function = &computed_field.definition.function;
                if catalog
                    .function(function.schema(), function.name())
                    .is_none()
                {
                    problems.push(format!(
                        "computed field \"{}\" on \"{schema}.{name}\" references missing function \"{}.{}\"",
                        computed_field.name,
                        function.schema(),
                        function.name()
                    ));
                }
            }
        }
    }
}

fn push_plan_error(problems: &mut Vec<String>, error: PlanError) {
    problems.push(format!("{}: {}", error.path, error.message));
}
