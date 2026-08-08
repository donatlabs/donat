//! The YAML rules metadata → executable `donat-rules` catalog adapter.
//!
//! This lives beside the planner rather than in the server because the
//! planner is the only thing that needs it, and it has to reach `wasm32`:
//! an embedded host compiles its own rule catalog inside the wasm core, and
//! a rule that compiled differently there than in `donat-server` would make
//! the two disagree about what a command is allowed to do.
//!
//! The adapter resolves named declarations before compiling executable
//! source. Unknown names are never silently widened to strings or JSON
//! values.

use std::collections::{BTreeMap, HashSet};

use donat_metadata::{Metadata, RuleTypeDeclaration};
use donat_rules::{
    DecisionRow as RuleDecisionRow, DecisionTableDefinition as RuleDecisionTableDefinition,
    DecisionTableTestCase as RuleDecisionTableTestCase,
    DecisionTestExpectation as RuleDecisionTestExpectation, ExpressionContext, ExpressionOwner,
    HitPolicy, RuleCatalog, RuleDefinition, RuleType,
};

use crate::PlanError;

/// Translate the YAML metadata shape into the strict rules crate model and
/// compile it before a candidate snapshot can publish.
///
/// The adapter resolves named declarations before compiling executable source.
/// Unknown names are never silently widened to strings or JSON values.
pub fn compile_rule_catalog(metadata: &Metadata) -> Result<RuleCatalog, PlanError> {
    let declared_types = resolve_declared_rule_types(&metadata.rules.types)?;
    let rules_and_contexts = metadata
        .rules
        .rules
        .iter()
        .enumerate()
        .map(|(index, definition)| {
            let path = format!("rules.yaml.rules[{index}]");
            Ok((
                RuleDefinition {
                    name: definition.name.clone(),
                    bindings: compile_rule_types(&definition.parameters, &declared_types, &path)?,
                    result: parse_rule_type_ref(
                        &definition.result,
                        &declared_types,
                        &format!("{path}.result"),
                    )?,
                    expression: definition.expression.clone(),
                },
                ExpressionContext {
                    metadata_path: format!("{path}.expression"),
                    expression_owner: ExpressionOwner::Rule {
                        name: definition.name.clone(),
                    },
                },
            ))
        })
        .collect::<Result<Vec<_>, PlanError>>()?;
    let (rules, rule_contexts): (Vec<_>, Vec<_>) = rules_and_contexts.into_iter().unzip();
    let tables_and_contexts = metadata
        .rules
        .decision_tables
        .iter()
        .enumerate()
        .map(|(index, definition)| {
            let path = format!("rules.yaml.decision_tables[{index}]");
            let condition_contexts = definition
                .rows
                .iter()
                .enumerate()
                .map(|(row_index, row)| {
                    row.when
                        .keys()
                        .map(|input_name| {
                            (
                                input_name.clone(),
                                ExpressionContext {
                                    metadata_path: format!(
                                        "{path}.rows[{row_index}].when.{input_name}"
                                    ),
                                    expression_owner: ExpressionOwner::DecisionCondition {
                                        table_name: definition.name.clone(),
                                        row_id: row.id.clone(),
                                        input_name: input_name.clone(),
                                    },
                                },
                            )
                        })
                        .collect::<BTreeMap<_, _>>()
                })
                .collect::<Vec<_>>();
            Ok((
                RuleDecisionTableDefinition {
                    name: definition.name.clone(),
                    // The rules crate derives this deploy-time revision from
                    // the complete canonical compiled definition; the legacy
                    // field remains for its source-model compatibility only.
                    revision: definition.name.clone(),
                    inputs: compile_rule_types(
                        &definition.inputs,
                        &declared_types,
                        &format!("{path}.inputs"),
                    )?,
                    output: compile_rule_types(
                        &definition.output,
                        &declared_types,
                        &format!("{path}.output"),
                    )?,
                    hit_policy: HitPolicy::from_metadata(&definition.hit_policy),
                    rows: definition
                        .rows
                        .iter()
                        .map(|row| RuleDecisionRow {
                            id: row.id.clone(),
                            description: row.description.clone(),
                            when: row.when.clone(),
                            output: row.output.clone(),
                        })
                        .collect(),
                    test_cases: definition
                        .test_cases
                        .iter()
                        .map(|case| RuleDecisionTableTestCase {
                            name: case.name.clone(),
                            input: case.input.clone(),
                            expect: RuleDecisionTestExpectation {
                                output: case.expect.output.clone(),
                                matched_row_id: case.expect.matched_row_id.clone(),
                            },
                        })
                        .collect(),
                },
                condition_contexts,
            ))
        })
        .collect::<Result<Vec<_>, PlanError>>()?;
    let (tables, decision_condition_contexts): (Vec<_>, Vec<_>) =
        tables_and_contexts.into_iter().unzip();

    let declaration_order = metadata
        .rules
        .types
        .iter()
        .map(|declaration| declaration.name.clone())
        .collect::<Vec<_>>();
    let catalog =
        donat_rules::compile_catalog_with_declared_types_and_contexts_and_declaration_order(
            &declared_types,
            &declaration_order,
            &rules,
            &tables,
            &rule_contexts,
            &decision_condition_contexts,
        )
        .map_err(|error| {
            if let Some(diagnostic) = error.diagnostic() {
                PlanError::validation(
                    &diagnostic.context.metadata_path,
                    format!(
                        "declarative rule validation failed for {} at bytes {}..{}: {}",
                        render_expression_owner(&diagnostic.context.expression_owner),
                        diagnostic.span.start,
                        diagnostic.span.end,
                        diagnostic.message
                    ),
                )
            } else {
                PlanError::validation(
                    "rules.yaml",
                    format!("declarative rule validation failed: {error}"),
                )
            }
        })?;

    for rule in &rules {
        let compiled = catalog
            .rule(&rule.name)
            .expect("validated rule remains in the compiled catalog");
        tracing::debug!(
            rule = %compiled.name,
            profile_version = compiled.artifact.profile_version,
            source_sha256 = %compiled.artifact.source_sha256,
            canonical_ast_sha256 = %compiled.artifact.canonical_ast_sha256,
            "declarative rule profile artifact compiled"
        );
    }
    for table in &tables {
        let compiled = catalog
            .decision_table(&table.name)
            .expect("validated decision remains in the compiled catalog");
        tracing::debug!(
            table = %compiled.name,
            revision = %compiled.revision.0,
            "declarative decision revision compiled"
        );
    }
    Ok(catalog)
}

