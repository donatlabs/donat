use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use donat_connector_abi::OperationId;
use donat_connector_catalog::OperationSpec;
use donat_ir::{
    ProcessStartPolicy, TypeRef, ValueContractCatalog, ValueContractField, ValueScalar, ValueType,
};
use donat_metadata::{
    Metadata, Process, ProcessDeadline, ProcessErrorRoutes, ProcessField, ProcessForEachState,
    ProcessIdempotencyValue, ProcessLifecycle, ProcessRequestActivity, ProcessRequestState,
    ProcessRetry, ProcessSignalWait, ProcessState, ProcessStateOperation, ProcessValue,
    ProcessWaitState, SourceKind,
};
use donat_rules::RuleType;
use donat_schema::{
    CommandDescriptor, CompiledCommandCatalog, PlanError, ProcessEffectContract,
    ProcessEffectContractCatalog, ProcessSignalEffectContract,
};
use serde_json::{Map as JsonMap, Value as Json};
use sha2::{Digest, Sha256};

use crate::connectors::ConnectorRegistry;

pub const PROCESS_RUNTIME_ABI_EPOCH: u32 = 1;
pub const MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS: u64 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCommandDescriptor {
    pub source: String,
    pub name: String,
    pub arguments: ValueContractCatalog,
    pub result: ValueContractCatalog,
    pub allowed_roles: BTreeSet<String>,
    pub required_session_variables: BTreeMap<String, BTreeMap<String, TypeRef>>,
    pub definition_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRuleDescriptor {
    pub name: String,
    pub bindings: BTreeMap<String, RuleType>,
    pub result: RuleType,
    pub definition_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessDecisionDescriptor {
    pub name: String,
    pub inputs: BTreeMap<String, RuleType>,
    pub output: BTreeMap<String, RuleType>,
    pub definition_fingerprint: String,
}

pub struct ResolvedProcessConnectorOperation {
    pub spec: Arc<OperationSpec>,
    pub deployment_fingerprint: String,
}

/// Narrow, read-only seam between process compilation and the executable
/// connector registry.  The returned value contains no runtime handle, URL,
/// credential, resolved header, or secret.
pub trait ProcessConnectorCatalog {
    fn connector_operation(
        &self,
        source: &str,
        instance: &str,
        operation: &str,
    ) -> Result<Option<ResolvedProcessConnectorOperation>, String>;
}

impl ProcessConnectorCatalog for ConnectorRegistry {
    fn connector_operation(
        &self,
        source: &str,
        instance: &str,
        operation: &str,
    ) -> Result<Option<ResolvedProcessConnectorOperation>, String> {
        let operation_id = OperationId::parse(operation)
            .map_err(|_| format!("connector operation `{operation}` is not a canonical ABI ID"))?;
        let Some(spec) = self.operation_spec_handle(source, instance, operation_id) else {
            return Ok(None);
        };
        let Some(deployment_fingerprint) = self.configuration_fingerprint(instance, operation)
        else {
            return Err(format!(
                "connector operation `{source}.{instance}.{operation}` has no deployment fingerprint"
            ));
        };
        Ok(Some(ResolvedProcessConnectorOperation {
            spec,
            deployment_fingerprint: deployment_fingerprint.to_owned(),
        }))
    }
}

/// Complete immutable inputs consumed by the pure compiler.  Tests may provide
/// a small in-memory implementation; serving uses [`ServerProcessDependencies`].
pub trait ProcessDependencyCatalog {
    fn declared_type(&self, name: &str) -> Option<RuleType>;
    fn command(&self, source: &str, name: &str) -> Option<ProcessCommandDescriptor>;
    fn rule(&self, name: &str) -> Option<ProcessRuleDescriptor>;
    fn decision_table(&self, name: &str) -> Option<ProcessDecisionDescriptor>;
    fn connector_operation(
        &self,
        source: &str,
        instance: &str,
        operation: &str,
    ) -> Result<Option<ResolvedProcessConnectorOperation>, String>;
}

pub struct ServerProcessDependencies<'a> {
    metadata: &'a Metadata,
    commands: &'a CompiledCommandCatalog,
    rules: &'a donat_rules::RuleCatalog,
    connectors: &'a dyn ProcessConnectorCatalog,
}

impl<'a> ServerProcessDependencies<'a> {
    pub fn new(
        metadata: &'a Metadata,
        commands: &'a CompiledCommandCatalog,
        rules: &'a donat_rules::RuleCatalog,
        connectors: &'a dyn ProcessConnectorCatalog,
    ) -> Self {
        Self {
            metadata,
            commands,
            rules,
            connectors,
        }
    }
}

impl ProcessDependencyCatalog for ServerProcessDependencies<'_> {
    fn declared_type(&self, name: &str) -> Option<RuleType> {
        self.rules.declared_type(name).cloned()
    }

    fn command(&self, source: &str, name: &str) -> Option<ProcessCommandDescriptor> {
        let descriptor = self.commands.source(source)?.command(name)?.descriptor();
        Some(command_descriptor(descriptor))
    }

    fn rule(&self, name: &str) -> Option<ProcessRuleDescriptor> {
        let rule = self.rules.rule(name)?;
        Some(ProcessRuleDescriptor {
            name: rule.name.clone(),
            bindings: rule.bindings.clone(),
            result: rule.result.clone(),
            definition_fingerprint: domain_hash(
                b"donat.process.rule-dependency.v1\0",
                format!(
                    "{}\0{}\0{}",
                    rule.artifact.profile_version,
                    rule.artifact.canonical_ast_sha256,
                    rule.artifact.source_sha256
                )
                .as_bytes(),
            ),
        })
    }

    fn decision_table(&self, name: &str) -> Option<ProcessDecisionDescriptor> {
        let compiled = self.rules.decision_table(name)?;
        let definition = self
            .metadata
            .rules
            .decision_tables
            .iter()
            .find(|definition| definition.name == name)?;
        let output = definition
            .output
            .keys()
            .map(|field| {
                compiled
                    .output_field(field)
                    .map(|compiled| (field.clone(), compiled.type_.clone()))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        Some(ProcessDecisionDescriptor {
            name: name.to_owned(),
            inputs: compiled
                .input_types()
                .map(|(name, type_)| (name.clone(), type_.clone()))
                .collect(),
            output,
            definition_fingerprint: compiled.revision.0.clone(),
        })
    }

    fn connector_operation(
        &self,
        source: &str,
        instance: &str,
        operation: &str,
    ) -> Result<Option<ResolvedProcessConnectorOperation>, String> {
        self.connectors
            .connector_operation(source, instance, operation)
    }
}

fn command_descriptor(descriptor: &CommandDescriptor) -> ProcessCommandDescriptor {
    ProcessCommandDescriptor {
        source: descriptor.source.clone(),
        name: descriptor.name.clone(),
        arguments: descriptor.arguments.clone(),
        result: descriptor.result.clone(),
        allowed_roles: descriptor.allowed_roles.clone(),
        required_session_variables: descriptor.required_session_variables.clone(),
        definition_fingerprint: descriptor.definition_fingerprint.clone(),
    }
}

#[derive(Clone)]
pub struct ProcessDependencyClosure {
    pub commands: BTreeMap<(String, String), ProcessCommandDescriptor>,
    pub rules: BTreeMap<String, ProcessRuleDescriptor>,
    pub decision_tables: BTreeMap<String, ProcessDecisionDescriptor>,
    pub connector_operations: BTreeMap<(String, String, OperationId), PinnedConnectorOperation>,
}

impl std::fmt::Debug for ProcessDependencyClosure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessDependencyClosure")
            .field("commands", &self.commands)
            .field("rules", &self.rules)
            .field("decision_tables", &self.decision_tables)
            .field(
                "connector_operation_count",
                &self.connector_operations.len(),
            )
            .finish()
    }
}

#[derive(Clone)]
pub struct PinnedConnectorOperation {
    pub source: String,
    pub instance: String,
    pub spec: Arc<OperationSpec>,
    pub deployment_fingerprint: String,
}

impl std::fmt::Debug for PinnedConnectorOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PinnedConnectorOperation")
            .field("source", &self.source)
            .field("instance", &self.instance)
            .field("operation", &self.spec.operation.as_str())
            .field("runtime_abi_epoch", &self.spec.runtime_abi_epoch)
            .field("deployment_fingerprint", &self.deployment_fingerprint)
            .finish_non_exhaustive()
    }
}

