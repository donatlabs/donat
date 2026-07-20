//! Composition of independently-authoritative source planners.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use donat_catalog::Catalog;
use donat_ir::{MutationRoot, RootField};
use donat_metadata::{Columns, Metadata, PermissionEntry, SelectPermission};
use graphql_parser::query::{
    Definition, Document, Field as GqlField, OperationDefinition, Selection, SelectionSet,
    TypeCondition,
};
use serde_json::{Map as JsonMap, Value as Json};

use crate::introspection::{build_schema_json, execute_introspection_schema_lazy};
use crate::naming::table_base_name;
use crate::plan::{
    Fragments, Plan, PlanError, Planner, PlannerIndex, Session, flatten, value_to_json,
};

/// A source-local query IR, ready for exactly one backend request.
#[derive(Debug, Clone)]
pub struct SourceQueryPlan {
    pub source: String,
    pub roots: Vec<RootField>,
}

/// One top-level response key in client order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryResponseSlot {
    SourceField { key: String },
    LocalTypename { key: String, value: String },
}

/// Composite plan. Queries are partitioned per source; mutations may use at
/// most one source to retain the child planner's transaction semantics.
#[derive(Debug, Clone)]
pub enum MultiSourcePlan {
    Query {
        sources: Vec<SourceQueryPlan>,
        response: Vec<QueryResponseSlot>,
    },
    Mutation {
        source: Option<String>,
        roots: Vec<MutationRoot>,
        response: Vec<QueryResponseSlot>,
    },
}

struct ChildPlanner<'a> {
    source: String,
    planner: Planner<'a>,
}

type RootOwners = HashMap<String, String>;

struct RoleSchemas {
    standard: [Json; 2],
    relay: [Json; 2],
}

/// Immutable schema and source-index state compiled from one metadata/catalog
/// snapshot. It owns no references into that snapshot.
pub struct CompiledMultiSourceSchema {
    source_indexes: Vec<Arc<PlannerIndex>>,
    query_owners: HashMap<String, String>,
    relay_query_owners: HashMap<String, String>,
    mutation_owners: HashMap<String, String>,
    schema_template: Json,
    relay_id_types: HashSet<String>,
    relay_error: Option<PlanError>,
    role_schemas: HashMap<String, RoleSchemas>,
    unknown_role_schemas: RoleSchemas,
    infer_function_permissions: bool,
}

impl std::fmt::Debug for CompiledMultiSourceSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledMultiSourceSchema")
            .field("sources", &self.source_indexes.len())
            .field("roles", &self.role_schemas.len())
            .finish()
    }
}

/// Planner facade for Hasura metadata containing multiple data sources.
pub struct MultiSourcePlanner<'a> {
    children: Vec<ChildPlanner<'a>>,
    compiled: &'a CompiledMultiSourceSchema,
    relay: bool,
}

impl std::fmt::Debug for MultiSourcePlanner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiSourcePlanner")
            .field(
                "sources",
                &self
                    .children
                    .iter()
                    .map(|child| &child.source)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl CompiledMultiSourceSchema {
    pub fn compile(
        metadata: &Metadata,
        catalogs: &HashMap<String, Catalog>,
        infer_function_permissions: bool,
    ) -> Result<Self, PlanError> {
        let source_indexes = metadata
            .sources
            .iter()
            .map(Planner::compile_index)
            .collect::<Vec<_>>();
        let mut children = build_children(
            metadata,
            catalogs,
            &source_indexes,
            infer_function_permissions,
        )?;
        let (query_owners, mutation_owners) = root_owners(&children)?;
        let schema_template = build_role_independent_schema(metadata, catalogs, &source_indexes)?;
        let roles = metadata_roles(metadata);
        let unknown_role = denied_role_name(&roles);
        let standard = compose_role_schemas(&children, &roles)?;
        let unknown_standard = compose_role_schema(&children, &unknown_role)?;

        let relay_result = (|| {
            let mut relay_query_owners = query_owners.clone();
            let mut relay_id_types = HashSet::new();
            for child in &mut children {
                child.planner.relay = child.planner.supports_relay();
                if child.planner.relay {
                    relay_id_types.extend(child.planner.tables().iter().map(table_base_name));
                    for root in child.planner.relay_root_names() {
                        register_owner(&mut relay_query_owners, &root, &child.source, "query")?;
                    }
                }
            }
            Ok((
                relay_query_owners,
                relay_id_types,
                compose_role_schemas(&children, &roles)?,
                compose_role_schema(&children, &unknown_role)?,
            ))
        })();
        let (relay_query_owners, relay_id_types, mut relay, unknown_relay, relay_error) =
            match relay_result {
                Ok((owners, id_types, schemas, unknown)) => {
                    (owners, id_types, schemas, unknown, None)
                }
                Err(error) => (
                    query_owners.clone(),
                    HashSet::new(),
                    standard.clone(),
                    unknown_standard.clone(),
                    Some(error),
                ),
            };
        let mut standard = standard;
        let role_schemas = roles
            .into_iter()
            .map(|role| {
                let schemas = RoleSchemas {
                    standard: standard
                        .remove(&role)
                        .expect("standard role schema was composed"),
                    relay: relay.remove(&role).expect("Relay role schema was composed"),
                };
                (role, schemas)
            })
            .collect();

        Ok(Self {
            source_indexes,
            query_owners,
            relay_query_owners,
            mutation_owners,
            schema_template,
            relay_id_types,
            relay_error,
            role_schemas,
            unknown_role_schemas: RoleSchemas {
                standard: unknown_standard,
                relay: unknown_relay,
            },
            infer_function_permissions,
        })
    }

    pub fn source_planner<'a>(
        &'a self,
        metadata: &'a Metadata,
        catalogs: &'a HashMap<String, Catalog>,
        source_name: &str,
    ) -> Result<Planner<'a>, PlanError> {
        let (index, source) = metadata
            .sources
            .iter()
            .enumerate()
            .find(|(_, source)| source.name == source_name)
            .ok_or_else(|| {
                PlanError::new(
                    "$",
                    "not-found",
                    format!("source '{source_name}' not found"),
                )
            })?;
        let catalog = catalogs.get(source_name).ok_or_else(|| {
            PlanError::new(
                "$",
                "not-found",
                format!("catalog for source '{source_name}' not found"),
            )
        })?;
        let source_index = self
            .source_indexes
            .get(index)
            .ok_or_else(|| PlanError::new("$", "unexpected", "compiled source index is missing"))?;
        let mut planner =
            Planner::for_source_with_index(metadata, source, catalog, source_index.clone());
        planner.infer_function_permissions = self.infer_function_permissions;
        Ok(planner)
    }

    fn schema(&self, session: &Session, relay: bool) -> &Json {
        let schemas = self
            .role_schemas
            .get(&session.role)
            .unwrap_or(&self.unknown_role_schemas);
        let pair = if relay {
            &schemas.relay
        } else {
            &schemas.standard
        };
        &pair[usize::from(session.backend_request)]
    }
}