fn render_expression_owner(owner: &ExpressionOwner) -> String {
    match owner {
        ExpressionOwner::Rule { name } => format!("rule `{name}`"),
        ExpressionOwner::DecisionCondition {
            table_name,
            row_id,
            input_name,
        } => format!("decision table `{table_name}` row `{row_id}` condition `{input_name}`"),
    }
}

fn compile_rule_types(
    types: &BTreeMap<String, String>,
    declared: &BTreeMap<String, RuleType>,
    path: &str,
) -> Result<BTreeMap<String, RuleType>, PlanError> {
    types
        .iter()
        .map(|(name, type_)| {
            parse_rule_type_ref(type_, declared, &format!("{path}.{name}"))
                .map(|type_| (name.clone(), type_))
        })
        .collect()
}

fn parse_rule_type_ref(
    source: &str,
    declared: &BTreeMap<String, RuleType>,
    path: &str,
) -> Result<RuleType, PlanError> {
    let (source, required) = source
        .strip_suffix('!')
        .map_or((source, false), |inner| (inner, true));
    if source.is_empty() {
        return Err(PlanError::validation(
            path,
            "rule type reference cannot be empty",
        ));
    }
    let type_ = if source.starts_with('[') || source.ends_with(']') {
        let Some(inner) = source
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        else {
            return Err(PlanError::validation(
                path,
                format!("invalid rule type reference `{source}`"),
            ));
        };
        RuleType::List(Box::new(parse_rule_type_ref(inner, declared, path)?))
    } else {
        match scalar_rule_type(source) {
            Some(type_) => type_,
            None => declared.get(source).cloned().ok_or_else(|| {
                PlanError::validation(path, format!("unsupported rule type `{source}`"))
            })?,
        }
    };
    Ok(if required {
        type_
    } else {
        RuleType::nullable(type_)
    })
}

#[derive(Debug, Clone)]
enum TypeResolution {
    Visiting,
    Resolved(RuleType),
}

fn resolve_declared_rule_types(
    declarations: &[RuleTypeDeclaration],
) -> Result<BTreeMap<String, RuleType>, PlanError> {
    let mut positions = BTreeMap::new();
    for (index, declaration) in declarations.iter().enumerate() {
        let path = format!("rules.yaml.types[{index}]");
        if declaration.name.is_empty() {
            return Err(PlanError::validation(
                &path,
                "declared rule type name cannot be empty",
            ));
        }
        if is_scalar_rule_type_name(&declaration.name) {
            return Err(PlanError::validation(
                &path,
                format!(
                    "declared rule type `{}` collides with scalar profile type",
                    declaration.name
                ),
            ));
        }
        if positions.insert(declaration.name.clone(), index).is_some() {
            return Err(PlanError::validation(
                &path,
                format!("duplicate declared rule type `{}`", declaration.name),
            ));
        }
        let body_count = usize::from(declaration.object.is_some())
            + usize::from(declaration.enum_values.is_some())
            + usize::from(declaration.opaque_json.is_some());
        if body_count != 1 {
            return Err(PlanError::validation(
                &path,
                "a declared rule type requires exactly one of object, enum, or opaque_json",
            ));
        }
    }

    let mut resolved = BTreeMap::new();
    for name in positions.keys() {
        resolve_declared_rule_type(name, declarations, &positions, &mut resolved)?;
    }
    resolved
        .into_iter()
        .map(|(name, resolution)| match resolution {
            TypeResolution::Resolved(type_) => Ok((name, type_)),
            TypeResolution::Visiting => Err(PlanError::validation(
                "rules.yaml.types",
                "declared rule type resolution did not finish",
            )),
        })
        .collect()
}

