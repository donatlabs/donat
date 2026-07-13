//! Composition of independently-authoritative source planners.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use donat_catalog::Catalog;
use donat_ir::{MutationRoot, RootField};
use donat_metadata::{Columns, Metadata, PermissionEntry, SelectPermission};
use graphql_parser::query::{
    Definition, Document, Field as GqlField, OperationDefinition, Selection, SelectionSet,
    TypeCondition,
};
use serde_json::{Map as JsonMap, Value as Json};

use crate::introspection::{build_schema_json, execute_introspection_schema};
use crate::naming::table_base_name;
use crate::plan::{Fragments, Plan, PlanError, Planner, Session, flatten, value_to_json};

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

/// Planner facade for Hasura metadata containing multiple data sources.
pub struct MultiSourcePlanner<'a> {
    children: Vec<ChildPlanner<'a>>,
    base_query_owners: HashMap<String, String>,
    query_owners: HashMap<String, String>,
    mutation_owners: HashMap<String, String>,
    schema_template: Json,
    relay_id_types: HashSet<String>,
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

impl<'a> MultiSourcePlanner<'a> {
    /// Build one child planner for every metadata source. Catalog lookup is
    /// exact by source name; there is deliberately no default-source fallback.
    pub fn new(
        metadata: &'a Metadata,
        catalogs: &'a HashMap<String, Catalog>,
    ) -> Result<Self, PlanError> {
        let mut children = vec![];
        let mut query_owners = HashMap::new();
        let mut mutation_owners = HashMap::new();

        for source in &metadata.sources {
            let catalog = catalogs.get(&source.name).ok_or_else(|| {
                PlanError::new(
                    "$",
                    "not-found",
                    format!("catalog for source '{}' not found", source.name),
                )
            })?;
            let planner = Planner::for_source(metadata, source, catalog);
            register_owners(
                &mut query_owners,
                planner.query_root_names(),
                &source.name,
                "query",
            )?;
            register_owners(
                &mut mutation_owners,
                planner.mutation_root_names(),
                &source.name,
                "mutation",
            )?;
            children.push(ChildPlanner {
                source: source.name.clone(),
                planner,
            });
        }

        let schema_template = build_role_independent_schema(metadata, catalogs)?;
        validate_role_projections(metadata, &children)?;
        Ok(Self {
            children,
            base_query_owners: query_owners.clone(),
            query_owners,
            mutation_owners,
            schema_template,
            relay_id_types: HashSet::new(),
        })
    }

    pub fn set_infer_function_permissions(&mut self, enabled: bool) {
        for child in &mut self.children {
            child.planner.infer_function_permissions = enabled;
        }
    }