impl<'a> MultiSourcePlanner<'a> {
    pub fn from_compiled(
        metadata: &'a Metadata,
        catalogs: &'a HashMap<String, Catalog>,
        compiled: &'a CompiledMultiSourceSchema,
    ) -> Result<Self, PlanError> {
        let children = build_children(
            metadata,
            catalogs,
            &compiled.source_indexes,
            compiled.infer_function_permissions,
        )?;
        Ok(Self {
            children,
            compiled,
            relay: false,
        })
    }

    /// Apply the Relay mode that was validated during snapshot compilation.
    pub fn set_relay(&mut self, enabled: bool) -> Result<(), PlanError> {
        if enabled && let Some(error) = &self.compiled.relay_error {
            return Err(error.clone());
        }
        for child in &mut self.children {
            child.planner.relay = enabled && child.planner.supports_relay();
        }
        self.relay = enabled;
        Ok(())
    }

    pub fn plan(
        &self,
        doc: &Document<'static, String>,
        operation_name: Option<&str>,
        variables: &JsonMap<String, Json>,
        session: &Session,
    ) -> Result<MultiSourcePlan, PlanError> {
        let (operation, fragments) = select_operation(doc, operation_name)?;
        let (selection_set, variable_definitions, is_mutation) = match operation {
            OperationDefinition::Query(query) => (
                &query.selection_set,
                query.variable_definitions.as_slice(),
                false,
            ),
            OperationDefinition::SelectionSet(selection_set) => {
                (selection_set, [].as_slice(), false)
            }
            OperationDefinition::Mutation(mutation) => (
                &mutation.selection_set,
                mutation.variable_definitions.as_slice(),
                true,
            ),
            OperationDefinition::Subscription(subscription) => (
                &subscription.selection_set,
                subscription.variable_definitions.as_slice(),
                false,
            ),
        };
        let vars = effective_variables(variables, variable_definitions)?;
        if is_mutation
            && !self
                .children
                .iter()
                .any(|child| child.planner.role_has_any_mutation(session))
        {
            return Err(PlanError::validation("$", "no mutations exist"));
        }
        let empty_relay_id_types = HashSet::new();
        let relay_id_types = if self.relay {
            &self.compiled.relay_id_types
        } else {
            &empty_relay_id_types
        };
        let fields = collect_fields(
            selection_set,
            &fragments,
            &vars,
            "$.selectionSet",
            &self.compiled.schema_template,
            relay_id_types,
        )?;
        if fields.is_empty() {
            return Err(PlanError::validation("$", "selection set cannot be empty"));
        }
        let owners = if is_mutation {
            &self.compiled.mutation_owners
        } else if self.relay {
            &self.compiled.relay_query_owners
        } else {
            &self.compiled.query_owners
        };
        let collected = assign_owners(fields, owners, is_mutation)?;
        let response = collected
            .iter()
            .map(|field| match &field.source {
                Some(_) => QueryResponseSlot::SourceField {
                    key: field.key.clone(),
                },
                None => QueryResponseSlot::LocalTypename {
                    key: field.key.clone(),
                    value: if is_mutation {
                        "mutation_root".to_string()
                    } else {
                        "query_root".to_string()
                    },
                },
            })
            .collect();

        if is_mutation {
            return self.plan_mutation(operation, &fragments, &vars, session, collected, response);
        }
        self.plan_query(operation, &fragments, &vars, session, collected, response)
    }

