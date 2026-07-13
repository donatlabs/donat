//! Composition of independently-authoritative source planners.

use std::collections::{BTreeMap, HashMap, HashSet};

use donat_catalog::Catalog;
use donat_ir::{MutationRoot, RootField};
use donat_metadata::Metadata;
use graphql_parser::query::{
    Definition, Document, Field as GqlField, OperationDefinition, Selection, SelectionSet,
};
use serde_json::{Map as JsonMap, Value as Json};

use crate::introspection::{build_schema_json, execute_introspection_schema};
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
    query_owners: HashMap<String, String>,
    mutation_owners: HashMap<String, String>,
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

        Ok(Self {
            children,
            query_owners,
            mutation_owners,
        })
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
        let fields = collect_fields(selection_set, &fragments, &vars)?;
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
            return self.plan_mutation(
                doc,
                operation,
                operation_name,
                &vars,
                session,
                collected,
                response,
            );
        }
        self.plan_query(
            doc,
            operation,
            operation_name,
            &vars,
            session,
            collected,
            response,
        )
    }

    fn plan_query(
        &self,
        doc: &Document<'static, String>,
        operation: &OperationDefinition<'static, String>,
        operation_name: Option<&str>,
        variables: &JsonMap<String, Json>,
        session: &Session,
        fields: Vec<CollectedField>,
        response: Vec<QueryResponseSlot>,
    ) -> Result<MultiSourcePlan, PlanError> {
        let partitions = partition_fields(fields);
        let mut sources = vec![];
        for (source, fields) in partitions {
            let child = self.child(&source)?;
            let source_doc = source_document(doc, operation, fields);
            let Plan::Query(roots) =
                child
                    .planner
                    .plan(&source_doc, operation_name, variables, session)?
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
        doc: &Document<'static, String>,
        operation: &OperationDefinition<'static, String>,
        operation_name: Option<&str>,
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
        let source_doc = source_document(
            doc,
            operation,
            fields
                .into_iter()
                .filter(|field| field.source.is_some())
                .map(|field| field.field)
                .collect(),
        );
        let Plan::Mutation(roots) =
            child
                .planner
                .plan(&source_doc, operation_name, variables, session)?
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
        let mut types = vec![];
        let mut by_name = HashMap::<String, Json>::new();
        let mut query_fields = vec![];
        let mut subscription_fields = vec![];
        let mut mutation_fields = vec![];
        let mut query_names = HashSet::new();
        let mut subscription_names = HashSet::new();
        let mut mutation_names = HashSet::new();
        let mut base_schema = None;

        for child in &self.children {
            let schema = build_schema_json(&child.planner, session);
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
                    "mutation_root" => append_root_fields(
                        &mut mutation_fields,
                        &mut mutation_names,
                        ty,
                        "mutation",
                    )?,
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
        if let Some(existing) = owners.insert(root.to_string(), source.to_string())
            && existing != source
        {
            return Err(PlanError::validation(
                "$",
                format!("{root_kind} root '{root}' is owned by both '{existing}' and '{source}'"),
            ));
        }
    }
    Ok(())
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
) -> Result<Vec<CollectedField>, PlanError> {
    let mut fields: Vec<CollectedField> = vec![];
    for field in flatten(selection_set, fragments, vars, None)? {
        let key = field.alias.clone().unwrap_or_else(|| field.name.clone());
        if let Some(existing) = fields.iter_mut().find(|existing| existing.key == key) {
            if existing.field.name != field.name || !arguments_match(&existing.field, field, vars)?
            {
                return Err(PlanError::validation(
                    &format!("$.selectionSet.{}", field.name),
                    format!("fields with response key '{key}' conflict"),
                ));
            }
            existing
                .field
                .selection_set
                .items
                .extend(field.selection_set.items.clone());
        } else {
            fields.push(CollectedField {
                key,
                field: field.clone(),
                source: None,
            });
        }
    }
    if fields.is_empty() {
        return Err(PlanError::validation("$", "selection set cannot be empty"));
    }
    Ok(fields)
}

fn arguments_match(
    left: &GqlField<'static, String>,
    right: &GqlField<'static, String>,
    vars: &JsonMap<String, Json>,
) -> Result<bool, PlanError> {
    if left.arguments.len() != right.arguments.len() {
        return Ok(false);
    }
    let normalize =
        |field: &GqlField<'static, String>| -> Result<BTreeMap<String, Json>, PlanError> {
            field
                .arguments
                .iter()
                .map(|(name, value)| Ok((name.clone(), value_to_json(value, vars, "$")?)))
                .collect()
        };
    Ok(normalize(left)? == normalize(right)?)
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

fn source_document(
    original: &Document<'static, String>,
    operation: &OperationDefinition<'static, String>,
    fields: Vec<GqlField<'static, String>>,
) -> Document<'static, String> {
    let selection_set = SelectionSet {
        span: operation_selection_set(operation).span,
        items: fields.into_iter().map(Selection::Field).collect(),
    };
    let operation = match operation {
        OperationDefinition::SelectionSet(_) => OperationDefinition::SelectionSet(selection_set),
        OperationDefinition::Query(query) => {
            let mut query = query.clone();
            query.selection_set = selection_set;
            OperationDefinition::Query(query)
        }
        OperationDefinition::Mutation(mutation) => {
            let mut mutation = mutation.clone();
            mutation.selection_set = selection_set;
            OperationDefinition::Mutation(mutation)
        }
        OperationDefinition::Subscription(subscription) => {
            let mut subscription = subscription.clone();
            subscription.selection_set = selection_set;
            OperationDefinition::Subscription(subscription)
        }
    };
    let mut definitions = vec![Definition::Operation(operation)];
    definitions.extend(
        original
            .definitions
            .iter()
            .filter_map(|definition| match definition {
                Definition::Fragment(fragment) => Some(Definition::Fragment(fragment.clone())),
                Definition::Operation(_) => None,
            }),
    );
    Document { definitions }
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