    /// Apply relay mode to each capable child and rebuild composite relay
    /// ownership. The update is atomic when relay roots collide.
    pub fn set_relay(&mut self, enabled: bool) -> Result<(), PlanError> {
        let previous: Vec<bool> = self
            .children
            .iter()
            .map(|child| child.planner.relay)
            .collect();
        let previous_id_types = self.relay_id_types.clone();
        let mut owners = self.base_query_owners.clone();
        let mut relay_id_types = HashSet::new();
        let result = (|| {
            for child in &mut self.children {
                child.planner.relay = enabled && child.planner.supports_relay();
                if child.planner.relay {
                    relay_id_types.extend(child.planner.tables().iter().map(table_base_name));
                    for root in child.planner.relay_root_names() {
                        register_owner(&mut owners, &root, &child.source, "query")?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            for (child, relay) in self.children.iter_mut().zip(previous) {
                child.planner.relay = relay;
            }
            self.relay_id_types = previous_id_types;
            return Err(error);
        }
        self.query_owners = owners;
        self.relay_id_types = relay_id_types;
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
        let fields = collect_fields(
            selection_set,
            &fragments,
            &vars,
            "$.selectionSet",
            &self.schema_template,
            &self.relay_id_types,
        )?;
        if fields.is_empty() {
            return Err(PlanError::validation("$", "selection set cannot be empty"));
        }
        let owners = if is_mutation {
            &self.mutation_owners
        } else {
            &self.query_owners
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

    fn schema_json(&self, session: &Session) -> Result<Json, PlanError> {
        compose_schema(
            self.children.iter().map(|child| &child.planner),
            session,
            Some(&self.schema_template),
        )
    }
}

fn validate_role_projections(
    metadata: &Metadata,
    children: &[ChildPlanner<'_>],
) -> Result<(), PlanError> {
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

    for role in roles {
        for backend_request in [false, true] {
            let session = Session {
                role: role.clone(),
                vars: HashMap::new(),
                backend_request,
            };
            compose_schema(children.iter().map(|child| &child.planner), &session, None)?;
        }
    }
    Ok(())
}

/// Composite equivalent of [`crate::execute_introspection`].
pub fn execute_multi_source_introspection(
    planner: &MultiSourcePlanner,
    session: &Session,
    doc: &Document<'static, String>,
    operation_name: Option<&str>,
    variables: &JsonMap<String, Json>,
) -> Option<Result<Json, PlanError>> {
    match planner.schema_json(session) {
        Ok(schema) => execute_introspection_schema(&schema, doc, operation_name, variables),
        Err(error) => Some(Err(error)),
    }
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
    for source in &validation_metadata.sources {
        let catalog = catalogs.get(&source.name).ok_or_else(|| {
            PlanError::new(
                "$",
                "not-found",
                format!("catalog for source '{}' not found", source.name),
            )
        })?;
        planners.push(Planner::for_source(&validation_metadata, source, catalog));
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
    for field in flatten(selection_set, fragments, vars, None)? {
        let key = field.alias.clone().unwrap_or_else(|| field.name.clone());
        if let Some(existing) = fields.iter_mut().find(|existing| existing.key == key) {
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
            fields.push(CollectedField {
                key,
                field,
                source: None,
            });
        }
    }
    for collected in &fields {
        let nested_path = format!("{path}.{}.selectionSet", collected.field.name);
        validate_selection_conflicts(
            vec![(collected.field.selection_set.clone(), vec![])],
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
}

fn validate_selection_conflicts(
    selection_sets: Vec<(SelectionSet<'static, String>, Vec<String>)>,
    fragments: &Fragments,
    vars: &JsonMap<String, Json>,
    path: &str,
    schema: &Json,
    relay_id_types: &HashSet<String>,
) -> Result<(), PlanError> {
    let mut fields = vec![];
    for (selection_set, conditions) in &selection_sets {
        collect_scoped_fields(
            selection_set,
            fragments,
            vars,
            conditions,
            &mut vec![],
            &mut fields,
        )?;
    }

    for (index, left) in fields.iter().enumerate() {
        let left_key = left
            .field
            .alias
            .as_deref()
            .unwrap_or(left.field.name.as_str());
        for right in fields.iter().skip(index + 1) {
            let right_key = right
                .field
                .alias
                .as_deref()
                .unwrap_or(right.field.name.as_str());
            if left_key != right_key {
                continue;
            }
            if !scoped_response_shapes_match(left, right, schema, relay_id_types) {
                return Err(PlanError::validation(
                    &format!("{path}.{}", right.field.name),
                    format!("fields with response key '{right_key}' conflict"),
                ));
            }
            if !scopes_overlap(&left.conditions, &right.conditions, schema) {
                continue;
            }
            if left.field.name != right.field.name || !arguments_match(&left.field, &right.field) {
                return Err(PlanError::validation(
                    &format!("{path}.{}", right.field.name),
                    format!("fields with response key '{right_key}' conflict"),
                ));
            }
        }
    }

    let mut groups: Vec<(String, Vec<ScopedField>)> = vec![];
    for field in fields {
        let key = field
            .field
            .alias
            .clone()
            .unwrap_or_else(|| field.field.name.clone());
        if let Some((_, group)) = groups.iter_mut().find(|(candidate, _)| candidate == &key) {
            group.push(field);
        } else {
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
            .map(|field| (field.field.selection_set, field.conditions))
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

fn scoped_response_shapes_match(
    left: &ScopedField,
    right: &ScopedField,
    schema: &Json,
    relay_id_types: &HashSet<String>,
) -> bool {
    let relay_id = |field: &ScopedField| {
        field.field.name == "id"
            && field
                .conditions
                .iter()
                .rev()
                .any(|condition| relay_id_types.contains(condition))
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