    fn plan_query(
        &self,
        operation: &OperationDefinition<'static, String>,
        fragments: &Fragments,
        variables: &JsonMap<String, Json>,
        session: &Session,
        fields: Vec<CollectedField>,
        response: Vec<QueryResponseSlot>,
    ) -> Result<MultiSourcePlan, PlanError> {
        let partitions = partition_fields(fields);
        let mut sources = vec![];
        for (source, fields) in partitions {
            let child = self.child(&source)?;
            let selection_set = source_selection_set(operation, fields);
            let Plan::Query(roots) = child.planner.plan_selected(
                operation,
                &selection_set,
                fragments,
                variables,
                session,
            )?
            else {
                return Err(PlanError::validation("$", "expected a query operation"));
            };
            sources.push(SourceQueryPlan { source, roots });
        }
        Ok(MultiSourcePlan::Query { sources, response })
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_mutation(
        &self,
        operation: &OperationDefinition<'static, String>,
        fragments: &Fragments,
        variables: &JsonMap<String, Json>,
        session: &Session,
        fields: Vec<CollectedField>,
        response: Vec<QueryResponseSlot>,
    ) -> Result<MultiSourcePlan, PlanError> {
        let source_names: HashSet<&str> = fields
            .iter()
            .filter_map(|field| field.source.as_deref())
            .collect();
        if source_names.len() > 1 {
            return Err(PlanError::validation(
                "$.selectionSet",
                "mutation fields cannot span multiple sources",
            ));
        }
        let Some(source) = source_names.into_iter().next().map(str::to_string) else {
            return Ok(MultiSourcePlan::Mutation {
                source: None,
                roots: vec![],
                response,
            });
        };
        let child = self.child(&source)?;
        let selection_set = source_selection_set(
            operation,
            fields
                .into_iter()
                .filter(|field| field.source.is_some())
                .map(|field| field.field)
                .collect(),
        );
        let Plan::Mutation(roots) = child.planner.plan_selected(
            operation,
            &selection_set,
            fragments,
            variables,
            session,
        )?
        else {
            return Err(PlanError::validation("$", "expected a mutation operation"));
        };
        Ok(MultiSourcePlan::Mutation {
            source: Some(source),
            roots,
            response,
        })
    }

    fn child(&self, source: &str) -> Result<&ChildPlanner<'a>, PlanError> {
        self.children
            .iter()
            .find(|child| child.source == source)
            .ok_or_else(|| PlanError::new("$", "not-found", format!("source '{source}' not found")))
    }

    fn schema_json(&self, session: &Session) -> Result<Cow<'_, Json>, PlanError> {
        Ok(Cow::Borrowed(self.compiled.schema(session, self.relay)))
    }
}

fn build_children<'a>(
    metadata: &'a Metadata,
    catalogs: &'a HashMap<String, Catalog>,
    source_indexes: &[Arc<PlannerIndex>],
    infer_function_permissions: bool,
) -> Result<Vec<ChildPlanner<'a>>, PlanError> {
    if metadata.sources.len() != source_indexes.len() {
        return Err(PlanError::new(
            "$",
            "unexpected",
            "compiled source indexes do not match metadata",
        ));
    }
    metadata
        .sources
        .iter()
        .zip(source_indexes)
        .map(|(source, index)| {
            let catalog = catalogs.get(&source.name).ok_or_else(|| {
                PlanError::new(
                    "$",
                    "not-found",
                    format!("catalog for source '{}' not found", source.name),
                )
            })?;
            let mut planner =
                Planner::for_source_with_index(metadata, source, catalog, index.clone());
            planner.infer_function_permissions = infer_function_permissions;
            Ok(ChildPlanner {
                source: source.name.clone(),
                planner,
            })
        })
        .collect()
}