fn resolve_declared_rule_type(
    name: &str,
    declarations: &[RuleTypeDeclaration],
    positions: &BTreeMap<String, usize>,
    resolved: &mut BTreeMap<String, TypeResolution>,
) -> Result<RuleType, PlanError> {
    if let Some(resolution) = resolved.get(name) {
        return match resolution {
            TypeResolution::Resolved(type_) => Ok(type_.clone()),
            TypeResolution::Visiting => Err(PlanError::validation(
                "rules.yaml.types",
                format!("declared rule type cycle includes `{name}`"),
            )),
        };
    }
    let index = *positions.get(name).ok_or_else(|| {
        PlanError::validation(
            "rules.yaml.types",
            format!("unknown declared rule type `{name}`"),
        )
    })?;
    let declaration = &declarations[index];
    let path = format!("rules.yaml.types[{index}]");
    resolved.insert(name.to_owned(), TypeResolution::Visiting);

    let type_ = if let Some(symbols) = &declaration.enum_values {
        if symbols.is_empty() || symbols.iter().any(String::is_empty) {
            return Err(PlanError::validation(
                &path,
                "an enum declaration requires non-empty symbols",
            ));
        }
        let unique = symbols.iter().collect::<HashSet<_>>();
        if unique.len() != symbols.len() {
            return Err(PlanError::validation(
                &path,
                "an enum declaration requires unique symbols",
            ));
        }
        RuleType::Enum {
            name: declaration.name.clone(),
            symbols: symbols.clone(),
        }
    } else if let Some(fields) = &declaration.object {
        if fields.is_empty() {
            return Err(PlanError::validation(
                &path,
                "an object declaration requires at least one field",
            ));
        }
        let fields = fields
            .iter()
            .map(|(field, source)| {
                if field.is_empty() {
                    return Err(PlanError::validation(
                        &format!("{path}.object"),
                        "an object field name cannot be empty",
                    ));
                }
                resolve_declared_type_ref(
                    source,
                    declarations,
                    positions,
                    resolved,
                    &format!("{path}.object.{field}"),
                )
                .map(|type_| (field.clone(), type_))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        RuleType::Object {
            name: declaration.name.clone(),
            fields,
        }
    } else if let Some(opaque) = &declaration.opaque_json {
        if opaque.maximum_bytes == 0 || opaque.maximum_depth == 0 || opaque.maximum_nodes == 0 {
            return Err(PlanError::validation(
                &format!("{path}.opaque_json"),
                "an opaque JSON declaration requires non-zero bounds",
            ));
        }
        RuleType::OpaqueJson {
            name: declaration.name.clone(),
            maximum_bytes: opaque.maximum_bytes,
            maximum_depth: opaque.maximum_depth,
            maximum_nodes: opaque.maximum_nodes,
        }
    } else {
        return Err(PlanError::validation(
            &path,
            "a declared rule type requires exactly one of object, enum, or opaque_json",
        ));
    };
    resolved.insert(name.to_owned(), TypeResolution::Resolved(type_.clone()));
    Ok(type_)
}

fn resolve_declared_type_ref(
    source: &str,
    declarations: &[RuleTypeDeclaration],
    positions: &BTreeMap<String, usize>,
    resolved: &mut BTreeMap<String, TypeResolution>,
    path: &str,
) -> Result<RuleType, PlanError> {
    let (source, required) = source
        .strip_suffix('!')
        .map_or((source, false), |inner| (inner, true));
    if source.is_empty() {
        return Err(PlanError::validation(
            path,
            "rule type reference cannot be empty",
        ));
    }
    let type_ = if source.starts_with('[') || source.ends_with(']') {
        let Some(inner) = source
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        else {
            return Err(PlanError::validation(
                path,
                format!("invalid rule type reference `{source}`"),
            ));
        };
        RuleType::List(Box::new(resolve_declared_type_ref(
            inner,
            declarations,
            positions,
            resolved,
            path,
        )?))
    } else if let Some(scalar) = scalar_rule_type(source) {
        scalar
    } else if positions.contains_key(source) {
        resolve_declared_rule_type(source, declarations, positions, resolved)?
    } else {
        return Err(PlanError::validation(
            path,
            format!("unknown declared rule type `{source}`"),
        ));
    };
    Ok(if required {
        type_
    } else {
        RuleType::nullable(type_)
    })
}

fn scalar_rule_type(source: &str) -> Option<RuleType> {
    match source {
        "bool" => Some(RuleType::Bool),
        "string" => Some(RuleType::String),
        "int" => Some(RuleType::Int),
        "bigint" => Some(RuleType::Int64),
        "decimal" => Some(RuleType::Decimal),
        "uuid" => Some(RuleType::Uuid),
        "date" => Some(RuleType::Date),
        "timestamp" | "timestamptz" => Some(RuleType::Timestamp),
        _ => None,
    }
}

fn is_scalar_rule_type_name(source: &str) -> bool {
    scalar_rule_type(source).is_some()
}