impl ProcessDependencyClosure {
    fn empty() -> Self {
        Self {
            commands: BTreeMap::new(),
            rules: BTreeMap::new(),
            decision_tables: BTreeMap::new(),
            connector_operations: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompiledProcessState {
    pub id: String,
    pub output: ValueContractCatalog,
    pub maximum_send_horizons_ms: BTreeMap<String, u64>,
    pub operation: CompiledProcessStateOperation,
}

#[derive(Debug, Clone)]
pub enum CompiledProcessStateOperation {
    Command(CompiledProcessCommandState),
    Request,
    When(CompiledProcessWhenState),
    Wait,
    ForEach,
    Output(CompiledProcessOutputState),
    Fail(CompiledProcessFailState),
}

#[derive(Debug, Clone)]
pub struct CompiledProcessCommandState {
    pub name: String,
    pub role: CompiledProcessCommandRole,
    pub arguments: BTreeMap<String, ProcessValue>,
    pub next: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledProcessCommandRole {
    Caller {
        required_session_variables: BTreeMap<String, BTreeSet<String>>,
    },
    Fixed {
        role: String,
    },
}

#[derive(Debug, Clone)]
pub struct CompiledProcessWhenState {
    pub decision_table: Option<CompiledProcessDecisionCall>,
    pub cases: Vec<CompiledProcessWhenCase>,
    pub default: String,
    /// Ordinary literal cases compare one exact ancestor output. Retaining its
    /// state ID prevents runtime graph heuristics from diverging from the
    /// deploy-time type check.
    pub literal_output_state: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompiledProcessDecisionCall {
    pub name: String,
    pub input: BTreeMap<String, ProcessValue>,
}

#[derive(Debug, Clone)]
pub struct CompiledProcessWhenCase {
    pub predicate: CompiledProcessWhenPredicate,
    pub next: String,
}

#[derive(Debug, Clone)]
pub enum CompiledProcessWhenPredicate {
    Matches(BTreeMap<String, Json>),
    Rule {
        name: String,
        bindings: BTreeMap<String, ProcessValue>,
    },
}

#[derive(Debug, Clone)]
pub struct CompiledProcessOutputState {
    pub values: BTreeMap<String, ProcessValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledProcessFailState {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct CompiledProcessDefinition {
    pub source: String,
    pub name: String,
    pub version: u32,
    pub definition: Process,
    pub input: ValueContractCatalog,
    pub output: ValueContractCatalog,
    pub process_key: Option<TypeRef>,
    pub signals: BTreeMap<String, CompiledProcessSignal>,
    pub states: BTreeMap<String, CompiledProcessState>,
    pub caller_session_variables: BTreeMap<String, BTreeSet<String>>,
    pub dependencies: ProcessDependencyClosure,
    pub definition_fingerprint: String,
    pub revision_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledProcessSignal {
    pub role: Option<String>,
    pub correlation: ValueContractCatalog,
    pub payload: ValueContractCatalog,
    pub contract_fingerprint: String,
}

#[derive(Debug, Clone, Default)]
pub struct CompiledSourceProcessCatalog {
    processes: BTreeMap<String, CompiledProcessDefinition>,
}

impl CompiledSourceProcessCatalog {
    pub fn process(&self, name: &str) -> Option<&CompiledProcessDefinition> {
        self.processes.get(name)
    }

    pub fn len(&self) -> usize {
        self.processes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &CompiledProcessDefinition)> {
        self.processes.iter()
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompiledProcessCatalog {
    sources: BTreeMap<String, CompiledSourceProcessCatalog>,
}

impl CompiledProcessCatalog {
    pub fn source(&self, source: &str) -> Option<&CompiledSourceProcessCatalog> {
        self.sources.get(source)
    }

    pub fn len(&self) -> usize {
        self.sources
            .values()
            .map(CompiledSourceProcessCatalog::len)
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.sources
            .values()
            .all(CompiledSourceProcessCatalog::is_empty)
    }

    pub fn sources(&self) -> impl Iterator<Item = (&String, &CompiledSourceProcessCatalog)> {
        self.sources.iter()
    }

    /// Assemble one independently compiled source for effect-contract
    /// validation without inventing catalogs for unselected databases.
    pub fn single_source(
        source_name: impl Into<String>,
        source: CompiledSourceProcessCatalog,
    ) -> Self {
        Self {
            sources: BTreeMap::from([(source_name.into(), source)]),
        }
    }
}

pub fn compile_process_catalog(
    metadata: &Metadata,
    dependencies: &dyn ProcessDependencyCatalog,
) -> Result<CompiledProcessCatalog, PlanError> {
    let mut catalog = CompiledProcessCatalog::default();
    for (process_index, process) in metadata.processes.iter().enumerate() {
        let compiled = compile_process(metadata, dependencies, process_index, process)?;
        let source = catalog.sources.entry(process.source.clone()).or_default();
        if source
            .processes
            .insert(process.name.clone(), compiled)
            .is_some()
        {
            return Err(validation(
                format!("processes[{process_index}].name"),
                format!(
                    "process `{}` is declared more than once in source `{}`",
                    process.name, process.source
                ),
            ));
        }
    }
    Ok(catalog)
}

/// Compile only the Process declarations owned by one selected Postgres
/// source. Deployment uses this entry point after introspecting that source's
/// real catalog; it never substitutes the selected catalog for another source.
pub fn compile_process_source_catalog(
    metadata: &Metadata,
    source_name: &str,
    commands: &donat_schema::CompiledSourceCommandCatalog,
    rules: &donat_rules::RuleCatalog,
    connectors: &dyn ProcessConnectorCatalog,
) -> Result<CompiledSourceProcessCatalog, PlanError> {
    let source = metadata
        .sources
        .iter()
        .find(|source| source.name == source_name)
        .ok_or_else(|| {
            validation(
                "processes",
                format!("process source `{source_name}` does not exist"),
            )
        })?;
    if source.kind != SourceKind::Postgres {
        return Err(validation(
            "processes",
            format!("process source `{source_name}` requires a Postgres source"),
        ));
    }

    let aggregate_commands =
        CompiledCommandCatalog::single_source(source_name.to_owned(), commands.clone());
    let dependencies =
        ServerProcessDependencies::new(metadata, &aggregate_commands, rules, connectors);
    let mut compiled = CompiledSourceProcessCatalog::default();
    for (process_index, process) in metadata.processes.iter().enumerate() {
        if process.source != source_name {
            continue;
        }
        let definition = compile_process(metadata, &dependencies, process_index, process)?;
        if compiled
            .processes
            .insert(process.name.clone(), definition)
            .is_some()
        {
            return Err(validation(
                format!("processes[{process_index}].name"),
                format!(
                    "process `{}` is declared more than once in source `{source_name}`",
                    process.name
                ),
            ));
        }
    }
    Ok(compiled)
}

pub fn build_process_effect_contract_catalog(
    processes: &CompiledProcessCatalog,
) -> Result<ProcessEffectContractCatalog, PlanError> {
    let mut sources = BTreeMap::new();
    for (source_name, source) in processes.sources() {
        let mut contracts = BTreeMap::new();
        for (process_name, process) in source.iter() {
            let signals = process
                .signals
                .iter()
                .map(|(signal_name, signal)| {
                    (
                        signal_name.clone(),
                        ProcessSignalEffectContract {
                            signal_name: signal_name.clone(),
                            contract_revision: process.revision_fingerprint.clone(),
                            correlation: signal.correlation.clone(),
                            payload: signal.payload.clone(),
                            compatible_revisions: BTreeSet::from([process
                                .revision_fingerprint
                                .clone()]),
                        },
                    )
                })
                .collect();
            if contracts
                .insert(
                    process_name.clone(),
                    ProcessEffectContract {
                        process_name: process_name.clone(),
                        current_revision: process.revision_fingerprint.clone(),
                        start_policy: match process.definition.lifecycle {
                            ProcessLifecycle::Active => ProcessStartPolicy::Enabled,
                            ProcessLifecycle::Retired => ProcessStartPolicy::RejectRetired,
                        },
                        start_input: process.input.clone(),
                        process_key: process.process_key.clone(),
                        caller_session_variables: process.caller_session_variables.clone(),
                        signals,
                    },
                )
                .is_some()
            {
                return Err(validation(
                    "processes",
                    format!("process effect contract `{source_name}.{process_name}` is duplicated"),
                ));
            }
        }
        sources.insert(source_name.clone(), contracts);
    }
    Ok(ProcessEffectContractCatalog { sources })
}

fn compile_process(
    metadata: &Metadata,
    dependencies: &dyn ProcessDependencyCatalog,
    process_index: usize,
    process: &Process,
) -> Result<CompiledProcessDefinition, PlanError> {
    let base = format!("processes[{process_index}]");
    validate_identity(metadata, process, &base)?;
    let input = compile_fields(&process.input, dependencies, &format!("{base}.input"))?;
    let output = compile_fields(&process.output, dependencies, &format!("{base}.output"))?;
    validate_process_roles(process, &base)?;
    validate_owner_and_idempotency(process, dependencies, &input, &base)?;
    let signals = compile_signals(process, dependencies, &base)?;
    let graph = validate_graph(process, &base)?;
    let mut closure = ProcessDependencyClosure::empty();
    let process_key = validate_start(process, dependencies, &input, &mut closure, &base)?;
    let mut states = BTreeMap::new();
    let mut state_types = BTreeMap::new();
    for state_index in graph.topological_order {
        let state = &process.states[state_index];
        let path = format!("{base}.states[{state_index}]");
        let mut context = CompileContext {
            process,
            input: &input,
            state_types: &state_types,
            ancestors: &graph.ancestors[state_index],
            state_indices: &graph.state_indices,
            signals: &signals,
            dependencies,
            closure: &mut closure,
            path: &path,
            item: None,
        };
        let compiled = compile_state(state, &mut context)?;
        state_types.insert(state.id.clone(), compiled.output.clone());
        states.insert(state.id.clone(), compiled);
    }
    let caller_session_variables =
        collect_caller_session_variables(process, &closure, &format!("{base}.states"))?;
    let definition_fingerprint = definition_fingerprint(process, &input, &output, &signals)?;
    let revision_fingerprint = revision_fingerprint(&definition_fingerprint, &closure)?;
    Ok(CompiledProcessDefinition {
        source: process.source.clone(),
        name: process.name.clone(),
        version: process.version,
        definition: process.clone(),
        input,
        output,
        process_key,
        signals,
        states,
        caller_session_variables,
        dependencies: closure,
        definition_fingerprint,
        revision_fingerprint,
    })
}

fn collect_caller_session_variables(
    process: &Process,
    closure: &ProcessDependencyClosure,
    path: &str,
) -> Result<BTreeMap<String, BTreeSet<String>>, PlanError> {
    let mut by_role = process
        .permissions
        .iter()
        .map(|permission| (permission.role.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for permission in &process.permissions {
        if let Some(name) = &permission.owner_session_variable {
            by_role
                .get_mut(&permission.role)
                .expect("permission role initialized")
                .insert(name.to_ascii_lowercase());
        }
    }

    let mut ambient = BTreeSet::new();
    if let Some(owner) = &process.owner {
        collect_value_session_variables(&owner.capture, &mut ambient);
    }
    if let Some(idempotency) = &process.idempotency {
        for value in std::iter::once(&idempotency.key).chain(&idempotency.scope) {
            if let ProcessIdempotencyValue::SessionVariable { session_variable } = value {
                ambient.insert(session_variable.to_ascii_lowercase());
            }
        }
    }

    for (state_index, state) in process.states.iter().enumerate() {
        let state_path = format!("{path}[{state_index}]");
        match &state.operation {
            ProcessStateOperation::Command { command } => {
                collect_value_map_session_variables(&command.arguments, &mut ambient);
                collect_caller_command_session_variables(
                    process,
                    closure,
                    &command.name,
                    &command.run_as,
                    &state_path,
                    &mut by_role,
                )?;
            }
            ProcessStateOperation::Request { request } => {
                collect_value_map_session_variables(&request.input, &mut ambient);
            }
            ProcessStateOperation::When { when } => {
                collect_value_map_session_variables(&when.input, &mut ambient);
                for case in &when.cases {
                    collect_value_map_session_variables(&case.bindings, &mut ambient);
                }
            }
            ProcessStateOperation::Wait { wait } => match wait {
                ProcessWaitState::Signal(wait) => {
                    collect_value_map_session_variables(&wait.correlate, &mut ambient);
                    if let ProcessDeadline::Value(value) = &wait.deadline {
                        collect_value_session_variables(value, &mut ambient);
                    }
                }
                ProcessWaitState::Timer(wait) => {
                    collect_value_map_session_variables(&wait.timer.bindings, &mut ambient);
                }
            },
            ProcessStateOperation::ForEach { for_each } => match for_each.as_ref() {
                ProcessForEachState::Command { input, command, .. } => {
                    collect_value_session_variables(input, &mut ambient);
                    collect_value_map_session_variables(&command.arguments, &mut ambient);
                    collect_caller_command_session_variables(
                        process,
                        closure,
                        &command.name,
                        &command.run_as,
                        &state_path,
                        &mut by_role,
                    )?;
                }
                ProcessForEachState::Request { input, request, .. } => {
                    collect_value_session_variables(input, &mut ambient);
                    collect_value_map_session_variables(&request.input, &mut ambient);
                }
            },
            ProcessStateOperation::Output { output } => {
                collect_value_map_session_variables(&output.values, &mut ambient);
            }
            ProcessStateOperation::Fail { .. } => {}
        }
    }

    for variables in by_role.values_mut() {
        variables.extend(ambient.iter().cloned());
    }
    Ok(by_role)
}

fn collect_caller_command_session_variables(
    process: &Process,
    closure: &ProcessDependencyClosure,
    command_name: &str,
    run_as: &str,
    path: &str,
    by_role: &mut BTreeMap<String, BTreeSet<String>>,
) -> Result<(), PlanError> {
    if run_as != "caller" {
        return Ok(());
    }
    let descriptor = closure
        .commands
        .get(&(process.source.clone(), command_name.to_owned()))
        .ok_or_else(|| {
            validation(
                path,
                format!(
                    "compiled caller command `{command_name}` is absent from the dependency closure"
                ),
            )
        })?;
    for permission in &process.permissions {
        let target = by_role
            .get_mut(&permission.role)
            .expect("permission role initialized");
        target.extend(
            descriptor
                .required_session_variables
                .get(&permission.role)
                .into_iter()
                .flat_map(|variables| variables.keys())
                .map(|name| name.to_ascii_lowercase()),
        );
    }
    Ok(())
}

fn collect_value_map_session_variables(
    values: &BTreeMap<String, ProcessValue>,
    output: &mut BTreeSet<String>,
) {
    for value in values.values() {
        collect_value_session_variables(value, output);
    }
}

fn collect_value_session_variables(value: &ProcessValue, output: &mut BTreeSet<String>) {
    match value {
        ProcessValue::SessionVariable { session_variable } => {
            output.insert(session_variable.to_ascii_lowercase());
        }
        ProcessValue::BoundedConcat { bounded_concat } => {
            for value in &bounded_concat.inputs {
                collect_value_session_variables(value, output);
            }
        }
        ProcessValue::BoundedFlatten { bounded_flatten } => {
            collect_value_session_variables(&bounded_flatten.from, output);
        }
        ProcessValue::Input { .. }
        | ProcessValue::State { .. }
        | ProcessValue::Item { .. }
        | ProcessValue::Literal { .. }
        | ProcessValue::ActivityKey { .. }
        | ProcessValue::ActivityKeyForState { .. }
        | ProcessValue::Run { .. }
        | ProcessValue::WorkflowTime { .. } => {}
    }
}

fn validation(path: impl Into<String>, message: impl Into<String>) -> PlanError {
    PlanError::validation(&path.into(), message)
}

fn validate_identity(metadata: &Metadata, process: &Process, path: &str) -> Result<(), PlanError> {
    if process.name.trim().is_empty() {
        return Err(validation(
            format!("{path}.name"),
            "process name must not be empty",
        ));
    }
    if process.version == 0 {
        return Err(validation(
            format!("{path}.version"),
            "process version must be greater than zero",
        ));
    }
    let Some(source) = metadata
        .sources
        .iter()
        .find(|source| source.name == process.source)
    else {
        return Err(validation(
            format!("{path}.source"),
            format!("process source `{}` does not exist", process.source),
        ));
    };
    if source.kind != SourceKind::Postgres {
        return Err(validation(
            format!("{path}.source"),
            format!(
                "durable process source `{}` must be postgres",
                process.source
            ),
        ));
    }
    if process.states.is_empty() {
        return Err(validation(
            format!("{path}.states"),
            "process must declare at least one state",
        ));
    }
    Ok(())
}

fn validate_process_roles(process: &Process, path: &str) -> Result<(), PlanError> {
    if process.permissions.is_empty() {
        return Err(validation(
            format!("{path}.permissions"),
            "process must declare at least one explicit role",
        ));
    }
    let mut roles = BTreeSet::new();
    for (index, permission) in process.permissions.iter().enumerate() {
        if permission.role.trim().is_empty() || permission.role == "admin" {
            return Err(validation(
                format!("{path}.permissions[{index}].role"),
                "process role must be a non-empty explicit non-admin role",
            ));
        }
        if !roles.insert(permission.role.as_str()) {
            return Err(validation(
                format!("{path}.permissions[{index}].role"),
                format!(
                    "process role `{}` is declared more than once",
                    permission.role
                ),
            ));
        }
        if permission
            .owner_session_variable
            .as_deref()
            .is_some_and(str::is_empty)
        {
            return Err(validation(
                format!("{path}.permissions[{index}].owner_session_variable"),
                "owner session variable must not be empty",
            ));
        }
    }
    Ok(())
}

fn validate_owner_and_idempotency(
    process: &Process,
    dependencies: &dyn ProcessDependencyCatalog,
    input: &ValueContractCatalog,
    path: &str,
) -> Result<(), PlanError> {
    if let Some(owner) = &process.owner {
        let owner_type =
            type_from_string(&owner.type_, dependencies, &format!("{path}.owner.type"))?;
        if owner_type.nullable {
            return Err(validation(
                format!("{path}.owner.type"),
                "process owner type must be non-null",
            ));
        }
        match &owner.capture {
            ProcessValue::Input {
                input: field,
                as_: None,
                require_non_null: false,
            } => {
                let source = root_type(input, field).ok_or_else(|| {
                    validation(
                        format!("{path}.owner.capture"),
                        format!("owner capture references unknown input `{field}`"),
                    )
                })?;
                if !type_assignable(&owner_type, source) {
                    return Err(type_mismatch(
                        format!("{path}.owner.capture"),
                        &owner_type,
                        source,
                    ));
                }
            }
            ProcessValue::SessionVariable { session_variable } if !session_variable.is_empty() => {}
            _ => {
                return Err(validation(
                    format!("{path}.owner.capture"),
                    "process owner must capture one input or explicit session variable",
                ));
            }
        }
    }

    if let Some(idempotency) = &process.idempotency {
        validate_idempotency_value(&idempotency.key, input, &format!("{path}.idempotency.key"))?;
        let mut seen = BTreeSet::new();
        for (index, value) in idempotency.scope.iter().enumerate() {
            validate_idempotency_value(
                value,
                input,
                &format!("{path}.idempotency.scope[{index}]"),
            )?;
            let identity = match value {
                ProcessIdempotencyValue::Input { input } => format!("input:{input}"),
                ProcessIdempotencyValue::SessionVariable { session_variable } => {
                    format!("session:{session_variable}")
                }
            };
            if !seen.insert(identity) {
                return Err(validation(
                    format!("{path}.idempotency.scope[{index}]"),
                    "process idempotency scope contains a duplicate value",
                ));
            }
        }
    }
    Ok(())
}

fn validate_idempotency_value(
    value: &ProcessIdempotencyValue,
    input: &ValueContractCatalog,
    path: &str,
) -> Result<(), PlanError> {
    match value {
        ProcessIdempotencyValue::Input { input: field } => {
            let type_ = root_type(input, field).ok_or_else(|| {
                validation(
                    path,
                    format!("idempotency references unknown input `{field}`"),
                )
            })?;
            if type_.nullable || !scalar_like(type_) {
                return Err(validation(
                    path,
                    "idempotency input must be a non-null scalar or enum",
                ));
            }
        }
        ProcessIdempotencyValue::SessionVariable { session_variable } => {
            if session_variable.is_empty() {
                return Err(validation(
                    path,
                    "idempotency session variable must not be empty",
                ));
            }
        }
    }
    Ok(())
}

fn compile_fields(
    fields: &[ProcessField],
    dependencies: &dyn ProcessDependencyCatalog,
    path: &str,
) -> Result<ValueContractCatalog, PlanError> {
    let mut roots = BTreeMap::new();
    for (index, field) in fields.iter().enumerate() {
        if field.name.trim().is_empty() {
            return Err(validation(
                format!("{path}[{index}].name"),
                "field name must not be empty",
            ));
        }
        let type_ref =
            type_from_string(&field.type_, dependencies, &format!("{path}[{index}].type"))?;
        if roots
            .insert(
                field.name.clone(),
                ValueContractField {
                    required: !type_ref.nullable,
                    type_ref,
                },
            )
            .is_some()
        {
            return Err(validation(
                format!("{path}[{index}].name"),
                format!("field `{}` is declared more than once", field.name),
            ));
        }
    }
    Ok(ValueContractCatalog {
        roots,
        named_objects: BTreeMap::new(),
    })
}

fn compile_type_map(
    fields: &BTreeMap<String, String>,
    dependencies: &dyn ProcessDependencyCatalog,
    path: &str,
) -> Result<ValueContractCatalog, PlanError> {
    let mut roots = BTreeMap::new();
    for (name, source) in fields {
        let type_ref = type_from_string(source, dependencies, &format!("{path}.{name}"))?;
        roots.insert(
            name.clone(),
            ValueContractField {
                required: !type_ref.nullable,
                type_ref,
            },
        );
    }
    Ok(ValueContractCatalog {
        roots,
        named_objects: BTreeMap::new(),
    })
}

fn compile_signals(
    process: &Process,
    dependencies: &dyn ProcessDependencyCatalog,
    path: &str,
) -> Result<BTreeMap<String, CompiledProcessSignal>, PlanError> {
    let mut signals = BTreeMap::new();
    for (index, signal) in process.signals.iter().enumerate() {
        let signal_path = format!("{path}.signals[{index}]");
        if signal.name.trim().is_empty() {
            return Err(validation(
                format!("{signal_path}.name"),
                "signal name must not be empty",
            ));
        }
        if signal
            .role
            .as_deref()
            .is_some_and(|role| role.is_empty() || role == "admin")
        {
            return Err(validation(
                format!("{signal_path}.role"),
                "signal role must be an explicit non-admin role",
            ));
        }
        let correlation = compile_type_map(
            &signal.correlation,
            dependencies,
            &format!("{signal_path}.correlation"),
        )?;
        if correlation.roots.is_empty() {
            return Err(validation(
                format!("{signal_path}.correlation"),
                "signal correlation must not be empty",
            ));
        }
        for (name, field) in &correlation.roots {
            if field.type_ref.nullable || !scalar_like(&field.type_ref) {
                return Err(validation(
                    format!("{signal_path}.correlation.{name}"),
                    "signal correlation must use non-null scalar or enum fields",
                ));
            }
        }
        let payload = compile_type_map(
            &signal.payload,
            dependencies,
            &format!("{signal_path}.payload"),
        )?;
        let contract_fingerprint = hash_json(
            b"donat.process.signal-contract.v1\0",
            &serde_json::json!({
                "name": signal.name,
                "role": signal.role,
                "correlation": contract_material(&correlation),
                "payload": contract_material(&payload),
            }),
        )?;
        if signals
            .insert(
                signal.name.clone(),
                CompiledProcessSignal {
                    role: signal.role.clone(),
                    correlation,
                    payload,
                    contract_fingerprint,
                },
            )
            .is_some()
        {
            return Err(validation(
                format!("{signal_path}.name"),
                format!("signal `{}` is declared more than once", signal.name),
            ));
        }
    }
    Ok(signals)
}

struct ValidatedGraph {
    state_indices: BTreeMap<String, usize>,
    topological_order: Vec<usize>,
    ancestors: Vec<BTreeSet<usize>>,
}

fn validate_graph(process: &Process, path: &str) -> Result<ValidatedGraph, PlanError> {
    let mut state_indices = BTreeMap::new();
    for (index, state) in process.states.iter().enumerate() {
        if state.id.trim().is_empty() {
            return Err(validation(
                format!("{path}.states[{index}].id"),
                "state id must not be empty",
            ));
        }
        if state_indices.insert(state.id.clone(), index).is_some() {
            return Err(validation(
                format!("{path}.states[{index}].id"),
                format!("state `{}` is declared more than once", state.id),
            ));
        }
    }
    let Some(&start_index) = state_indices.get(&process.start_at) else {
        return Err(validation(
            format!("{path}.start_at"),
            format!("initial state `{}` does not exist", process.start_at),
        ));
    };

    let mut edges = vec![Vec::new(); process.states.len()];
    for (index, state) in process.states.iter().enumerate() {
        for target in state_targets(state) {
            let Some(&target_index) = state_indices.get(target) else {
                return Err(validation(
                    format!("{path}.states[{index}]"),
                    format!("state transition target `{target}` does not exist"),
                ));
            };
            if !edges[index].contains(&target_index) {
                edges[index].push(target_index);
            }
        }
    }

    let mut reachable = vec![false; process.states.len()];
    let mut pending = vec![start_index];
    while let Some(index) = pending.pop() {
        if std::mem::replace(&mut reachable[index], true) {
            continue;
        }
        pending.extend(edges[index].iter().copied());
    }
    if let Some((index, state)) = process
        .states
        .iter()
        .enumerate()
        .find(|(index, _)| !reachable[*index])
    {
        return Err(validation(
            format!("{path}.states[{index}].id"),
            format!("state `{}` is unreachable from start_at", state.id),
        ));
    }

    let mut indegree = vec![0_usize; process.states.len()];
    for targets in &edges {
        for target in targets {
            indegree[*target] += 1;
        }
    }
    let mut queue = VecDeque::from_iter(
        indegree
            .iter()
            .enumerate()
            .filter_map(|(index, degree)| (*degree == 0).then_some(index)),
    );
    let mut topological_order = Vec::with_capacity(process.states.len());
    while let Some(index) = queue.pop_front() {
        topological_order.push(index);
        for target in &edges[index] {
            indegree[*target] -= 1;
            if indegree[*target] == 0 {
                queue.push_back(*target);
            }
        }
    }
    if topological_order.len() != process.states.len() {
        let index = indegree
            .iter()
            .position(|degree| *degree > 0)
            .unwrap_or_default();
        return Err(validation(
            format!("{path}.states[{index}]"),
            "process transitions must form an acyclic graph",
        ));
    }

    let mut ancestors = vec![BTreeSet::new(); process.states.len()];
    for index in &topological_order {
        for target in &edges[*index] {
            let inherited = ancestors[*index].clone();
            ancestors[*target].insert(*index);
            ancestors[*target].extend(inherited);
        }
    }
    Ok(ValidatedGraph {
        state_indices,
        topological_order,
        ancestors,
    })
}

fn state_targets(state: &ProcessState) -> Vec<&str> {
    let mut targets = Vec::new();
    match &state.operation {
        ProcessStateOperation::Command { command } => targets.push(command.next.as_str()),
        ProcessStateOperation::Request { request } => {
            targets.push(request.next.as_str());
            push_error_targets(&mut targets, request.on_error.as_ref());
        }
        ProcessStateOperation::When { when } => {
            targets.extend(when.cases.iter().map(|case| case.next.as_str()));
            targets.push(when.default.as_str());
        }
        ProcessStateOperation::Wait { wait } => match wait {
            ProcessWaitState::Signal(signal) => {
                targets.push(signal.next.as_str());
                targets.push(signal.on_timeout.as_str());
            }
            ProcessWaitState::Timer(timer) => targets.push(timer.next.as_str()),
        },
        ProcessStateOperation::ForEach { for_each } => match for_each.as_ref() {
            ProcessForEachState::Command { next, .. } => targets.push(next.as_str()),
            ProcessForEachState::Request { request, next, .. } => {
                targets.push(next.as_str());
                push_error_targets(&mut targets, request.on_error.as_ref());
            }
        },
        ProcessStateOperation::Output { .. } | ProcessStateOperation::Fail { .. } => {}
    }
    targets
}

fn push_error_targets<'a>(targets: &mut Vec<&'a str>, routes: Option<&'a ProcessErrorRoutes>) {
    if let Some(routes) = routes {
        targets.extend(routes.routes.iter().map(|route| route.next.as_str()));
        targets.push(routes.fallback.next.as_str());
    }
}

fn validate_start(
    process: &Process,
    dependencies: &dyn ProcessDependencyCatalog,
    input: &ValueContractCatalog,
    closure: &mut ProcessDependencyClosure,
    path: &str,
) -> Result<Option<TypeRef>, PlanError> {
    let Some(start) = &process.start else {
        return Ok(None);
    };
    let start_path = format!("{path}.start");
    let descriptor = resolve_command(
        dependencies,
        &process.source,
        &start.command,
        &format!("{start_path}.command"),
    )?;
    validate_caller_role(process, &descriptor, &format!("{start_path}.command"))?;
    let arguments = normalize_contract(
        &descriptor.arguments,
        dependencies,
        &format!("{start_path}.command"),
    )?;
    let result = normalize_contract(
        &descriptor.result,
        dependencies,
        &format!("{start_path}.command"),
    )?;

    if start.input.len() != input.roots.len() {
        return Err(validation(
            format!("{start_path}.input"),
            "process start input must map every process input exactly once",
        ));
    }
    for (field, target) in &input.roots {
        let reference = start.input.get(field).ok_or_else(|| {
            validation(
                format!("{start_path}.input"),
                format!("process start input is missing `{field}`"),
            )
        })?;
        let source = root_type(&result, &reference.command_result).ok_or_else(|| {
            validation(
                format!("{start_path}.input.{field}.command_result"),
                format!(
                    "command `{}` has no result field `{}`",
                    start.command, reference.command_result
                ),
            )
        })?;
        if !type_assignable(&target.type_ref, source) {
            return Err(type_mismatch(
                format!("{start_path}.input.{field}.command_result"),
                &target.type_ref,
                source,
            ));
        }
    }
    if let Some(extra) = start
        .input
        .keys()
        .find(|field| !input.roots.contains_key(*field))
    {
        return Err(validation(
            format!("{start_path}.input.{extra}"),
            format!("process start input contains undeclared field `{extra}`"),
        ));
    }

    let idempotency_type = root_type(&arguments, &start.idempotency_key.command_argument)
        .ok_or_else(|| {
            validation(
                format!("{start_path}.idempotency_key.command_argument"),
                format!(
                    "command `{}` has no argument `{}`",
                    start.command, start.idempotency_key.command_argument
                ),
            )
        })?;
    if idempotency_type.nullable || !scalar_like(idempotency_type) {
        return Err(validation(
            format!("{start_path}.idempotency_key.command_argument"),
            "process start idempotency argument must be a non-null scalar or enum",
        ));
    }
    let process_key = root_type(&result, &start.process_key.command_result)
        .cloned()
        .ok_or_else(|| {
            validation(
                format!("{start_path}.process_key.command_result"),
                format!(
                    "command `{}` has no result field `{}`",
                    start.command, start.process_key.command_result
                ),
            )
        })?;
    if process_key.nullable || !scalar_like(&process_key) {
        return Err(validation(
            format!("{start_path}.process_key.command_result"),
            "process key must be a non-null scalar or enum",
        ));
    }
    closure.commands.insert(
        (process.source.clone(), start.command.clone()),
        ProcessCommandDescriptor {
            arguments,
            result,
            ..descriptor
        },
    );
    Ok(Some(process_key))
}

struct CompileContext<'a, 'b> {
    process: &'a Process,
    input: &'a ValueContractCatalog,
    state_types: &'a BTreeMap<String, ValueContractCatalog>,
    ancestors: &'a BTreeSet<usize>,
    state_indices: &'a BTreeMap<String, usize>,
    signals: &'a BTreeMap<String, CompiledProcessSignal>,
    dependencies: &'a dyn ProcessDependencyCatalog,
    closure: &'b mut ProcessDependencyClosure,
    path: &'a str,
    item: Option<TypeRef>,
}

fn compile_state(
    state: &ProcessState,
    context: &mut CompileContext<'_, '_>,
) -> Result<CompiledProcessState, PlanError> {
    let (output, maximum_send_horizons_ms, operation) = match &state.operation {
        ProcessStateOperation::Command { command } => {
            let descriptor = compile_command_activity(
                &command.name,
                &command.run_as,
                &command.arguments,
                context,
                &format!("{}.command", context.path),
            )?;
            let role = compile_command_role(
                context.process,
                &descriptor,
                &command.run_as,
                &format!("{}.command.run_as", context.path),
            )?;
            (
                descriptor.result,
                BTreeMap::new(),
                CompiledProcessStateOperation::Command(CompiledProcessCommandState {
                    name: command.name.clone(),
                    role,
                    arguments: command.arguments.clone(),
                    next: command.next.clone(),
                }),
            )
        }
        ProcessStateOperation::Request { request } => {
            let (output, horizons) = compile_request_state(
                request,
                &state.id,
                None,
                context,
                &format!("{}.request", context.path),
            )?;
            (output, horizons, CompiledProcessStateOperation::Request)
        }
        ProcessStateOperation::When { when } => {
            let (output, compiled) = compile_when_state(when, context)?;
            (
                output,
                BTreeMap::new(),
                CompiledProcessStateOperation::When(compiled),
            )
        }
        ProcessStateOperation::Wait { wait } => (
            compile_wait_state(wait, context)?,
            BTreeMap::new(),
            CompiledProcessStateOperation::Wait,
        ),
        ProcessStateOperation::ForEach { for_each } => {
            let (output, horizons) = compile_for_each_state(for_each, &state.id, context)?;
            (output, horizons, CompiledProcessStateOperation::ForEach)
        }
        ProcessStateOperation::Output { output } => {
            validate_binding_map(
                &output.values,
                &compile_fields(
                    &context.process.output,
                    context.dependencies,
                    &format!("{}.output", context.path),
                )?,
                context,
                &format!("{}.output.values", context.path),
            )?;
            (
                compile_fields(
                    &context.process.output,
                    context.dependencies,
                    &format!("{}.output", context.path),
                )?,
                BTreeMap::new(),
                CompiledProcessStateOperation::Output(CompiledProcessOutputState {
                    values: output.values.clone(),
                }),
            )
        }
        ProcessStateOperation::Fail { fail } => {
            if fail.code.trim().is_empty() || fail.message.trim().is_empty() {
                return Err(validation(
                    format!("{}.fail", context.path),
                    "fail state code and safe message must not be empty",
                ));
            }
            (
                empty_contract(),
                BTreeMap::new(),
                CompiledProcessStateOperation::Fail(CompiledProcessFailState {
                    code: fail.code.clone(),
                    message: fail.message.clone(),
                }),
            )
        }
    };
    Ok(CompiledProcessState {
        id: state.id.clone(),
        output,
        maximum_send_horizons_ms,
        operation,
    })
}

fn resolve_command(
    dependencies: &dyn ProcessDependencyCatalog,
    source: &str,
    name: &str,
    path: &str,
) -> Result<ProcessCommandDescriptor, PlanError> {
    let descriptor = dependencies
        .command(source, name)
        .ok_or_else(|| validation(path, format!("command `{source}.{name}` does not exist")))?;
    if descriptor.source != source || descriptor.name != name {
        return Err(validation(
            path,
            format!(
                "command descriptor `{}` is not source-local to `{source}.{name}`",
                descriptor.source
            ),
        ));
    }
    Ok(descriptor)
}

fn compile_command_activity(
    name: &str,
    run_as: &str,
    arguments: &BTreeMap<String, ProcessValue>,
    context: &mut CompileContext<'_, '_>,
    path: &str,
) -> Result<ProcessCommandDescriptor, PlanError> {
    let mut descriptor = resolve_command(
        context.dependencies,
        &context.process.source,
        name,
        &format!("{path}.name"),
    )?;
    validate_run_as(
        context.process,
        &descriptor,
        run_as,
        &format!("{path}.run_as"),
    )?;
    descriptor.arguments = normalize_contract(
        &descriptor.arguments,
        context.dependencies,
        &format!("{path}.arguments"),
    )?;
    descriptor.result = normalize_contract(
        &descriptor.result,
        context.dependencies,
        &format!("{path}.result"),
    )?;
    validate_binding_map(
        arguments,
        &descriptor.arguments,
        context,
        &format!("{path}.arguments"),
    )?;
    context.closure.commands.insert(
        (context.process.source.clone(), name.to_owned()),
        descriptor.clone(),
    );
    Ok(descriptor)
}

fn validate_run_as(
    process: &Process,
    descriptor: &ProcessCommandDescriptor,
    run_as: &str,
    path: &str,
) -> Result<(), PlanError> {
    if run_as == "caller" {
        return validate_caller_role(process, descriptor, path);
    }
    if run_as.is_empty() || run_as == "admin" {
        return Err(validation(
            path,
            "command run_as must be `caller` or an explicit non-admin role",
        ));
    }
    if !descriptor.allowed_roles.contains(run_as) {
        return Err(validation(
            path,
            format!(
                "command `{}` is not executable as role `{run_as}`",
                descriptor.name
            ),
        ));
    }
    if descriptor
        .required_session_variables
        .get(run_as)
        .is_some_and(|variables| !variables.is_empty())
    {
        return Err(validation(
            path,
            format!(
                "fixed Process command role `{run_as}` requires session variables, but fixed roles have no ambient request session"
            ),
        ));
    }
    Ok(())
}

fn compile_command_role(
    process: &Process,
    descriptor: &ProcessCommandDescriptor,
    run_as: &str,
    path: &str,
) -> Result<CompiledProcessCommandRole, PlanError> {
    validate_run_as(process, descriptor, run_as, path)?;
    if run_as != "caller" {
        return Ok(CompiledProcessCommandRole::Fixed {
            role: run_as.to_owned(),
        });
    }
    let required_session_variables = process
        .permissions
        .iter()
        .map(|permission| {
            (
                permission.role.clone(),
                descriptor
                    .required_session_variables
                    .get(&permission.role)
                    .into_iter()
                    .flat_map(|variables| variables.keys())
                    .map(|name| name.to_ascii_lowercase())
                    .collect(),
            )
        })
        .collect();
    Ok(CompiledProcessCommandRole::Caller {
        required_session_variables,
    })
}

fn validate_caller_role(
    process: &Process,
    descriptor: &ProcessCommandDescriptor,
    path: &str,
) -> Result<(), PlanError> {
    for permission in &process.permissions {
        if !descriptor.allowed_roles.contains(&permission.role) {
            return Err(validation(
                path,
                format!(
                    "command `{}` is not executable by process caller role `{}`",
                    descriptor.name, permission.role
                ),
            ));
        }
    }
    Ok(())
}

fn compile_request_state(
    request: &ProcessRequestState,
    state_id: &str,
    item_key: Option<&str>,
    context: &mut CompileContext<'_, '_>,
    path: &str,
) -> Result<(ValueContractCatalog, BTreeMap<String, u64>), PlanError> {
    compile_request(
        &request.connector,
        &request.operation,
        &request.input,
        request.idempotency_key.as_ref(),
        &request.timeout,
        &request.retry,
        request.on_error.as_ref(),
        state_id,
        item_key,
        context,
        path,
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_request(
    connector: &str,
    operation: &str,
    input: &BTreeMap<String, ProcessValue>,
    idempotency_key: Option<&donat_metadata::ProcessRequestIdempotencyKey>,
    timeout: &donat_metadata::ProcessTimeout,
    retry: &ProcessRetry,
    on_error: Option<&ProcessErrorRoutes>,
    state_id: &str,
    item_key: Option<&str>,
    context: &mut CompileContext<'_, '_>,
    path: &str,
) -> Result<(ValueContractCatalog, BTreeMap<String, u64>), PlanError> {
    if connector.trim().is_empty() || operation.trim().is_empty() {
        return Err(validation(
            path,
            "request connector and operation must not be empty",
        ));
    }
    let resolved = context
        .dependencies
        .connector_operation(&context.process.source, connector, operation)
        .map_err(|message| validation(format!("{path}.operation"), message))?
        .ok_or_else(|| {
            validation(
                format!("{path}.operation"),
                format!(
                    "connector operation `{}.{connector}.{operation}` is not executable",
                    context.process.source
                ),
            )
        })?;
    if resolved.spec.operation.as_str() != operation {
        return Err(validation(
            format!("{path}.operation"),
            "connector catalog returned a different operation identity",
        ));
    }
    let input_contract = normalize_contract(
        &resolved.spec.input,
        context.dependencies,
        &format!("{path}.input"),
    )?;
    let output_contract = normalize_contract(
        &resolved.spec.output,
        context.dependencies,
        &format!("{path}.output"),
    )?;
    validate_binding_map(input, &input_contract, context, &format!("{path}.input"))?;
    validate_error_routes(on_error, &format!("{path}.on_error"))?;
    let schedule_to_start = parse_duration(
        &timeout.schedule_to_start,
        &format!("{path}.timeout.schedule_to_start"),
    )?;
    let start_to_close = parse_duration(
        &timeout.start_to_close,
        &format!("{path}.timeout.start_to_close"),
    )?;
    if resolved.spec.bounds.deadline_ms.get() > start_to_close {
        return Err(validation(
            format!("{path}.timeout.start_to_close"),
            format!(
                "connector operation deadline {} ms exceeds start_to_close {start_to_close} ms",
                resolved.spec.bounds.deadline_ms
            ),
        ));
    }
    let maximum_send_horizon_ms = compile_retry_horizon(
        retry,
        schedule_to_start,
        start_to_close,
        &format!("{path}.retry"),
    )?;
    let mut horizons = BTreeMap::new();
    match &resolved.spec.effect {
        donat_connector_catalog::OperationEffect::ReadOnly => {
            if idempotency_key.is_some() {
                return Err(validation(
                    format!("{path}.idempotency_key"),
                    "read-only connector operations must remain headerless",
                ));
            }
        }
        donat_connector_catalog::OperationEffect::ProviderIdempotent { side_effect_steps } => {
            let key = idempotency_key.ok_or_else(|| {
                validation(
                    format!("{path}.idempotency_key"),
                    "provider-idempotent operation requires a stable activity key",
                )
            })?;
            validate_stable_activity_key(
                &key.stable,
                state_id,
                item_key,
                &format!("{path}.idempotency_key.stable"),
            )?;
            for step in side_effect_steps {
                let retention = step.minimum_retention_ms.get();
                let margin = step.clock_safety_margin_ms.get();
                let usable = retention.checked_sub(margin).ok_or_else(|| {
                    validation(
                        format!("{path}.retry"),
                        format!(
                            "connector step `{}` has an invalid retention window",
                            step.step.as_str()
                        ),
                    )
                })?;
                if maximum_send_horizon_ms > usable {
                    return Err(validation(
                        format!("{path}.retry"),
                        format!(
                            "maximum send horizon {} ms exceeds usable provider retention {} ms for step `{}`",
                            format_number(maximum_send_horizon_ms),
                            format_number(usable),
                            step.step.as_str()
                        ),
                    ));
                }
                horizons.insert(step.step.as_str().to_owned(), maximum_send_horizon_ms);
            }
        }
    }
    let key = (
        context.process.source.clone(),
        connector.to_owned(),
        resolved.spec.operation,
    );
    let pinned = PinnedConnectorOperation {
        source: context.process.source.clone(),
        instance: connector.to_owned(),
        spec: resolved.spec,
        deployment_fingerprint: resolved.deployment_fingerprint,
    };
    if let Some(existing) = context.closure.connector_operations.get(&key) {
        if existing.deployment_fingerprint != pinned.deployment_fingerprint
            || !Arc::ptr_eq(&existing.spec, &pinned.spec)
        {
            return Err(validation(
                format!("{path}.operation"),
                "connector dependency changed during process compilation",
            ));
        }
    } else {
        context.closure.connector_operations.insert(key, pinned);
    }
    Ok((output_contract, horizons))
}

fn validate_stable_activity_key(
    key: &donat_metadata::ProcessStableActivityKey,
    state_id: &str,
    item_key: Option<&str>,
    path: &str,
) -> Result<(), PlanError> {
    if key.run != "id" {
        return Err(validation(
            format!("{path}.run"),
            "stable activity key run component must be `id`",
        ));
    }
    if key.state != state_id {
        return Err(validation(
            format!("{path}.state"),
            format!(
                "stable activity key state `{}` must equal owning state `{state_id}`",
                key.state
            ),
        ));
    }
    if key.item_key.as_deref() != item_key {
        return Err(validation(
            format!("{path}.item_key"),
            match item_key {
                Some(item_key) => {
                    format!("fan-out activity key must use item_key `{item_key}`")
                }
                None => "scalar activity key must not declare item_key".to_owned(),
            },
        ));
    }
    Ok(())
}

fn validate_error_routes(routes: Option<&ProcessErrorRoutes>, path: &str) -> Result<(), PlanError> {
    let Some(routes) = routes else {
        return Ok(());
    };
    if routes.routes.is_empty() {
        return Err(validation(
            format!("{path}.routes"),
            "on_error routes must not be empty",
        ));
    }
    let mut kinds = BTreeSet::new();
    for (route_index, route) in routes.routes.iter().enumerate() {
        if route.kinds.is_empty() {
            return Err(validation(
                format!("{path}.routes[{route_index}].kinds"),
                "error route kinds must not be empty",
            ));
        }
        for (kind_index, kind) in route.kinds.iter().enumerate() {
            let identity = format!("{kind:?}");
            if !kinds.insert(identity) {
                return Err(validation(
                    format!("{path}.routes[{route_index}].kinds[{kind_index}]"),
                    "an error kind may be routed only once",
                ));
            }
        }
    }
    Ok(())
}

fn compile_retry_horizon(
    retry: &ProcessRetry,
    schedule_to_start_ms: u64,
    start_to_close_ms: u64,
    path: &str,
) -> Result<u64, PlanError> {
    if retry.max_attempts == 0 {
        return Err(validation(
            format!("{path}.max_attempts"),
            "retry max_attempts must be greater than zero",
        ));
    }
    if retry.retry_on.is_empty() {
        return Err(validation(
            format!("{path}.retry_on"),
            "retry_on must not be empty",
        ));
    }
    let mut kinds = BTreeSet::new();
    for (index, kind) in retry.retry_on.iter().enumerate() {
        if !kinds.insert(format!("{kind:?}")) {
            return Err(validation(
                format!("{path}.retry_on[{index}]"),
                "retry_on contains a duplicate error kind",
            ));
        }
    }
    if retry.jitter != "deterministic_full" {
        return Err(validation(
            format!("{path}.jitter"),
            "retry jitter must be `deterministic_full`",
        ));
    }
    let initial = parse_duration(&retry.initial_interval, &format!("{path}.initial_interval"))?;
    let maximum = parse_duration(&retry.max_interval, &format!("{path}.max_interval"))?;
    if initial > maximum {
        return Err(validation(
            format!("{path}.max_interval"),
            "retry max_interval must be at least initial_interval",
        ));
    }
    let attempts = u64::from(retry.max_attempts);
    let per_attempt = start_to_close_ms
        .checked_add(MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS)
        .ok_or_else(|| validation(path, "activity horizon overflowed"))?;
    let mut horizon = attempts
        .checked_mul(per_attempt)
        .ok_or_else(|| validation(path, "activity horizon overflowed"))?;
    let mut retry_bound = initial;
    let mut retries_remaining = attempts - 1;
    while retries_remaining > 0 {
        horizon = horizon
            .checked_add(retry_bound)
            .and_then(|value| value.checked_add(schedule_to_start_ms))
            .ok_or_else(|| validation(path, "activity horizon overflowed"))?;
        retries_remaining -= 1;
        if retry_bound == maximum && retries_remaining > 0 {
            let saturated_retry = maximum
                .checked_add(schedule_to_start_ms)
                .and_then(|value| value.checked_mul(retries_remaining))
                .ok_or_else(|| validation(path, "activity horizon overflowed"))?;
            horizon = horizon
                .checked_add(saturated_retry)
                .ok_or_else(|| validation(path, "activity horizon overflowed"))?;
            break;
        }
        retry_bound = retry_bound.saturating_mul(2).min(maximum);
    }
    Ok(horizon)
}

fn parse_duration(source: &str, path: &str) -> Result<u64, PlanError> {
    let (digits, multiplier) = if let Some(value) = source.strip_suffix("ms") {
        (value, 1_u64)
    } else if let Some(value) = source.strip_suffix('s') {
        (value, 1_000)
    } else if let Some(value) = source.strip_suffix('m') {
        (value, 60_000)
    } else if let Some(value) = source.strip_suffix('h') {
        (value, 3_600_000)
    } else if let Some(value) = source.strip_suffix('d') {
        (value, 86_400_000)
    } else {
        return Err(validation(
            path,
            "duration must be a positive integer followed by ms, s, m, h, or d",
        ));
    };
    if digits.is_empty()
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(validation(
            path,
            "duration must use canonical positive base-10 spelling",
        ));
    }
    let value = digits
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(multiplier))
        .filter(|value| *value > 0)
        .ok_or_else(|| validation(path, "duration is zero or overflowed"))?;
    Ok(value)
}

fn format_number(value: u64) -> String {
    let source = value.to_string();
    let mut result = String::with_capacity(source.len() + source.len() / 3);
    for (index, byte) in source.bytes().enumerate() {
        if index > 0 && (source.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(char::from(byte));
    }
    result
}

fn compile_when_state(
    when: &donat_metadata::ProcessWhenState,
    context: &mut CompileContext<'_, '_>,
) -> Result<(ValueContractCatalog, CompiledProcessWhenState), PlanError> {
    let path = format!("{}.when", context.path);
    if when.cases.is_empty() {
        return Err(validation(
            format!("{path}.cases"),
            "when state must declare at least one case",
        ));
    }
    if let Some(table_name) = &when.decision_table {
        let descriptor = context
            .dependencies
            .decision_table(table_name)
            .ok_or_else(|| {
                validation(
                    format!("{path}.decision_table"),
                    format!("decision table `{table_name}` does not exist"),
                )
            })?;
        let input_contract = contract_from_rule_types(&descriptor.inputs);
        validate_binding_map(
            &when.input,
            &input_contract,
            context,
            &format!("{path}.input"),
        )?;
        let output_contract = contract_from_rule_types(&descriptor.output);
        for (case_index, case) in when.cases.iter().enumerate() {
            if case.rule.is_some() || !case.bindings.is_empty() || case.matches.is_none() {
                return Err(validation(
                    format!("{path}.cases[{case_index}]"),
                    "decision-table cases must contain only a matches object and next",
                ));
            }
            validate_literal_matches(
                case.matches.as_ref().expect("checked above"),
                &output_contract,
                &format!("{path}.cases[{case_index}].matches"),
            )?;
        }
        context
            .closure
            .decision_tables
            .insert(table_name.clone(), descriptor);
        return Ok((
            output_contract,
            CompiledProcessWhenState {
                decision_table: Some(CompiledProcessDecisionCall {
                    name: table_name.clone(),
                    input: when.input.clone(),
                }),
                cases: when
                    .cases
                    .iter()
                    .map(|case| CompiledProcessWhenCase {
                        predicate: CompiledProcessWhenPredicate::Matches(
                            case.matches
                                .clone()
                                .expect("decision cases were validated as literal matches"),
                        ),
                        next: case.next.clone(),
                    })
                    .collect(),
                default: when.default.clone(),
                literal_output_state: None,
            },
        ));
    }
    if !when.input.is_empty() {
        return Err(validation(
            format!("{path}.input"),
            "when input is valid only with decision_table",
        ));
    }

    let mut matched_contract: Option<ValueContractCatalog> = None;
    let mut literal_output_state: Option<String> = None;
    let mut compiled_cases = Vec::with_capacity(when.cases.len());
    for (case_index, case) in when.cases.iter().enumerate() {
        let case_path = format!("{path}.cases[{case_index}]");
        match (&case.matches, &case.rule) {
            (Some(matches), None) => {
                if !case.bindings.is_empty() {
                    return Err(validation(
                        format!("{case_path}.with"),
                        "literal match case must not declare rule bindings",
                    ));
                }
                let (source_state, source) = latest_matching_state_contract(matches, context)
                    .ok_or_else(|| {
                        validation(
                            format!("{case_path}.matches"),
                            "match fields do not resolve to one prior state output",
                        )
                    })?;
                if literal_output_state
                    .as_ref()
                    .is_some_and(|existing| existing != source_state)
                {
                    return Err(validation(
                        format!("{case_path}.matches"),
                        "all literal match cases in one when state must resolve to the same prior state output",
                    ));
                }
                literal_output_state.get_or_insert_with(|| source_state.to_owned());
                validate_literal_matches(matches, source, &format!("{case_path}.matches"))?;
                matched_contract.get_or_insert_with(|| source.clone());
                compiled_cases.push(CompiledProcessWhenCase {
                    predicate: CompiledProcessWhenPredicate::Matches(matches.clone()),
                    next: case.next.clone(),
                });
            }
            (None, Some(rule_name)) => {
                let descriptor = context.dependencies.rule(rule_name).ok_or_else(|| {
                    validation(
                        format!("{case_path}.rule"),
                        format!("rule `{rule_name}` does not exist"),
                    )
                })?;
                if descriptor.result != RuleType::Bool {
                    return Err(validation(
                        format!("{case_path}.rule"),
                        format!("when rule `{rule_name}` must return bool"),
                    ));
                }
                validate_binding_map(
                    &case.bindings,
                    &contract_from_rule_types(&descriptor.bindings),
                    context,
                    &format!("{case_path}.with"),
                )?;
                context.closure.rules.insert(rule_name.clone(), descriptor);
                compiled_cases.push(CompiledProcessWhenCase {
                    predicate: CompiledProcessWhenPredicate::Rule {
                        name: rule_name.clone(),
                        bindings: case.bindings.clone(),
                    },
                    next: case.next.clone(),
                });
            }
            _ => {
                return Err(validation(
                    case_path,
                    "when case must declare exactly one of matches or rule",
                ));
            }
        }
    }
    Ok((
        matched_contract.unwrap_or_else(empty_contract),
        CompiledProcessWhenState {
            decision_table: None,
            cases: compiled_cases,
            default: when.default.clone(),
            literal_output_state,
        },
    ))
}

fn latest_matching_state_contract<'a>(
    matches: &BTreeMap<String, Json>,
    context: &'a CompileContext<'_, '_>,
) -> Option<(&'a str, &'a ValueContractCatalog)> {
    context
        .state_indices
        .iter()
        .filter(|(_, index)| context.ancestors.contains(index))
        .filter_map(|(name, index)| {
            context
                .state_types
                .get(name)
                .filter(|contract| {
                    matches
                        .keys()
                        .all(|field| contract.roots.contains_key(field))
                })
                .map(|contract| (*index, name.as_str(), contract))
        })
        .max_by_key(|(index, _, _)| *index)
        .map(|(_, name, contract)| (name, contract))
}

fn validate_literal_matches(
    matches: &BTreeMap<String, Json>,
    contract: &ValueContractCatalog,
    path: &str,
) -> Result<(), PlanError> {
    if matches.is_empty() {
        return Err(validation(path, "matches object must not be empty"));
    }
    for (field, literal) in matches {
        let target = root_type(contract, field).ok_or_else(|| {
            validation(
                format!("{path}.{field}"),
                format!("matches references unknown output field `{field}`"),
            )
        })?;
        validate_json_literal(literal, target, &format!("{path}.{field}"))?;
    }
    Ok(())
}

fn compile_wait_state(
    wait: &ProcessWaitState,
    context: &mut CompileContext<'_, '_>,
) -> Result<ValueContractCatalog, PlanError> {
    match wait {
        ProcessWaitState::Signal(signal) => compile_signal_wait(signal, context),
        ProcessWaitState::Timer(timer) => {
            let path = format!("{}.wait.timer", context.path);
            let descriptor = context
                .dependencies
                .decision_table(&timer.timer.decision_table)
                .ok_or_else(|| {
                    validation(
                        format!("{path}.decision_table"),
                        format!(
                            "decision table `{}` does not exist",
                            timer.timer.decision_table
                        ),
                    )
                })?;
            validate_binding_map(
                &timer.timer.bindings,
                &contract_from_rule_types(&descriptor.inputs),
                context,
                &format!("{path}.with"),
            )?;
            let output_type = descriptor.output.get(&timer.timer.output).ok_or_else(|| {
                validation(
                    format!("{path}.output"),
                    format!(
                        "decision table `{}` has no output `{}`",
                        descriptor.name, timer.timer.output
                    ),
                )
            })?;
            if !matches!(
                strip_rule_nullable(output_type),
                RuleType::Int | RuleType::Int64
            ) {
                return Err(validation(
                    format!("{path}.output"),
                    "timer decision output must be an integer duration in seconds",
                ));
            }
            context
                .closure
                .decision_tables
                .insert(descriptor.name.clone(), descriptor.clone());
            Ok(contract_from_rule_types(&descriptor.output))
        }
    }
}

fn compile_signal_wait(
    wait: &ProcessSignalWait,
    context: &mut CompileContext<'_, '_>,
) -> Result<ValueContractCatalog, PlanError> {
    let path = format!("{}.wait", context.path);
    let signal = context.signals.get(&wait.signal).ok_or_else(|| {
        validation(
            format!("{path}.signal"),
            format!("signal `{}` is not declared by the process", wait.signal),
        )
    })?;
    if wait.role.is_empty() || wait.role == "admin" {
        return Err(validation(
            format!("{path}.role"),
            "signal wait role must be an explicit non-admin role",
        ));
    }
    match &signal.role {
        Some(role) if role != &wait.role => {
            return Err(validation(
                format!("{path}.role"),
                format!(
                    "signal `{}` is restricted to role `{role}`, not `{}`",
                    wait.signal, wait.role
                ),
            ));
        }
        None if !context
            .process
            .permissions
            .iter()
            .any(|permission| permission.role == wait.role) =>
        {
            return Err(validation(
                format!("{path}.role"),
                "an unrestricted signal wait role must be a process caller role",
            ));
        }
        _ => {}
    }
    if wait.verification != "required" {
        return Err(validation(
            format!("{path}.verification"),
            "signal verification must be `required`",
        ));
    }
    validate_binding_map(
        &wait.correlate,
        &signal.correlation,
        context,
        &format!("{path}.correlate"),
    )?;
    match &wait.deadline {
        ProcessDeadline::Duration(duration) => {
            parse_duration(duration, &format!("{path}.deadline"))?;
        }
        ProcessDeadline::Value(value) => {
            let inferred = infer_value(value, context, &format!("{path}.deadline"))?;
            let type_ref = inferred.known().ok_or_else(|| {
                validation(
                    format!("{path}.deadline"),
                    "signal deadline cannot be an untyped null literal",
                )
            })?;
            if !matches!(
                type_ref.value_type,
                ValueType::Scalar {
                    scalar: ValueScalar::Timestamp | ValueScalar::TimestampTz
                }
            ) || type_ref.nullable
            {
                return Err(validation(
                    format!("{path}.deadline"),
                    "signal deadline must be a non-null timestamp",
                ));
            }
        }
    }
    let mut roots = signal.correlation.roots.clone();
    for (name, field) in &signal.payload.roots {
        if roots.insert(name.clone(), field.clone()).is_some() {
            return Err(validation(
                format!("{path}.signal"),
                format!("signal field `{name}` collides across correlation and payload"),
            ));
        }
    }
    Ok(ValueContractCatalog {
        roots,
        named_objects: BTreeMap::new(),
    })
}

fn compile_for_each_state(
    for_each: &ProcessForEachState,
    state_id: &str,
    context: &mut CompileContext<'_, '_>,
) -> Result<(ValueContractCatalog, BTreeMap<String, u64>), PlanError> {
    let (input, item_key, max_items, max_concurrency, completion, preserve_input) = match for_each {
        ProcessForEachState::Command {
            input,
            item_key,
            max_items,
            max_concurrency,
            completion,
            preserve_input,
            ..
        }
        | ProcessForEachState::Request {
            input,
            item_key,
            max_items,
            max_concurrency,
            completion,
            preserve_input,
            ..
        } => (
            input,
            item_key,
            *max_items,
            *max_concurrency,
            completion,
            *preserve_input,
        ),
    };
    let path = format!("{}.for_each", context.path);
    if max_items == 0 || max_items > 256 {
        return Err(validation(
            format!("{path}.max_items"),
            "for_each max_items must be between 1 and 256",
        ));
    }
    if max_concurrency == 0 || max_concurrency > max_items {
        return Err(validation(
            format!("{path}.max_concurrency"),
            "for_each max_concurrency must be between 1 and max_items",
        ));
    }
    if completion != "collect" {
        return Err(validation(
            format!("{path}.completion"),
            "for_each completion must be `collect`",
        ));
    }
    let input_type = infer_value(input, context, &format!("{path}.input"))?
        .known()
        .cloned()
        .ok_or_else(|| validation(format!("{path}.input"), "for_each input cannot be null"))?;
    let ValueType::List { element } = &input_type.value_type else {
        return Err(validation(
            format!("{path}.input"),
            "for_each input must be a bounded list",
        ));
    };
    let item_type = (**element).clone();
    let item_fields = object_fields(&item_type).ok_or_else(|| {
        validation(
            format!("{path}.input"),
            "for_each list elements must be objects",
        )
    })?;
    let item_key_type = item_fields.get(item_key).ok_or_else(|| {
        validation(
            format!("{path}.item_key"),
            format!("item_key `{item_key}` is not present on the input item"),
        )
    })?;
    if item_key_type.type_ref.nullable || !scalar_like(&item_key_type.type_ref) {
        return Err(validation(
            format!("{path}.item_key"),
            "for_each item_key must name a non-null scalar or enum",
        ));
    }

    let previous_item = context.item.replace(item_type.clone());
    let activity = match for_each {
        ProcessForEachState::Command { command, .. } => {
            let descriptor = compile_command_activity(
                &command.name,
                &command.run_as,
                &command.arguments,
                context,
                &format!("{path}.command"),
            )?;
            (descriptor.result, BTreeMap::new())
        }
        ProcessForEachState::Request { request, .. } => compile_request_activity(
            request,
            state_id,
            item_key,
            context,
            &format!("{path}.request"),
        )?,
    };
    context.item = previous_item;
    let (activity_result, horizons) = activity;

    let activity_result_object = catalog_root_object(&activity_result);
    let successful_item = if preserve_input {
        merge_object_types(&item_type, &activity_result_object, &path)?
    } else {
        activity_result_object.clone()
    };
    let failure = failure_item_type(&item_type, item_key);
    Ok((
        ValueContractCatalog {
            roots: BTreeMap::from([
                (
                    "successful_items".to_owned(),
                    required_field(list_type(successful_item)),
                ),
                (
                    "failed_items".to_owned(),
                    required_field(list_type(failure)),
                ),
                (
                    "ordered_results".to_owned(),
                    required_field(list_type(activity_result_object)),
                ),
            ]),
            named_objects: BTreeMap::new(),
        },
        horizons,
    ))
}

fn compile_request_activity(
    request: &ProcessRequestActivity,
    state_id: &str,
    item_key: &str,
    context: &mut CompileContext<'_, '_>,
    path: &str,
) -> Result<(ValueContractCatalog, BTreeMap<String, u64>), PlanError> {
    compile_request(
        &request.connector,
        &request.operation,
        &request.input,
        request.idempotency_key.as_ref(),
        &request.timeout,
        &request.retry,
        request.on_error.as_ref(),
        state_id,
        Some(item_key),
        context,
        path,
    )
}

fn failure_item_type(item: &TypeRef, _item_key: &str) -> TypeRef {
    let mut fields = object_fields(item).cloned().unwrap_or_default();
    for (name, type_ref) in [
        ("item_key", scalar_type(ValueScalar::String, false)),
        ("stage", scalar_type(ValueScalar::String, false)),
        ("code", scalar_type(ValueScalar::String, false)),
        ("safe_message", scalar_type(ValueScalar::String, false)),
        (
            "requires_reconciliation",
            scalar_type(ValueScalar::Boolean, false),
        ),
        ("activity_key", scalar_type(ValueScalar::String, false)),
    ] {
        fields.insert(name.to_owned(), required_field(type_ref));
    }
    TypeRef {
        nullable: false,
        value_type: ValueType::Object { fields },
    }
}

fn catalog_root_object(contract: &ValueContractCatalog) -> TypeRef {
    TypeRef {
        nullable: false,
        value_type: ValueType::Object {
            fields: contract.roots.clone(),
        },
    }
}

fn merge_object_types(left: &TypeRef, right: &TypeRef, path: &str) -> Result<TypeRef, PlanError> {
    let mut fields = object_fields(left)
        .ok_or_else(|| validation(path, "fan-out item must be an object"))?
        .clone();
    for (name, field) in object_fields(right)
        .ok_or_else(|| validation(path, "fan-out activity result must be an object"))?
    {
        if let Some(existing) = fields.get(name) {
            if existing.type_ref != field.type_ref {
                return Err(validation(
                    path,
                    format!("fan-out result field `{name}` conflicts with its input item type"),
                ));
            }
        } else {
            fields.insert(name.clone(), field.clone());
        }
    }
    Ok(TypeRef {
        nullable: false,
        value_type: ValueType::Object { fields },
    })
}

enum InferredValue {
    Null,
    Known(TypeRef),
}

impl InferredValue {
    fn known(&self) -> Option<&TypeRef> {
        match self {
            Self::Null => None,
            Self::Known(type_ref) => Some(type_ref),
        }
    }
}

fn validate_binding_map(
    values: &BTreeMap<String, ProcessValue>,
    target: &ValueContractCatalog,
    context: &CompileContext<'_, '_>,
    path: &str,
) -> Result<(), PlanError> {
    for (name, field) in &target.roots {
        let Some(value) = values.get(name) else {
            if field.required {
                return Err(validation(
                    path,
                    format!("required binding `{name}` is missing"),
                ));
            }
            continue;
        };
        validate_value_against(value, &field.type_ref, context, &format!("{path}.{name}"))?;
    }
    if let Some(extra) = values.keys().find(|name| !target.roots.contains_key(*name)) {
        return Err(validation(
            format!("{path}.{extra}"),
            format!("binding `{extra}` is not declared by the target contract"),
        ));
    }
    Ok(())
}

fn validate_value_against(
    value: &ProcessValue,
    target: &TypeRef,
    context: &CompileContext<'_, '_>,
    path: &str,
) -> Result<(), PlanError> {
    if let ProcessValue::Literal { literal } = value {
        return validate_json_literal(literal, target, path);
    }
    let actual = infer_value(value, context, path)?;
    let Some(actual) = actual.known() else {
        if target.nullable {
            return Ok(());
        }
        return Err(validation(
            path,
            "null is not assignable to a non-null type",
        ));
    };
    if !type_assignable(target, actual) {
        return Err(type_mismatch(path, target, actual));
    }
    Ok(())
}

fn infer_value(
    value: &ProcessValue,
    context: &CompileContext<'_, '_>,
    path: &str,
) -> Result<InferredValue, PlanError> {
    let type_ref = match value {
        ProcessValue::Input {
            input,
            as_,
            require_non_null,
        } => {
            let mut source = root_type(context.input, input).cloned().ok_or_else(|| {
                validation(path, format!("process input `{input}` does not exist"))
            })?;
            if *require_non_null {
                source.nullable = false;
            }
            if let Some(as_) = as_ {
                source = refine_process_scalar(source, as_, context.dependencies, path)?;
            }
            source
        }
        ProcessValue::State {
            state,
            field,
            project,
            as_,
            require_non_null,
        } => {
            let state_index = context
                .state_indices
                .get(state)
                .ok_or_else(|| validation(path, format!("state `{state}` does not exist")))?;
            if !context.ancestors.contains(state_index) {
                return Err(validation(
                    path,
                    format!("state reference `{state}.{field}` must target a transition ancestor"),
                ));
            }
            let contract = context.state_types.get(state).ok_or_else(|| {
                validation(
                    path,
                    format!("state `{state}` output is not available at this transition"),
                )
            })?;
            let mut source = root_type(contract, field).cloned().ok_or_else(|| {
                validation(
                    path,
                    format!("state `{state}` has no output field `{field}`"),
                )
            })?;
            if let Some(fields) = project {
                let ValueType::List { element } = &source.value_type else {
                    return Err(validation(path, "state project requires a list value"));
                };
                let object = object_fields(element).ok_or_else(|| {
                    validation(path, "state project requires object list elements")
                })?;
                let mut projected = BTreeMap::new();
                for name in fields {
                    let field = object.get(name).ok_or_else(|| {
                        validation(path, format!("project references unknown field `{name}`"))
                    })?;
                    if projected.insert(name.clone(), field.clone()).is_some() {
                        return Err(validation(
                            path,
                            format!("project field `{name}` is duplicated"),
                        ));
                    }
                }
                source = list_type(TypeRef {
                    nullable: false,
                    value_type: ValueType::Object { fields: projected },
                });
            }
            if *require_non_null {
                source.nullable = false;
            }
            if let Some(as_) = as_ {
                source = refine_process_scalar(source, as_, context.dependencies, path)?;
            }
            source
        }
        ProcessValue::Item { item, as_ } => {
            let item_type = context.item.as_ref().ok_or_else(|| {
                validation(path, "item references are valid only inside for_each")
            })?;
            let mut source = object_fields(item_type)
                .and_then(|fields| fields.get(item))
                .map(|field| field.type_ref.clone())
                .ok_or_else(|| validation(path, format!("for_each item has no field `{item}`")))?;
            if let Some(as_) = as_ {
                source = refine_process_scalar(source, as_, context.dependencies, path)?;
            }
            source
        }
        ProcessValue::Literal { literal } => {
            return infer_json_literal(literal, path);
        }
        ProcessValue::ActivityKey { activity_key, as_ } => {
            if activity_key.is_empty() {
                return Err(validation(path, "activity key component must not be empty"));
            }
            match as_.as_deref() {
                None => scalar_type(ValueScalar::String, false),
                Some("uuid") => scalar_type(ValueScalar::Uuid, false),
                Some(_) => {
                    return Err(validation(
                        path,
                        "activity key cast supports only deterministic `as: uuid`",
                    ));
                }
            }
        }
        ProcessValue::ActivityKeyForState {
            activity_key_for_state,
            as_,
        } => {
            let state_index = context
                .state_indices
                .get(activity_key_for_state)
                .ok_or_else(|| {
                    validation(
                        path,
                        format!(
                            "activity_key_for_state references unknown state `{activity_key_for_state}`"
                        ),
                    )
                })?;
            if !context.ancestors.contains(state_index) {
                return Err(validation(
                    path,
                    "activity_key_for_state must reference a transition ancestor",
                ));
            }
            match as_.as_deref() {
                None => scalar_type(ValueScalar::String, false),
                Some("uuid") => scalar_type(ValueScalar::Uuid, false),
                Some(_) => {
                    return Err(validation(
                        path,
                        "activity_key_for_state cast supports only deterministic `as: uuid`",
                    ));
                }
            }
        }
        ProcessValue::Run { run } => {
            if run != "id" {
                return Err(validation(path, "run value supports only `{ run: id }`"));
            }
            scalar_type(ValueScalar::Uuid, false)
        }
        ProcessValue::WorkflowTime { workflow_time } => {
            if workflow_time != "now" {
                return Err(validation(
                    path,
                    "workflow_time supports only `{ workflow_time: now }`",
                ));
            }
            scalar_type(ValueScalar::TimestampTz, false)
        }
        ProcessValue::SessionVariable { session_variable } => {
            if session_variable.is_empty() {
                return Err(validation(path, "session variable must not be empty"));
            }
            scalar_type(ValueScalar::String, false)
        }
        ProcessValue::BoundedConcat { bounded_concat } => {
            if bounded_concat.inputs.is_empty()
                || bounded_concat.maximum_lists == 0
                || bounded_concat.maximum_items == 0
                || bounded_concat.inputs.len() > bounded_concat.maximum_lists as usize
            {
                return Err(validation(
                    path,
                    "bounded_concat limits must be positive and cover every input list",
                ));
            }
            let mut common: Option<TypeRef> = None;
            for (index, input) in bounded_concat.inputs.iter().enumerate() {
                let input_type = infer_value(input, context, &format!("{path}.inputs[{index}]"))?
                    .known()
                    .cloned()
                    .ok_or_else(|| {
                        validation(format!("{path}.inputs[{index}]"), "list cannot be null")
                    })?;
                let ValueType::List { element } = input_type.value_type else {
                    return Err(validation(
                        format!("{path}.inputs[{index}]"),
                        "bounded_concat input must be a list",
                    ));
                };
                common = Some(match common {
                    None => *element,
                    Some(current) => common_type(&current, &element).ok_or_else(|| {
                        validation(
                            path,
                            "bounded_concat input lists have incompatible element types",
                        )
                    })?,
                });
            }
            list_type(common.expect("non-empty checked above"))
        }
        ProcessValue::BoundedFlatten { bounded_flatten } => {
            if bounded_flatten.maximum_lists == 0 || bounded_flatten.maximum_items == 0 {
                return Err(validation(path, "bounded_flatten limits must be positive"));
            }
            let source = infer_value(&bounded_flatten.from, context, &format!("{path}.from"))?
                .known()
                .cloned()
                .ok_or_else(|| validation(format!("{path}.from"), "list cannot be null"))?;
            let ValueType::List { element } = source.value_type else {
                return Err(validation(
                    format!("{path}.from"),
                    "bounded_flatten source must be a list",
                ));
            };
            let flattened = if let Some(field) = &bounded_flatten.field {
                let object = object_fields(&element).ok_or_else(|| {
                    validation(
                        format!("{path}.from"),
                        "bounded_flatten field requires object list elements",
                    )
                })?;
                let nested = object.get(field).ok_or_else(|| {
                    validation(
                        format!("{path}.field"),
                        format!("bounded_flatten source has no field `{field}`"),
                    )
                })?;
                let ValueType::List { element } = &nested.type_ref.value_type else {
                    return Err(validation(
                        format!("{path}.field"),
                        "bounded_flatten selected field must be a list",
                    ));
                };
                (**element).clone()
            } else {
                let ValueType::List { element } = &element.value_type else {
                    return Err(validation(
                        format!("{path}.from"),
                        "bounded_flatten without field requires a list of lists",
                    ));
                };
                (**element).clone()
            };
            let flattened = if let Some(project) = &bounded_flatten.project {
                let object = object_fields(&flattened).ok_or_else(|| {
                    validation(path, "bounded_flatten project requires object items")
                })?;
                let mut fields = BTreeMap::new();
                for (target, source) in project {
                    let field = object.get(source).ok_or_else(|| {
                        validation(
                            format!("{path}.project.{target}"),
                            format!("bounded_flatten source has no field `{source}`"),
                        )
                    })?;
                    fields.insert(target.clone(), field.clone());
                }
                TypeRef {
                    nullable: false,
                    value_type: ValueType::Object { fields },
                }
            } else {
                flattened
            };
            list_type(flattened)
        }
    };
    Ok(InferredValue::Known(type_ref))
}

fn infer_json_literal(literal: &Json, path: &str) -> Result<InferredValue, PlanError> {
    let type_ref =
        match literal {
            Json::Null => return Ok(InferredValue::Null),
            Json::Bool(_) => scalar_type(ValueScalar::Boolean, false),
            Json::String(_) => scalar_type(ValueScalar::String, false),
            Json::Number(number) if number.as_i64().is_some() => {
                let value = number.as_i64().expect("checked");
                scalar_type(
                    if i32::try_from(value).is_ok() {
                        ValueScalar::Int32
                    } else {
                        ValueScalar::Int64
                    },
                    false,
                )
            }
            Json::Number(number) if number.as_u64().is_some() => {
                scalar_type(ValueScalar::UInt64, false)
            }
            Json::Number(_) => scalar_type(ValueScalar::Decimal, false),
            Json::Array(values) => {
                let mut common: Option<TypeRef> = None;
                for (index, value) in values.iter().enumerate() {
                    let inferred = infer_json_literal(value, &format!("{path}[{index}]"))?;
                    let Some(type_ref) = inferred.known() else {
                        return Err(validation(
                            format!("{path}[{index}]"),
                            "list literal null needs a typed destination",
                        ));
                    };
                    common = Some(match common {
                        None => type_ref.clone(),
                        Some(current) => common_type(&current, type_ref).ok_or_else(|| {
                            validation(path, "list literal elements have incompatible types")
                        })?,
                    });
                }
                list_type(common.ok_or_else(|| {
                    validation(path, "empty list literal needs a typed destination")
                })?)
            }
            Json::Object(values) => TypeRef {
                nullable: false,
                value_type: ValueType::Object {
                    fields: values
                        .iter()
                        .map(|(name, value)| {
                            let inferred = infer_json_literal(value, &format!("{path}.{name}"))?;
                            let type_ref = inferred.known().cloned().ok_or_else(|| {
                                validation(
                                    format!("{path}.{name}"),
                                    "object literal null needs a typed destination",
                                )
                            })?;
                            Ok((name.clone(), required_field(type_ref)))
                        })
                        .collect::<Result<_, PlanError>>()?,
                },
            },
        };
    Ok(InferredValue::Known(type_ref))
}

fn refine_process_scalar(
    source: TypeRef,
    target: &str,
    dependencies: &dyn ProcessDependencyCatalog,
    path: &str,
) -> Result<TypeRef, PlanError> {
    if target == "string" {
        if !scalar_like(&source) {
            return Err(validation(
                path,
                "process scalar cast requires a scalar or enum source",
            ));
        }
        return Ok(scalar_type(ValueScalar::String, source.nullable));
    }

    if target.is_empty() || target.contains('!') || target.contains('[') || target.contains(']') {
        return Err(validation(
            path,
            "process nominal refinement must name one declared enum; use `require_non_null` separately",
        ));
    }
    let mut refined = type_from_string(target, dependencies, path)?;
    if !matches!(
        &source.value_type,
        ValueType::Scalar {
            scalar: ValueScalar::String
        }
    ) || !matches!(&refined.value_type, ValueType::Enum { .. })
    {
        return Err(validation(
            path,
            "process nominal refinement supports only string-to-declared-enum validation",
        ));
    }
    refined.nullable = source.nullable;
    Ok(refined)
}

fn type_from_string(
    source: &str,
    dependencies: &dyn ProcessDependencyCatalog,
    path: &str,
) -> Result<TypeRef, PlanError> {
    let parsed = TypeRef::parse(source)
        .map_err(|error| validation(path, format!("invalid type `{source}`: {error}")))?;
    normalize_type(
        &parsed,
        &BTreeMap::new(),
        dependencies,
        &mut BTreeSet::new(),
        path,
    )
}

fn normalize_contract(
    contract: &ValueContractCatalog,
    dependencies: &dyn ProcessDependencyCatalog,
    path: &str,
) -> Result<ValueContractCatalog, PlanError> {
    let roots = contract
        .roots
        .iter()
        .map(|(name, field)| {
            Ok((
                name.clone(),
                ValueContractField {
                    required: field.required,
                    type_ref: normalize_type(
                        &field.type_ref,
                        &contract.named_objects,
                        dependencies,
                        &mut BTreeSet::new(),
                        &format!("{path}.{name}"),
                    )?,
                },
            ))
        })
        .collect::<Result<_, PlanError>>()?;
    Ok(ValueContractCatalog {
        roots,
        named_objects: BTreeMap::new(),
    })
}

fn normalize_type(
    type_ref: &TypeRef,
    named_objects: &BTreeMap<String, donat_ir::ValueObjectContract>,
    dependencies: &dyn ProcessDependencyCatalog,
    resolving: &mut BTreeSet<String>,
    path: &str,
) -> Result<TypeRef, PlanError> {
    let value_type = match &type_ref.value_type {
        ValueType::Scalar { scalar } => ValueType::Scalar {
            scalar: scalar.clone(),
        },
        ValueType::Enum { name, values } => ValueType::Enum {
            name: name.clone(),
            values: values.clone(),
        },
        ValueType::Object { fields } => ValueType::Object {
            fields: normalize_object_fields(fields, named_objects, dependencies, resolving, path)?,
        },
        ValueType::List { element } => ValueType::List {
            element: Box::new(normalize_type(
                element,
                named_objects,
                dependencies,
                resolving,
                path,
            )?),
        },
        ValueType::Ref { name } => {
            if !resolving.insert(name.clone()) {
                return Err(validation(
                    path,
                    format!("recursive value type `{name}` is not executable"),
                ));
            }
            let resolved = if let Some(object) = named_objects.get(name) {
                ValueType::Object {
                    fields: normalize_object_fields(
                        &object.fields,
                        named_objects,
                        dependencies,
                        resolving,
                        path,
                    )?,
                }
            } else if let Some(rule_type) = dependencies.declared_type(name) {
                rule_type_ref(&rule_type).value_type
            } else if name == "bigint" {
                ValueType::Scalar {
                    scalar: ValueScalar::Int64,
                }
            } else {
                return Err(validation(path, format!("unknown named type `{name}`")));
            };
            resolving.remove(name);
            resolved
        }
    };
    Ok(TypeRef {
        nullable: type_ref.nullable,
        value_type,
    })
}

fn normalize_object_fields(
    fields: &BTreeMap<String, ValueContractField>,
    named_objects: &BTreeMap<String, donat_ir::ValueObjectContract>,
    dependencies: &dyn ProcessDependencyCatalog,
    resolving: &mut BTreeSet<String>,
    path: &str,
) -> Result<BTreeMap<String, ValueContractField>, PlanError> {
    fields
        .iter()
        .map(|(name, field)| {
            Ok((
                name.clone(),
                ValueContractField {
                    required: field.required,
                    type_ref: normalize_type(
                        &field.type_ref,
                        named_objects,
                        dependencies,
                        resolving,
                        &format!("{path}.{name}"),
                    )?,
                },
            ))
        })
        .collect()
}

fn contract_from_rule_types(fields: &BTreeMap<String, RuleType>) -> ValueContractCatalog {
    ValueContractCatalog {
        roots: fields
            .iter()
            .map(|(name, type_)| {
                let type_ref = rule_type_ref(type_);
                (
                    name.clone(),
                    ValueContractField {
                        required: !type_ref.nullable,
                        type_ref,
                    },
                )
            })
            .collect(),
        named_objects: BTreeMap::new(),
    }
}

fn rule_type_ref(type_: &RuleType) -> TypeRef {
    match type_ {
        RuleType::Nullable(inner) => {
            let mut type_ref = rule_type_ref(inner);
            type_ref.nullable = true;
            type_ref
        }
        RuleType::Bool => scalar_type(ValueScalar::Boolean, false),
        RuleType::String => scalar_type(ValueScalar::String, false),
        RuleType::Int => scalar_type(ValueScalar::Int32, false),
        RuleType::Int64 => scalar_type(ValueScalar::Int64, false),
        RuleType::Decimal => scalar_type(ValueScalar::Decimal, false),
        RuleType::Uuid => scalar_type(ValueScalar::Uuid, false),
        RuleType::Date => scalar_type(ValueScalar::Date, false),
        RuleType::Timestamp => scalar_type(ValueScalar::TimestampTz, false),
        RuleType::Enum { name, symbols } => TypeRef {
            nullable: false,
            value_type: ValueType::Enum {
                name: name.clone(),
                values: symbols.clone(),
            },
        },
        RuleType::List(element) => list_type(rule_type_ref(element)),
        RuleType::Object { fields, .. } => TypeRef {
            nullable: false,
            value_type: ValueType::Object {
                fields: fields
                    .iter()
                    .map(|(name, type_)| {
                        let type_ref = rule_type_ref(type_);
                        (
                            name.clone(),
                            ValueContractField {
                                required: !type_ref.nullable,
                                type_ref,
                            },
                        )
                    })
                    .collect(),
            },
        },
        RuleType::OpaqueJson { .. } => scalar_type(ValueScalar::Json, false),
    }
}

fn strip_rule_nullable(type_: &RuleType) -> &RuleType {
    match type_ {
        RuleType::Nullable(inner) => strip_rule_nullable(inner),
        other => other,
    }
}

fn root_type<'a>(contract: &'a ValueContractCatalog, name: &str) -> Option<&'a TypeRef> {
    contract.roots.get(name).map(|field| &field.type_ref)
}

fn object_fields(type_ref: &TypeRef) -> Option<&BTreeMap<String, ValueContractField>> {
    match &type_ref.value_type {
        ValueType::Object { fields } => Some(fields),
        _ => None,
    }
}

fn scalar_like(type_ref: &TypeRef) -> bool {
    matches!(
        type_ref.value_type,
        ValueType::Scalar { .. } | ValueType::Enum { .. }
    )
}

fn scalar_type(scalar: ValueScalar, nullable: bool) -> TypeRef {
    TypeRef {
        nullable,
        value_type: ValueType::Scalar { scalar },
    }
}

fn list_type(element: TypeRef) -> TypeRef {
    TypeRef {
        nullable: false,
        value_type: ValueType::List {
            element: Box::new(element),
        },
    }
}

fn required_field(type_ref: TypeRef) -> ValueContractField {
    ValueContractField {
        required: !type_ref.nullable,
        type_ref,
    }
}

fn empty_contract() -> ValueContractCatalog {
    ValueContractCatalog {
        roots: BTreeMap::new(),
        named_objects: BTreeMap::new(),
    }
}

fn type_assignable(target: &TypeRef, source: &TypeRef) -> bool {
    if source.nullable && !target.nullable {
        return false;
    }
    match (&target.value_type, &source.value_type) {
        (
            ValueType::Scalar {
                scalar: ValueScalar::Json,
            },
            _,
        ) => true,
        (
            ValueType::Scalar {
                scalar: target_scalar,
            },
            ValueType::Scalar {
                scalar: source_scalar,
            },
        ) => target_scalar == source_scalar,
        (
            ValueType::Enum {
                name: target_name,
                values: target_values,
            },
            ValueType::Enum {
                name: source_name,
                values: source_values,
            },
        ) => target_name == source_name && target_values == source_values,
        (
            ValueType::List {
                element: target_element,
            },
            ValueType::List {
                element: source_element,
            },
        ) => type_assignable(target_element, source_element),
        (
            ValueType::Object {
                fields: target_fields,
            },
            ValueType::Object {
                fields: source_fields,
            },
        ) => target_fields.iter().all(|(name, target_field)| {
            source_fields.get(name).is_some_and(|source_field| {
                (!target_field.required || source_field.required)
                    && type_assignable(&target_field.type_ref, &source_field.type_ref)
            })
        }),
        _ => false,
    }
}

fn common_type(left: &TypeRef, right: &TypeRef) -> Option<TypeRef> {
    let nullable = left.nullable || right.nullable;
    let value_type = match (&left.value_type, &right.value_type) {
        (
            ValueType::Scalar {
                scalar: left_scalar,
            },
            ValueType::Scalar {
                scalar: right_scalar,
            },
        ) if left_scalar == right_scalar => ValueType::Scalar {
            scalar: left_scalar.clone(),
        },
        (
            ValueType::Enum {
                name: left_name,
                values: left_values,
            },
            ValueType::Enum {
                name: right_name,
                values: right_values,
            },
        ) if left_name == right_name && left_values == right_values => ValueType::Enum {
            name: left_name.clone(),
            values: left_values.clone(),
        },
        (
            ValueType::List {
                element: left_element,
            },
            ValueType::List {
                element: right_element,
            },
        ) => ValueType::List {
            element: Box::new(common_type(left_element, right_element)?),
        },
        (
            ValueType::Object {
                fields: left_fields,
            },
            ValueType::Object {
                fields: right_fields,
            },
        ) => {
            let fields = left_fields
                .iter()
                .filter_map(|(name, left_field)| {
                    let right_field = right_fields.get(name)?;
                    let type_ref = common_type(&left_field.type_ref, &right_field.type_ref)?;
                    Some((
                        name.clone(),
                        ValueContractField {
                            required: left_field.required && right_field.required,
                            type_ref,
                        },
                    ))
                })
                .collect();
            ValueType::Object { fields }
        }
        _ => return None,
    };
    Some(TypeRef {
        nullable,
        value_type,
    })
}

fn validate_json_literal(literal: &Json, target: &TypeRef, path: &str) -> Result<(), PlanError> {
    if literal.is_null() {
        return if target.nullable {
            Ok(())
        } else {
            Err(validation(
                path,
                "null is not assignable to a non-null type",
            ))
        };
    }
    let valid = match (&target.value_type, literal) {
        (
            ValueType::Scalar {
                scalar: ValueScalar::Json,
            },
            _,
        ) => true,
        (
            ValueType::Scalar {
                scalar: ValueScalar::Boolean,
            },
            Json::Bool(_),
        ) => true,
        (
            ValueType::Scalar {
                scalar: ValueScalar::String,
            },
            Json::String(_),
        ) => true,
        (
            ValueType::Scalar {
                scalar: ValueScalar::Int32,
            },
            Json::Number(number),
        ) => number
            .as_i64()
            .is_some_and(|value| i32::try_from(value).is_ok()),
        (
            ValueType::Scalar {
                scalar: ValueScalar::Int64,
            },
            Json::Number(number),
        ) => number.as_i64().is_some(),
        (
            ValueType::Scalar {
                scalar: ValueScalar::UInt64,
            },
            Json::Number(number),
        ) => number.as_u64().is_some(),
        (
            ValueType::Scalar {
                scalar: ValueScalar::Decimal,
            },
            Json::Number(_),
        ) => true,
        (
            ValueType::Scalar {
                scalar:
                    ValueScalar::Uuid
                    | ValueScalar::Date
                    | ValueScalar::Timestamp
                    | ValueScalar::TimestampTz
                    | ValueScalar::Custom { .. },
            },
            Json::String(_),
        ) => true,
        (ValueType::Enum { values, .. }, Json::String(value)) => values.contains(value),
        (ValueType::List { element }, Json::Array(values)) => {
            for (index, value) in values.iter().enumerate() {
                validate_json_literal(value, element, &format!("{path}[{index}]"))?;
            }
            true
        }
        (ValueType::Object { fields }, Json::Object(values)) => {
            for (name, field) in fields {
                match values.get(name) {
                    Some(value) => {
                        validate_json_literal(value, &field.type_ref, &format!("{path}.{name}"))?
                    }
                    None if field.required => {
                        return Err(validation(
                            path,
                            format!("object literal is missing required field `{name}`"),
                        ));
                    }
                    None => {}
                }
            }
            if let Some(extra) = values.keys().find(|name| !fields.contains_key(*name)) {
                return Err(validation(
                    format!("{path}.{extra}"),
                    format!("object literal contains unknown field `{extra}`"),
                ));
            }
            true
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(validation(
            path,
            format!("literal is not assignable to {}", display_type(target)),
        ))
    }
}

fn type_mismatch(path: impl Into<String>, target: &TypeRef, source: &TypeRef) -> PlanError {
    validation(
        path,
        format!(
            "{} is not assignable to {}",
            display_type(source),
            display_type(target)
        ),
    )
}

fn display_type(type_ref: &TypeRef) -> String {
    let body = match &type_ref.value_type {
        ValueType::Scalar { scalar } => format!("{scalar:?}"),
        ValueType::Enum { name, .. } => format!("enum {name}"),
        ValueType::Object { fields } => format!(
            "object{{{}}}",
            fields
                .iter()
                .map(|(name, field)| {
                    let optional = if field.required { "" } else { "?" };
                    format!("{name}{optional}:{}", display_type(&field.type_ref))
                })
                .collect::<Vec<_>>()
                .join(",")
        ),
        ValueType::List { element } => format!("[{}]", display_type(element)),
        ValueType::Ref { name } => name.clone(),
    };
    if type_ref.nullable {
        format!("nullable {body}")
    } else {
        body
    }
}

fn definition_fingerprint(
    process: &Process,
    input: &ValueContractCatalog,
    output: &ValueContractCatalog,
    signals: &BTreeMap<String, CompiledProcessSignal>,
) -> Result<String, PlanError> {
    let signal_material = signals
        .iter()
        .map(|(name, signal)| {
            (
                name.clone(),
                serde_json::json!({
                    "role": signal.role,
                    "correlation": contract_material(&signal.correlation),
                    "payload": contract_material(&signal.payload),
                    "contract_fingerprint": signal.contract_fingerprint,
                }),
            )
        })
        .collect::<JsonMap<_, _>>();
    hash_json(
        b"donat.process.definition.v1\0",
        &serde_json::json!({
            "runtime_abi_epoch": PROCESS_RUNTIME_ABI_EPOCH,
            "definition": process,
            "input": contract_material(input),
            "output": contract_material(output),
            "signals": signal_material,
        }),
    )
}

fn revision_fingerprint(
    definition_fingerprint: &str,
    dependencies: &ProcessDependencyClosure,
) -> Result<String, PlanError> {
    hash_json(
        b"donat.process.revision.v1\0",
        &process_dependency_descriptors(definition_fingerprint, dependencies),
    )
}

/// Canonical, non-secret dependency closure persisted beside one executable
/// Process definition. This is the exact material hashed into the revision,
/// so a loader can recompile and compare the complete closure byte-for-byte.
pub fn process_dependency_descriptors(
    definition_fingerprint: &str,
    dependencies: &ProcessDependencyClosure,
) -> Json {
    let commands = dependencies
        .commands
        .iter()
        .map(|((source, name), descriptor)| {
            serde_json::json!({
                "source": source,
                "name": name,
                "arguments": contract_material(&descriptor.arguments),
                "result": contract_material(&descriptor.result),
                "allowed_roles": descriptor.allowed_roles,
                "definition_fingerprint": descriptor.definition_fingerprint,
            })
        })
        .collect::<Vec<_>>();
    let rules = dependencies
        .rules
        .values()
        .map(|descriptor| {
            serde_json::json!({
                "name": descriptor.name,
                "bindings": descriptor.bindings,
                "result": descriptor.result,
                "definition_fingerprint": descriptor.definition_fingerprint,
            })
        })
        .collect::<Vec<_>>();
    let decisions = dependencies
        .decision_tables
        .values()
        .map(|descriptor| {
            serde_json::json!({
                "name": descriptor.name,
                "inputs": descriptor.inputs,
                "output": descriptor.output,
                "definition_fingerprint": descriptor.definition_fingerprint,
            })
        })
        .collect::<Vec<_>>();
    let connector_operations = dependencies
        .connector_operations
        .values()
        .map(|dependency| {
            let spec = dependency.spec.as_ref();
            serde_json::json!({
                "source": dependency.source,
                "instance": dependency.instance,
                "connector": spec.connector.as_str(),
                "connector_version": [
                    spec.connector_version.major,
                    spec.connector_version.minor,
                    spec.connector_version.patch
                ],
                "operation": spec.operation.as_str(),
                "operation_version": [
                    spec.operation_version.major,
                    spec.operation_version.minor,
                    spec.operation_version.patch
                ],
                "runtime_abi_epoch": spec.runtime_abi_epoch,
                "value_language_epoch": spec.value_language_epoch,
                "input_contract_sha256": hex_hash(&spec.input_contract_sha256),
                "output_contract_sha256": hex_hash(&spec.output_contract_sha256),
                "deployment_fingerprint": dependency.deployment_fingerprint,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "definition_fingerprint": definition_fingerprint,
        "commands": commands,
        "rules": rules,
        "decision_tables": decisions,
        "connector_operations": connector_operations,
    })
}

fn contract_material(contract: &ValueContractCatalog) -> Json {
    Json::Object(
        contract
            .roots
            .iter()
            .map(|(name, field)| {
                (
                    name.clone(),
                    serde_json::json!({
                        "required": field.required,
                        "type": type_material(&field.type_ref),
                    }),
                )
            })
            .collect(),
    )
}

fn type_material(type_ref: &TypeRef) -> Json {
    let value = match &type_ref.value_type {
        ValueType::Scalar { scalar } => serde_json::json!({
            "kind": "scalar",
            "name": match scalar {
                ValueScalar::Boolean => "boolean",
                ValueScalar::String => "string",
                ValueScalar::Int32 => "int32",
                ValueScalar::Int64 => "int64",
                ValueScalar::UInt64 => "uint64",
                ValueScalar::Decimal => "decimal",
                ValueScalar::Uuid => "uuid",
                ValueScalar::Date => "date",
                ValueScalar::Timestamp => "timestamp",
                ValueScalar::TimestampTz => "timestamptz",
                ValueScalar::Json => "json",
                ValueScalar::Custom { name } => name,
            }
        }),
        ValueType::Enum { name, values } => serde_json::json!({
            "kind": "enum",
            "name": name,
            "values": values,
        }),
        ValueType::Object { fields } => serde_json::json!({
            "kind": "object",
            "fields": fields.iter().map(|(name, field)| (
                name.clone(),
                serde_json::json!({
                    "required": field.required,
                    "type": type_material(&field.type_ref),
                })
            )).collect::<JsonMap<_, _>>(),
        }),
        ValueType::List { element } => serde_json::json!({
            "kind": "list",
            "element": type_material(element),
        }),
        ValueType::Ref { name } => serde_json::json!({
            "kind": "ref",
            "name": name,
        }),
    };
    serde_json::json!({
        "nullable": type_ref.nullable,
        "value": value,
    })
}

fn hash_json(domain: &[u8], value: &Json) -> Result<String, PlanError> {
    let canonical = canonical_json(value);
    let bytes = serde_json::to_vec(&canonical).map_err(|error| {
        validation(
            "processes",
            format!("fingerprint serialization failed: {error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn canonical_json(value: &Json) -> Json {
    match value {
        Json::Object(object) => Json::Object(
            object
                .iter()
                .map(|(name, value)| (name.clone(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Json::Array(values) => Json::Array(values.iter().map(canonical_json).collect()),
        value => value.clone(),
    }
}

fn domain_hash(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn hex_hash(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