fn root_owners(children: &[ChildPlanner<'_>]) -> Result<(RootOwners, RootOwners), PlanError> {
    let mut query_owners = HashMap::new();
    let mut mutation_owners = HashMap::new();
    for child in children {
        register_owners(
            &mut query_owners,
            child.planner.query_root_names(),
            &child.source,
            "query",
        )?;
        register_owners(
            &mut mutation_owners,
            child.planner.mutation_root_names(),
            &child.source,
            "mutation",
        )?;
    }
    Ok((query_owners, mutation_owners))
}

fn metadata_roles(metadata: &Metadata) -> BTreeSet<String> {
    let mut roles = BTreeSet::new();
    for inherited in &metadata.inherited_roles {
        roles.insert(inherited.role_name.clone());
        roles.extend(inherited.role_set.iter().cloned());
    }
    for source in &metadata.sources {
        for function in &source.functions {
            roles.extend(
                function
                    .permissions
                    .iter()
                    .map(|permission| permission.role.clone()),
            );
        }
        for table in &source.tables {
            roles.extend(
                table
                    .select_permissions
                    .iter()
                    .map(|permission| permission.role.clone()),
            );
            roles.extend(
                table
                    .insert_permissions
                    .iter()
                    .map(|permission| permission.role.clone()),
            );
            roles.extend(
                table
                    .update_permissions
                    .iter()
                    .map(|permission| permission.role.clone()),
            );
            roles.extend(
                table
                    .delete_permissions
                    .iter()
                    .map(|permission| permission.role.clone()),
            );
        }
    }

    roles
}

fn denied_role_name(roles: &BTreeSet<String>) -> String {
    let mut role = "__donat_unknown_role".to_string();
    while roles.contains(&role) {
        role.push('_');
    }
    role
}

fn compose_role_schema(children: &[ChildPlanner<'_>], role: &str) -> Result<[Json; 2], PlanError> {
    let compose = |backend_request| {
        let session = Session {
            role: role.to_string(),
            vars: HashMap::new(),
            backend_request,
        };
        compose_schema(children.iter().map(|child| &child.planner), &session, None)
    };
    Ok([compose(false)?, compose(true)?])
}

fn compose_role_schemas(
    children: &[ChildPlanner<'_>],
    roles: &BTreeSet<String>,
) -> Result<HashMap<String, [Json; 2]>, PlanError> {
    roles
        .iter()
        .map(|role| Ok((role.clone(), compose_role_schema(children, role)?)))
        .collect()
}

/// Composite equivalent of [`crate::execute_introspection`].
pub fn execute_multi_source_introspection(
    planner: &MultiSourcePlanner,
    session: &Session,
    doc: &Document<'static, String>,
    operation_name: Option<&str>,
    variables: &JsonMap<String, Json>,
) -> Option<Result<Json, PlanError>> {
    execute_introspection_schema_lazy(
        || planner.schema_json(session),
        doc,
        operation_name,
        variables,
    )
}

fn register_owners<'a>(
    owners: &mut HashMap<String, String>,
    roots: impl Iterator<Item = &'a str>,
    source: &str,
    root_kind: &str,
) -> Result<(), PlanError> {
    for root in roots {
        register_owner(owners, root, source, root_kind)?;
    }
    Ok(())
}

fn register_owner(
    owners: &mut HashMap<String, String>,
    root: &str,
    source: &str,
    root_kind: &str,
) -> Result<(), PlanError> {
    if let Some(existing) = owners.insert(root.to_string(), source.to_string())
        && existing != source
    {
        return Err(PlanError::validation(
            "$",
            format!("{root_kind} root '{root}' is owned by both '{existing}' and '{source}'"),
        ));
    }
    Ok(())
}

fn build_role_independent_schema(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    source_indexes: &[Arc<PlannerIndex>],
) -> Result<Json, PlanError> {
    let mut validation_metadata = metadata.clone();
    let mut validation_role = "__donat_composite_schema_validation".to_string();
    while validation_metadata.sources.iter().any(|source| {
        source.tables.iter().any(|table| {
            table
                .select_permissions
                .iter()
                .any(|permission| permission.role == validation_role)
        })
    }) {
        validation_role.push('_');
    }
    for source in &mut validation_metadata.sources {
        for table in &mut source.tables {
            table.select_permissions.push(PermissionEntry {
                role: validation_role.clone(),
                permission: SelectPermission {
                    columns: Columns::Star,
                    filter: serde_json::json!({}),
                    limit: None,
                    allow_aggregations: true,
                    computed_fields: table
                        .computed_fields
                        .iter()
                        .map(|field| field.name.clone())
                        .collect(),
                },
                comment: None,
            });
        }
    }
    let mut planners = vec![];
    for (source, index) in validation_metadata.sources.iter().zip(source_indexes) {
        let catalog = catalogs.get(&source.name).ok_or_else(|| {
            PlanError::new(
                "$",
                "not-found",
                format!("catalog for source '{}' not found", source.name),
            )
        })?;
        planners.push(Planner::for_source_with_index(
            &validation_metadata,
            source,
            catalog,
            index.clone(),
        ));
    }
    let session = Session {
        role: validation_role,
        vars: HashMap::new(),
        backend_request: false,
    };
    compose_schema(planners.iter(), &session, None)
}

fn compose_schema<'planner, 'data>(
    planners: impl IntoIterator<Item = &'planner Planner<'data>>,
    session: &Session,
    template: Option<&Json>,
) -> Result<Json, PlanError>
where
    'data: 'planner,
{
    let mut types = vec![];
    let mut by_name = HashMap::<String, Json>::new();
    let mut query_fields = vec![];
    let mut subscription_fields = vec![];
    let mut mutation_fields = vec![];
    let mut query_names = HashSet::new();
    let mut subscription_names = HashSet::new();
    let mut mutation_names = HashSet::new();
    let mut base_schema = template.cloned();

    for planner in planners {
        let schema = build_schema_json(planner, session);
        if base_schema.is_none() {
            base_schema = Some(schema.clone());
        }
        for ty in schema["types"].as_array().into_iter().flatten() {
            let Some(name) = ty["name"].as_str() else {
                continue;
            };
            match name {
                "query_root" => {
                    append_root_fields(&mut query_fields, &mut query_names, ty, "query")?
                }
                "subscription_root" => append_root_fields(
                    &mut subscription_fields,
                    &mut subscription_names,
                    ty,
                    "subscription",
                )?,
                "mutation_root" => {
                    append_root_fields(&mut mutation_fields, &mut mutation_names, ty, "mutation")?
                }
                _ => {
                    if let Some(existing) = by_name.get(name) {
                        if existing != ty {
                            return Err(PlanError::validation(
                                "$",
                                format!("incompatible type collision for '{name}'"),
                            ));
                        }
                    } else {
                        by_name.insert(name.to_string(), ty.clone());
                        types.push(ty.clone());
                    }
                }
            }
        }
    }

    let mut schema = base_schema.unwrap_or_else(|| serde_json::json!({}));
    types.push(root_type("query_root", query_fields));
    types.push(root_type("subscription_root", subscription_fields));
    if !mutation_fields.is_empty() {
        types.push(root_type("mutation_root", mutation_fields));
        schema["mutationType"] = serde_json::json!({
            "__typename": "__Type", "name": "mutation_root", "kind": "OBJECT"
        });
    } else {
        schema["mutationType"] = Json::Null;
    }
    schema["types"] = Json::Array(types);
    Ok(schema)
}

fn select_operation<'d>(
    doc: &'d Document<'static, String>,
    operation_name: Option<&str>,
) -> Result<(&'d OperationDefinition<'static, String>, Fragments<'d>), PlanError> {
    let mut fragments = HashMap::new();
    let mut operations = vec![];
    for definition in &doc.definitions {
        match definition {
            Definition::Fragment(fragment) => {
                fragments.insert(fragment.name.clone(), fragment);
            }
            Definition::Operation(operation) => operations.push(operation),
        }
    }
    let operation = match operation_name {
        Some(name) => operations
            .iter()
            .find(|operation| operation_name_of(operation) == Some(name))
            .copied()
            .ok_or_else(|| {
                PlanError::validation(
                    "$",
                    format!("no such operation found in the document: \"{name}\""),
                )
            })?,
        None if operations.len() == 1 => operations[0],
        None => {
            return Err(PlanError::validation(
                "$",
                "exactly one operation has to be present in the document when operationName is not specified",
            ));
        }
    };
    Ok((operation, fragments))
}

fn operation_name_of<'a>(operation: &'a OperationDefinition<'static, String>) -> Option<&'a str> {
    match operation {
        OperationDefinition::Query(query) => query.name.as_deref(),
        OperationDefinition::Mutation(mutation) => mutation.name.as_deref(),
        OperationDefinition::Subscription(subscription) => subscription.name.as_deref(),
        OperationDefinition::SelectionSet(_) => None,
    }
}

fn effective_variables(
    variables: &JsonMap<String, Json>,
    definitions: &[graphql_parser::query::VariableDefinition<'static, String>],
) -> Result<JsonMap<String, Json>, PlanError> {
    let mut vars = variables.clone();
    for definition in definitions {
        if !vars.contains_key(&definition.name)
            && let Some(default) = &definition.default_value
        {
            vars.insert(definition.name.clone(), value_to_json(default, &vars, "$")?);
        }
    }
    Ok(vars)
}

struct CollectedField {
    key: String,
    field: GqlField<'static, String>,
    source: Option<String>,
}

fn collect_fields(
    selection_set: &SelectionSet<'static, String>,
    fragments: &Fragments,
    vars: &JsonMap<String, Json>,
    path: &str,
    schema: &Json,
    relay_id_types: &HashSet<String>,
) -> Result<Vec<CollectedField>, PlanError> {
    let mut fields: Vec<CollectedField> = vec![];
    let mut field_indexes: HashMap<String, usize> = HashMap::new();
    for field in flatten(selection_set, fragments, vars, None)? {
        let key = field.alias.clone().unwrap_or_else(|| field.name.clone());
        if let Some(index) = field_indexes.get(&key).copied() {
            let existing = &mut fields[index];
            if existing.field.name != field.name || !arguments_match(&existing.field, field) {
                return Err(PlanError::validation(
                    &format!("{path}.{}", field.name),
                    format!("fields with response key '{key}' conflict"),
                ));
            }
            existing
                .field
                .selection_set
                .items
                .extend(field.selection_set.items.clone());
        } else {
            let mut field = field.clone();
            field.directives.clear();
            field_indexes.insert(key.clone(), fields.len());
            fields.push(CollectedField {
                key,
                field,
                source: None,
            });
        }
    }
    for collected in &fields {
        let nested_path = format!("{path}.{}.selectionSet", collected.field.name);
        let relay_selection = if relay_id_types.is_empty() {
            RelaySelectionType::None
        } else if collected.field.name == "node" {
            RelaySelectionType::Node
        } else if collected.field.name.ends_with("_connection") {
            RelaySelectionType::Connection
        } else {
            RelaySelectionType::None
        };
        validate_selection_conflicts(
            vec![(
                collected.field.selection_set.clone(),
                vec![],
                relay_selection,
            )],
            fragments,
            vars,
            &nested_path,
            schema,
            relay_id_types,
        )?;
    }
    Ok(fields)
}

#[derive(Clone)]
struct ScopedField {
    field: GqlField<'static, String>,
    conditions: Vec<String>,
    relay_selection: RelaySelectionType,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RelaySelectionType {
    None,
    Connection,
    Edge,
    Node,
}

fn validate_selection_conflicts(
    selection_sets: Vec<(
        SelectionSet<'static, String>,
        Vec<String>,
        RelaySelectionType,
    )>,
    fragments: &Fragments,
    vars: &JsonMap<String, Json>,
    path: &str,
    schema: &Json,
    relay_id_types: &HashSet<String>,
) -> Result<(), PlanError> {
    let mut fields = vec![];
    for (selection_set, conditions, relay_selection) in &selection_sets {
        collect_scoped_fields(
            selection_set,
            fragments,
            vars,
            conditions,
            *relay_selection,
            &mut vec![],
            &mut fields,
        )?;
    }

    // A fragment can be spread thousands of times at the same level. Equal
    // fields have identical response shapes, conditions, and arguments, so
    // comparing every copy with every other copy adds quadratic planner work
    // without changing the result. Keep one representative for conflict
    // validation; retain `fields` below because every nested selection still
    // participates in recursive validation.
    let mut conflict_fields: Vec<&ScopedField> = vec![];
    for field in &fields {
        if !conflict_fields
            .iter()
            .any(|existing| same_conflict_identity(existing, field))
        {
            conflict_fields.push(field);
        }
    }

    let mut indexes_by_key: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut keys_in_client_order = vec![];
    for (index, field) in conflict_fields.iter().enumerate() {
        let key = field
            .field
            .alias
            .as_deref()
            .unwrap_or(field.field.name.as_str());
        if !indexes_by_key.contains_key(key) {
            keys_in_client_order.push(key);
        }
        indexes_by_key.entry(key).or_default().push(index);
    }
    for key in keys_in_client_order {
        let indexes = &indexes_by_key[key];
        for (position, left_index) in indexes.iter().enumerate() {
            let left = conflict_fields[*left_index];
            for right_index in indexes.iter().skip(position + 1) {
                let right = conflict_fields[*right_index];
                let right_key = right
                    .field
                    .alias
                    .as_deref()
                    .unwrap_or(right.field.name.as_str());
                if !scoped_response_shapes_match(left, right, schema, relay_id_types) {
                    return Err(PlanError::validation(
                        &format!("{path}.{}", right.field.name),
                        format!("fields with response key '{right_key}' conflict"),
                    ));
                }
                if !scopes_overlap(&left.conditions, &right.conditions, schema) {
                    continue;
                }
                if left.field.name != right.field.name
                    || !arguments_match(&left.field, &right.field)
                {
                    return Err(PlanError::validation(
                        &format!("{path}.{}", right.field.name),
                        format!("fields with response key '{right_key}' conflict"),
                    ));
                }
            }
        }
    }

    let mut groups: Vec<(String, Vec<ScopedField>)> = vec![];
    let mut group_indexes: HashMap<String, usize> = HashMap::new();
    for field in fields {
        let key = field
            .field
            .alias
            .clone()
            .unwrap_or_else(|| field.field.name.clone());
        if let Some(index) = group_indexes.get(&key).copied() {
            groups[index].1.push(field);
        } else {
            group_indexes.insert(key.clone(), groups.len());
            groups.push((key, vec![field]));
        }
    }
    for (_, group) in groups {
        let Some(first) = group.first() else { continue };
        if group
            .iter()
            .all(|field| field.field.selection_set.items.is_empty())
        {
            continue;
        }
        let nested_path = format!("{path}.{}.selectionSet", first.field.name);
        let nested = group
            .into_iter()
            .map(|field| {
                let relay_selection = match (field.relay_selection, field.field.name.as_str()) {
                    (RelaySelectionType::Connection, "edges") => RelaySelectionType::Edge,
                    (RelaySelectionType::Edge, "node") => RelaySelectionType::Node,
                    _ => RelaySelectionType::None,
                };
                (field.field.selection_set, field.conditions, relay_selection)
            })
            .collect();
        validate_selection_conflicts(
            nested,
            fragments,
            vars,
            &nested_path,
            schema,
            relay_id_types,
        )?;
    }
    Ok(())
}

fn same_conflict_identity(left: &ScopedField, right: &ScopedField) -> bool {
    left.conditions == right.conditions
        && left.relay_selection == right.relay_selection
        && left.field.name == right.field.name
        && left.field.alias == right.field.alias
        && arguments_match(&left.field, &right.field)
}

fn scoped_response_shapes_match(
    left: &ScopedField,
    right: &ScopedField,
    schema: &Json,
    relay_id_types: &HashSet<String>,
) -> bool {
    let relay_id = |field: &ScopedField| {
        field.field.name == "id"
            && (field.relay_selection == RelaySelectionType::Node
                || field
                    .conditions
                    .iter()
                    .rev()
                    .any(|condition| relay_id_types.contains(condition)))
    };
    let field_type = |field: &ScopedField| {
        field.conditions.iter().rev().find_map(|condition| {
            schema["types"]
                .as_array()?
                .iter()
                .find(|ty| ty["name"].as_str() == Some(condition.as_str()))?["fields"]
                .as_array()?
                .iter()
                .find(|candidate| candidate["name"] == field.field.name)
                .map(|candidate| &candidate["type"])
        })
    };
    match (relay_id(left), relay_id(right)) {
        (true, true) => return true,
        (true, false) => {
            return field_type(right)
                .map(response_type_is_relay_id)
                .unwrap_or(false);
        }
        (false, true) => {
            return field_type(left)
                .map(response_type_is_relay_id)
                .unwrap_or(false);
        }
        (false, false) => {}
    }
    match (field_type(left), field_type(right)) {
        (Some(left), Some(right)) => response_types_match(left, right),
        _ => true,
    }
}

fn response_type_is_relay_id(ty: &Json) -> bool {
    ty["kind"] == "NON_NULL" && ty["ofType"]["kind"] == "SCALAR" && ty["ofType"]["name"] == "ID"
}

fn response_types_match(left: &Json, right: &Json) -> bool {
    let left_kind = left["kind"].as_str();
    let right_kind = right["kind"].as_str();
    match (left_kind, right_kind) {
        (Some("NON_NULL"), Some("NON_NULL")) | (Some("LIST"), Some("LIST")) => {
            response_types_match(&left["ofType"], &right["ofType"])
        }
        (Some("NON_NULL" | "LIST"), _) | (_, Some("NON_NULL" | "LIST")) => false,
        (Some(left_kind), Some(right_kind)) => {
            let composite = |kind: &str| matches!(kind, "OBJECT" | "INTERFACE" | "UNION");
            if composite(left_kind) && composite(right_kind) {
                true
            } else {
                left_kind == right_kind && left["name"] == right["name"]
            }
        }
        _ => true,
    }
}

fn collect_scoped_fields(
    selection_set: &SelectionSet<'static, String>,
    fragments: &Fragments,
    vars: &JsonMap<String, Json>,
    conditions: &[String],
    relay_selection: RelaySelectionType,
    fragment_stack: &mut Vec<String>,
    output: &mut Vec<ScopedField>,
) -> Result<(), PlanError> {
    for selection in &selection_set.items {
        match selection {
            Selection::Field(field) => {
                if directives_include(&field.directives, vars)? {
                    output.push(ScopedField {
                        field: field.clone(),
                        conditions: conditions.to_vec(),
                        relay_selection,
                    });
                }
            }
            Selection::FragmentSpread(spread) => {
                if !directives_include(&spread.directives, vars)? {
                    continue;
                }
                let fragment = fragments.get(&spread.fragment_name).ok_or_else(|| {
                    PlanError::validation(
                        "$",
                        format!("fragment \"{}\" not found", spread.fragment_name),
                    )
                })?;
                if !directives_include(&fragment.directives, vars)? {
                    continue;
                }
                if fragment_stack.contains(&spread.fragment_name) {
                    return Err(PlanError::validation(
                        "$",
                        format!("fragment \"{}\" forms a cycle", spread.fragment_name),
                    ));
                }
                let TypeCondition::On(condition) = &fragment.type_condition;
                let mut nested_conditions = conditions.to_vec();
                nested_conditions.push(condition.clone());
                fragment_stack.push(spread.fragment_name.clone());
                let result = collect_scoped_fields(
                    &fragment.selection_set,
                    fragments,
                    vars,
                    &nested_conditions,
                    relay_selection,
                    fragment_stack,
                    output,
                );
                fragment_stack.pop();
                result?;
            }
            Selection::InlineFragment(inline) => {
                if !directives_include(&inline.directives, vars)? {
                    continue;
                }
                let mut nested_conditions = conditions.to_vec();
                if let Some(TypeCondition::On(condition)) = &inline.type_condition {
                    nested_conditions.push(condition.clone());
                }
                collect_scoped_fields(
                    &inline.selection_set,
                    fragments,
                    vars,
                    &nested_conditions,
                    relay_selection,
                    fragment_stack,
                    output,
                )?;
            }
        }
    }
    Ok(())
}

fn directives_include(
    directives: &[graphql_parser::query::Directive<'static, String>],
    vars: &JsonMap<String, Json>,
) -> Result<bool, PlanError> {
    for directive in directives {
        let condition = directive
            .arguments
            .iter()
            .find(|(name, _)| name == "if")
            .map(|(_, value)| value_to_json(value, vars, "$"))
            .transpose()?
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        match directive.name.as_str() {
            "include" if !condition => return Ok(false),
            "skip" if condition => return Ok(false),
            _ => {}
        }
    }
    Ok(true)
}

fn scopes_overlap(left: &[String], right: &[String], schema: &Json) -> bool {
    left.iter().all(|left| {
        right
            .iter()
            .all(|right| type_conditions_overlap(left, right, schema))
    })
}

fn type_conditions_overlap(left: &str, right: &str, schema: &Json) -> bool {
    if left == right {
        return true;
    }
    let possible_types = |name: &str| -> Option<HashSet<String>> {
        let ty = schema["types"]
            .as_array()?
            .iter()
            .find(|ty| ty["name"] == name)?;
        match ty["kind"].as_str()? {
            "OBJECT" => Some(HashSet::from([name.to_string()])),
            "INTERFACE" | "UNION" => Some(
                ty["possibleTypes"]
                    .as_array()?
                    .iter()
                    .filter_map(|possible| possible["name"].as_str().map(str::to_string))
                    .collect(),
            ),
            _ => None,
        }
    };
    match (possible_types(left), possible_types(right)) {
        (Some(left), Some(right)) => !left.is_disjoint(&right),
        _ => true,
    }
}

fn arguments_match(left: &GqlField<'static, String>, right: &GqlField<'static, String>) -> bool {
    if left.arguments.len() != right.arguments.len() {
        return false;
    }
    let normalize = |field: &GqlField<'static, String>| {
        field
            .arguments
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    normalize(left) == normalize(right)
}

fn assign_owners(
    mut fields: Vec<CollectedField>,
    owners: &HashMap<String, String>,
    mutation: bool,
) -> Result<Vec<CollectedField>, PlanError> {
    for field in &mut fields {
        if field.field.name == "__typename" {
            continue;
        }
        let Some(source) = owners.get(&field.field.name) else {
            let type_name = if mutation {
                "mutation_root"
            } else {
                "query_root"
            };
            return Err(PlanError::validation(
                &format!("$.selectionSet.{}", field.field.name),
                format!(
                    "field '{}' not found in type: '{type_name}'",
                    field.field.name
                ),
            ));
        };
        field.source = Some(source.clone());
    }
    Ok(fields)
}

fn partition_fields(fields: Vec<CollectedField>) -> Vec<(String, Vec<GqlField<'static, String>>)> {
    let mut partitions: Vec<(String, Vec<GqlField<'static, String>>)> = vec![];
    for field in fields {
        let Some(source) = field.source else { continue };
        if let Some((_, roots)) = partitions.iter_mut().find(|(owner, _)| owner == &source) {
            roots.push(field.field);
        } else {
            partitions.push((source, vec![field.field]));
        }
    }
    partitions
}

fn source_selection_set(
    operation: &OperationDefinition<'static, String>,
    fields: Vec<GqlField<'static, String>>,
) -> SelectionSet<'static, String> {
    SelectionSet {
        span: operation_selection_set(operation).span,
        items: fields.into_iter().map(Selection::Field).collect(),
    }
}

fn operation_selection_set<'a>(
    operation: &'a OperationDefinition<'static, String>,
) -> &'a SelectionSet<'static, String> {
    match operation {
        OperationDefinition::SelectionSet(selection_set) => selection_set,
        OperationDefinition::Query(query) => &query.selection_set,
        OperationDefinition::Mutation(mutation) => &mutation.selection_set,
        OperationDefinition::Subscription(subscription) => &subscription.selection_set,
    }
}

fn append_root_fields(
    output: &mut Vec<Json>,
    names: &mut HashSet<String>,
    ty: &Json,
    root_kind: &str,
) -> Result<(), PlanError> {
    for field in ty["fields"].as_array().into_iter().flatten() {
        let Some(name) = field["name"].as_str() else {
            continue;
        };
        if !names.insert(name.to_string()) {
            return Err(PlanError::validation(
                "$",
                format!("incompatible {root_kind} root collision for '{name}'"),
            ));
        }
        output.push(field.clone());
    }
    Ok(())
}

fn root_type(name: &str, fields: Vec<Json>) -> Json {
    serde_json::json!({
        "__typename": "__Type",
        "kind": "OBJECT",
        "name": name,
        "description": null,
        "fields": fields,
        "inputFields": null,
        "interfaces": [],
        "enumValues": null,
        "possibleTypes": null,
    })
}

#[cfg(test)]
mod performance_tests {
    use super::*;

    #[test]
    fn wide_unique_selection_is_collected_without_pairwise_scans() {
        let query = format!(
            "{{ {} }}",
            (0..2_000)
                .map(|index| format!("field_{index}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let document = graphql_parser::parse_query::<String>(&query)
            .expect("wide query parses")
            .into_static();
        let Definition::Operation(OperationDefinition::SelectionSet(selection_set)) =
            &document.definitions[0]
        else {
            unreachable!()
        };
        let fields = collect_fields(
            selection_set,
            &HashMap::new(),
            &JsonMap::new(),
            "$.selectionSet",
            &serde_json::json!({}),
            &HashSet::new(),
        )
        .expect("unique response keys do not conflict");
        assert_eq!(fields.len(), 2_000);
    }

    #[test]
    fn repeated_fragment_fields_are_validated_once_per_identity() {
        let query = format!(
            "query {{ item {{ {} }} }} fragment Repeated on item {{ id }}",
            "...Repeated ".repeat(10_000)
        );
        let document = graphql_parser::parse_query::<String>(&query)
            .expect("wide repeated query parses")
            .into_static();
        let (operation, fragments) = select_operation(&document, None).expect("operation found");

        validate_selection_conflicts(
            vec![(
                operation_selection_set(operation).clone(),
                vec![],
                RelaySelectionType::None,
            )],
            &fragments,
            &JsonMap::new(),
            "$.selectionSet",
            &serde_json::json!({}),
            &HashSet::new(),
        )
        .expect("identical fragment fields do not conflict");
    }
}
